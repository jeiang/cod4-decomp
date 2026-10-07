//! Produces WGSL + binding reflection for one D3D9 shader through any of the three paths.
use crate::{check, emit, sm3, spvsplit};
use std::{collections::HashMap, fs, path::Path};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PathId { Mojo, Vkd3d, Custom }
impl PathId {
    pub const ALL: [PathId; 3] = [PathId::Custom, PathId::Mojo, PathId::Vkd3d];
    pub fn label(self) -> &'static str { match self { PathId::Mojo => "a: MojoShader->SPIR-V->naga", PathId::Vkd3d => "b: vkd3d-shader 2.1->SPIR-V->naga", PathId::Custom => "c: custom SM3->WGSL" } }
    pub fn short(self) -> &'static str { match self { PathId::Mojo => "a-mojo", PathId::Vkd3d => "b-vkd3d", PathId::Custom => "c-custom" } }
}

#[derive(Clone, Debug)]
pub enum ResKind {
    /// float constant bank; `regmap` maps D3D register -> array index (None = identity)
    ConstBank { n: u32, regmap: Option<HashMap<u32, u32>> },
    Texture { reg: u32, dim: wgpu::TextureViewDimension },
    Sampler { reg: u32 },
    Other(String),
}
#[derive(Clone, Debug)]
pub struct Resource { pub group: u32, pub binding: u32, pub name: String, pub kind: ResKind }

#[derive(Clone, Debug)]
pub struct Prog {
    pub stage: sm3::Stage,
    pub wgsl: String,
    pub res: Vec<Resource>,
    /// (shader location, D3D input register) for vertex shaders
    pub vin: Vec<(u32, u32)>,
    /// D3D input/output semantics from the bytecode: reg -> (usage, index)
    pub dcl_in: HashMap<u32, (u32, u32)>,
    pub dcl_out: HashMap<u32, (u32, u32)>,
    pub ctab: Vec<sm3::CtabEntry>,
}

fn digits(s: &str) -> Option<u32> { let d: String = s.chars().filter(|c| c.is_ascii_digit()).collect(); d.parse().ok() }

fn reflect(wgsl: &str, path: PathId, stage: sm3::Stage) -> Result<(Vec<Resource>, Vec<(u32, u32)>), String> {
    let m = naga::front::wgsl::parse_str(wgsl).map_err(|e| e.emit_to_string(wgsl))?;
    let mut regmap = HashMap::new();
    for (_, c) in m.constants.iter() {
        if let Some(n) = &c.name {
            if path == PathId::Mojo && n.starts_with('c') && n.ends_with('_') {
                if let (Some(r), naga::Expression::Literal(naga::Literal::I32(i))) = (digits(n), &m.global_expressions[c.init]) { regmap.insert(r, *i as u32); }
            }
        }
    }
    let mut res = vec![];
    for (_, g) in m.global_variables.iter() {
        let Some(b) = &g.binding else { continue };
        let name = g.name.clone().unwrap_or_default();
        let ty = &m.types[g.ty].inner;
        let kind = match ty {
            naga::TypeInner::Image { dim, arrayed, .. } => {
                let reg = digits(&name).unwrap_or(0);
                let d = match (dim, arrayed) { (naga::ImageDimension::Cube, _) => wgpu::TextureViewDimension::Cube, (naga::ImageDimension::D3, _) => wgpu::TextureViewDimension::D3, _ => wgpu::TextureViewDimension::D2 };
                ResKind::Texture { reg, dim: d }
            }
            naga::TypeInner::Sampler { .. } => ResKind::Sampler { reg: digits(&name).unwrap_or(0) },
            naga::TypeInner::Array { size, .. } => ResKind::ConstBank { n: match size { naga::ArraySize::Constant(n) => n.get(), _ => 256 }, regmap: None },
            naga::TypeInner::Struct { members, .. } => {
                // first member must be the vec4 array
                let n = match &m.types[members[0].ty].inner { naga::TypeInner::Array { size: naga::ArraySize::Constant(n), .. } => n.get(), _ => 0 };
                if n == 0 { ResKind::Other(name.clone()) } else {
                    ResKind::ConstBank { n, regmap: if path == PathId::Mojo { Some(regmap.clone()) } else { None } }
                }
            }
            _ => ResKind::Other(name.clone()),
        };
        // the bool bank of the custom path is not a float bank
        let kind = if name == "bc" { ResKind::Other("bc".into()) } else { kind };
        res.push(Resource { group: b.group, binding: b.binding, name, kind });
    }
    res.sort_by_key(|r| (r.group, r.binding));
    let mut vin = vec![];
    if stage == sm3::Stage::Vertex {
        let ep = m.entry_points.first().ok_or("no entry point")?;
        for a in &ep.function.arguments {
            match (&a.binding, &m.types[a.ty].inner) {
                (Some(naga::Binding::Location { location, .. }), _) => { if let Some(r) = a.name.as_deref().and_then(digits) { vin.push((*location, r)); } }
                (None, naga::TypeInner::Struct { members, .. }) => for mem in members { if let (Some(naga::Binding::Location { location, .. }), Some(n)) = (&mem.binding, &mem.name) { if let Some(r) = digits(n) { vin.push((*location, r)); } } },
                _ => {}
            }
        }
    }
    Ok((res, vin))
}

fn dcls(s: &sm3::Shader) -> (HashMap<u32, (u32, u32)>, HashMap<u32, (u32, u32)>) {
    let (mut i, mut o) = (HashMap::new(), HashMap::new());
    for ins in &s.insts {
        if let sm3::Inst::Dcl { reg, usage, index, .. } = ins {
            match reg.ty { sm3::RegType::Input => { i.insert(reg.num, (*usage, *index)); } sm3::RegType::Output | sm3::RegType::AttrOut | sm3::RegType::RastOut => { o.insert(reg.num, (*usage, *index)); } _ => {} }
        }
    }
    (i, o)
}

/// Build the WGSL + reflection of shader `hash` (`kind` = "vs"/"ps") for `path`. `partner_vs` is the VS hash of the pair
/// (MojoShader links per pair). PS groups are shifted so that VS and PS never share a bind group.
pub fn build(path: PathId, kind: &str, hash: &str, partner_vs: &str, work: &Path) -> Result<Prog, String> {
    let code = fs::read(work.join("shaders").join(format!("{kind}_{hash}.bin"))).map_err(|e| e.to_string())?;
    let sh = sm3::parse(&code)?;
    let stage = sh.stage;
    let (dcl_in, dcl_out) = dcls(&sh);
    let sem = |r: u32, m: &HashMap<u32, (u32, u32)>| m.get(&r).map(|&(u, i)| emit::sem_loc(u, i)).unwrap_or(r.min(15));
    let wgsl = match path {
        PathId::Custom => emit::emit(&sh)?.wgsl,
        PathId::Mojo => {
            let f = if kind == "vs" { format!("vs_{hash}.spv") } else { format!("ps_{hash}__{partner_vs}.spv") };
            let words = check::words_of(&fs::read(work.join("mojo_out").join(f)).map_err(|e| e.to_string())?);
            let words = spvsplit::split_samplers(&words)?.0;
            check::spv_to_wgsl(&words).map_err(|f| format!("{}: {}", f.stage, f.msg))?
        }
        PathId::Vkd3d => {
            let words = check::words_of(&fs::read(work.join("vkd3d21_out").join(format!("{kind}_{hash}.spv"))).map_err(|e| e.to_string())?);
            let mut words = spvsplit::pointsize_to_location(&spvsplit::split_samplers(&words)?.0)?.0;
            // vkd3d numbers varyings by D3D register; D3D9 links by semantic -> remap to a semantic location
            if stage == sm3::Stage::Vertex { words = spvsplit::remap_locations(&words, 3, &|l| if l == 15 { 15 } else { sem(l, &dcl_out) })?; }
            else { words = spvsplit::remap_locations(&words, 1, &|l| sem(l, &dcl_in))?; words = spvsplit::shift_sets(&words, 1)?; }
            let w = check::spv_to_wgsl(&words).map_err(|f| format!("{}: {}", f.stage, f.msg))?;
            widen_interfaces(&w, stage)?
        }
    };
    let (res, vin) = reflect(&wgsl, path, stage)?;
    Ok(Prog { stage, wgsl, res, vin, dcl_in, dcl_out, ctab: sh.ctab })
}

/// vkd3d-shader narrows interpolants by the components the shader actually uses (vec3 colour input) while the VS
/// writes vec4; Metal rejects the mismatch at pipeline creation although WGSL/naga allows it. Text pass: widen every
/// non-builtin VS output member / PS input parameter to vec4 and swizzle back at the use site.
pub fn widen_interfaces(wgsl: &str, stage: sm3::Stage) -> Result<String, String> {
    let narrow = |t: &str| -> Option<(&'static str, &'static str)> { match t { "f32" => Some(("vec4<f32>(", ", 0.0, 0.0, 0.0)")), "vec2<f32>" => Some(("vec4<f32>(", ", 0.0, 0.0)")), "vec3<f32>" => Some(("vec4<f32>(", ", 0.0)")), _ => None } };
    let mut out = String::new();
    if stage == sm3::Stage::Vertex {
        // struct members
        let mut member_idx = 0usize; let mut widen: HashMap<usize, (&str, &str)> = HashMap::new();
        let mut in_struct = false; let mut lines: Vec<String> = vec![];
        for l in wgsl.lines() {
            if l.starts_with("struct VertexOutput") { in_struct = true; }
            if in_struct && l.starts_with('}') { in_struct = false; }
            if in_struct && l.trim_start().starts_with('@') {
                if l.contains("@location(") { if let Some((t, e)) = l.rsplit_once(": ").and_then(|(_, ty)| narrow(ty.trim_end_matches(','))) { widen.insert(member_idx, (t, e)); lines.push(l.rsplit_once(": ").map(|(a, _)| format!("{a}: vec4<f32>,")).unwrap()); member_idx += 1; continue; } }
                member_idx += 1;
            }
            lines.push(l.to_string());
        }
        let text = lines.join("\n");
        let ret = text.find("return VertexOutput(").ok_or("no VertexOutput return")?;
        let args_start = ret + "return VertexOutput(".len(); let args_end = text[args_start..].find(')').unwrap() + args_start;
        let args: Vec<&str> = text[args_start..args_end].split(", ").collect();
        let mut text = text.clone();
        for (i, a) in args.iter().enumerate() {
            if let Some((pre, post)) = widen.get(&i) {
                let pat = format!("let {a} = ");
                let p = text.find(&pat).ok_or("arg def not found")?; let e = p + text[p..].find(';').unwrap();
                let expr = text[p + pat.len()..e].to_string();
                text.replace_range(p..e, &format!("{pat}{pre}{expr}{post}"));
            }
        }
        out.push_str(&text); out.push('\n');
    } else {
        let p = wgsl.find("fn main(").ok_or("no main")?; let e = p + wgsl[p..].find(") -> ").unwrap();
        let params = &wgsl[p + 8..e];
        let mut new_params = vec![]; let mut fixes: Vec<(String, &str)> = vec![];
        for prm in params.split(", ") {
            if let Some((a, ty)) = prm.rsplit_once(": ") { if a.contains("@location(") { let nm = a.rsplit_once(' ').unwrap().1.to_string();
                match ty { "vec3<f32>" => { fixes.push((nm, ".xyz")); new_params.push(format!("{a}: vec4<f32>")); continue; } "vec2<f32>" => { fixes.push((nm, ".xy")); new_params.push(format!("{a}: vec4<f32>")); continue; } "f32" => { fixes.push((nm, ".x")); new_params.push(format!("{a}: vec4<f32>")); continue; } _ => {} } } }
            new_params.push(prm.to_string());
        }
        let mut text = format!("{}{}{}", &wgsl[..p + 8], new_params.join(", "), &wgsl[e..]);
        for (nm, sw) in fixes { let pat = format!("= {nm};"); if !text.contains(&pat) { return Err(format!("cannot narrow use of {nm}")); } text = text.replacen(&pat, &format!("= {nm}{sw};"), 1); }
        out.push_str(&text);
    }
    Ok(out)
}
