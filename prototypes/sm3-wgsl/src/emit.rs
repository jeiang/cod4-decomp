//! Path (c): minimal SM2/SM3 token stream -> WGSL emitter. Throwaway; covers exactly the opcodes the
//! stock MP SM3 corpus uses (see the histogram in the report). Semantics of individual ops follow the
//! documented D3D9 behaviour (rcp/rsq of 0 = FLT_MAX, pow(|a|,b), lrp = mix, cmp >= 0, ...).
use crate::sm3::*;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

pub const SAMPLER_BINDING_OFFSET: u32 = 16;

/// Location for a D3D9 vertex/interpolant semantic (usage enum, usage index). Shared by VS inputs,
/// VS outputs and PS inputs so the stages link by semantic exactly like D3D9 does.
pub fn sem_loc(usage: u32, index: u32) -> u32 {
    match usage { 5 => index.min(9), 10 => 10 + index.min(1), 11 => 12, 0 => 13, 3 => 14, 4 => 15, 6 => 13, 7 => 14, _ => 15 }
}
pub fn usage_name(u: u32) -> &'static str {
    ["position", "blendweight", "blendindices", "normal", "psize", "texcoord", "tangent", "binormal", "tessfactor", "positiont", "color", "fog", "depth", "sample"].get(u as usize).copied().unwrap_or("?")
}

#[derive(Default, Debug, Clone)]
pub struct Emitted {
    pub wgsl: String,
    pub stage: Option<Stage>,
    /// sampler register -> D3D sampler type (2 = 2D, 3 = cube, 4 = volume)
    pub samplers: BTreeMap<u32, u32>,
    /// float constant registers read from the uniform bank (not `def`ed)
    pub const_regs: BTreeSet<u32>,
    pub bool_regs: BTreeSet<u32>,
    /// (input register, usage, usage index)
    pub inputs: Vec<(u32, u32, u32)>,
    pub outputs: Vec<(u32, u32, u32)>,
    pub ctab: Vec<CtabEntry>,
}

fn comp(c: u8) -> char { ['x', 'y', 'z', 'w'][c as usize & 3] }
fn mask_comps(m: u8) -> Vec<usize> { (0..4).filter(|i| m & (1 << i) != 0).collect() }
fn vty(n: usize) -> String { if n == 1 { "f32".into() } else { format!("vec{n}<f32>") } }
fn lit(n: usize, v: f32) -> String { if n == 1 { fmt_f(v) } else { format!("vec{n}<f32>({})", fmt_f(v)) } }
fn fmt_f(v: f32) -> String { if v.is_nan() { "0.0".into() } else if v.is_infinite() { format!("{}3.4028235e38", if v < 0.0 { "-" } else { "" }) } else { let s = format!("{v:?}"); if s.contains('e') || s.contains('.') { s } else { format!("{s}.0") } } }

struct Ctx<'a> {
    s: &'a Shader,
    out: String,
    ind: usize,
    defs: BTreeMap<u32, [f32; 4]>,
    defi: BTreeMap<u32, [i32; 4]>,
    defb: BTreeMap<u32, bool>,
    em: Emitted,
    samp_ty: BTreeMap<u32, u32>,
    temps: BTreeSet<u32>,
    reps: usize,
    uses_lit: bool,
    colorouts: BTreeSet<u32>,
    err: Option<String>,
}

impl<'a> Ctx<'a> {
    fn line(&mut self, l: &str) { for _ in 0..self.ind { self.out.push_str("    "); } self.out.push_str(l); self.out.push('\n'); }
    fn fail(&mut self, m: String) { if self.err.is_none() { self.err = Some(m); } }

    /// Register as a full vec4 expression (no swizzle).
    fn reg(&mut self, r: &Reg) -> String {
        if r.rel.is_some() { self.fail("relative addressing".into()); return "vec4<f32>(0.0)".into(); }
        match r.ty {
            RegType::Temp => { self.temps.insert(r.num); format!("r{}", r.num) }
            RegType::Input => format!("v{}", r.num),
            RegType::Output | RegType::AttrOut | RegType::RastOut => format!("o{}", r.num),
            RegType::ColorOut => { self.colorouts.insert(r.num); format!("oc{}", r.num) }
            RegType::Const => {
                if self.defs.contains_key(&r.num) { format!("d{}", r.num) } else { self.em.const_regs.insert(r.num); format!("c[{}]", r.num) }
            }
            t => { self.fail(format!("register type {t:?}")); "vec4<f32>(0.0)".into() }
        }
    }

    /// Source with swizzle restricted to `comps` (positional, like D3D), then modifier. Returns expression of width comps.len().
    fn src(&mut self, s: &Src, comps: &[usize]) -> String {
        let base = self.reg(&s.reg);
        let sw: String = comps.iter().map(|&k| comp(s.swz[k])).collect();
        let e = format!("{base}.{sw}");
        let e = if comps.len() == 1 { e } else { e };
        match s.modifier { 0 => e, 1 => format!("(-{e})"), 11 => format!("abs({e})"), 12 => format!("(-abs({e}))"), m => { self.fail(format!("source modifier {m}")); e } }
    }
    /// Full 4-wide source (all swizzle comps) with modifier.
    fn src4(&mut self, s: &Src) -> String { self.src(s, &[0, 1, 2, 3]) }
    fn scalar(&mut self, s: &Src) -> String { self.src(s, &[0]) }

    fn write_dst(&mut self, d: &Dst, mut val: String, n: usize) {
        if d.sat { val = format!("clamp({val}, {}, {})", lit(n, 0.0), lit(n, 1.0)); }
        if d.shift != 0 { val = format!("({val} * {})", lit(n, 2f32.powi(d.shift as i32))); }
        let name = match d.reg.ty {
            RegType::Temp => { self.temps.insert(d.reg.num); format!("r{}", d.reg.num) }
            RegType::Output | RegType::AttrOut | RegType::RastOut => format!("o{}", d.reg.num),
            RegType::ColorOut => { self.colorouts.insert(d.reg.num); format!("oc{}", d.reg.num) }
            t => { self.fail(format!("dest register type {t:?}")); return; }
        };
        let cs = mask_comps(d.mask);
        if cs.len() == 4 { self.line(&format!("{name} = {val};")); return; }
        if n == 1 { for c in &cs { self.line(&format!("{name}.{} = {val};", comp(*c as u8))); } return; }
        let t = format!("t{}", self.out.len());
        self.line(&format!("let {t} = {val};"));
        for (i, c) in cs.iter().enumerate() { self.line(&format!("{name}.{} = {t}.{};", comp(*c as u8), comp(i as u8))); }
    }

    fn splat(&self, e: String, n: usize) -> String { if n == 1 { e } else { format!("vec{n}<f32>({e})") } }

    fn texld(&mut self, d: &Dst, coord: &Src, sampler: &Src, kind: &str) {
        let si = sampler.reg.num; let ty = *self.samp_ty.get(&si).unwrap_or(&2);
        self.em.samplers.insert(si, ty);
        let c4 = self.src4(coord);
        let ctmp = format!("co{}", self.out.len()); self.line(&format!("let {ctmp} = {c4};"));
        let dims = if ty == 2 { "xy" } else { "xyz" };
        let e = match kind {
            "texld" => format!("textureSample(tex{si}, smp{si}, {ctmp}.{dims})"),
            "texldp" => format!("textureSample(tex{si}, smp{si}, {ctmp}.{dims} / {ctmp}.w)"),
            "texldb" => format!("textureSampleBias(tex{si}, smp{si}, {ctmp}.{dims}, {ctmp}.w)"),
            _ => format!("textureSampleLevel(tex{si}, smp{si}, {ctmp}.{dims}, {ctmp}.w)"),
        };
        let cs = mask_comps(d.mask);
        // D3D texld returns rgba; apply dest mask by writing full vec4 then masking inside write_dst via swizzle
        let n = cs.len();
        let sw: String = cs.iter().map(|&k| comp(k as u8)).collect();
        // texture result component k corresponds to dest component k (no source swizzle for the sampler register)
        let val = if n == 4 { e } else { format!("({e}).{sw}") };
        self.write_dst(d, val, n);
    }

    fn inst(&mut self, i: &Inst) {
        let (op, ctl, dst, src) = match i { Inst::Op { op, ctl, dst, src, pred } => { if *pred { self.fail("predicated instruction".into()); return; } (*op, *ctl, dst, src) } _ => return };
        let cmpop = |c: u32| match c { 1 => ">", 2 => "==", 3 => ">=", 4 => "<", 5 => "!=", 6 => "<=", _ => "==" };
        match op {
            0 => {}
            41 => { let a = self.scalar(&src[0]); let b = self.scalar(&src[1]); self.line(&format!("if ({a} {} {b}) {{", cmpop(ctl))); self.ind += 1; }
            40 => {
                let r = &src[0].reg; let v = if let Some(b) = self.defb.get(&r.num) { b.to_string() } else { self.em.bool_regs.insert(r.num); format!("(bc[{}][{}] != 0u)", r.num / 4, r.num % 4) };
                self.line(&format!("if ({v}) {{")); self.ind += 1;
            }
            42 => { self.ind -= 1; self.line("} else {"); self.ind += 1; }
            43 | 39 => { self.ind -= 1; self.line("}"); }
            38 => {
                let r = &src[0].reg;
                let cnt = match (r.ty, self.defi.get(&r.num)) { (RegType::ConstInt, Some(v)) => v[0], _ => { self.fail("rep with non-def'd integer constant".into()); 0 } };
                let n = self.reps; self.reps += 1;
                self.line(&format!("for (var rep{n}: i32 = 0; rep{n} < {cnt}; rep{n}++) {{")); self.ind += 1;
            }
            65 => { let c = self.reg(&src[0].reg); self.line(&format!("if (any({c}.xyz < vec3<f32>(0.0))) {{ discard; }}")); }
            66 | 95 => {
                let d = dst.as_ref().unwrap();
                let kind = if op == 95 { "texldl" } else { match ctl { 0 => "texld", 1 => "texldp", 2 => "texldb", _ => { self.fail(format!("texld control {ctl}")); return; } } };
                self.texld(d, &src[0], &src[1], kind);
            }
            _ => {
                let d = match dst { Some(d) => d, None => { self.fail(format!("opcode {} without dest", opname(op))); return; } };
                let cs = mask_comps(d.mask); let n = cs.len();
                if n == 0 { return; }
                macro_rules! s { ($k:expr) => { self.src(&src[$k], &cs) } }
                let ty = vty(n);
                let val: String = match op {
                    1 => s!(0),
                    2 => format!("({} + {})", s!(0), s!(1)),
                    3 => format!("({} - {})", s!(0), s!(1)),
                    4 => format!("({} * {} + {})", s!(0), s!(1), s!(2)),
                    5 => format!("({} * {})", s!(0), s!(1)),
                    6 => { let a = self.scalar(&src[0]); let v = format!("select(1.0 / {a}, 3.4028235e38, {a} == 0.0)"); self.splat(v, n) }
                    7 => { let a = self.scalar(&src[0]); let v = format!("select(inverseSqrt(abs({a})), 3.4028235e38, {a} == 0.0)"); self.splat(v, n) }
                    8 | 9 => {
                        let w = if op == 8 { 3 } else { 4 };
                        let a = self.src4(&src[0]); let b = self.src4(&src[1]);
                        let sw = &"xyzw"[..w];
                        let v = if w == 3 { format!("dot(({a}).{sw}, ({b}).{sw})") } else { format!("dot({a}, {b})") };
                        self.splat(v, n)
                    }
                    10 => format!("min({}, {})", s!(0), s!(1)),
                    11 => format!("max({}, {})", s!(0), s!(1)),
                    12 => { let (a, b) = (s!(0), s!(1)); format!("select({}, {}, {a} < {b})", lit(n, 0.0), lit(n, 1.0)) }
                    13 => { let (a, b) = (s!(0), s!(1)); format!("select({}, {}, {a} >= {b})", lit(n, 0.0), lit(n, 1.0)) }
                    14 => format!("exp2({})", s!(0)),
                    15 => format!("log2(abs({}))", s!(0)),
                    16 => { self.uses_lit = true; let a = self.src4(&src[0]); let v = format!("sm3_lit({a})"); let t = format!("l{}", self.out.len()); self.line(&format!("let {t} = {v};")); if n == 4 { t } else { format!("{t}.{}", cs.iter().map(|&k| comp(k as u8)).collect::<String>()) } }
                    17 => { let a = self.src4(&src[0]); let b = self.src4(&src[1]); let t = format!("l{}", self.out.len()); self.line(&format!("let {t} = vec4<f32>(1.0, ({a}).y * ({b}).y, ({a}).z, ({b}).w);")); if n == 4 { t } else { format!("{t}.{}", cs.iter().map(|&k| comp(k as u8)).collect::<String>()) } }
                    18 => format!("mix({}, {}, {})", s!(2), s!(1), s!(0)),
                    19 => format!("fract({})", s!(0)),
                    32 => format!("pow(abs({}), {})", s!(0), s!(1)),
                    34 => format!("sign({})", s!(0)),
                    35 => format!("abs({})", s!(0)),
                    36 => {
                        if n == 1 { format!("sign({})", s!(0)) } else { let a = s!(0); let t = format!("n{}", self.out.len()); self.line(&format!("let {t} = {a};")); format!("select(vec{n}<f32>(0.0), normalize({t}), dot({t}, {t}) > 0.0)") }
                    }
                    37 => {
                        let a = self.scalar(&src[0]);
                        match d.mask & 3 { 1 => format!("cos({a})"), 2 => format!("sin({a})"), 3 => format!("vec2<f32>(cos({a}), sin({a}))"), _ => { self.fail("sincos mask".into()); String::new() } }
                    }
                    88 => { let (a, b, c) = (s!(0), s!(1), s!(2)); let z = lit(n, 0.0); format!("select({c}, {b}, {a} >= {z})") }
                    90 => { let a = self.src4(&src[0]); let b = self.src4(&src[1]); let c = self.scalar(&src[2]); let v = format!("(dot(({a}).xy, ({b}).xy) + {c})"); self.splat(v, n) }
                    91 => format!("dpdx({})", s!(0)),
                    92 => format!("dpdy({})", s!(0)),
                    o => { self.fail(format!("opcode {}", opname(o))); String::new() }
                };
                if self.err.is_some() { return; }
                let _ = ty;
                self.write_dst(d, val, n);
            }
        }
    }
}

pub fn emit(s: &Shader) -> Result<Emitted, String> {
    let mut c = Ctx { s, out: String::new(), ind: 1, defs: BTreeMap::new(), defi: BTreeMap::new(), defb: BTreeMap::new(), em: Emitted::default(), samp_ty: BTreeMap::new(), temps: BTreeSet::new(), reps: 0, uses_lit: false, colorouts: BTreeSet::new(), err: None };
    c.em.stage = Some(s.stage); c.em.ctab = s.ctab.clone();
    let mut ins: Vec<(u32, u32, u32)> = vec![]; let mut outs: Vec<(u32, u32, u32)> = vec![];
    for i in &s.insts {
        match i {
            Inst::Def { reg, v } => { c.defs.insert(reg.num, *v); }
            Inst::DefI { reg, v } => { c.defi.insert(reg.num, *v); }
            Inst::DefB { reg, v } => { c.defb.insert(reg.num, *v); }
            Inst::Dcl { reg, usage, index, samp_ty } => match reg.ty {
                RegType::Sampler => { c.samp_ty.insert(reg.num, *samp_ty); }
                RegType::Input => ins.push((reg.num, *usage, *index)),
                RegType::Output | RegType::AttrOut | RegType::RastOut => outs.push((reg.num, *usage, *index)),
                _ => {}
            },
            _ => {}
        }
    }
    c.em.inputs = ins.clone(); c.em.outputs = outs.clone();
    let body_start = c.out.len(); let _ = body_start;
    for i in &s.insts { c.inst(i); if let Some(e) = &c.err { return Err(e.clone()); } }
    if let Some(e) = c.err.take() { return Err(e); }

    let mut w = String::new();
    let _ = writeln!(w, "// generated by sm3-wgsl-proto custom emitter (path c) -- {:?} {}_{}", s.stage, s.major, s.minor);
    // bindings
    let vs = s.stage == Stage::Vertex;
    let cgroup = if vs { 0 } else { 1 };
    let _ = writeln!(w, "@group({cgroup}) @binding(0) var<uniform> c: array<vec4<f32>, 256>;");
    if !c.em.bool_regs.is_empty() { let _ = writeln!(w, "@group({cgroup}) @binding(1) var<uniform> bc: array<vec4<u32>, 2>;"); }
    for (si, ty) in &c.em.samplers {
        let tt = match ty { 3 => "texture_cube<f32>", 4 => "texture_3d<f32>", _ => "texture_2d<f32>" };
        let _ = writeln!(w, "@group(2) @binding({si}) var tex{si}: {tt};");
        let _ = writeln!(w, "@group(2) @binding({}) var smp{si}: sampler;", si + SAMPLER_BINDING_OFFSET);
    }
    for (r, v) in &c.defs { let _ = writeln!(w, "const d{r}: vec4<f32> = vec4<f32>({}, {}, {}, {});", fmt_f(v[0]), fmt_f(v[1]), fmt_f(v[2]), fmt_f(v[3])); }
    if c.uses_lit {
        w.push_str("fn sm3_lit(s: vec4<f32>) -> vec4<f32> {\n    let p = clamp(s.w, -127.9961, 127.9961);\n    var r = vec4<f32>(1.0, 0.0, 0.0, 1.0);\n    if (s.x > 0.0) { r.y = s.x; if (s.y > 0.0) { r.z = pow(s.y, p); } }\n    return r;\n}\n");
    }
    // interface
    let mut io = String::new();
    if vs {
        let mut fields = String::new();
        for (r, u, i) in &ins { let _ = writeln!(fields, "    @location({}) v{r}: vec4<f32>, // {}{i}", sem_loc(*u, *i), usage_name(*u)); }
        let mut of = String::new();
        for (r, u, i) in &outs {
            if *u == 0 { let _ = writeln!(of, "    @builtin(position) o{r}: vec4<f32>,"); } else { let _ = writeln!(of, "    @location({}) o{r}: vec4<f32>, // {}{i}", sem_loc(*u, *i), usage_name(*u)); }
        }
        let _ = writeln!(io, "struct VsIn {{\n{fields}}}\nstruct VsOut {{\n{of}}}");
        let _ = writeln!(io, "@vertex\nfn main(inp: VsIn) -> VsOut {{");
        for (r, _, _) in &ins { let _ = writeln!(io, "    let v{r} = inp.v{r};"); }
        for (r, _, _) in &outs { let _ = writeln!(io, "    var o{r} = vec4<f32>(0.0);"); }
    } else {
        let mut fields = String::new();
        for (r, u, i) in &ins { let _ = writeln!(fields, "    @location({}) v{r}: vec4<f32>, // {}{i}", sem_loc(*u, *i), usage_name(*u)); }
        let mut of = String::new();
        for k in &c.colorouts { let _ = writeln!(of, "    @location({k}) oc{k}: vec4<f32>,"); }
        if ins.is_empty() {
            let _ = writeln!(io, "struct PsOut {{\n{of}}}");
            let _ = writeln!(io, "@fragment\nfn main() -> PsOut {{");
        } else {
            let _ = writeln!(io, "struct PsIn {{\n{fields}}}\nstruct PsOut {{\n{of}}}");
            let _ = writeln!(io, "@fragment\nfn main(inp: PsIn) -> PsOut {{");
        }
        for (r, _, _) in &ins { let _ = writeln!(io, "    let v{r} = inp.v{r};"); }
        for k in &c.colorouts { let _ = writeln!(io, "    var oc{k} = vec4<f32>(0.0);"); }
    }
    for t in &c.temps { let _ = writeln!(io, "    var r{t} = vec4<f32>(0.0);"); }
    w.push_str(&io); w.push_str(&c.out);
    if vs { let _ = writeln!(w, "    return VsOut({});", outs.iter().map(|(r, _, _)| format!("o{r}")).collect::<Vec<_>>().join(", ")); }
    else { let _ = writeln!(w, "    return PsOut({});", c.colorouts.iter().map(|k| format!("oc{k}")).collect::<Vec<_>>().join(", ")); }
    w.push_str("}\n");
    c.em.wgsl = w;
    let _ = c.s;
    Ok(c.em)
}
