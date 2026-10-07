// SPDX-License-Identifier: GPL-3.0-or-later
//! Install-gated: every stock SM3 blob must translate and validate with naga. Skips without `COD4_PATH`.
//! Run with `--nocapture` to see the per-zone translation times.
mod common;
use sm3::{Options, Stage, blob_len, translate};
use std::{collections::BTreeSet, fs, io::Read, path::PathBuf, time::Instant};

fn install() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("COD4_PATH").unwrap_or_else(|| "./COD4".into()));
    p.join("zone/english").is_dir().then_some(p)
}

/// Fastfile container: `IWffu100`, version 5, one zlib stream.
fn inflate_zone(path: &std::path::Path) -> Vec<u8> {
    let raw = fs::read(path).unwrap();
    assert!(
        raw.starts_with(b"IWffu100") && raw[8..12] == 5u32.to_le_bytes(),
        "{path:?}"
    );
    let mut out = vec![];
    flate2::read::ZlibDecoder::new(&raw[12..])
        .read_to_end(&mut out)
        .unwrap();
    out
}

/// Byte scan for `vs_3_0` / `ps_3_0` version tokens whose token stream ends with an END token and has a CTAB.
fn scan_blobs(zone: &[u8]) -> BTreeSet<Vec<u8>> {
    let mut found = BTreeSet::new();
    for (p, w) in zone.windows(4).enumerate() {
        if w[0] == 0
            && w[1] == 3
            && matches!(w[2], 0xFE | 0xFF)
            && w[3] == 0xFF
            && let Some(n) = blob_len(&zone[p..])
        {
            found.insert(zone[p..p + n].to_vec());
        }
    }
    found
}

/// The multiplayer zones: maps (`mp_*`), `*_mp` and their localized variants. Single-player zones, which a full
/// install also carries, are outside the corpus.
fn is_mp_zone(p: &std::path::Path) -> bool {
    let s = p.file_stem().unwrap().to_string_lossy();
    s.starts_with("mp_") || s.ends_with("_mp")
}

#[test]
fn stock_sm3_blobs_translate_and_validate() {
    let Some(root) = install() else {
        eprintln!("skipped: no CoD4 install (set COD4_PATH)");
        return;
    };
    let mut zones: Vec<_> = fs::read_dir(root.join("zone/english"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ff"))
        .collect();
    zones.sort();
    let opts = Options::default();
    let mut unique = BTreeSet::new();
    let mut failures = vec![];
    for z in &zones {
        if !is_mp_zone(z) {
            continue;
        }
        let blobs = scan_blobs(&inflate_zone(z));
        let mut wgsl = vec![];
        let t0 = Instant::now();
        for b in &blobs {
            match translate(b, &opts) {
                Ok(t) => wgsl.push(t.wgsl),
                Err(e) => failures.push(format!("{}: translate: {e}", z.display())),
            }
        }
        let dt = t0.elapsed();
        for w in &wgsl {
            if let Err(e) = common::validate(w) {
                failures.push(format!("{}: naga: {e}", z.display()));
            }
        }
        if !blobs.is_empty() {
            eprintln!(
                "{:<28} {:>4} blobs  translate {:>8.2?}  ({:.1?}/blob)",
                z.file_name().unwrap().to_string_lossy(),
                blobs.len(),
                dt,
                dt / blobs.len() as u32
            );
        }
        unique.extend(blobs);
    }
    assert!(
        failures.is_empty(),
        "{} failures, first: {:#?}",
        failures.len(),
        &failures[..failures.len().min(5)]
    );
    let vs = unique
        .iter()
        .filter(|b| sm3::parse(b).unwrap().stage == Stage::Vertex)
        .count();
    eprintln!(
        "unique blobs: {} ({vs} VS + {} PS)",
        unique.len(),
        unique.len() - vs
    );
    // Full MP set from the original install: 114 VS + 548 PS.
    assert_eq!((vs, unique.len() - vs), (114, 548));
}
