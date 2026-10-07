//! Minimal D3D9 shader-model 2/3 token-stream parser (written from the public token-format documentation
//! facts; no third-party code). Throwaway.
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage { Vertex, Pixel }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RegType { Temp, Input, Const, Addr, RastOut, AttrOut, Output, ConstInt, ColorOut, DepthOut, Sampler, ConstBool, Loop, Misc, Label, Predicate, Other(u32) }

impl RegType {
    pub fn from(n: u32) -> RegType {
        match n { 0 => RegType::Temp, 1 => RegType::Input, 2 => RegType::Const, 3 => RegType::Addr, 4 => RegType::RastOut, 5 => RegType::AttrOut,
            6 => RegType::Output, 7 => RegType::ConstInt, 8 => RegType::ColorOut, 9 => RegType::DepthOut, 10 => RegType::Sampler,
            11 | 12 | 13 => RegType::Const, 14 => RegType::ConstBool, 15 => RegType::Loop, 17 => RegType::Misc, 18 => RegType::Label, 19 => RegType::Predicate, o => RegType::Other(o) }
    }
}

#[derive(Clone, Debug)]
pub struct Reg { pub ty: RegType, pub num: u32, pub rel: Option<Box<Src>> }

#[derive(Clone, Debug)]
pub struct Src { pub reg: Reg, pub swz: [u8; 4], pub modifier: u32 }

#[derive(Clone, Debug)]
pub struct Dst { pub reg: Reg, pub mask: u8, pub sat: bool, pub pp: bool, pub centroid: bool, pub shift: i8 }

#[derive(Clone, Debug)]
pub enum Inst {
    Dcl { reg: Reg, usage: u32, index: u32, samp_ty: u32 },
    Def { reg: Reg, v: [f32; 4] },
    DefI { reg: Reg, v: [i32; 4] },
    DefB { reg: Reg, v: bool },
    Op { op: u32, ctl: u32, dst: Option<Dst>, src: Vec<Src>, pred: bool },
}

pub struct Shader { pub stage: Stage, pub major: u32, pub minor: u32, pub insts: Vec<Inst>, pub ctab: Vec<CtabEntry> }

#[derive(Clone, Debug)]
pub struct CtabEntry { pub name: String, pub regset: u16, pub reg: u16, pub count: u16, pub class: u16, pub ty: u16 }

pub fn opname(op: u32) -> String {
    let n = match op {
        0 => "nop", 1 => "mov", 2 => "add", 3 => "sub", 4 => "mad", 5 => "mul", 6 => "rcp", 7 => "rsq", 8 => "dp3", 9 => "dp4", 10 => "min", 11 => "max", 12 => "slt", 13 => "sge",
        14 => "exp", 15 => "log", 16 => "lit", 17 => "dst", 18 => "lrp", 19 => "frc", 20 => "m4x4", 21 => "m4x3", 22 => "m3x4", 23 => "m3x3", 24 => "m3x2", 25 => "call", 26 => "callnz",
        27 => "loop", 28 => "ret", 29 => "endloop", 30 => "label", 31 => "dcl", 32 => "pow", 33 => "crs", 34 => "sgn", 35 => "abs", 36 => "nrm", 37 => "sincos", 38 => "rep", 39 => "endrep",
        40 => "if", 41 => "ifc", 42 => "else", 43 => "endif", 44 => "break", 45 => "breakc", 46 => "mova", 47 => "defb", 48 => "defi", 66 => "texld", 65 => "texkill", 81 => "def",
        88 => "cmp", 89 => "bem", 90 => "dp2add", 91 => "dsx", 92 => "dsy", 93 => "texldd", 94 => "setp", 95 => "texldl", 96 => "breakp",
        o => return format!("op{o}"),
    };
    n.to_string()
}

fn parse_ctab(b: &[u8]) -> Vec<CtabEntry> {
    let rd = |o: usize, n: usize| -> u32 { let mut v = 0u32; for i in 0..n { v |= (*b.get(o + i).unwrap_or(&0) as u32) << (8 * i); } v };
    let n = rd(12, 4) as usize; let off = rd(16, 4) as usize;
    (0..n).map(|k| {
        let e = off + 20 * k; let no = rd(e, 4) as usize; let to = rd(e + 12, 4) as usize;
        let name = b.get(no..).map(|s| String::from_utf8_lossy(&s[..s.iter().position(|&c| c == 0).unwrap_or(0)]).into_owned()).unwrap_or_default();
        CtabEntry { name, regset: rd(e + 4, 2) as u16, reg: rd(e + 6, 2) as u16, count: rd(e + 8, 2) as u16, class: rd(to, 2) as u16, ty: rd(to + 2, 2) as u16 }
    }).collect()
}

pub fn parse(code: &[u8]) -> Result<Shader, String> {
    let w: Vec<u32> = code.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    if w.is_empty() { return Err("empty".into()); }
    let stage = match w[0] >> 16 { 0xFFFE => Stage::Vertex, 0xFFFF => Stage::Pixel, _ => return Err("bad version token".into()) };
    let (major, minor) = ((w[0] >> 8) & 0xFF, w[0] & 0xFF);
    let mut i = 1; let mut insts = vec![]; let mut ctab = vec![];
    let reg_of = |t: u32| Reg { ty: RegType::from(((t >> 28) & 7) | ((t >> 8) & 0x18)), num: t & 0x7FF, rel: None };
    while i < w.len() {
        let t = w[i]; let op = t & 0xFFFF;
        if op == 0xFFFF { break; }
        if op == 0xFFFE {
            let len = ((t >> 16) & 0x7FFF) as usize;
            let bytes = &code[(i + 1) * 4..(i + 1 + len) * 4];
            if &bytes[..4.min(bytes.len())] == b"CTAB" { ctab = parse_ctab(&bytes[4..]); }
            i += 1 + len; continue;
        }
        let len = ((t >> 24) & 0xF) as usize;
        let ctl = (t >> 16) & 0xFF; let pred = t & 0x1000_0000 != 0;
        let toks = w.get(i + 1..i + 1 + len).ok_or("truncated")?;
        i += 1 + len;
        match op {
            31 => { let u = toks[0]; insts.push(Inst::Dcl { reg: reg_of(toks[1]), usage: u & 0x1F, index: (u >> 16) & 0xF, samp_ty: (u >> 27) & 0xF }); }
            81 => insts.push(Inst::Def { reg: reg_of(toks[0]), v: [f32::from_bits(toks[1]), f32::from_bits(toks[2]), f32::from_bits(toks[3]), f32::from_bits(toks[4])] }),
            48 => insts.push(Inst::DefI { reg: reg_of(toks[0]), v: [toks[1] as i32, toks[2] as i32, toks[3] as i32, toks[4] as i32] }),
            47 => insts.push(Inst::DefB { reg: reg_of(toks[0]), v: toks[1] != 0 }),
            _ => {
                let has_dst = !matches!(op, 25 | 26 | 27 | 28 | 29 | 30 | 38 | 39 | 40 | 41 | 42 | 43 | 44 | 45 | 65 | 96 | 0) || op == 65;
                let mut k = 0; let mut dst = None; let mut src = vec![];
                if has_dst && op != 65 && !toks.is_empty() {
                    let d = toks[0]; k = 1;
                    let mut reg = reg_of(d);
                    if d & 0x2000 != 0 { let r = toks.get(k).copied().ok_or("trunc rel")?; k += 1; reg.rel = Some(Box::new(Src { reg: reg_of(r), swz: [(r >> 16) as u8 & 3, (r >> 18) as u8 & 3, (r >> 20) as u8 & 3, (r >> 22) as u8 & 3], modifier: 0 })); }
                    let sh = ((d >> 24) & 0xF) as i8; let sh = if sh > 7 { sh - 16 } else { sh };
                    dst = Some(Dst { reg, mask: ((d >> 16) & 0xF) as u8, sat: d & (1 << 20) != 0, pp: d & (2 << 20) != 0, centroid: d & (4 << 20) != 0, shift: sh });
                }
                if pred && k < toks.len() { /* predicate register token is last */ }
                let nsrc_end = if pred { toks.len() - 1 } else { toks.len() };
                while k < nsrc_end {
                    let s = toks[k]; k += 1;
                    let mut reg = reg_of(s);
                    if s & 0x2000 != 0 { let r = *toks.get(k).ok_or("trunc rel")?; k += 1; reg.rel = Some(Box::new(Src { reg: reg_of(r), swz: [(r >> 16) as u8 & 3, (r >> 18) as u8 & 3, (r >> 20) as u8 & 3, (r >> 22) as u8 & 3], modifier: 0 })); }
                    let sw = (s >> 16) & 0xFF;
                    src.push(Src { reg, swz: [(sw & 3) as u8, ((sw >> 2) & 3) as u8, ((sw >> 4) & 3) as u8, ((sw >> 6) & 3) as u8], modifier: (s >> 24) & 0xF });
                }
                insts.push(Inst::Op { op, ctl, dst, src, pred });
            }
        }
    }
    Ok(Shader { stage, major, minor, insts, ctab })
}

/// Feature histogram used for the coverage report.
#[derive(Default)]
pub struct Hist { pub ops: BTreeMap<String, usize>, pub feats: BTreeMap<String, usize>, pub shaders_with: BTreeMap<String, usize> }
impl Hist {
    pub fn add(&mut self, s: &Shader) {
        let mut seen = std::collections::BTreeSet::new();
        let mut bump = |h: &mut BTreeMap<String, usize>, seen: &mut std::collections::BTreeSet<String>, k: String| { *h.entry(k.clone()).or_default() += 1; seen.insert(k); };
        for i in &s.insts {
            match i {
                Inst::Dcl { reg, usage, samp_ty, .. } => {
                    if reg.ty == RegType::Sampler { bump(&mut self.feats, &mut seen, format!("dcl sampler type {}", ["?", "?", "2d", "cube", "volume"].get(*samp_ty as usize).unwrap_or(&"?"))); }
                    else { bump(&mut self.feats, &mut seen, format!("dcl usage {usage} ({:?})", reg.ty)); }
                }
                Inst::Def { .. } => bump(&mut self.feats, &mut seen, "def float const".into()),
                Inst::DefI { .. } => bump(&mut self.feats, &mut seen, "defi int const".into()),
                Inst::DefB { .. } => bump(&mut self.feats, &mut seen, "defb bool const".into()),
                Inst::Op { op, ctl, dst, src, pred } => {
                    let mut name = opname(*op);
                    if *op == 66 { name = match ctl { 0 => "texld".into(), 1 => "texldp".into(), 2 => "texldb".into(), c => format!("texld(ctl={c})") }; }
                    bump(&mut self.ops, &mut seen, name.clone());
                    if *pred { bump(&mut self.feats, &mut seen, "predicated instruction".into()); }
                    if let Some(d) = dst {
                        if d.sat { bump(&mut self.feats, &mut seen, "dst _sat".into()); }
                        if d.pp { bump(&mut self.feats, &mut seen, "dst _pp".into()); }
                        if d.centroid { bump(&mut self.feats, &mut seen, "dst _centroid".into()); }
                        if d.shift != 0 { bump(&mut self.feats, &mut seen, "dst shift scale".into()); }
                        if d.reg.rel.is_some() { bump(&mut self.feats, &mut seen, "dst relative addressing".into()); }
                        bump(&mut self.feats, &mut seen, format!("dst reg {:?}", d.reg.ty));
                    }
                    for sx in src {
                        if sx.reg.rel.is_some() { bump(&mut self.feats, &mut seen, format!("src relative addressing ({:?})", sx.reg.ty)); }
                        if sx.modifier != 0 { bump(&mut self.feats, &mut seen, format!("src modifier {}", ["", "neg", "bias", "bias+neg", "bx2", "bx2+neg", "comp", "x2", "x2+neg", "dz", "dw", "abs", "abs+neg", "not", "?", "?"][sx.modifier as usize])); }
                        bump(&mut self.feats, &mut seen, format!("src reg {:?}", sx.reg.ty));
                    }
                    if matches!(op, 40 | 41 | 42 | 43) { bump(&mut self.feats, &mut seen, "static/dynamic branching".into()); }
                    if matches!(op, 27 | 29 | 38 | 39) { bump(&mut self.feats, &mut seen, "loop/rep".into()); }
                }
            }
        }
        for k in seen { *self.shaders_with.entry(k).or_default() += 1; }
    }
}
