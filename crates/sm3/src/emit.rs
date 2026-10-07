// SPDX-License-Identifier: GPL-3.0-or-later
//! Token stream to WGSL. Covers the opcodes the stock MP SM3 corpus uses. Semantics follow documented D3D9
//! behaviour (rcp/rsq of 0 = FLT_MAX, pow(|a|, b), lrp = mix, cmp selects on >= 0, ...).

use crate::{
    Error,
    ctab::{CtabEntry, RegisterSet},
    parse::{Dst, Inst, Reg, RegType, Shader, Src, Stage, opname},
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

/// Sampler `s` binds its texture at binding `s` and its sampler at `s + SAMPLER_BINDING_OFFSET` of group 2.
pub const SAMPLER_BINDING_OFFSET: u32 = 16;

/// Shader location for a D3D9 vertex/interpolant semantic (usage enum, usage index). Shared by VS inputs, VS outputs
/// and PS inputs so the stages link by semantic.
pub fn semantic_location(usage: u32, index: u32) -> u32 {
    match usage {
        5 => index.min(9),
        10 => 10 + index.min(1),
        11 => 12,
        0 | 6 => 13,
        3 | 7 => 14,
        _ => 15,
    }
}

fn usage_name(u: u32) -> &'static str {
    const N: [&str; 14] = [
        "position",
        "blendweight",
        "blendindices",
        "normal",
        "psize",
        "texcoord",
        "tangent",
        "binormal",
        "tessfactor",
        "positiont",
        "color",
        "fog",
        "depth",
        "sample",
    ];
    N.get(u as usize).copied().unwrap_or("?")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compare {
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

/// Alpha test: a fragment survives when `alpha <func> reference`; otherwise it is discarded. Applied to the final
/// alpha of color output 0, after the shader body.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlphaTest {
    pub func: Compare,
    pub reference: f32,
}

/// Conversion applied to a vertex shader input after fetch, for D3D9 vertex formats wgpu has no equivalent of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VertexFix {
    /// `D3DDECLTYPE_UBYTE4`: bytes arrive as floats `0..255`; the stream is declared `unorm8x4`, so scale by 255.
    Scale255,
    /// `D3DDECLTYPE_D3DCOLOR`: bytes are stored `b g r a`; the register holds `(r, g, b, a)`.
    Bgra,
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Vertex shaders only: fixes by input semantic `(usage, usage index)`.
    pub vertex_fixes: BTreeMap<(u32, u32), VertexFix>,
    /// Pixel shaders only. `None` = no alpha test.
    pub alpha_test: Option<AlphaTest>,
    /// Sampler registers sampled as depth textures with a comparison sampler (`textureSampleCompare`, reference =
    /// coordinate z, 2D only; the result is splatted to all four components).
    pub comparison_samplers: BTreeSet<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SamplerDim {
    D2,
    Cube,
    D3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SamplerUse {
    pub dim: SamplerDim,
    pub comparison: bool,
}

/// A declared input or output register and its D3D semantic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Semantic {
    pub register: u32,
    pub usage: u32,
    pub index: u32,
    pub location: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Reflection {
    pub ctab: Vec<CtabEntry>,
    /// Sampler registers actually sampled.
    pub samplers: BTreeMap<u32, SamplerUse>,
    /// Float constant registers read from the uniform bank (not `def`ined).
    pub float_consts: BTreeSet<u32>,
    pub bool_consts: BTreeSet<u32>,
    pub inputs: Vec<Semantic>,
    pub outputs: Vec<Semantic>,
    pub color_outputs: BTreeSet<u32>,
}

impl Reflection {
    /// CTAB name of the float constant that owns `register`.
    pub fn constant_name(&self, register: u32) -> Option<&str> {
        self.ctab
            .iter()
            .find(|e| {
                e.set == RegisterSet::Float4
                    && (u32::from(e.register)..u32::from(e.register) + u32::from(e.count))
                        .contains(&register)
            })
            .map(|e| e.name.as_str())
    }

    /// CTAB name of the sampler at `register`.
    pub fn sampler_name(&self, register: u32) -> Option<&str> {
        self.ctab
            .iter()
            .find(|e| e.set == RegisterSet::Sampler && u32::from(e.register) == register)
            .map(|e| e.name.as_str())
    }
}

#[derive(Clone, Debug)]
pub struct Translation {
    pub stage: Stage,
    pub wgsl: String,
    pub reflection: Reflection,
}

const COMP: [char; 4] = ['x', 'y', 'z', 'w'];

fn mask_comps(m: u8) -> Vec<usize> {
    (0..4).filter(|i| m & (1 << i) != 0).collect()
}

fn swz(comps: &[usize]) -> String {
    comps.iter().map(|&k| COMP[k]).collect()
}

fn lit(n: usize, v: f32) -> String {
    if n == 1 {
        fmt_f(v)
    } else {
        format!("vec{n}<f32>({})", fmt_f(v))
    }
}

/// `f32::MAX` as WGSL text. The shortest round-trip form, `3.4028235e38`, is above the maximum as a decimal number,
/// which Tint (Chrome's WGSL compiler) rejects as unrepresentable; this one is just below it and rounds to it.
const F32_MAX: &str = "3.4028234663852885e38";

fn fmt_f(v: f32) -> String {
    if v.is_nan() {
        "0.0".into()
    } else if v.is_infinite() || v.abs() == f32::MAX {
        format!("{}{F32_MAX}", if v < 0.0 { "-" } else { "" })
    } else {
        let s = format!("{v:?}");
        if s.contains(['e', '.']) {
            s
        } else {
            format!("{s}.0")
        }
    }
}

fn splat(e: String, n: usize) -> String {
    if n == 1 {
        e
    } else {
        format!("vec{n}<f32>({e})")
    }
}

/// Number of source operands an implemented opcode needs.
fn arity(op: u32) -> Option<usize> {
    Some(match op {
        0 | 42 | 43 | 39 => 0,
        1 | 6 | 7 | 14 | 15 | 16 | 19 | 34 | 35 | 36 | 37 | 40 | 91 | 92 | 65 | 38 => 1,
        2 | 3 | 5 | 8 | 9 | 10 | 11 | 12 | 13 | 17 | 32 | 41 | 66 | 95 => 2,
        4 | 18 | 88 | 90 => 3,
        _ => return None,
    })
}

struct Ctx<'a> {
    opts: &'a Options,
    out: String,
    ind: usize,
    uniq: usize,
    defs: BTreeMap<u32, [f32; 4]>,
    defi: BTreeMap<u32, [i32; 4]>,
    defb: BTreeMap<u32, bool>,
    refl: Reflection,
    sampler_types: BTreeMap<u32, u32>,
    temps: BTreeSet<u32>,
    uses_lit: bool,
    err: Option<Error>,
}

impl Ctx<'_> {
    fn line(&mut self, l: &str) {
        for _ in 0..self.ind {
            self.out.push_str("    ");
        }
        self.out.push_str(l);
        self.out.push('\n');
    }

    fn fail(&mut self, m: String) {
        self.err.get_or_insert(Error::Unsupported(m));
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.uniq += 1;
        format!("{prefix}{}", self.uniq)
    }

    /// `let` a vec4-or-narrower expression to a fresh name and return the name.
    fn bind(&mut self, prefix: &str, e: String) -> String {
        let n = self.fresh(prefix);
        self.line(&format!("let {n} = {e};"));
        n
    }

    /// Register as a full vec4 expression (no swizzle).
    fn reg(&mut self, r: &Reg) -> String {
        if r.rel.is_some() {
            self.fail("relative addressing".into());
            return "vec4<f32>(0.0)".into();
        }
        match r.ty {
            RegType::Temp => {
                self.temps.insert(r.num);
                format!("r{}", r.num)
            }
            RegType::Input => format!("v{}", r.num),
            RegType::Output | RegType::AttrOut | RegType::RastOut => format!("o{}", r.num),
            RegType::ColorOut => {
                self.refl.color_outputs.insert(r.num);
                format!("oc{}", r.num)
            }
            RegType::Const if self.defs.contains_key(&r.num) => format!("d{}", r.num),
            RegType::Const => {
                self.refl.float_consts.insert(r.num);
                format!("c[{}]", r.num)
            }
            t => {
                self.fail(format!("register type {t:?}"));
                "vec4<f32>(0.0)".into()
            }
        }
    }

    /// Source swizzled positionally by `comps`, then modified.
    fn src(&mut self, s: &Src, comps: &[usize]) -> String {
        let base = self.reg(&s.reg);
        let sw: String = comps
            .iter()
            .map(|&k| COMP[s.swizzle[k] as usize & 3])
            .collect();
        let e = format!("{base}.{sw}");
        match s.modifier {
            0 => e,
            1 => format!("(-{e})"),
            11 => format!("abs({e})"),
            12 => format!("(-abs({e}))"),
            m => {
                self.fail(format!("source modifier {m}"));
                e
            }
        }
    }

    fn src4(&mut self, s: &Src) -> String {
        self.src(s, &[0, 1, 2, 3])
    }

    fn scalar(&mut self, s: &Src) -> String {
        self.src(s, &[0])
    }

    fn write_dst(&mut self, d: &Dst, mut val: String, n: usize) {
        if d.saturate {
            val = format!("clamp({val}, {}, {})", lit(n, 0.0), lit(n, 1.0));
        }
        if d.shift != 0 {
            val = format!("({val} * {})", lit(n, 2f32.powi(i32::from(d.shift))));
        }
        let name = match d.reg.ty {
            RegType::Temp => {
                self.temps.insert(d.reg.num);
                format!("r{}", d.reg.num)
            }
            RegType::Output | RegType::AttrOut | RegType::RastOut => format!("o{}", d.reg.num),
            RegType::ColorOut => {
                self.refl.color_outputs.insert(d.reg.num);
                format!("oc{}", d.reg.num)
            }
            t => {
                self.fail(format!("destination register type {t:?}"));
                return;
            }
        };
        let cs = mask_comps(d.mask);
        if cs.len() == 4 {
            self.line(&format!("{name} = {val};"));
        } else if n == 1 {
            for &c in &cs {
                self.line(&format!("{name}.{} = {val};", COMP[c]));
            }
        } else {
            let t = self.bind("t", val);
            for (i, &c) in cs.iter().enumerate() {
                self.line(&format!("{name}.{} = {t}.{};", COMP[c], COMP[i]));
            }
        }
    }

    fn texld(&mut self, d: &Dst, coord: &Src, sampler: &Src, kind: &str) {
        let si = sampler.reg.num;
        let dim = match self.sampler_types.get(&si) {
            Some(3) => SamplerDim::Cube,
            Some(4) => SamplerDim::D3,
            _ => SamplerDim::D2,
        };
        let comparison = self.opts.comparison_samplers.contains(&si);
        if comparison && dim != SamplerDim::D2 {
            self.fail(format!("comparison sampler s{si} is not 2D"));
            return;
        }
        self.refl
            .samplers
            .insert(si, SamplerUse { dim, comparison });
        let c4 = self.src4(coord);
        let c = self.bind("co", c4);
        let dims = if dim == SamplerDim::D2 { "xy" } else { "xyz" };
        let e = if comparison {
            let coord = match kind {
                "texldp" => format!("{c}.xy / {c}.w, {c}.z / {c}.w"),
                _ => format!("{c}.xy, {c}.z"),
            };
            let f = if kind == "texldl" {
                "textureSampleCompareLevel"
            } else {
                "textureSampleCompare"
            };
            splat(format!("{f}(tex{si}, smp{si}, {coord})"), 4)
        } else {
            match kind {
                "texld" => format!("textureSample(tex{si}, smp{si}, {c}.{dims})"),
                "texldp" => format!("textureSample(tex{si}, smp{si}, {c}.{dims} / {c}.w)"),
                "texldb" => format!("textureSampleBias(tex{si}, smp{si}, {c}.{dims}, {c}.w)"),
                _ => format!("textureSampleLevel(tex{si}, smp{si}, {c}.{dims}, {c}.w)"),
            }
        };
        // result component k goes to destination component k; the sampler register has no swizzle
        let cs = mask_comps(d.mask);
        let val = if cs.len() == 4 {
            e
        } else {
            format!("({e}).{}", swz(&cs))
        };
        self.write_dst(d, val, cs.len());
    }

    fn inst(&mut self, i: &Inst) {
        let Inst::Op {
            op,
            ctl,
            dst,
            src,
            predicated,
        } = i
        else {
            return;
        };
        let (op, ctl) = (*op, *ctl);
        if *predicated {
            return self.fail("predicated instruction".into());
        }
        let Some(nsrc) = arity(op) else {
            return self.fail(format!("opcode {}", opname(op)));
        };
        if src.len() < nsrc {
            self.err
                .get_or_insert(Error::Malformed("missing source operand"));
            return;
        }
        let cmp = |c: u32| match c {
            1 => ">",
            2 => "==",
            3 => ">=",
            4 => "<",
            5 => "!=",
            6 => "<=",
            _ => "==",
        };
        match op {
            0 => {}
            41 => {
                let (a, b) = (self.scalar(&src[0]), self.scalar(&src[1]));
                self.line(&format!("if ({a} {} {b}) {{", cmp(ctl)));
                self.ind += 1;
            }
            40 => {
                let n = src[0].reg.num;
                let v = match self.defb.get(&n) {
                    Some(b) => b.to_string(),
                    None => {
                        self.refl.bool_consts.insert(n);
                        format!("(bc[{}][{}] != 0u)", n / 4, n % 4)
                    }
                };
                self.line(&format!("if ({v}) {{"));
                self.ind += 1;
            }
            42 => {
                self.ind -= 1;
                self.line("} else {");
                self.ind += 1;
            }
            43 | 39 => {
                self.ind -= 1;
                self.line("}");
            }
            38 => {
                let r = &src[0].reg;
                let Some(v) = self.defi.get(&r.num).filter(|_| r.ty == RegType::ConstInt) else {
                    return self.fail("rep with non-def'd integer constant".into());
                };
                let cnt = v[0];
                let n = self.fresh("rep");
                self.line(&format!("for (var {n}: i32 = 0; {n} < {cnt}; {n}++) {{"));
                self.ind += 1;
            }
            65 => {
                let c = self.reg(&src[0].reg);
                self.line(&format!(
                    "if (any({c}.xyz < vec3<f32>(0.0))) {{ discard; }}"
                ));
            }
            66 | 95 => {
                let Some(d) = dst else {
                    return self.fail("texld without destination".into());
                };
                let kind = if op == 95 {
                    "texldl"
                } else {
                    match ctl {
                        0 => "texld",
                        1 => "texldp",
                        2 => "texldb",
                        _ => return self.fail(format!("texld control {ctl}")),
                    }
                };
                self.texld(d, &src[0], &src[1], kind);
            }
            _ => {
                let Some(d) = dst else {
                    return self.fail(format!("opcode {} without destination", opname(op)));
                };
                let cs = mask_comps(d.mask);
                let n = cs.len();
                if n == 0 {
                    return;
                }
                macro_rules! s {
                    ($k:expr) => {
                        self.src(&src[$k], &cs)
                    };
                }
                let val = match op {
                    1 => s!(0),
                    2 => format!("({} + {})", s!(0), s!(1)),
                    3 => format!("({} - {})", s!(0), s!(1)),
                    4 => format!("({} * {} + {})", s!(0), s!(1), s!(2)),
                    5 => format!("({} * {})", s!(0), s!(1)),
                    6 => {
                        let a = self.scalar(&src[0]);
                        splat(format!("select(1.0 / {a}, {F32_MAX}, {a} == 0.0)"), n)
                    }
                    7 => {
                        let a = self.scalar(&src[0]);
                        splat(
                            format!("select(inverseSqrt(abs({a})), {F32_MAX}, {a} == 0.0)"),
                            n,
                        )
                    }
                    8 | 9 => {
                        let w = if op == 8 { 3 } else { 4 };
                        let (a, b) = (self.src4(&src[0]), self.src4(&src[1]));
                        let sw = &"xyzw"[..w];
                        splat(format!("dot(({a}).{sw}, ({b}).{sw})"), n)
                    }
                    10 => format!("min({}, {})", s!(0), s!(1)),
                    11 => format!("max({}, {})", s!(0), s!(1)),
                    12 => {
                        let (a, b) = (s!(0), s!(1));
                        format!("select({}, {}, {a} < {b})", lit(n, 0.0), lit(n, 1.0))
                    }
                    13 => {
                        let (a, b) = (s!(0), s!(1));
                        format!("select({}, {}, {a} >= {b})", lit(n, 0.0), lit(n, 1.0))
                    }
                    14 => format!("exp2({})", s!(0)),
                    15 => format!("log2(abs({}))", s!(0)),
                    16 => {
                        self.uses_lit = true;
                        let a = self.src4(&src[0]);
                        let t = self.bind("l", format!("sm3_lit({a})"));
                        if n == 4 {
                            t
                        } else {
                            format!("{t}.{}", swz(&cs))
                        }
                    }
                    17 => {
                        let (a, b) = (self.src4(&src[0]), self.src4(&src[1]));
                        let t = self.bind(
                            "l",
                            format!("vec4<f32>(1.0, ({a}).y * ({b}).y, ({a}).z, ({b}).w)"),
                        );
                        if n == 4 {
                            t
                        } else {
                            format!("{t}.{}", swz(&cs))
                        }
                    }
                    18 => format!("mix({}, {}, {})", s!(2), s!(1), s!(0)),
                    19 => format!("fract({})", s!(0)),
                    32 => format!("pow(abs({}), {})", s!(0), s!(1)),
                    34 => format!("sign({})", s!(0)),
                    35 => format!("abs({})", s!(0)),
                    36 if n == 1 => format!("sign({})", s!(0)),
                    36 => {
                        let a = s!(0);
                        let t = self.bind("n", a);
                        format!("select(vec{n}<f32>(0.0), normalize({t}), dot({t}, {t}) > 0.0)")
                    }
                    37 => {
                        let a = self.scalar(&src[0]);
                        match d.mask & 3 {
                            1 => format!("cos({a})"),
                            2 => format!("sin({a})"),
                            3 => format!("vec2<f32>(cos({a}), sin({a}))"),
                            _ => return self.fail("sincos mask".into()),
                        }
                    }
                    88 => {
                        let (a, b, c) = (s!(0), s!(1), s!(2));
                        format!("select({c}, {b}, {a} >= {})", lit(n, 0.0))
                    }
                    90 => {
                        let (a, b, c) =
                            (self.src4(&src[0]), self.src4(&src[1]), self.scalar(&src[2]));
                        splat(format!("(dot(({a}).xy, ({b}).xy) + {c})"), n)
                    }
                    91 => format!("dpdx({})", s!(0)),
                    92 => format!("dpdy({})", s!(0)),
                    _ => return self.fail(format!("opcode {}", opname(op))),
                };
                if self.err.is_none() {
                    self.write_dst(d, val, n);
                }
            }
        }
    }
}

fn io_fields(w: &mut String, regs: &[Semantic], vertex_out: bool) {
    for s in regs {
        let Semantic {
            register: r,
            usage: u,
            index: i,
            location,
        } = *s;
        if vertex_out && u == 0 {
            let _ = writeln!(w, "    @builtin(position) o{r}: vec4<f32>,");
        } else if vertex_out {
            let _ = writeln!(
                w,
                "    @location({location}) o{r}: vec4<f32>, // {}{i}",
                usage_name(u)
            );
        } else {
            let _ = writeln!(
                w,
                "    @location({location}) v{r}: vec4<f32>, // {}{i}",
                usage_name(u)
            );
        }
    }
}

pub(crate) fn emit(s: &Shader, opts: &Options) -> Result<Translation, Error> {
    let mut c = Ctx {
        opts,
        out: String::new(),
        ind: 1,
        uniq: 0,
        defs: BTreeMap::new(),
        defi: BTreeMap::new(),
        defb: BTreeMap::new(),
        refl: Reflection {
            ctab: s.ctab.clone(),
            ..Reflection::default()
        },
        sampler_types: BTreeMap::new(),
        temps: BTreeSet::new(),
        uses_lit: false,
        err: None,
    };
    let vs = s.stage == Stage::Vertex;
    if vs && opts.alpha_test.is_some() {
        return Err(Error::Unsupported("alpha test on a vertex shader".into()));
    }
    for i in &s.insts {
        match i {
            Inst::Def { reg, v } => {
                c.defs.insert(reg.num, *v);
            }
            Inst::DefI { reg, v } => {
                c.defi.insert(reg.num, *v);
            }
            Inst::DefB { reg, v } => {
                c.defb.insert(reg.num, *v);
            }
            Inst::Dcl {
                reg,
                usage,
                index,
                sampler_type,
            } => {
                let sem = Semantic {
                    register: reg.num,
                    usage: *usage,
                    index: *index,
                    location: semantic_location(*usage, *index),
                };
                match reg.ty {
                    RegType::Sampler => {
                        c.sampler_types.insert(reg.num, *sampler_type);
                    }
                    RegType::Input => c.refl.inputs.push(sem),
                    RegType::Output | RegType::AttrOut | RegType::RastOut => {
                        c.refl.outputs.push(sem)
                    }
                    _ => {}
                }
            }
            Inst::Op { .. } => {}
        }
    }
    for i in &s.insts {
        c.inst(i);
        if let Some(e) = c.err.take() {
            return Err(e);
        }
    }
    if let Some(a) = opts.alpha_test {
        if !c.refl.color_outputs.contains(&0) {
            return Err(Error::Unsupported(
                "alpha test without color output 0".into(),
            ));
        }
        let op = match a.func {
            Compare::Less => "<",
            Compare::LessEqual => "<=",
            Compare::Greater => ">",
            Compare::GreaterEqual => ">=",
        };
        c.line(&format!(
            "if (!(oc0.w {op} {})) {{ discard; }}",
            fmt_f(a.reference)
        ));
    }

    let mut w = String::new();
    let _ = writeln!(
        w,
        "// generated by the sm3 crate: {:?} {}_{}",
        s.stage, s.major, s.minor
    );
    let group = if vs { 0 } else { 1 };
    let _ = writeln!(
        w,
        "@group({group}) @binding(0) var<uniform> c: array<vec4<f32>, 256>;"
    );
    if !c.refl.bool_consts.is_empty() {
        let _ = writeln!(
            w,
            "@group({group}) @binding(1) var<uniform> bc: array<vec4<u32>, 2>;"
        );
    }
    for (si, u) in &c.refl.samplers {
        let (tex, smp) = match (u.comparison, u.dim) {
            (true, _) => ("texture_depth_2d", "sampler_comparison"),
            (_, SamplerDim::Cube) => ("texture_cube<f32>", "sampler"),
            (_, SamplerDim::D3) => ("texture_3d<f32>", "sampler"),
            (_, SamplerDim::D2) => ("texture_2d<f32>", "sampler"),
        };
        let _ = writeln!(w, "@group(2) @binding({si}) var tex{si}: {tex};");
        let _ = writeln!(
            w,
            "@group(2) @binding({}) var smp{si}: {smp};",
            si + SAMPLER_BINDING_OFFSET
        );
    }
    for (r, v) in &c.defs {
        let _ = writeln!(
            w,
            "const d{r}: vec4<f32> = vec4<f32>({}, {}, {}, {});",
            fmt_f(v[0]),
            fmt_f(v[1]),
            fmt_f(v[2]),
            fmt_f(v[3])
        );
    }
    if c.uses_lit {
        w.push_str(
            "fn sm3_lit(s: vec4<f32>) -> vec4<f32> {\n    let p = clamp(s.w, -127.9961, 127.9961);\n    var r = vec4<f32>(1.0, 0.0, 0.0, 1.0);\n    if (s.x > 0.0) { r.y = s.x; if (s.y > 0.0) { r.z = pow(s.y, p); } }\n    return r;\n}\n",
        );
    }
    let (ins, outs) = (c.refl.inputs.clone(), c.refl.outputs.clone());
    if vs {
        w.push_str("struct VsIn {\n");
        io_fields(&mut w, &ins, false);
        w.push_str("}\nstruct VsOut {\n");
        io_fields(&mut w, &outs, true);
        w.push_str("}\n@vertex\nfn main(inp: VsIn) -> VsOut {\n");
        for s in &ins {
            let r = s.register;
            let e = match opts.vertex_fixes.get(&(s.usage, s.index)) {
                Some(VertexFix::Scale255) => format!("inp.v{r} * 255.0"),
                Some(VertexFix::Bgra) => format!("inp.v{r}.zyxw"),
                None => format!("inp.v{r}"),
            };
            let _ = writeln!(w, "    let v{r} = {e};");
        }
        for r in outs.iter().map(|s| s.register) {
            let _ = writeln!(w, "    var o{r} = vec4<f32>(0.0);");
        }
    } else {
        if !ins.is_empty() {
            w.push_str("struct PsIn {\n");
            io_fields(&mut w, &ins, false);
            w.push_str("}\n");
        }
        w.push_str("struct PsOut {\n");
        for k in &c.refl.color_outputs {
            let _ = writeln!(w, "    @location({k}) oc{k}: vec4<f32>,");
        }
        w.push_str("}\n@fragment\n");
        w.push_str(if ins.is_empty() {
            "fn main() -> PsOut {\n"
        } else {
            "fn main(inp: PsIn) -> PsOut {\n"
        });
        for r in ins.iter().map(|s| s.register) {
            let _ = writeln!(w, "    let v{r} = inp.v{r};");
        }
        for k in &c.refl.color_outputs {
            let _ = writeln!(w, "    var oc{k} = vec4<f32>(0.0);");
        }
    }
    for t in &c.temps {
        let _ = writeln!(w, "    var r{t} = vec4<f32>(0.0);");
    }
    w.push_str(&c.out);
    let ret: Vec<String> = if vs {
        outs.iter().map(|s| format!("o{}", s.register)).collect()
    } else {
        c.refl
            .color_outputs
            .iter()
            .map(|k| format!("oc{k}"))
            .collect()
    };
    let _ = writeln!(
        w,
        "    return {}({});\n}}",
        if vs { "VsOut" } else { "PsOut" },
        ret.join(", ")
    );
    Ok(Translation {
        stage: s.stage,
        wgsl: w,
        reflection: c.refl,
    })
}
