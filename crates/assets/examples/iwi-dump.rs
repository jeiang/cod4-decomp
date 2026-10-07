// SPDX-License-Identifier: GPL-3.0-or-later
//! `iwi-dump <install> <image name> <out.png> [mip] [face]`: decode one stock
//! image to a PNG (RGBA8) for eyeballing.

use assets::iwi;
use assets::vfs::Vfs;
use flate2::{Compression, Crc, write::ZlibEncoder};
use std::io::Write;

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend((body.len() as u32).to_be_bytes());
    out.extend(kind);
    out.extend(body);
    let mut crc = Crc::new();
    crc.update(kind);
    crc.update(body);
    out.extend(crc.sum().to_be_bytes());
}

fn png(w: usize, h: usize, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((w * 4 + 1) * h);
    for row in rgba.chunks_exact(w * 4) {
        raw.push(0);
        raw.extend(row);
    }
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    z.write_all(&raw).unwrap();
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend((w as u32).to_be_bytes());
    ihdr.extend((h as u32).to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z.finish().unwrap());
    chunk(&mut out, b"IEND", &[]);
    out
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let [_, root, name, out] = &a[..4] else {
        eprintln!("usage: iwi-dump <install> <image name> <out.png> [mip] [face]");
        std::process::exit(2);
    };
    let mip = a.get(4).map_or(0, |s| s.parse().unwrap());
    let face = a.get(5).map_or(0, |s| s.parse().unwrap());
    let vfs = Vfs::open_stock(root.as_ref(), 0).unwrap();
    let img = iwi::load(&vfs, name).unwrap();
    let l = img.level(mip, face);
    eprintln!(
        "{name}: {:?} {}x{} mips={} faces={}",
        img.header.format,
        l.width,
        l.height,
        img.mip_count(),
        img.faces
    );
    std::fs::write(out, png(l.width, l.height, &l.to_rgba8(img.texels))).unwrap();
}
