// SPDX-License-Identifier: GPL-3.0-or-later
//! CTAB (constant table) reflection: names, register sets and registers of constants and samplers.

/// D3DXREGISTER_SET.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterSet {
    Bool,
    Int4,
    Float4,
    Sampler,
    Other(u16),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CtabEntry {
    pub name: String,
    pub set: RegisterSet,
    pub register: u16,
    /// Number of consecutive registers.
    pub count: u16,
    /// D3DXPARAMETER_CLASS (0 scalar, 1 vector, 2 matrix rows, 3 matrix columns, 4 object, 5 struct).
    pub class: u16,
    /// D3DXPARAMETER_TYPE.
    pub ty: u16,
}

fn rd(b: &[u8], off: usize, n: usize) -> Option<u32> {
    let s = b.get(off..off.checked_add(n)?)?;
    Some(s.iter().rev().fold(0, |v, &x| v << 8 | u32::from(x)))
}

/// `b` is the comment payload after the `CTAB` magic; all offsets inside are relative to it.
pub(crate) fn parse_ctab(b: &[u8]) -> Option<Vec<CtabEntry>> {
    let n = rd(b, 12, 4)? as usize;
    let off = rd(b, 16, 4)? as usize;
    (0..n)
        .map(|k| {
            let e = off.checked_add(20usize.checked_mul(k)?)?;
            let name_off = rd(b, e, 4)? as usize;
            let ty_off = rd(b, e + 12, 4)? as usize;
            let name = b.get(name_off..)?;
            let name = &name[..name.iter().position(|&c| c == 0)?];
            Some(CtabEntry {
                name: String::from_utf8_lossy(name).into_owned(),
                set: match rd(b, e + 4, 2)? {
                    0 => RegisterSet::Bool,
                    1 => RegisterSet::Int4,
                    2 => RegisterSet::Float4,
                    3 => RegisterSet::Sampler,
                    o => RegisterSet::Other(o as u16),
                },
                register: rd(b, e + 6, 2)? as u16,
                count: rd(b, e + 8, 2)? as u16,
                class: rd(b, ty_off, 2)? as u16,
                ty: rd(b, ty_off + 2, 2)? as u16,
            })
        })
        .collect()
}
