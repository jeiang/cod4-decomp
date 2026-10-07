// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU decode of the three block-compressed formats IWI uses (BC1/2/3, a.k.a.
//! DXT1/3/5) to RGBA8, for adapters without BC texture support.
//!
//! Decoding follows the D3D9 definitions: BC1 switches to 3 colors plus
//! transparent black when `c0 <= c1`; BC2/BC3 colour blocks are always 4-color.

use super::Texels;

/// Bytes per 4x4 block.
pub(super) fn block_bytes(t: Texels) -> usize {
    match t {
        Texels::Bc1 => 8,
        Texels::Bc2 | Texels::Bc3 => 16,
        Texels::Rgba8 => 4,
    }
}

/// Size in bytes of one mip of the given pixel size.
pub(super) fn level_bytes(t: Texels, w: usize, h: usize) -> usize {
    match t {
        Texels::Rgba8 => w * h * 4,
        _ => w.div_ceil(4) * h.div_ceil(4) * block_bytes(t),
    }
}

fn expand565(c: u16) -> [u8; 3] {
    let r = (c >> 11) as u8 & 31;
    let g = (c >> 5) as u8 & 63;
    let b = c as u8 & 31;
    [r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2]
}

fn lerp(a: u8, b: u8, wa: u32, wb: u32, div: u32) -> u8 {
    ((a as u32 * wa + b as u32 * wb) / div) as u8
}

/// Colour block to 16 RGBA texels (row-major).
fn color_block(b: &[u8], four_color_only: bool) -> [[u8; 4]; 16] {
    let c0 = u16::from_le_bytes([b[0], b[1]]);
    let c1 = u16::from_le_bytes([b[2], b[3]]);
    let (p0, p1) = (expand565(c0), expand565(c1));
    let mut pal = [[0u8; 4]; 4];
    pal[0] = [p0[0], p0[1], p0[2], 255];
    pal[1] = [p1[0], p1[1], p1[2], 255];
    if c0 > c1 || four_color_only {
        for i in 0..3 {
            pal[2][i] = lerp(p0[i], p1[i], 2, 1, 3);
            pal[3][i] = lerp(p0[i], p1[i], 1, 2, 3);
        }
        pal[2][3] = 255;
        pal[3][3] = 255;
    } else {
        for i in 0..3 {
            pal[2][i] = lerp(p0[i], p1[i], 1, 1, 2);
        }
        pal[2][3] = 255;
        // pal[3] stays transparent black.
    }
    let bits = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
    let mut out = [[0u8; 4]; 16];
    for (i, px) in out.iter_mut().enumerate() {
        *px = pal[(bits >> (2 * i)) as usize & 3];
    }
    out
}

fn bc3_alpha(b: &[u8]) -> [u8; 16] {
    let (a0, a1) = (b[0], b[1]);
    let mut pal = [0u8; 8];
    pal[0] = a0;
    pal[1] = a1;
    if a0 > a1 {
        for k in 1..7u32 {
            pal[k as usize + 1] = lerp(a0, a1, 7 - k, k, 7);
        }
    } else {
        for k in 1..5u32 {
            pal[k as usize + 1] = lerp(a0, a1, 5 - k, k, 5);
        }
        pal[6] = 0;
        pal[7] = 255;
    }
    let mut bits = 0u64;
    for (i, &byte) in b[2..8].iter().enumerate() {
        bits |= (byte as u64) << (8 * i);
    }
    let mut out = [0u8; 16];
    for (i, a) in out.iter_mut().enumerate() {
        *a = pal[(bits >> (3 * i)) as usize & 7];
    }
    out
}

/// Decode one mip. `data` must hold at least [`level_bytes`] bytes.
pub(super) fn decode(t: Texels, w: usize, h: usize, data: &[u8]) -> Vec<u8> {
    let bb = block_bytes(t);
    let bw = w.div_ceil(4);
    let mut out = vec![0u8; w * h * 4];
    for (bi, blk) in data.chunks_exact(bb).take(bw * h.div_ceil(4)).enumerate() {
        let (bx, by) = (bi % bw * 4, bi / bw * 4);
        let mut px = match t {
            Texels::Bc1 => color_block(blk, false),
            Texels::Bc2 => {
                let mut px = color_block(&blk[8..], true);
                for (i, p) in px.iter_mut().enumerate() {
                    let a = blk[i / 2] >> (4 * (i & 1)) & 15;
                    p[3] = a * 17;
                }
                px
            }
            Texels::Bc3 => {
                let mut px = color_block(&blk[8..], true);
                for (p, a) in px.iter_mut().zip(bc3_alpha(blk)) {
                    p[3] = a;
                }
                px
            }
            Texels::Rgba8 => unreachable!("not a block format"),
        };
        for (i, p) in px.iter_mut().enumerate() {
            let (x, y) = (bx + i % 4, by + i / 4);
            if x < w && y < h {
                out[(y * w + x) * 4..][..4].copy_from_slice(p);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bc1_four_color_and_punch_through() {
        // c0 = pure red (f800), c1 = pure blue (001f); indices 0,1,2,3 on row 0.
        let mut blk = [0u8; 8];
        blk[..2].copy_from_slice(&0xf800u16.to_le_bytes());
        blk[2..4].copy_from_slice(&0x001fu16.to_le_bytes());
        blk[4] = 0b11_10_01_00;
        let px = decode(Texels::Bc1, 4, 4, &blk);
        assert_eq!(&px[0..4], &[255, 0, 0, 255]);
        assert_eq!(&px[4..8], &[0, 0, 255, 255]);
        assert_eq!(&px[8..12], &[170, 0, 85, 255]);
        assert_eq!(&px[12..16], &[85, 0, 170, 255]);
        // Swap order: c0 < c1 selects 3 colors + transparent black.
        blk[..2].copy_from_slice(&0x001fu16.to_le_bytes());
        blk[2..4].copy_from_slice(&0xf800u16.to_le_bytes());
        let px = decode(Texels::Bc1, 4, 4, &blk);
        assert_eq!(&px[8..12], &[127, 0, 127, 255]);
        assert_eq!(&px[12..16], &[0, 0, 0, 0]);
    }

    #[test]
    fn bc2_alpha_nibbles_and_bc3_ramps() {
        let mut blk = [0u8; 16];
        blk[0] = 0xf0; // texel 0 alpha 0, texel 1 alpha 15
        blk[8..10].copy_from_slice(&0xffffu16.to_le_bytes());
        let px = decode(Texels::Bc2, 4, 4, &blk);
        assert_eq!((px[3], px[7]), (0, 255));

        let mut blk = [0u8; 16];
        blk[0] = 255;
        blk[1] = 0;
        blk[2] = 0b001_000; // texel 0 -> a0, texel 1 -> a1
        blk[3] = 0;
        let px = decode(Texels::Bc3, 4, 4, &blk);
        assert_eq!((px[3], px[7], px[11]), (255, 0, 255));
        // a0 <= a1: 6-step mode with explicit 0 and 255 at indices 6, 7.
        let a = bc3_alpha(&[0, 255, 0b1111_0000, 1, 0, 0, 0, 0]);
        assert_eq!(a[1], 0);
        assert_eq!(a[2], 255);
    }

    #[test]
    fn partial_blocks_are_cropped() {
        let mut blk = [0u8; 8];
        blk[..2].copy_from_slice(&0xffffu16.to_le_bytes());
        let px = decode(Texels::Bc1, 2, 1, &blk);
        assert_eq!(px, [255; 8]);
    }
}
