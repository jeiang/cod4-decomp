// SPDX-License-Identifier: GPL-3.0-or-later
//! IWI images: the version-6 container, raw and wavelet pixel formats and
//! DXT1/3/5, as stored in `images/<name>.iwi` inside IWDs.
//!
//! File layout (little endian): tag `IWi`, version `6`, `format` (u8), `flags`
//! (u8), `dimensions[3]` (u16, width/height/depth), `fileSizeForPicmip[4]`
//! (u32), then pixel data, smallest mip first, one slice per cube face within
//! a mip. `fileSizeForPicmip[p]` is the file prefix needed to load with
//! picmip `p` (the `p` largest mips dropped); entry 0 is the whole file.
//!
//! Parsed images are upload-ready: block-compressed formats stay compressed
//! ([`Texels::Bc1`]..), everything else becomes RGBA8 with D3D sampling
//! semantics (`L8` -> `L L L 1`, `A8` -> `0 0 0 A`, `L8A8` -> `L L L A`).
//! [`Level::to_rgba8`] decodes block formats on the CPU.

mod bc;
mod wavelet;
mod wavelet_codes;

use crate::vfs::Vfs;
use std::borrow::Cow;
use std::fmt;
use std::io;

pub const HEADER_LEN: usize = 28;
const TAG: &[u8; 4] = b"IWi\x06";

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// Not in the search path.
    NotFound(String),
    BadHeader(&'static str),
    UnsupportedFormat(u8),
    Unsupported(&'static str),
    Truncated,
    Malformed(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "i/o error: {e}"),
            Error::NotFound(n) => write!(f, "image `{n}` not found"),
            Error::BadHeader(m) => write!(f, "bad IWI header: {m}"),
            Error::UnsupportedFormat(v) => write!(f, "unsupported IWI format {v}"),
            Error::Unsupported(m) => write!(f, "unsupported IWI feature: {m}"),
            Error::Truncated => f.write_str("truncated IWI data"),
            Error::Malformed(m) => write!(f, "malformed IWI data: {m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// On-disk pixel format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `B G R A` bytes.
    Bgra8 = 1,
    /// `B G R` bytes.
    Bgr8 = 2,
    /// `L A` bytes.
    La8 = 3,
    L8 = 4,
    A8 = 5,
    WaveletBgra = 6,
    WaveletBgr = 7,
    WaveletLa = 8,
    WaveletL = 9,
    WaveletA = 10,
    Dxt1 = 11,
    Dxt3 = 12,
    Dxt5 = 13,
}

impl Format {
    fn from_u8(v: u8) -> Result<Self, Error> {
        Ok(match v {
            1 => Format::Bgra8,
            2 => Format::Bgr8,
            3 => Format::La8,
            4 => Format::L8,
            5 => Format::A8,
            6 => Format::WaveletBgra,
            7 => Format::WaveletBgr,
            8 => Format::WaveletLa,
            9 => Format::WaveletL,
            10 => Format::WaveletA,
            11 => Format::Dxt1,
            12 => Format::Dxt3,
            13 => Format::Dxt5,
            v => return Err(Error::UnsupportedFormat(v)),
        })
    }

    pub fn is_wavelet(self) -> bool {
        matches!(self as u8, 6..=10)
    }

    /// Stored channels per pixel for raw and wavelet formats.
    fn channels(self) -> usize {
        match self {
            Format::Bgra8 | Format::WaveletBgra => 4,
            Format::Bgr8 | Format::WaveletBgr => 3,
            Format::La8 | Format::WaveletLa => 2,
            Format::L8 | Format::A8 | Format::WaveletL | Format::WaveletA => 1,
            Format::Dxt1 | Format::Dxt3 | Format::Dxt5 => 0,
        }
    }

    /// Memory layout of the (decoded) stored channels.
    fn layout(self) -> Layout {
        match self {
            Format::Bgra8 | Format::WaveletBgra | Format::WaveletBgr => Layout::Bgra,
            Format::Bgr8 => Layout::Bgr,
            Format::La8 | Format::WaveletLa => Layout::La,
            Format::L8 | Format::WaveletL => Layout::L,
            Format::A8 | Format::WaveletA => Layout::A,
            Format::Dxt1 | Format::Dxt3 | Format::Dxt5 => Layout::Bgra,
        }
    }
}

#[derive(Clone, Copy)]
enum Layout {
    Bgra,
    Bgr,
    La,
    L,
    A,
}

impl Layout {
    fn to_rgba8(self, src: &[u8]) -> Vec<u8> {
        match self {
            Layout::Bgra => src
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[2], p[1], p[0], p[3]])
                .collect(),
            Layout::Bgr => src
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|p| [p[2], p[1], p[0], 255])
                .collect(),
            Layout::La => src
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[0], p[0], p[1]])
                .collect(),
            Layout::L => src.iter().flat_map(|&l| [l, l, l, 255]).collect(),
            Layout::A => src.iter().flat_map(|&a| [0, 0, 0, a]).collect(),
        }
    }
}

/// Header flag bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags(pub u8);

impl Flags {
    pub const NOPICMIP: u8 = 0x01;
    pub const NOMIPMAPS: u8 = 0x02;
    pub const CUBEMAP: u8 = 0x04;
    pub const VOLMAP: u8 = 0x08;
    pub const STREAMING: u8 = 0x10;
    pub const LEGACY_NORMALS: u8 = 0x20;
    pub const CLAMP_U: u8 = 0x40;
    pub const CLAMP_V: u8 = 0x80;

    pub fn has(self, bit: u8) -> bool {
        self.0 & bit != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub format: Format,
    pub flags: Flags,
    pub width: u16,
    pub height: u16,
    pub depth: u16,
    /// File prefix length needed to load with picmip 0..=3.
    pub file_size_for_picmip: [u32; 4],
}

impl Header {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < HEADER_LEN {
            return Err(Error::Truncated);
        }
        if &bytes[..4] != TAG {
            return Err(Error::BadHeader("tag or version"));
        }
        let u16_at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
        let u32_at =
            |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
        let h = Header {
            format: Format::from_u8(bytes[4])?,
            flags: Flags(bytes[5]),
            width: u16_at(6),
            height: u16_at(8),
            depth: u16_at(10),
            file_size_for_picmip: [u32_at(12), u32_at(16), u32_at(20), u32_at(24)],
        };
        if h.width == 0 || h.height == 0 {
            return Err(Error::BadHeader("zero dimension"));
        }
        if h.flags.has(Flags::VOLMAP) {
            return Err(Error::Unsupported("volume maps"));
        }
        Ok(h)
    }

    /// Mip levels stored in the file.
    pub fn mip_count(&self) -> usize {
        if self.flags.has(Flags::NOMIPMAPS) {
            return 1;
        }
        let max = self.width.max(self.height).max(self.depth) as u32;
        (u32::BITS - max.leading_zeros()) as usize
    }

    pub fn face_count(&self) -> usize {
        if self.flags.has(Flags::CUBEMAP) { 6 } else { 1 }
    }

    /// Whether picmip may drop mips of this image.
    pub fn allows_picmip(&self) -> bool {
        !self.flags.has(Flags::NOPICMIP | Flags::NOMIPMAPS) && self.width.min(self.height) >= 32
    }
}

/// How [`Level::data`] is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Texels {
    /// RGBA, 8 bits per channel.
    Rgba8,
    /// 4x4 blocks of 8 bytes (DXT1).
    Bc1,
    /// 4x4 blocks of 16 bytes (DXT3).
    Bc2,
    /// 4x4 blocks of 16 bytes (DXT5).
    Bc3,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Level {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl Level {
    /// RGBA8 pixels, decoding block formats on the CPU.
    pub fn to_rgba8(&self, texels: Texels) -> Cow<'_, [u8]> {
        match texels {
            Texels::Rgba8 => Cow::Borrowed(&self.data),
            t => Cow::Owned(bc::decode(t, self.width, self.height, &self.data)),
        }
    }
}

/// A decoded image. Levels are ordered largest first; with cube maps the six
/// faces of a mip are consecutive (+X -X +Y -Y +Z -Z).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub header: Header,
    pub texels: Texels,
    /// Largest mips of the file that were skipped.
    pub picmip: usize,
    pub faces: usize,
    pub levels: Vec<Level>,
}

impl Image {
    pub fn mip_count(&self) -> usize {
        self.levels.len() / self.faces
    }

    pub fn level(&self, mip: usize, face: usize) -> &Level {
        &self.levels[mip * self.faces + face]
    }

    /// Decode a whole file with every mip.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        Self::parse_picmip(bytes, 0)
    }

    /// Decode a file as the engine does for texture quality `picmip`: that
    /// many of the largest mips are dropped (never more than leaves one mip,
    /// and none for images that opt out).
    pub fn parse_picmip(bytes: &[u8], picmip: usize) -> Result<Self, Error> {
        let header = Header::parse(bytes)?;
        let mips = header.mip_count();
        let picmip = if header.allows_picmip() {
            picmip.min(mips - 1)
        } else {
            0
        };
        let faces = header.face_count();
        let (width, height) = (header.width as usize, header.height as usize);
        let data = &bytes[HEADER_LEN..];
        let format = header.format;

        let (texels, mut levels) = match format {
            Format::Dxt1 | Format::Dxt3 | Format::Dxt5 => {
                let texels = match format {
                    Format::Dxt1 => Texels::Bc1,
                    Format::Dxt3 => Texels::Bc2,
                    _ => Texels::Bc3,
                };
                let mut off = 0;
                let mut by_mip = Vec::new();
                for mip in (picmip..mips).rev() {
                    let (w, h) = ((width >> mip).max(1), (height >> mip).max(1));
                    let n = bc::level_bytes(texels, w, h);
                    let mut faces_v = Vec::with_capacity(faces);
                    for _ in 0..faces {
                        let d = data.get(off..off + n).ok_or(Error::Truncated)?;
                        faces_v.push(Level {
                            width: w,
                            height: h,
                            data: d.to_vec(),
                        });
                        off += n;
                    }
                    by_mip.push(faces_v);
                }
                (
                    texels,
                    by_mip.into_iter().rev().flatten().collect::<Vec<_>>(),
                )
            }
            Format::WaveletBgra
            | Format::WaveletBgr
            | Format::WaveletLa
            | Format::WaveletL
            | Format::WaveletA => {
                let decoded =
                    wavelet::decode(data, width, height, mips, picmip, faces, format.channels())?;
                let layout = format.layout();
                let levels = decoded
                    .into_iter()
                    .map(|d| Level {
                        width: d.width,
                        height: d.height,
                        data: layout.to_rgba8(&d.data),
                    })
                    .collect();
                (Texels::Rgba8, levels)
            }
            _ => {
                let ch = format.channels();
                let layout = format.layout();
                let mut off = 0;
                let mut by_mip = Vec::new();
                for mip in (picmip..mips).rev() {
                    let (w, h) = ((width >> mip).max(1), (height >> mip).max(1));
                    let n = w * h * ch;
                    let mut faces_v = Vec::with_capacity(faces);
                    for _ in 0..faces {
                        let d = data.get(off..off + n).ok_or(Error::Truncated)?;
                        faces_v.push(Level {
                            width: w,
                            height: h,
                            data: layout.to_rgba8(d),
                        });
                        off += n;
                    }
                    by_mip.push(faces_v);
                }
                (Texels::Rgba8, by_mip.into_iter().rev().flatten().collect())
            }
        };
        levels.shrink_to_fit();
        Ok(Image {
            header,
            texels,
            picmip,
            faces,
            levels,
        })
    }
}

/// Load `images/<name>.iwi` through the search path with all mips.
pub fn load(vfs: &Vfs, name: &str) -> Result<Image, Error> {
    load_picmip(vfs, name, 0)
}

/// Like [`load`], honoring a picmip setting (see [`Image::parse_picmip`]).
pub fn load_picmip(vfs: &Vfs, name: &str, picmip: usize) -> Result<Image, Error> {
    let path = format!("images/{name}.iwi");
    let bytes = vfs.read(&path)?.ok_or(Error::NotFound(path))?;
    Image::parse_picmip(&bytes, picmip)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(format: u8, flags: u8, w: u16, h: u16, body: &[u8]) -> Vec<u8> {
        let mut v = b"IWi\x06".to_vec();
        v.extend([format, flags]);
        for d in [w, h, 1] {
            v.extend(d.to_le_bytes());
        }
        let total = (HEADER_LEN + body.len()) as u32;
        for _ in 0..4 {
            v.extend(total.to_le_bytes());
        }
        v.extend(body);
        v
    }

    #[test]
    fn bitmap_formats_expand_to_rgba() {
        // 2x1 BGR8, mips: 1x1 first then 2x1.
        let f = file(2, 0, 2, 1, &[1, 2, 3, 10, 20, 30, 40, 50, 60]);
        let img = Image::parse(&f).unwrap();
        assert_eq!(img.mip_count(), 2);
        assert_eq!(img.level(0, 0).data, [30, 20, 10, 255, 60, 50, 40, 255]);
        assert_eq!(img.level(1, 0).data, [3, 2, 1, 255]);
        // L8 -> L L L 1, A8 -> 0 0 0 A, L8A8 -> L L L A (1x1, no mips).
        let l = Image::parse(&file(4, 2, 1, 1, &[7])).unwrap();
        assert_eq!(l.level(0, 0).data, [7, 7, 7, 255]);
        let a = Image::parse(&file(5, 2, 1, 1, &[7])).unwrap();
        assert_eq!(a.level(0, 0).data, [0, 0, 0, 7]);
        let la = Image::parse(&file(3, 2, 1, 1, &[7, 9])).unwrap();
        assert_eq!(la.level(0, 0).data, [7, 7, 7, 9]);
    }

    #[test]
    fn cube_faces_are_consecutive_per_mip() {
        // 1x1 cube, L8: one mip, six faces.
        let img = Image::parse(&file(4, 4, 1, 1, &[1, 2, 3, 4, 5, 6])).unwrap();
        assert_eq!(img.faces, 6);
        assert_eq!(img.level(0, 5).data[0], 6);
    }

    #[test]
    fn truncated_and_bad_input_error() {
        let f = file(11, 0, 4, 4, &[0; 8 + 8 + 7]);
        assert!(matches!(Image::parse(&f), Err(Error::Truncated)));
        let mut f = file(4, 2, 1, 1, &[7]);
        f[0] = b'X';
        assert!(matches!(Image::parse(&f), Err(Error::BadHeader(_))));
        assert!(matches!(
            Image::parse(&file(14, 0, 4, 4, &[])),
            Err(Error::UnsupportedFormat(14))
        ));
    }

    #[test]
    fn wavelet_hand_built_level() {
        // 2x2 single channel: raw 1x1 level (10), then one coded 2x2 level:
        // bits (LSB first) delta=0, parity=1, three coefficients coded as the
        // 1-bit zero code. => out00 = 1 + ((20) >> 1), the rest 10.
        let f = file(9, 0, 2, 2, &[10, 0b0001_1110, 0, 0]);
        let img = Image::parse(&f).unwrap();
        let top = &img.level(0, 0).data;
        let l: Vec<u8> = top.chunks(4).map(|p| p[0]).collect();
        assert_eq!(l, [11, 10, 10, 10]);
        assert_eq!(img.level(1, 0).data[0], 10);
    }

    #[test]
    fn small_images_ignore_picmip() {
        let body = vec![0u8; 16 + 4 + 1];
        let f = file(4, 0, 4, 4, &body);
        assert_eq!(Image::parse_picmip(&f, 0).unwrap().level(0, 0).width, 4);
        // min(w, h) < 32: engine never applies picmip.
        assert_eq!(Image::parse_picmip(&f, 1).unwrap().level(0, 0).width, 4);
    }
}
