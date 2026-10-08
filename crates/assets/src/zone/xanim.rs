// SPDX-License-Identifier: GPL-3.0-only
//! XAnimParts: animation keyframe data, notetracks and per-bone delta parts.

use super::error::{Result, ZoneError};
use super::gfx::{Name, raw_of};
use super::stream::{Fields, Ptr, Stream};
use std::sync::Arc;

/// Index into the zone's script-string table.
pub type ScriptString = u16;

#[derive(Debug)]
pub struct XAnimParts {
    pub name: Name,
    pub num_frames: u16,
    pub looping: bool,
    pub has_delta: bool,
    /// Bones per part type; the last entry is the total and sizes `names`.
    pub bone_counts: [u8; 10],
    pub asset_type: u8,
    pub is_default: bool,
    pub frame_rate: f32,
    pub frequency: f32,
    pub names: Vec<ScriptString>,
    pub notify: Vec<Notify>,
    pub delta: Option<DeltaPart>,
    pub data_byte: Vec<u8>,
    pub data_short: Vec<i16>,
    pub data_int: Vec<i32>,
    pub random_data_short: Vec<i16>,
    pub random_data_byte: Vec<u8>,
    pub random_data_int: Vec<i32>,
    pub indices: Indices,
}

#[derive(Debug)]
pub struct Notify {
    pub name: ScriptString,
    pub time: f32,
}

/// Frame indices: bytes while the animation has fewer than 256 frames, else shorts.
#[derive(Debug, Clone)]
pub enum Indices {
    None,
    Byte(Vec<u8>),
    Short(Vec<u16>),
}

#[derive(Debug, Clone)]
pub struct DeltaPart {
    pub trans: Option<Trans>,
    pub quat: Option<Quat>,
}

/// Translation track. With `size == 0` only `frame0` is set; otherwise
/// `size + 1` frames are stored at `indices`.
#[derive(Debug, Clone)]
pub struct Trans {
    pub size: u16,
    pub small: bool,
    pub frame0: [f32; 3],
    pub mins: [f32; 3],
    pub extent: [f32; 3],
    pub indices: Indices,
    /// Quantised frames (bytes when `small`, else shorts).
    pub frames: TransFrames,
}

#[derive(Debug, Clone)]
pub enum TransFrames {
    None,
    Byte(Vec<[u8; 3]>),
    Short(Vec<[u16; 3]>),
}

/// Rotation track (two quantised components per frame).
#[derive(Debug, Clone)]
pub struct Quat {
    pub size: u16,
    pub frame0: [i16; 2],
    pub indices: Indices,
    pub frames: Vec<[i16; 2]>,
}

pub(super) const HEADER_SIZE: u32 = 88;

fn chunks<const N: usize, T>(b: &[u8], f: impl Fn([u8; N]) -> T) -> Vec<T> {
    b.as_chunks::<N>().0.iter().map(|c| f(*c)).collect()
}

fn indices(s: &mut Stream, wide: bool, count: u32) -> Result<Indices> {
    Ok(if wide {
        let b = s.load(1, count * 2)?.1;
        Indices::Short(chunks(&b, u16::from_le_bytes))
    } else {
        Indices::Byte(s.load(1, count)?.1)
    })
}

fn trans(s: &mut Stream, wide: bool) -> Result<Trans> {
    let hdr = s.load(4, 4)?.1;
    let mut f = Fields::new(&hdr);
    let size = f.u16();
    let small = f.u8() != 0;
    let mut t = Trans {
        size,
        small,
        frame0: [0.0; 3],
        mins: [0.0; 3],
        extent: [0.0; 3],
        indices: Indices::None,
        frames: TransFrames::None,
    };
    if size == 0 {
        let b = s.load(1, 12)?.1;
        let mut f = Fields::new(&b);
        t.frame0 = [f.f32(), f.f32(), f.f32()];
        return Ok(t);
    }
    let b = s.load(1, 28)?.1;
    let mut f = Fields::new(&b);
    t.mins = [f.f32(), f.f32(), f.f32()];
    t.extent = [f.f32(), f.f32(), f.f32()];
    let frames = f.ptr()?;
    let n = size as u32 + 1;
    t.indices = indices(s, wide, n)?;
    match frames {
        Ptr::Null => {}
        Ptr::Follow if small => {
            let b = s.load(1, n * 3)?.1;
            t.frames = TransFrames::Byte(chunks(&b, |c| c));
        }
        Ptr::Follow => {
            let b = s.load(4, n * 6)?.1;
            t.frames = TransFrames::Short(chunks(&b, |c: [u8; 6]| {
                [
                    u16::from_le_bytes([c[0], c[1]]),
                    u16::from_le_bytes([c[2], c[3]]),
                    u16::from_le_bytes([c[4], c[5]]),
                ]
            }));
        }
        p => return Err(ZoneError::BadPointer(raw_of(p))),
    }
    Ok(t)
}

fn quat(s: &mut Stream, wide: bool) -> Result<Quat> {
    let size = Fields::new(&s.load(4, 4)?.1).u16();
    let mut q = Quat {
        size,
        frame0: [0; 2],
        indices: Indices::None,
        frames: Vec::new(),
    };
    if size == 0 {
        let b = s.load(1, 4)?.1;
        q.frame0 = [
            i16::from_le_bytes([b[0], b[1]]),
            i16::from_le_bytes([b[2], b[3]]),
        ];
        return Ok(q);
    }
    let frames = Fields::new(&s.load(1, 4)?.1).ptr()?;
    let n = size as u32 + 1;
    q.indices = indices(s, wide, n)?;
    match frames {
        Ptr::Null => {}
        Ptr::Follow => {
            let b = s.load(4, n * 4)?.1;
            q.frames = chunks(&b, |c: [u8; 4]| {
                [
                    i16::from_le_bytes([c[0], c[1]]),
                    i16::from_le_bytes([c[2], c[3]]),
                ]
            });
        }
        p => return Err(ZoneError::BadPointer(raw_of(p))),
    }
    Ok(q)
}

fn delta_part(s: &mut Stream, wide: bool) -> Result<DeltaPart> {
    let hdr = s.load(4, 8)?.1;
    let mut f = Fields::new(&hdr);
    let (t, q) = (f.ptr()?, f.ptr()?);
    let (has_t, has_q) = (pointer(t)?, pointer(q)?);
    Ok(DeltaPart {
        trans: if has_t { Some(trans(s, wide)?) } else { None },
        quat: if has_q { Some(quat(s, wide)?) } else { None },
    })
}

fn pointer(p: Ptr) -> Result<bool> {
    match p {
        Ptr::Null => Ok(false),
        Ptr::Follow => Ok(true),
        p => Err(ZoneError::BadPointer(raw_of(p))),
    }
}

pub(super) fn parts(s: &mut Stream, h: &[u8]) -> Result<XAnimParts> {
    let mut f = Fields::new(h);
    let name = f.ptr()?;
    let (data_byte_n, data_short_n, data_int_n) = (f.u16() as u32, f.u16() as u32, f.u16() as u32);
    let (random_byte_n, random_int_n) = (f.u16() as u32, f.u16() as u32);
    let num_frames = f.u16();
    let looping = f.u8() != 0;
    let has_delta = f.u8() != 0;
    let bone_counts = f.bytes::<10>();
    let notify_n = f.u8() as u32;
    let asset_type = f.u8();
    let is_default = f.u8() != 0;
    f.skip(1);
    let random_short_n = f.u32();
    let index_n = f.u32();
    let frame_rate = f.f32();
    let frequency = f.f32();
    let names = pointer(f.ptr()?)?;
    let data_byte = pointer(f.ptr()?)?;
    let data_short = pointer(f.ptr()?)?;
    let data_int = pointer(f.ptr()?)?;
    let random_short = pointer(f.ptr()?)?;
    let random_byte = pointer(f.ptr()?)?;
    let random_int = pointer(f.ptr()?)?;
    let index = pointer(f.ptr()?)?;
    let notify = pointer(f.ptr()?)?;
    let delta = pointer(f.ptr()?)?;
    let wide = num_frames >= 256;

    let name = s.string(name)?;
    let names = if names {
        let b = s.load(2, bone_counts[9] as u32 * 2)?.1;
        chunks(&b, u16::from_le_bytes)
    } else {
        Vec::new()
    };
    let notify = if notify {
        let b = s.load(4, notify_n * 8)?.1;
        chunks(&b, |c: [u8; 8]| Notify {
            name: u16::from_le_bytes([c[0], c[1]]),
            time: f32::from_le_bytes([c[4], c[5], c[6], c[7]]),
        })
    } else {
        Vec::new()
    };
    let delta = if delta {
        Some(delta_part(s, wide)?)
    } else {
        None
    };
    let data_byte = if data_byte {
        s.load(1, data_byte_n)?.1
    } else {
        Vec::new()
    };
    let data_short = if data_short {
        let b = s.load(2, data_short_n * 2)?.1;
        chunks(&b, i16::from_le_bytes)
    } else {
        Vec::new()
    };
    let data_int = if data_int {
        let b = s.load(4, data_int_n * 4)?.1;
        chunks(&b, i32::from_le_bytes)
    } else {
        Vec::new()
    };
    let random_data_short = if random_short {
        let b = s.load(2, random_short_n * 2)?.1;
        chunks(&b, i16::from_le_bytes)
    } else {
        Vec::new()
    };
    let random_data_byte = if random_byte {
        s.load(1, random_byte_n)?.1
    } else {
        Vec::new()
    };
    let random_data_int = if random_int {
        let b = s.load(4, random_int_n * 4)?.1;
        chunks(&b, i32::from_le_bytes)
    } else {
        Vec::new()
    };
    let indices = if !index {
        Indices::None
    } else if wide {
        let b = s.load(2, index_n * 2)?.1;
        Indices::Short(chunks(&b, u16::from_le_bytes))
    } else {
        Indices::Byte(s.load(1, index_n)?.1)
    };
    Ok(XAnimParts {
        name,
        num_frames,
        looping,
        has_delta,
        bone_counts,
        asset_type,
        is_default,
        frame_rate,
        frequency,
        names,
        notify,
        delta,
        data_byte,
        data_short,
        data_int,
        random_data_short,
        random_data_byte,
        random_data_int,
        indices,
    })
}

pub(super) fn load(s: &mut Stream, p: Ptr) -> Result<Option<Arc<XAnimParts>>> {
    s.temp_asset(p, 4, HEADER_SIZE, parts)
}
