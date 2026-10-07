// SPDX-License-Identifier: GPL-3.0-or-later
//! Install-gated fastfile tests; they skip when the original install is absent.

use assets::zone::gfx::{Material, TextureSource};
use assets::zone::{Asset, Block, Consumer, KeepAll, XAssetType, Zone};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

fn zone_dir() -> Option<PathBuf> {
    let root = std::env::var_os("COD4_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../COD4")));
    let dir = root.join("zone/english");
    if dir.is_dir() {
        Some(dir)
    } else {
        eprintln!("skipping: no original install at {}", dir.display());
        None
    }
}

fn zones(dir: &PathBuf) -> Vec<(String, PathBuf)> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            (p.extension()? == "ff").then(|| (p.file_stem().unwrap().to_string_lossy().into(), p))
        })
        .collect();
    v.sort();
    v
}

fn open(p: &PathBuf) -> Zone<'static> {
    Zone::open(BufReader::new(File::open(p).unwrap())).unwrap()
}

fn count(c: &[usize], t: XAssetType) -> usize {
    c[t as usize]
}

#[test]
fn every_zone_lists_its_assets() {
    let Some(dir) = zone_dir() else { return };
    let all = zones(&dir);
    let mut total = [0usize; XAssetType::COUNT];
    for (name, p) in &all {
        let z = open(p);
        let c = z.counts();
        assert_eq!(c.iter().sum::<usize>(), z.asset_types().len(), "{name}");
        for (t, n) in total.iter_mut().zip(c) {
            *t += n;
        }
        match name.as_str() {
            "common_mp" => {
                assert_eq!(z.asset_types().len(), 2595);
                assert_eq!(z.script_strings().len(), 591);
                assert_eq!(z.header().size, 41_520_538);
                assert_eq!(z.header().block_sizes[Block::Virtual as usize], 30_188_532);
                for (t, n) in [
                    (XAssetType::TechniqueSet, 154),
                    (XAssetType::Material, 9),
                    (XAssetType::XAnimParts, 1125),
                    (XAssetType::XModel, 597),
                    (XAssetType::LightDef, 1),
                    (XAssetType::MenuList, 34),
                    (XAssetType::Weapon, 116),
                    (XAssetType::Fx, 281),
                    (XAssetType::ImpactFx, 1),
                    (XAssetType::RawFile, 277),
                ] {
                    assert_eq!(count(&c, t), n, "common_mp {}", t.name());
                }
            }
            "mp_crash" => {
                assert_eq!(z.asset_types().len(), 823);
                assert_eq!(z.script_strings().len(), 372);
                assert_eq!(z.header().size, 63_106_889);
                for (t, n) in [
                    (XAssetType::TechniqueSet, 226),
                    (XAssetType::Material, 1),
                    (XAssetType::XAnimParts, 1),
                    (XAssetType::XModel, 297),
                    (XAssetType::Sound, 149),
                    (XAssetType::ClipmapPvs, 1),
                    (XAssetType::ComWorld, 1),
                    (XAssetType::GameWorldMp, 1),
                    (XAssetType::GfxWorld, 1),
                    (XAssetType::LightDef, 2),
                    (XAssetType::Fx, 135),
                    (XAssetType::ImpactFx, 1),
                    (XAssetType::RawFile, 7),
                ] {
                    assert_eq!(count(&c, t), n, "mp_crash {}", t.name());
                }
            }
            _ => {}
        }
    }
    if all.len() == 72 {
        for (t, n) in [
            (XAssetType::Sound, 81_402),
            (XAssetType::XModel, 14_281),
            (XAssetType::Localize, 10_920),
            (XAssetType::XAnimParts, 10_464),
            (XAssetType::TechniqueSet, 9_918),
            (XAssetType::Fx, 6_728),
            (XAssetType::RawFile, 1_987),
            (XAssetType::Material, 1_051),
            (XAssetType::Weapon, 615),
            (XAssetType::Image, 58),
            (XAssetType::LightDef, 50),
            (XAssetType::MenuList, 45),
            (XAssetType::ComWorld, 43),
            (XAssetType::GfxWorld, 43),
            (XAssetType::StringTable, 32),
            (XAssetType::ImpactFx, 29),
            (XAssetType::GameWorldSp, 22),
            (XAssetType::Clipmap, 22),
            (XAssetType::GameWorldMp, 21),
            (XAssetType::ClipmapPvs, 21),
            (XAssetType::Font, 18),
            (XAssetType::PhysPreset, 11),
        ] {
            assert_eq!(count(&total, t), n, "all zones {}", t.name());
        }
    }
}

#[test]
fn load_zones_decode_byte_exact() {
    let Some(dir) = zone_dir() else { return };
    let mut n = 0;
    for (name, p) in zones(&dir) {
        if !name.starts_with("mp_") || !name.ends_with("_load") {
            continue;
        }
        let zone = open(&p);
        let declared = zone.header().block_sizes;
        let mut decoded = 0;
        let st = zone
            .decode(&KeepAll, |_| decoded += 1)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(decoded, 6, "{name}");
        for b in Block::all() {
            let i = b as usize;
            if b == Block::Temp {
                // TEMP is scratch: it only has to stay inside its declared maximum.
                assert!(st.usage.peak[i] <= declared[i], "{name} TEMP");
            } else {
                assert_eq!(st.usage.used[i], declared[i], "{name} {b:?}");
            }
        }
        n += 1;
    }
    assert!(n == 0 || n == 21, "expected 21 load zones, found {n}");
}

/// The 47 multiplayer zones: the two per-map zones, the shared zones and the
/// UI and localized zones.
fn mp_zones(dir: &PathBuf) -> Vec<(String, PathBuf)> {
    zones(dir)
        .into_iter()
        .filter(|(n, _)| {
            n.starts_with("mp_")
                || matches!(
                    n.as_str(),
                    "common_mp"
                        | "code_post_gfx_mp"
                        | "localized_common_mp"
                        | "localized_code_post_gfx_mp"
                        | "ui_mp"
                )
        })
        .collect()
}

#[test]
fn every_mp_zone_consumes_its_whole_stream() {
    let Some(dir) = zone_dir() else { return };
    let all = mp_zones(&dir);
    assert!(
        all.is_empty() || all.len() == 47,
        "expected 47 MP zones, found {}",
        all.len()
    );
    for (name, p) in &all {
        let zone = open(p);
        let declared = zone.header().block_sizes;
        let n = zone.asset_types().len();
        // `decode` fails unless the stream is consumed exactly to its end.
        let st = zone
            .decode(&Consumer::Server, |_| {})
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(st.assets, n, "{name}");
        for b in Block::all() {
            let i = b as usize;
            assert!(st.usage.peak[i] <= declared[i], "{name} {b:?}");
        }
    }
}

fn assert_no_pixels(what: &str, m: &Material) {
    let mut images = Vec::new();
    for t in m.textures.iter() {
        match &t.source {
            TextureSource::Image(i) => images.extend(i.iter()),
            TextureSource::Water(w) => {
                if let Some(w) = w {
                    images.extend(w.image.iter());
                    assert!(w.h0.is_empty() && w.w_term.is_empty(), "{what}: water grid");
                }
            }
        }
    }
    for i in images {
        if let Some(def) = &i.load_def {
            assert!(def.data.is_empty(), "{what}: image {:?} has texels", i.name);
        }
    }
    assert!(m.technique_set.is_none(), "{what}: technique set kept");
}

#[test]
fn server_decode_keeps_no_image_pixels_or_render_bulk() {
    let Some(dir) = zone_dir() else { return };
    let (mut materials, mut models) = (0, 0);
    for (name, p) in mp_zones(&dir) {
        open(&p)
            .decode(&Consumer::Server, |a| match a {
                Asset::Material(m) => {
                    materials += 1;
                    assert_no_pixels(&name, &m);
                }
                Asset::XModel(x) => {
                    models += 1;
                    for m in x.materials.iter().flatten() {
                        assert_no_pixels(&name, m);
                    }
                    for s in x.surfs.iter() {
                        assert!(s.verts.is_empty() && s.tri_indices.is_empty(), "{name}");
                        assert!(s.blends.is_empty(), "{name}");
                        for v in s.vert_list.iter() {
                            assert!(v.collision_tree.is_none(), "{name}");
                        }
                    }
                }
                Asset::Weapon(w) => {
                    for m in [
                        &w.hud_icon,
                        &w.ammo_counter_icon,
                        &w.reticle_center,
                        &w.reticle_side,
                        &w.overlay_material,
                        &w.overlay_material_low_res,
                        &w.kill_icon,
                        &w.dpad_icon,
                    ]
                    .into_iter()
                    .flatten()
                    {
                        assert_no_pixels(&name, m);
                    }
                }
                Asset::Image(_) | Asset::GfxWorld(_) => panic!("{name}: renderer asset kept"),
                _ => {}
            })
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    eprintln!("checked {materials} materials, {models} models");
}
