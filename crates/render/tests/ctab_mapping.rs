// SPDX-License-Identifier: GPL-3.0-only
//! Install-gated: every code constant and code sampler a stock technique binds must be named, in the shader's CTAB,
//! by something `codeconst` maps back to the same id. Skips without an install.

use assets::zone::gfx::ArgValue;
use assets::zone::{Asset, KeepAll, Zone};
use render::codeconst;
use std::collections::BTreeSet;

#[test]
fn ctab_names_map_to_the_ids_the_techniques_bind() {
    let Some(root) = std::env::var_os("COD4_PATH").map(std::path::PathBuf::from) else {
        eprintln!("skipped: COD4_PATH not set");
        return;
    };
    let mut bad = BTreeSet::new();
    let mut checked = 0;
    for zone in ["common_mp", "mp_crash"] {
        let path = root.join("zone/english").join(format!("{zone}.ff"));
        let Ok(file) = std::fs::File::open(path) else {
            eprintln!("skipped: no {zone}.ff");
            return;
        };
        let z = Zone::open(std::io::BufReader::new(file)).unwrap();
        z.decode(&KeepAll, |a| {
            let Asset::TechniqueSet(set) = a else { return };
            for tech in set.techniques.iter().flatten() {
                for pass in &tech.passes {
                    for (shader, ps) in [(&pass.vertex_shader, false), (&pass.pixel_shader, true)] {
                        let Some(sh) = shader else { continue };
                        let bytes: Vec<u8> =
                            sh.program.iter().flat_map(|t| t.to_le_bytes()).collect();
                        let Ok(tr) = sm3::translate(&bytes, &sm3::Options::default()) else {
                            continue;
                        };
                        for a in &pass.args {
                            let in_stage =
                                matches!((a.kind, ps), (3, false) | (5, true) | (4, true));
                            if !in_stage {
                                continue;
                            }
                            match (&a.value, a.kind) {
                                (ArgValue::CodeConst { index, .. }, _) => {
                                    let Some(name) = tr.reflection.constant_name(u32::from(a.dest))
                                    else {
                                        continue;
                                    };
                                    checked += 1;
                                    let got = codeconst::from_ctab_name(name);
                                    // Arrays (filterTap) resolve to their first element.
                                    let same_matrix = |g: u32| {
                                        g >= codeconst::FIRST_MATRIX
                                            && *index >= codeconst::FIRST_MATRIX as u16
                                            && (g - codeconst::FIRST_MATRIX) / 4
                                                == (u32::from(*index) - codeconst::FIRST_MATRIX) / 4
                                    };
                                    let ok = got == Some(u32::from(*index))
                                        || got.is_some_and(|g| {
                                            same_matrix(g)
                                                || (name.starts_with("filterTap")
                                                    && g <= u32::from(*index))
                                        });
                                    if !ok {
                                        bad.insert(format!(
                                            "const {name}: id {index:#x}, mapped {got:?}"
                                        ));
                                    }
                                }
                                (ArgValue::CodeSampler(id), _) => {
                                    let Some(name) = tr.reflection.sampler_name(u32::from(a.dest))
                                    else {
                                        continue;
                                    };
                                    // Render targets reuse the generic material sampler name; only the argument names them.
                                    if name == "colorMapSampler" {
                                        continue;
                                    }
                                    checked += 1;
                                    let got = codeconst::texture_from_ctab_name(name);
                                    if got != Some(*id) {
                                        bad.insert(format!(
                                            "sampler {name}: id {id}, mapped {got:?}"
                                        ));
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        })
        .unwrap();
    }
    assert!(checked > 1000, "only {checked} bindings checked");
    assert!(bad.is_empty(), "unmapped: {bad:#?}");
}
