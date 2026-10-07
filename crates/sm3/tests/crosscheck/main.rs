// SPDX-License-Identifier: GPL-3.0-or-later
//! D3D9-free cross-check of the SM3 -> WGSL translation.
//!
//! Every stock MP SM3 blob is also translated by two independent reference translators run as external programs
//! (`COD4E_MOJOSHADER_DRV`, `COD4E_VKD3D_DRV`; build them with `scripts/build-crosscheck-tools.sh`). The same fixed
//! frame is rendered offscreen with our WGSL and with each reference's SPIR-V (through naga, after a sampler-split
//! pass) for every VS/PS pair, and the readbacks must agree per channel within [`TOLERANCE`].
//! Skips (and passes) when a driver variable, `COD4_PATH` or a GPU adapter is missing.
#[path = "../common/mod.rs"]
mod common;
mod gpu;
mod paths;
mod spvsplit;

use paths::{Path, Prog};
use sm3::{Options, Stage, Translation};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    process::Command,
};

/// Largest accepted per-channel difference (of 255) between any two translators' pixels.
const TOLERANCE: u8 = 2;

fn fnv(b: &[u8]) -> String {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &x in b {
        h = (h ^ u64::from(x)).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn driver(var: &str) -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os(var)?);
    p.is_file().then_some(p)
}

struct Blob {
    hash: String,
    tr: Translation,
}

/// The outputs of every VS and inputs of every PS as D3D (usage, index) sets.
fn sems(r: &[sm3::Semantic]) -> BTreeSet<(u32, u32)> {
    r.iter().map(|s| (s.usage, s.index)).collect()
}

fn run_driver(
    exe: &PathBuf,
    list: &std::path::Path,
    dir: &std::path::Path,
    out: &std::path::Path,
) -> Vec<String> {
    let o = Command::new(exe)
        .arg(list)
        .arg(dir)
        .arg(out)
        .output()
        .unwrap_or_else(|e| panic!("run {exe:?}: {e}"));
    assert!(
        o.status.success(),
        "{exe:?} failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// (max channel difference, differing pixels) between two renders.
fn diff(a: &gpu::Frame, b: &gpu::Frame) -> (u8, usize) {
    let (mut mx, mut n) = (0u8, 0usize);
    for (ta, tb) in a.targets.iter().zip(&b.targets) {
        for (pa, pb) in ta.as_chunks::<4>().0.iter().zip(tb.as_chunks::<4>().0) {
            let m = (0..4).map(|c| pa[c].abs_diff(pb[c])).max().unwrap();
            mx = mx.max(m);
            n += usize::from(m > 0);
        }
    }
    (mx, n)
}

fn covered(f: &gpu::Frame) -> bool {
    f.targets
        .iter()
        .any(|t| t.as_chunks::<4>().0.iter().any(|p| *p != gpu::CLEAR))
}

#[test]
fn translators_agree_on_the_synthetic_frame() {
    let (Some(mojo), Some(vkd3d)) = (driver("COD4E_MOJOSHADER_DRV"), driver("COD4E_VKD3D_DRV"))
    else {
        eprintln!(
            "skipped: set COD4E_MOJOSHADER_DRV and COD4E_VKD3D_DRV to the built drivers (scripts/build-crosscheck-tools.sh)"
        );
        return;
    };
    let Some(root) = common::install() else {
        eprintln!("skipped: no CoD4 install (set COD4_PATH)");
        return;
    };
    let gpu = match gpu::Gpu::new() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("skipped: no GPU adapter ({e})");
            return;
        }
    };
    eprintln!("adapter: {}", gpu.adapter);

    // --- corpus and pairs
    let mut vs: BTreeMap<String, Blob> = BTreeMap::new();
    let mut ps: BTreeMap<String, Blob> = BTreeMap::new();
    let mut bytes: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for b in common::mp_corpus(&root) {
        let tr = sm3::translate(&b, &Options::default()).expect("corpus blob translates");
        let hash = fnv(&b);
        let m = if tr.stage == Stage::Vertex {
            &mut vs
        } else {
            &mut ps
        };
        m.insert(
            hash.clone(),
            Blob {
                hash: hash.clone(),
                tr,
            },
        );
        bytes.insert(hash, b);
    }
    // Pair each PS with the first VS that writes every semantic it reads and whose frame, rendered with our
    // translation, actually covers pixels (a VS that decodes packed vertex data never does with this synthetic
    // input; after three blanks it is skipped). Every VS is also paired with the first PS that fits.
    let fits = |v: &Blob, p: &Blob| {
        sems(&p.tr.reflection.inputs).is_subset(&sems(&v.tr.reflection.outputs))
    };
    let ours_covers = |v: &Blob, p: &Blob| -> bool {
        paths::build(Path::Ours, &v.tr, None)
            .and_then(|pv| Ok((pv, paths::build(Path::Ours, &p.tr, None)?)))
            .and_then(|(pv, pp)| gpu::render(&gpu, &pv, &pp, &v.tr.reflection, &p.tr.reflection))
            .is_ok_and(|f| covered(&f))
    };
    let mut pairs: BTreeSet<(String, String)> = BTreeSet::new();
    let mut blanks: BTreeMap<&str, u32> = BTreeMap::new();
    let mut good: BTreeSet<&str> = BTreeSet::new();
    for p in ps.values() {
        let cands: Vec<&Blob> = vs.values().filter(|v| fits(v, p)).collect();
        let mut pick = cands.first().copied();
        for v in &cands {
            let h = v.hash.as_str();
            if !good.contains(h) && blanks.get(h).is_some_and(|&n| n >= 3) {
                continue;
            }
            if ours_covers(v, p) {
                good.insert(h);
                pick = Some(v);
                break;
            }
            *blanks.entry(h).or_default() += 1;
        }
        if let Some(v) = pick {
            pairs.insert((v.hash.clone(), p.hash.clone()));
        }
    }
    for v in vs.values() {
        if let Some(p) = ps.values().find(|p| fits(v, p)) {
            pairs.insert((v.hash.clone(), p.hash.clone()));
        }
    }

    // --- run the reference translators
    let dir = std::env::temp_dir().join(format!("cod4e-crosscheck-{}", std::process::id()));
    let (blobs, mo, vk) = (dir.join("blobs"), dir.join("mojo"), dir.join("vkd3d"));
    for d in [&blobs, &mo, &vk] {
        fs::create_dir_all(d).unwrap();
    }
    let mut list_pairs = String::new();
    let mut list_shaders = String::new();
    let mut used = BTreeSet::new();
    for (v, p) in &pairs {
        list_pairs += &format!("{v} {p}\n");
        used.insert(("vs", v.clone()));
        used.insert(("ps", p.clone()));
    }
    for (k, h) in &used {
        fs::write(blobs.join(format!("{k}_{h}.bin")), &bytes[h]).unwrap();
        list_shaders += &format!("{k} {h}\n");
    }
    fs::write(dir.join("pairs.txt"), list_pairs).unwrap();
    fs::write(dir.join("shaders.txt"), list_shaders).unwrap();
    let mojo_log = run_driver(&mojo, &dir.join("pairs.txt"), &blobs, &mo);
    let vkd3d_log = run_driver(&vkd3d, &dir.join("shaders.txt"), &blobs, &vk);
    let mut failures: Vec<String> = mojo_log
        .iter()
        .chain(&vkd3d_log)
        .filter(|l| l.contains(" ERR "))
        .cloned()
        .collect();

    // --- render
    struct Stat {
        max: u8,
        over: Vec<String>,
        covered: usize,
    }
    let mut stats: BTreeMap<Path, Stat> = BTreeMap::new();
    let (mut compared, mut ours_covered) = (0, 0);
    for (vh, ph) in &pairs {
        let (v, p) = (&vs[vh], &ps[ph]);
        let mut frames: Vec<(Path, gpu::Frame)> = vec![];
        for path in Path::ALL {
            let spv = |kind: &str, name: String| {
                let f = fs::read(dir.join(kind).join(name));
                f.map_err(|e| e.to_string())
            };
            let progs = (|| -> Result<(Prog, Prog), String> {
                Ok(match path {
                    Path::Ours => (
                        paths::build(path, &v.tr, None)?,
                        paths::build(path, &p.tr, None)?,
                    ),
                    Path::Mojo => (
                        paths::build(
                            path,
                            &v.tr,
                            Some(&spv("mojo", format!("vs_{vh}__{ph}.spv"))?),
                        )?,
                        paths::build(
                            path,
                            &p.tr,
                            Some(&spv("mojo", format!("ps_{ph}__{vh}.spv"))?),
                        )?,
                    ),
                    Path::Vkd3d => (
                        paths::build(path, &v.tr, Some(&spv("vkd3d", format!("vs_{vh}.spv"))?))?,
                        paths::build(path, &p.tr, Some(&spv("vkd3d", format!("ps_{ph}.spv"))?))?,
                    ),
                })
            })();
            let frame = progs.and_then(|(pv, pp)| {
                if std::env::var("CROSSCHECK_DUMP").is_ok_and(|d| format!("{vh} {ph}").contains(&d))
                {
                    eprintln!("== {vh} {ph} {path:?}\n{}\n{}", pv.wgsl, pp.wgsl);
                }
                gpu::render(&gpu, &pv, &pp, &v.tr.reflection, &p.tr.reflection)
            });
            match frame {
                Ok(f) => frames.push((path, f)),
                Err(e) => failures.push(format!("pair {vh} {ph} {}: {e}", path.name())),
            }
        }
        let Some((_, ours)) = frames.iter().find(|(p, _)| *p == Path::Ours) else {
            continue;
        };
        ours_covered += usize::from(covered(ours));
        if !covered(ours) && std::env::var_os("CROSSCHECK_BLANK").is_some() {
            eprintln!("blank {vh} {ph}");
        }
        for (path, f) in frames.iter().filter(|(p, _)| *p != Path::Ours) {
            compared += 1;
            let (mx, n) = diff(ours, f);
            let s = stats.entry(*path).or_insert(Stat {
                max: 0,
                over: vec![],
                covered: 0,
            });
            s.max = s.max.max(mx);
            s.covered += usize::from(covered(f));
            if mx > TOLERANCE {
                s.over
                    .push(format!("pair {vh} {ph}: max diff {mx}, {n} px"));
            }
        }
    }
    if std::env::var_os("CROSSCHECK_KEEP").is_some() {
        eprintln!("kept {}", dir.display());
    } else {
        let _ = fs::remove_dir_all(&dir);
    }
    eprintln!(
        "{} unique blobs ({} VS + {} PS), {} pairs, {compared} ours-vs-reference comparisons, ours covers pixels in {ours_covered} pairs",
        vs.len() + ps.len(),
        vs.len(),
        ps.len(),
        pairs.len()
    );
    for (path, s) in &stats {
        eprintln!(
            "{:<13} max channel diff {}/255 (tolerance {TOLERANCE}), {} pairs over, reference covers pixels in {} pairs",
            path.name(),
            s.max,
            s.over.len(),
            s.covered
        );
        for o in s.over.iter().take(10) {
            eprintln!("  {o}");
        }
    }
    for f in failures.iter().take(10) {
        eprintln!("FAIL {f}");
    }
    let over: usize = stats.values().map(|s| s.over.len()).sum();
    assert!(
        failures.is_empty() && over == 0,
        "{} failures, {over} pairs over tolerance",
        failures.len()
    );
}
