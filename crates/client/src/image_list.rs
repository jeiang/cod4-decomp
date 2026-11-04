// SPDX-License-Identifier: GPL-3.0-only
// Image-list formatting translated from KisakCOD gfx_d3d/r_image.cpp (R_ImageList_f,
// imagecompare); copyright KisakCOD contributors, GPL-3.0.
//! The fastfile image inventory, independent of GPU texture residency.

use assets::iwi::{Format, Header};
use assets::vfs::Vfs;
use assets::zone::gfx::GfxImage;
use std::collections::BTreeMap;
use std::fmt::Write;

const TRACKS: [&str; 10] = [
    "misc", "debug", "$tex+?", "ui", "lmap", "light", "f/x", "hud", "model", "world",
];
const PLATFORMS: [&str; 2] = ["current", "min_pc"];

/// Stock fastfile rows and totals. Later occurrences of a name replace earlier ones,
/// so callers combine UI images before the active map/content images. No image texels
/// are decoded: external images need only the existing IWI header parser.
pub fn format<'a>(images: impl IntoIterator<Item = &'a GfxImage>, vfs: &Vfs) -> Vec<String> {
    let mut active = BTreeMap::new();
    for image in images {
        if let Some(name) = image.name.as_deref() {
            let key = name.trim_start_matches(',').to_ascii_lowercase();
            if !name.starts_with(',') || !active.contains_key(&key) {
                active.insert(key, image);
            }
        }
    }
    let mut images: Vec<_> = active.into_values().collect();
    images.sort_by_key(|image| (image.track, image.card_memory[0]));
    let mut lines = vec!["\n-fmt- -dimension-currentmin_pc  --name-------".to_owned()];
    let mut totals = [0i64; 2];
    for image in &images {
        let name = image
            .name
            .as_deref()
            .unwrap_or_default()
            .trim_start_matches(',');
        let pixel_format = match pixel_format(image, vfs) {
            Ok(format) => format,
            Err(error) => {
                lines.push(format!("WARNING: imagelist image '{name}': {error}"));
                "??????"
            }
        };
        let track = TRACKS
            .get(usize::from(image.track))
            .copied()
            .unwrap_or("unknown");
        let mut line = format!(
            "{:4} x {:<4} {pixel_format}  {track}",
            image.width, image.height
        );
        for (total, bytes) in totals.iter_mut().zip(image.card_memory) {
            let kb = f64::from(bytes) / 1024.0;
            if kb >= 10.0 {
                write!(line, "{kb:7.0}k").expect("writing to a String cannot fail");
            } else {
                write!(line, "{kb:7.1}k").expect("writing to a String cannot fail");
            }
            *total += i64::from(bytes);
        }
        line.push_str("  ");
        line.push_str(name);
        lines.push(line);
    }
    lines.push(" ---------".to_owned());
    lines.push(format!(" {} total images", images.len()));
    for (bytes, platform) in totals.into_iter().zip(PLATFORMS) {
        lines.push(format!(
            " {:5.1} MB {platform} total image size",
            bytes as f64 / 1048576.0
        ));
    }
    lines.push("Related commands: meminfo, imagelist, gfx_world, gfx_model, cg_drawfps, com_statmon, tempmeminfo".to_owned());
    lines
}

fn pixel_format(image: &GfxImage, vfs: &Vfs) -> Result<&'static str, String> {
    if let Some(def) = &image.load_def {
        return d3d_format(def.format)
            .ok_or_else(|| format!("unsupported D3D image format {}", def.format));
    }
    let name = image
        .name
        .as_deref()
        .unwrap_or_default()
        .trim_start_matches(',');
    let path = format!("images/{name}.iwi");
    let bytes = vfs
        .read(&path)
        .map_err(|error| format!("{path}: {error}"))?
        .ok_or_else(|| format!("{path} not found"))?;
    let header = Header::parse(&bytes).map_err(|error| format!("{path}: {error}"))?;
    Ok(iwi_format(header.format))
}

fn d3d_format(format: i32) -> Option<&'static str> {
    Some(match format {
        0x3154_5844 => "DXT1  ",
        0x3354_5844 => "DXT3  ",
        0x3554_5844 => "DXT5  ",
        114 => "R32F  ",
        51 => "AL16  ",
        21 => "RGBA32",
        22 => "RGB32 ",
        28 => "A8    ",
        50 => "L8    ",
        _ => return None,
    })
}

fn iwi_format(format: Format) -> &'static str {
    match format {
        Format::Bgra8 | Format::WaveletBgra => "RGBA32",
        Format::Bgr8 | Format::WaveletBgr => "RGB32 ",
        Format::La8 | Format::WaveletLa => "AL16  ",
        Format::L8 | Format::WaveletL => "L8    ",
        Format::A8 | Format::WaveletA => "A8    ",
        Format::Dxt1 => "DXT1  ",
        Format::Dxt3 => "DXT3  ",
        Format::Dxt5 => "DXT5  ",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assets::zone::gfx::ImageLoadDef;
    use std::sync::Arc;

    #[test]
    fn external_image_uses_only_the_iwi_header_from_the_search_path() {
        let root = std::env::temp_dir().join(format!(
            "cod4-imagelist-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("main/images")).unwrap();
        // Deliberately no texel payload: listing must not invoke Image::parse.
        let mut header = [0u8; 28];
        header[..4].copy_from_slice(b"IWi\x06");
        header[4] = Format::Dxt5 as u8;
        header[6..8].copy_from_slice(&64u16.to_le_bytes());
        header[8..10].copy_from_slice(&32u16.to_le_bytes());
        header[10..12].copy_from_slice(&1u16.to_le_bytes());
        std::fs::write(root.join("main/images/external.iwi"), header).unwrap();
        let mut builder = assets::vfs::Builder::new(&root);
        builder.add_game_dir("main").unwrap();
        let vfs = builder.finish(0);
        let mut image = image("external", 9, [1024; 2]);
        image.load_def = None;
        assert_eq!(pixel_format(&image, &vfs).unwrap(), "DXT5  ");
        std::fs::remove_dir_all(root).unwrap();
    }

    fn image(name: &str, track: u8, memory: [i32; 2]) -> GfxImage {
        GfxImage {
            map_type: 3,
            picmip: [0; 2],
            no_picmip: false,
            semantic: 2,
            track,
            card_memory: memory,
            width: 64,
            height: 32,
            depth: 1,
            category: 0,
            delay_load_pixels: false,
            name: Some(name.into()),
            load_def: Some(Arc::new(ImageLoadDef {
                level_count: 1,
                flags: 0,
                dimensions: [64, 32, 1],
                format: 0x3154_5844,
                data: vec![],
            })),
        }
    }

    #[test]
    fn stock_rows_sort_deduplicate_and_total_both_platforms() {
        let vfs = assets::vfs::Builder::new(std::path::Path::new(".")).finish(0);
        let old = image("old", 9, [1024, 2048]);
        let replaced = image("OLD", 3, [10240, 20480]);
        let first = image("first", 3, [1024, 2048]);
        let lines = format([&old, &replaced, &first], &vfs);
        assert_eq!(lines[0], "\n-fmt- -dimension-currentmin_pc  --name-------");
        assert_eq!(lines[1], "  64 x 32   DXT1    ui    1.0k    2.0k  first");
        assert_eq!(lines[2], "  64 x 32   DXT1    ui     10k     20k  OLD");
        assert_eq!(lines[4], " 2 total images");
        assert_eq!(lines[5], "   0.0 MB current total image size");
        assert_eq!(lines[6], "   0.0 MB min_pc total image size");
        assert!(lines[7].starts_with("Related commands: meminfo, imagelist"));
    }

    #[test]
    fn iwi_wavelets_use_their_decoded_gpu_format_without_decoding_pixels() {
        for (source, expected) in [
            (Format::WaveletBgra, "RGBA32"),
            (Format::WaveletBgr, "RGB32 "),
            (Format::WaveletLa, "AL16  "),
            (Format::WaveletL, "L8    "),
            (Format::WaveletA, "A8    "),
        ] {
            assert_eq!(iwi_format(source), expected);
        }
        assert_eq!(d3d_format(114), Some("R32F  "));
        assert_eq!(d3d_format(-1), None);
    }
}
