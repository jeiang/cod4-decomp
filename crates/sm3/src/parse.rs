// SPDX-License-Identifier: GPL-3.0-only
//! D3D9 shader-model 2/3 token-stream parser, written from the public token-format documentation.

use crate::{
    Error,
    ctab::{CtabEntry, parse_ctab},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage {
    Vertex,
    Pixel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RegType {
    Temp,
    Input,
    Const,
    Addr,
    RastOut,
    AttrOut,
    Output,
    ConstInt,
    ColorOut,
    DepthOut,
    Sampler,
    ConstBool,
    Loop,
    Misc,
    Label,
    Predicate,
    Other(u32),
}

impl RegType {
    fn from_code(n: u32) -> RegType {
        match n {
            0 => RegType::Temp,
            1 => RegType::Input,
            2 | 11..=13 => RegType::Const,
            3 => RegType::Addr,
            4 => RegType::RastOut,
            5 => RegType::AttrOut,
            6 => RegType::Output,
            7 => RegType::ConstInt,
            8 => RegType::ColorOut,
            9 => RegType::DepthOut,
            10 => RegType::Sampler,
            14 => RegType::ConstBool,
            15 => RegType::Loop,
            17 => RegType::Misc,
            18 => RegType::Label,
            19 => RegType::Predicate,
            o => RegType::Other(o),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Reg {
    pub ty: RegType,
    pub num: u32,
    /// Relative-addressing register; not supported by the emitter.
    pub rel: Option<Box<Src>>,
}

#[derive(Clone, Debug)]
pub struct Src {
    pub reg: Reg,
    pub swizzle: [u8; 4],
    /// D3DSPSM_* source modifier.
    pub modifier: u32,
}

#[derive(Clone, Debug)]
pub struct Dst {
    pub reg: Reg,
    pub mask: u8,
    pub saturate: bool,
    pub partial_precision: bool,
    pub centroid: bool,
    /// Result shift scale exponent (`_x2` = 1, `_d2` = -1).
    pub shift: i8,
}

#[derive(Clone, Debug)]
pub enum Inst {
    Dcl {
        reg: Reg,
        usage: u32,
        index: u32,
        sampler_type: u32,
    },
    Def {
        reg: Reg,
        v: [f32; 4],
    },
    DefI {
        reg: Reg,
        v: [i32; 4],
    },
    DefB {
        reg: Reg,
        v: bool,
    },
    Op {
        op: u32,
        ctl: u32,
        dst: Option<Dst>,
        src: Vec<Src>,
        predicated: bool,
    },
}

#[derive(Debug)]
pub struct Shader {
    pub stage: Stage,
    pub major: u32,
    pub minor: u32,
    pub insts: Vec<Inst>,
    pub ctab: Vec<CtabEntry>,
}

pub(crate) fn opname(op: u32) -> String {
    let n = match op {
        0 => "nop",
        1 => "mov",
        2 => "add",
        3 => "sub",
        4 => "mad",
        5 => "mul",
        6 => "rcp",
        7 => "rsq",
        8 => "dp3",
        9 => "dp4",
        10 => "min",
        11 => "max",
        12 => "slt",
        13 => "sge",
        14 => "exp",
        15 => "log",
        16 => "lit",
        17 => "dst",
        18 => "lrp",
        19 => "frc",
        20 => "m4x4",
        21 => "m4x3",
        22 => "m3x4",
        23 => "m3x3",
        24 => "m3x2",
        25 => "call",
        26 => "callnz",
        27 => "loop",
        28 => "ret",
        29 => "endloop",
        30 => "label",
        31 => "dcl",
        32 => "pow",
        33 => "crs",
        34 => "sgn",
        35 => "abs",
        36 => "nrm",
        37 => "sincos",
        38 => "rep",
        39 => "endrep",
        40 => "if",
        41 => "ifc",
        42 => "else",
        43 => "endif",
        44 => "break",
        45 => "breakc",
        46 => "mova",
        47 => "defb",
        48 => "defi",
        65 => "texkill",
        66 => "texld",
        81 => "def",
        88 => "cmp",
        89 => "bem",
        90 => "dp2add",
        91 => "dsx",
        92 => "dsy",
        93 => "texldd",
        94 => "setp",
        95 => "texldl",
        96 => "breakp",
        o => return format!("op{o}"),
    };
    n.to_string()
}

const COMMENT: u32 = 0xFFFE;
const END: u32 = 0xFFFF;

fn words(code: &[u8]) -> Vec<u32> {
    code.as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect()
}

fn is_version(t: u32) -> bool {
    matches!(t >> 16, 0xFFFE | 0xFFFF) && (t >> 8) & 0xFF <= 3
}

/// Length in bytes of the shader (version token through END token) at the start of `code`, if it is a
/// well-formed SM2/SM3 token stream that carries a CTAB comment. Used to cut blobs out of larger buffers.
pub fn blob_len(code: &[u8]) -> Option<usize> {
    let mut have_ctab = false;
    let mut i = 1usize;
    let w = |i: usize| -> Option<u32> {
        let b = code.get(i * 4..i * 4 + 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if !is_version(w(0)?) {
        return None;
    }
    loop {
        let t = w(i)?;
        match t & 0xFFFF {
            END => return have_ctab.then_some((i + 1) * 4),
            COMMENT => {
                let len = ((t >> 16) & 0x7FFF) as usize;
                if code.get((i + 1) * 4..(i + 2) * 4)? == b"CTAB" {
                    have_ctab = true;
                }
                w(i + len)?;
                i += 1 + len;
            }
            _ => i += 1 + ((t >> 24) & 0xF) as usize,
        }
    }
}

fn reg_of(t: u32) -> Reg {
    Reg {
        ty: RegType::from_code(((t >> 28) & 7) | ((t >> 8) & 0x18)),
        num: t & 0x7FF,
        rel: None,
    }
}

fn swizzle(t: u32) -> [u8; 4] {
    let s = (t >> 16) & 0xFF;
    [0, 2, 4, 6].map(|k| ((s >> k) & 3) as u8)
}

/// Register token at `toks[*k]`, consuming the relative-address token when flagged.
fn reg_tok(toks: &[u32], k: &mut usize) -> Result<(u32, Reg), Error> {
    let t = *toks.get(*k).ok_or(Error::Malformed("truncated operand"))?;
    *k += 1;
    let mut reg = reg_of(t);
    if t & 0x2000 != 0 {
        let r = *toks
            .get(*k)
            .ok_or(Error::Malformed("truncated relative address"))?;
        *k += 1;
        reg.rel = Some(Box::new(Src {
            reg: reg_of(r),
            swizzle: swizzle(r),
            modifier: 0,
        }));
    }
    Ok((t, reg))
}

/// Opcodes whose first operand is not a destination register.
fn has_dst(op: u32) -> bool {
    !matches!(op, 0 | 25..=30 | 38..=45 | 65 | 96)
}

pub fn parse(code: &[u8]) -> Result<Shader, Error> {
    let w = words(code);
    let &ver = w.first().ok_or(Error::Malformed("empty"))?;
    if !is_version(ver) {
        return Err(Error::Malformed("bad version token"));
    }
    let stage = if ver >> 16 == 0xFFFE {
        Stage::Vertex
    } else {
        Stage::Pixel
    };
    let (major, minor) = ((ver >> 8) & 0xFF, ver & 0xFF);
    let (mut i, mut insts, mut ctab) = (1, vec![], vec![]);
    while i < w.len() {
        let t = w[i];
        let op = t & 0xFFFF;
        if op == END {
            break;
        }
        if op == COMMENT {
            let len = ((t >> 16) & 0x7FFF) as usize;
            let bytes = code
                .get((i + 1) * 4..(i + 1 + len) * 4)
                .ok_or(Error::Malformed("truncated comment"))?;
            if let Some(b) = bytes.strip_prefix(b"CTAB") {
                ctab = parse_ctab(b).ok_or(Error::Malformed("bad CTAB"))?;
            }
            i += 1 + len;
            continue;
        }
        let len = ((t >> 24) & 0xF) as usize;
        let ctl = (t >> 16) & 0xFF;
        let predicated = t & 0x1000_0000 != 0;
        let toks = w
            .get(i + 1..i + 1 + len)
            .ok_or(Error::Malformed("truncated instruction"))?;
        i += 1 + len;
        let need = |n: usize| {
            if toks.len() >= n {
                Ok(())
            } else {
                Err(Error::Malformed("short instruction"))
            }
        };
        match op {
            31 => {
                need(2)?;
                let u = toks[0];
                insts.push(Inst::Dcl {
                    reg: reg_of(toks[1]),
                    usage: u & 0x1F,
                    index: (u >> 16) & 0xF,
                    sampler_type: (u >> 27) & 0xF,
                });
            }
            81 => {
                need(5)?;
                let v = [1, 2, 3, 4].map(|k| f32::from_bits(toks[k]));
                insts.push(Inst::Def {
                    reg: reg_of(toks[0]),
                    v,
                });
            }
            48 => {
                need(5)?;
                let v = [1, 2, 3, 4].map(|k| toks[k] as i32);
                insts.push(Inst::DefI {
                    reg: reg_of(toks[0]),
                    v,
                });
            }
            47 => {
                need(2)?;
                insts.push(Inst::DefB {
                    reg: reg_of(toks[0]),
                    v: toks[1] != 0,
                });
            }
            _ => {
                let (mut k, mut dst, mut src) = (0, None, vec![]);
                if has_dst(op) {
                    let (d, reg) = reg_tok(toks, &mut k)?;
                    let shift = ((d >> 24) & 0xF) as i8;
                    dst = Some(Dst {
                        reg,
                        mask: ((d >> 16) & 0xF) as u8,
                        saturate: d & (1 << 20) != 0,
                        partial_precision: d & (2 << 20) != 0,
                        centroid: d & (4 << 20) != 0,
                        shift: if shift > 7 { shift - 16 } else { shift },
                    });
                }
                // a predicated instruction ends with the predicate register token
                let end = toks.len() - usize::from(predicated && !toks.is_empty());
                let toks = &toks[..end];
                while k < toks.len() {
                    let (s, reg) = reg_tok(toks, &mut k)?;
                    src.push(Src {
                        reg,
                        swizzle: swizzle(s),
                        modifier: (s >> 24) & 0xF,
                    });
                }
                insts.push(Inst::Op {
                    op,
                    ctl,
                    dst,
                    src,
                    predicated,
                });
            }
        }
    }
    Ok(Shader {
        stage,
        major,
        minor,
        insts,
        ctab,
    })
}
