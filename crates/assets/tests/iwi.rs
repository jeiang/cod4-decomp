// SPDX-License-Identifier: GPL-3.0-or-later
//! Install-gated IWI tests; they skip when the original install is absent.

use assets::iwi::{self, Format, Header, Image, Texels};
use assets::vfs::{NodeKind, Vfs};
use std::path::PathBuf;

fn install() -> Option<PathBuf> {
    let root = std::env::var_os("COD4_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../COD4")));
    if root.join("main").is_dir() {
        Some(root)
    } else {
        eprintln!("skipping: no original install at {}", root.display());
        None
    }
}

/// Every `images/*.iwi` entry of every IWD, shadowed duplicates included.
fn all_iwi(vfs: &Vfs) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for node in vfs.nodes() {
        if let NodeKind::Iwd { iwd, path } = &node.kind {
            for e in iwd.entries() {
                let n = e.name.to_ascii_lowercase();
                if n.starts_with("images/") && n.ends_with(".iwi") {
                    let b = iwd.read(e).unwrap();
                    out.push((format!("{}:{}", path.display(), e.name), b));
                }
            }
        }
    }
    out
}

#[test]
fn every_stock_iwi_loads() {
    let Some(root) = install() else { return };
    let vfs = Vfs::open_stock(&root, 0).unwrap();
    let files = all_iwi(&vfs);
    assert!(!files.is_empty());
    let mut per_format = std::collections::BTreeMap::new();
    for (i, (name, bytes)) in files.iter().enumerate() {
        let header = Header::parse(bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            header.file_size_for_picmip[0] as usize,
            bytes.len(),
            "{name}"
        );
        let img = Image::parse(bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(img.mip_count(), header.mip_count(), "{name}");
        assert_eq!(img.level(0, 0).width, header.width as usize, "{name}");
        // CPU block decode is slow unoptimized: exercise every 16th file fully.
        for l in img
            .levels
            .iter()
            .filter(|_| i % 16 == 0 || img.texels == Texels::Rgba8)
        {
            let px = l.to_rgba8(img.texels);
            assert_eq!(px.len(), l.width * l.height * 4, "{name}");
        }
        // The picmip size table is the prefix needed to drop the largest mips.
        if !header.format.is_wavelet() && header.allows_picmip() {
            for p in 1..header.mip_count().min(4) {
                let cut = header.file_size_for_picmip[p] as usize;
                let img = Image::parse_picmip(&bytes[..cut], p)
                    .unwrap_or_else(|e| panic!("{name} picmip {p}: {e}"));
                assert_eq!(
                    img.level(0, 0).width,
                    (header.width as usize >> p).max(1),
                    "{name}"
                );
            }
        }
        *per_format
            .entry(format!("{:?}", header.format))
            .or_insert(0usize) += 1;
    }
    eprintln!("{} IWI files loaded: {per_format:?}", files.len());
}

#[test]
fn loads_by_name_through_the_vfs() {
    let Some(root) = install() else { return };
    let vfs = Vfs::open_stock(&root, 0).unwrap();
    let img = iwi::load(&vfs, "specialty_rof_256").unwrap();
    assert_eq!(img.header.format, Format::WaveletBgra);
    assert_eq!((img.level(0, 0).width, img.level(0, 0).height), (256, 256));
    assert_eq!(img.level(img.mip_count() - 1, 0).width, 1);
    assert!(matches!(
        iwi::load(&vfs, "no_such_image_xyz"),
        Err(iwi::Error::NotFound(_))
    ));
}

/// Wavelet levels are predicted from the half-size level, so a correct decode
/// has every mip (16 px and up) near the box-filtered mip above it (art detail makes ~15 normal; garbage gives 60+; normal maps differ most) and no degenerate
/// (flat) content; a wrong code table or channel mapping breaks both.
#[test]
fn wavelet_images_are_self_consistent() {
    let Some(root) = install() else { return };
    let vfs = Vfs::open_stock(&root, 0).unwrap();
    let mut n = 0;
    let mut worst = 0f64;
    for (name, bytes) in all_iwi(&vfs) {
        let h = Header::parse(&bytes).unwrap();
        if !h.format.is_wavelet() {
            continue;
        }
        n += 1;
        let img = Image::parse(&bytes).unwrap();
        assert_eq!(img.texels, Texels::Rgba8);
        let top = img.level(0, 0);
        let mut hist = [0u32; 256];
        for px in top.data.as_chunks::<4>().0 {
            hist[px[0] as usize] += 1;
            hist[px[3] as usize] += 1;
        }
        let distinct = hist.iter().filter(|&&c| c > 0).count();
        assert!(distinct >= 2, "{name}: flat image");
        for mip in 0..img.mip_count() - 1 {
            let (big, small) = (img.level(mip, 0), img.level(mip + 1, 0));
            if small.width < 16 || small.height < 16 {
                continue;
            }
            let mut sq = 0f64;
            let mut cnt = 0f64;
            for y in 0..small.height {
                for x in 0..small.width {
                    for c in 0..4 {
                        let at =
                            |xx: usize, yy: usize| big.data[(yy * big.width + xx) * 4 + c] as f64;
                        let avg = (at(2 * x, 2 * y)
                            + at(2 * x + 1, 2 * y)
                            + at(2 * x, 2 * y + 1)
                            + at(2 * x + 1, 2 * y + 1))
                            / 4.0;
                        let d = avg - small.data[(y * small.width + x) * 4 + c] as f64;
                        sq += d * d;
                        cnt += 1.0;
                    }
                }
            }
            let rmse = (sq / cnt).sqrt();
            if rmse > worst {
                eprintln!("{name} mip {mip} rmse {rmse:.1}");
            }
            worst = worst.max(rmse);
            assert!(rmse < 30.0, "{name}: mip {mip}->{} rmse {rmse:.1}", mip + 1);
        }
    }
    assert!(n > 0);
    eprintln!("{n} wavelet images checked, worst mip rmse {worst:.2}");
}
