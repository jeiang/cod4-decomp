// SPDX-License-Identifier: GPL-3.0-or-later
//! Install-gated: every stock SM3 blob must translate and validate with naga. Skips without `COD4_PATH`.
//! Run with `--nocapture` to see the per-zone translation times.
mod common;
use common::{inflate_zone, install, is_mp_zone, scan_blobs};
use sm3::{Options, Stage, translate};
use std::{collections::BTreeSet, fs, time::Instant};

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
