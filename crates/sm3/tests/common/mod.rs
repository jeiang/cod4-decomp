// SPDX-License-Identifier: GPL-3.0-only
// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use naga::valid::{Capabilities, ValidationFlags, Validator};
use sm3::blob_len;
use std::{collections::BTreeSet, fs, io::Read, path::PathBuf};

/// Parse WGSL with naga and validate it with no optional capabilities (the WebGL2-compatible floor).
pub fn validate(wgsl: &str) -> Result<(), String> {
    let m = naga::front::wgsl::parse_str(wgsl).map_err(|e| e.emit_to_string(wgsl))?;
    Validator::new(ValidationFlags::all(), Capabilities::empty())
        .validate(&m)
        .map(|_| ())
        .map_err(|e| format!("{:?}", e.into_inner()))
}

pub fn install() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("COD4_PATH").unwrap_or_else(|| "./COD4".into()));
    p.join("zone/english").is_dir().then_some(p)
}

/// Fastfile container: `IWffu100`, version 5, one zlib stream.
pub fn inflate_zone(path: &std::path::Path) -> Vec<u8> {
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
pub fn scan_blobs(zone: &[u8]) -> BTreeSet<Vec<u8>> {
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
pub fn is_mp_zone(p: &std::path::Path) -> bool {
    let s = p.file_stem().unwrap().to_string_lossy();
    s.starts_with("mp_") || s.ends_with("_mp")
}

/// Unique SM3 blobs of every MP zone of the install.
pub fn mp_corpus(root: &std::path::Path) -> BTreeSet<Vec<u8>> {
    let mut zones: Vec<_> = fs::read_dir(root.join("zone/english"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ff") && is_mp_zone(p))
        .collect();
    zones.sort();
    let mut unique = BTreeSet::new();
    for z in &zones {
        unique.extend(scan_blobs(&inflate_zone(z)));
    }
    unique
}
