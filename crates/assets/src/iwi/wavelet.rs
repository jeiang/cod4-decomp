// SPDX-License-Identifier: GPL-3.0-only
//! Wavelet-compressed IWI pixel data (formats 6-10).
//!
//! Reconstructed from `iw3mp.exe` 1.7 (image load path: format switch, level
//! loop, per-level decompressor, the three prefix-code lookup tables).
//!
//! Stream layout, per face, smallest mip first:
//! * A level whose width or height is below 2 is stored raw, `channels`
//!   bytes per pixel. All raw levels precede the first coded level.
//! * A coded level is a transform of the previous (half-size) level. The first
//!   coded level's bit stream starts at the byte after the raw data; the
//!   stream is LSB-first and continues unbroken across levels.
//!   1. one flag bit: if set, every channel of every previous-level pixel gets
//!      a coded delta added (alpha code, 9-bit escape, bias 255);
//!   2. for each 2x2 output block, in raster order, per channel group a parity
//!      bit then three coefficients (c0, c1, c2). With `b = 2 * parent`:
//!      `out00 = parity + ((c0 + c1 + c2 + b) >> 1)`,
//!      `out10 = (c0 + b - c1 - c2) >> 1`, `out01 = (c1 - c2 + b - c0) >> 1`,
//!      `out11 = (b - c0 - c1 + c2) >> 1` (all truncated to a byte).
//!      Channel 0 uses the blue code (9-bit escape, bias 255). With 3+
//!      channels, channels 1 and 2 use the red/green code (10-bit escape,
//!      bias 510) and their coefficients are added to channel 0's. A trailing
//!      alpha-like channel (2 or 4 channels) uses the alpha code; a single
//!      channel image uses the alpha code for its only channel. With 3
//!      channels the padding byte is 255.
//!
//! Decoded levels use the D3D memory order: `B G R A`, `L A`, `L` or `A`.

use super::Error;
use super::wavelet_codes;

const ESCAPE: i16 = i16::MIN;

/// 12-bit lookup: (value, bit length).
struct Code(Box<[(i16, u8); 4096]>);

impl Code {
    fn build(entries: &[(i16, u8, u16)]) -> Self {
        let mut t = Box::new([(0i16, 0u8); 4096]);
        for &(value, bits, code) in entries {
            for hi in 0..1usize << (12 - bits) {
                t[code as usize | hi << bits] = (value, bits);
            }
        }
        Code(t)
    }
}

struct Codes {
    blue: Code,
    red_green: Code,
    alpha: Code,
}

impl Codes {
    fn new() -> Self {
        Codes {
            blue: Code::build(wavelet_codes::BLUE),
            red_green: Code::build(wavelet_codes::RED_GREEN),
            alpha: Code::build(wavelet_codes::ALPHA),
        }
    }
}

struct Bits<'a> {
    data: &'a [u8],
    /// Bit position of the next unread bit.
    pos: usize,
}

impl Bits<'_> {
    /// The next 16 bits, first bit in the least significant position.
    fn window(&self) -> u32 {
        let byte = self.pos >> 3;
        let b = |i: usize| self.data.get(byte + i).copied().unwrap_or(0) as u32;
        (b(0) | b(1) << 8 | b(2) << 16) >> (self.pos & 7) & 0xffff
    }

    fn bit(&mut self) -> u8 {
        let v = self.window() as u8 & 1;
        self.pos += 1;
        v
    }

    fn value(&mut self, code: &Code, escape_bits: u32, bias: i32) -> i32 {
        let (v, bits) = code.0[self.window() as usize & 0xfff];
        self.pos += bits as usize;
        if v == ESCAPE {
            let raw = (self.window() & ((1 << escape_bits) - 1)) as i32;
            self.pos += escape_bits as usize;
            raw - bias
        } else {
            v as i32
        }
    }
}

/// One decoded mip of one face, tightly packed `width * height * bpp` bytes.
pub(super) struct Decoded {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

/// Bytes per pixel of the decoded layout: 3 channels are padded to 4.
pub(super) fn bytes_per_pixel(channels: usize) -> usize {
    if channels == 3 { 4 } else { channels }
}

/// Decode mips `lowest..mips` (smallest is stored first) of `faces` faces; all
/// larger mips are skipped by stopping early.
/// Returns levels largest first, `mip * faces + face` order.
pub(super) fn decode(
    data: &[u8],
    width: usize,
    height: usize,
    mips: usize,
    lowest: usize,
    faces: usize,
    channels: usize,
) -> Result<Vec<Decoded>, Error> {
    let bpp = bytes_per_pixel(channels);
    let codes = Codes::new();
    let mut byte_pos = 0usize;
    let mut bits: Option<Bits> = None;
    let mut prev: Vec<Option<Vec<u8>>> = vec![None; faces];
    let mut out: Vec<Decoded> = Vec::with_capacity(mips * faces);

    for level in (lowest..mips).rev() {
        let (w, h) = ((width >> level).max(1), (height >> level).max(1));
        for prev in prev.iter_mut() {
            let mut px = vec![0u8; w * h * bpp];
            if w < 2 || h < 2 {
                if bits.is_some() {
                    return Err(Error::Malformed("raw wavelet level after coded level"));
                }
                let n = w * h;
                let src = data
                    .get(byte_pos..byte_pos + n * channels)
                    .ok_or(Error::Truncated)?;
                for (d, s) in px.chunks_exact_mut(bpp).zip(src.chunks_exact(channels)) {
                    d[..channels].copy_from_slice(s);
                    if bpp != channels {
                        d[3] = 255;
                    }
                }
                byte_pos += n * channels;
            } else {
                if w % 2 != 0 || h % 2 != 0 {
                    return Err(Error::Malformed("wavelet level with odd size"));
                }
                let parent = prev
                    .take()
                    .ok_or(Error::Malformed("wavelet image without mip chain"))?;
                let r = bits.get_or_insert(Bits {
                    data,
                    pos: byte_pos * 8,
                });
                decode_level(r, &codes, parent, &mut px, w, h, channels, bpp);
                if r.pos > data.len() * 8 {
                    return Err(Error::Truncated);
                }
            }
            *prev = Some(px.clone());
            out.push(Decoded {
                width: w,
                height: h,
                data: px,
            });
        }
    }
    // Emitted smallest first: reorder to largest first, keeping face order.
    let mut by_mip: Vec<Vec<Decoded>> = Vec::with_capacity(mips - lowest);
    let mut it = out.into_iter();
    for _ in lowest..mips {
        by_mip.push(it.by_ref().take(faces).collect());
    }
    Ok(by_mip.into_iter().rev().flatten().collect())
}

#[allow(clippy::too_many_arguments)]
fn decode_level(
    r: &mut Bits,
    codes: &Codes,
    mut parent: Vec<u8>,
    dst: &mut [u8],
    w: usize,
    h: usize,
    channels: usize,
    bpp: usize,
) {
    if r.bit() != 0 {
        for px in parent.chunks_exact_mut(bpp).take(w * h / 4) {
            for c in px.iter_mut().take(channels) {
                *c = c.wrapping_add(r.value(&codes.alpha, 9, 255) as u8);
            }
        }
    }
    let stride = w * bpp;
    let mut src = 0;
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let at = y * stride + x * bpp;
            let mut put = |ch: usize, parity: i32, c: [i32; 3]| {
                let b = 2 * parent[src + ch] as i32;
                let o = at + ch;
                dst[o] = (parity + ((c[2] + c[1] + c[0] + b) >> 1)) as u8;
                dst[o + bpp] = ((c[0] + b - (c[2] + c[1])) >> 1) as u8;
                dst[o + stride] = ((c[1] - c[2] + b - c[0]) >> 1) as u8;
                dst[o + stride + bpp] = ((b - c[0] - (c[1] - c[2])) >> 1) as u8;
            };
            let mut base = [0i32; 3];
            if channels != 1 {
                let parity = r.bit() as i32;
                for c in &mut base {
                    *c = r.value(&codes.blue, 9, 255);
                }
                put(0, parity, base);
                if channels >= 3 {
                    for ch in 1..3 {
                        let parity = r.bit() as i32;
                        let mut c = base;
                        for v in &mut c {
                            *v += r.value(&codes.red_green, 10, 510);
                        }
                        put(ch, parity, c);
                    }
                }
            }
            if channels == 3 {
                if bpp != channels {
                    for o in [0, bpp, stride, stride + bpp] {
                        dst[at + o + 3] = 255;
                    }
                }
            } else {
                let parity = r.bit() as i32;
                let mut c = [0i32; 3];
                for v in &mut c {
                    *v = r.value(&codes.alpha, 9, 255);
                }
                put(channels - 1, parity, c);
            }
            src += bpp;
        }
    }
}
