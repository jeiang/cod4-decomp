// SPDX-License-Identifier: GPL-3.0-or-later
//! Compiles every stock MP script from an original install. Skips without `COD4_PATH`.
//!
//! Rawfile records are found by byte scan until the zone decoder (#29) lands:
//! `u32 -1, u32 len, u32 -1, name\0, text[len], \0`.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::Instant;

use gsc::{Builtins, ErrorKind, Options, compile};

fn inflate_zone(path: &Path) -> Vec<u8> {
    let raw = std::fs::read(path).unwrap();
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(&raw[12..])
        .read_to_end(&mut out)
        .unwrap();
    out
}

/// Finds `.gsc` rawfile records in an inflated zone stream.
fn scan_gsc(zone: &[u8], out: &mut BTreeMap<String, String>) {
    let mut i = 0;
    while let Some(p) = zone[i..].windows(5).position(|w| w == b".gsc\0") {
        let end = i + p + 4; // NUL position
        i = end + 1;
        let mut start = end;
        while start > 0 && (0x20..0x7f).contains(&zone[start - 1]) {
            start -= 1;
        }
        if start < 12
            || zone[start - 12..start - 8] != [0xff; 4]
            || zone[start - 4..start] != [0xff; 4]
        {
            continue;
        }
        let len = u32::from_le_bytes(zone[start - 8..start - 4].try_into().unwrap()) as usize;
        let Some(text) = zone.get(end + 1..end + 1 + len + 1) else {
            continue;
        };
        if text[len] != 0 {
            continue;
        }
        let name = String::from_utf8_lossy(&zone[start..end]).to_string();
        let text = String::from_utf8_lossy(&text[..len]).to_string();
        if let Some(old) = out.insert(name.clone(), text.clone()) {
            assert_eq!(old, text, "{name} differs between zones");
        }
    }
}

fn stock_mp_scripts() -> Option<BTreeMap<String, String>> {
    let dir = Path::new(&std::env::var_os("COD4_PATH")?).join("zone/english");
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).ok()? {
        let path = e.unwrap().path();
        let stem = path.file_stem()?.to_str()?.to_string();
        let mp = stem == "common_mp" || (stem.starts_with("mp_") && !stem.ends_with("_load"));
        if mp && path.extension()? == "ff" {
            scan_gsc(&inflate_zone(&path), &mut out);
        }
    }
    Some(out)
}

#[test]
fn all_stock_mp_scripts_compile() {
    let Some(scripts) = stock_mp_scripts() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    assert_eq!(scripts.len(), 210, "research/gsc counts 210 MP scripts");
    let sources: Vec<(&str, &str)> = scripts
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    let builtins = Builtins::stock_mp();
    for developer in [false, true] {
        let t = Instant::now();
        let result = compile(&sources, &builtins, Options { developer });
        let dt = t.elapsed();
        match result {
            Ok(p) => eprintln!(
                "developer={developer}: {} files, {} functions, {} bytes of code, {} strings, {dt:?}",
                p.files.len(),
                p.functions.len(),
                p.functions.iter().map(|f| f.code.len()).sum::<usize>(),
                p.strings.len(),
            ),
            Err(errs) => {
                for e in errs.iter().take(50) {
                    eprintln!("{e}");
                }
                let unknown: std::collections::BTreeSet<_> = errs
                    .iter()
                    .filter(|e| e.kind == ErrorKind::UnknownFunction)
                    .map(|e| e.message.as_str())
                    .collect();
                panic!("{} errors; unknown names: {unknown:?}", errs.len());
            }
        }
    }
}
