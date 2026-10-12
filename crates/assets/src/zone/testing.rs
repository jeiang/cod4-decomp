// SPDX-License-Identifier: GPL-3.0-only
//! Fastfiles made up for tests of what reads them (a mod's `mod.ff`).

use super::XAssetType;
use flate2::{Compression, write::ZlibEncoder};
use std::io::Write;

/// A fastfile holding the rawfiles `(name, text)`, as a mod's `mod.ff` carries its scripts and menu files.
pub fn rawfile_zone(files: &[(&str, &str)]) -> Vec<u8> {
    let le = |v: &mut Vec<u8>, w: &[u32]| w.iter().for_each(|x| v.extend(x.to_le_bytes()));
    let mut body = Vec::new();
    le(&mut body, &[0, 0, files.len() as u32, 0xFFFF_FFFF]); // XAssetList
    for _ in files {
        le(&mut body, &[XAssetType::RawFile as u32, 0xFFFF_FFFF]);
    }
    for (name, text) in files {
        le(&mut body, &[0xFFFF_FFFF, text.len() as u32, 0xFFFF_FFFF]);
        body.extend(name.as_bytes());
        body.push(0);
        body.extend(text.as_bytes());
        body.push(0);
    }
    let mut x = Vec::new();
    // Size, external size, then the nine block sizes (temp first, virtual fifth).
    le(
        &mut x,
        &[body.len() as u32, 0, 4096, 0, 0, 0, 4096, 0, 0, 0, 0],
    );
    x.extend(body);
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    z.write_all(&x).unwrap();
    let mut out = b"IWffu100".to_vec();
    out.extend(5u32.to_le_bytes());
    out.extend(z.finish().unwrap());
    out
}
