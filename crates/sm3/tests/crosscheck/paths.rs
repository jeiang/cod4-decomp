// SPDX-License-Identifier: GPL-3.0-or-later
//! WGSL plus binding reflection of one shader for each translation path.
use crate::spvsplit;
use naga::valid::{Capabilities, ValidationFlags, Validator};
use sm3::{Stage, Translation};
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Path {
    Ours,
    Mojo,
    Vkd3d,
}

impl Path {
    pub const ALL: [Path; 3] = [Path::Ours, Path::Mojo, Path::Vkd3d];
    pub fn name(self) -> &'static str {
        match self {
            Path::Ours => "ours",
            Path::Mojo => "mojoshader",
            Path::Vkd3d => "vkd3d-shader",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dim {
    D2,
    D3,
    Cube,
}

#[derive(Clone, Debug)]
pub enum Kind {
    /// Float constant bank of `vec4s` entries. `regmap` maps D3D register -> array index (identity if `None`).
    Float {
        vec4s: u32,
        regmap: Option<HashMap<u32, u32>>,
    },
    /// Any other uniform data (boolean/integer banks): bound as zeros.
    Zero {
        bytes: u64,
    },
    Texture {
        reg: u32,
        dim: Dim,
    },
    Sampler,
}

#[derive(Clone, Debug)]
pub struct Res {
    pub group: u32,
    pub binding: u32,
    pub kind: Kind,
}

pub struct Prog {
    pub wgsl: String,
    pub res: Vec<Res>,
    /// Vertex shader: (shader location, D3D semantic usage, usage index) of every input.
    pub vin: Vec<(u32, u32, u32)>,
    /// Pixel shader: number of color outputs.
    pub targets: u32,
}

fn digits(s: &str) -> Option<u32> {
    let d: String = s.chars().filter(char::is_ascii_digit).collect();
    d.parse().ok()
}

fn validate(m: &naga::Module) -> Result<naga::valid::ModuleInfo, String> {
    Validator::new(ValidationFlags::all(), Capabilities::empty())
        .validate(m)
        .map_err(|e| format!("naga validate: {:?}", e.into_inner()))
}

/// Reference SPIR-V -> naga -> WGSL (the sampler split, PointSize and locations fixed up on the way).
fn spv_to_wgsl(
    spv: &[u8],
    path: Path,
    stage: Stage,
    vkd3d_remap: &dyn Fn(u32) -> u32,
) -> Result<(String, HashMap<u32, u32>), String> {
    let mut comps = HashMap::new();
    let mut words: Vec<u32> = spv
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect();
    words = spvsplit::split_samplers(&words)?.0;
    if path == Path::Vkd3d {
        words = spvsplit::pointsize_to_location(&words)?.0;
        // vkd3d numbers varyings by D3D register; D3D9 links by semantic.
        words = if stage == Stage::Vertex {
            spvsplit::remap_locations(&words, 3, &|l| {
                if l == 15 { 15 } else { vkd3d_remap(l) }
            })?
        } else {
            spvsplit::remap_locations(&words, 1, vkd3d_remap)?
        };
        comps = spvsplit::component_offsets(&words, if stage == Stage::Vertex { 3 } else { 1 })?;
        // Keep vertex and pixel resources in separate bind groups.
        if stage == Stage::Pixel {
            words = spvsplit::shift_sets(&words, 1)?;
        }
    }
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let opts = naga::front::spv::Options {
        adjust_coordinate_space: false,
        strict_capabilities: false,
        block_ctx_dump_prefix: None,
    };
    let m = naga::front::spv::parse_u8_slice(&bytes, &opts).map_err(|e| format!("spv-in: {e}"))?;
    let info = validate(&m)?;
    let wgsl = naga::back::wgsl::write_string(&m, &info, naga::back::wgsl::WriterFlags::empty())
        .map_err(|e| format!("wgsl-out: {e}"))?;
    Ok((wgsl, comps))
}

/// Build `Prog` for `path`. `spv` is the driver output (ignored for `Ours`).
pub fn build(path: Path, tr: &Translation, spv: Option<&[u8]>) -> Result<Prog, String> {
    let stage = tr.stage;
    let sem_loc = |regs: &[sm3::Semantic], r: u32| {
        regs.iter()
            .find(|s| s.register == r)
            .map_or(r.min(15), |s| s.location)
    };
    let wgsl = match path {
        Path::Ours => tr.wgsl.clone(),
        Path::Mojo => spv_to_wgsl(spv.ok_or("no spirv")?, path, stage, &|l| l)?.0,
        Path::Vkd3d => {
            let regs = if stage == Stage::Vertex {
                &tr.reflection.outputs
            } else {
                &tr.reflection.inputs
            };
            let w = spv_to_wgsl(spv.ok_or("no spirv")?, path, stage, &|l| sem_loc(regs, l))?;
            widen_interfaces(&w.0, stage, &w.1)?
        }
    };
    reflect(wgsl, path, tr)
}

fn reflect(wgsl: String, path: Path, tr: &Translation) -> Result<Prog, String> {
    let m = naga::front::wgsl::parse_str(&wgsl).map_err(|e| e.emit_to_string(&wgsl))?;
    let mut regmap = HashMap::new();
    if path == Path::Mojo {
        for (_, c) in m.constants.iter() {
            if let Some(n) = &c.name
                && n.starts_with('c')
                && n.ends_with('_')
                && let (Some(r), naga::Expression::Literal(naga::Literal::I32(i))) =
                    (digits(n), &m.global_expressions[c.init])
            {
                regmap.insert(r, *i as u32);
            }
        }
    }
    let mut res = vec![];
    for (_, g) in m.global_variables.iter() {
        let Some(b) = &g.binding else { continue };
        let name = g.name.clone().unwrap_or_default();
        let is_vec4f = |t: naga::Handle<naga::Type>| {
            matches!(
                m.types[t].inner,
                naga::TypeInner::Vector {
                    size: naga::VectorSize::Quad,
                    scalar: naga::Scalar {
                        kind: naga::ScalarKind::Float,
                        width: 4
                    }
                }
            )
        };
        let kind = match &m.types[g.ty].inner {
            naga::TypeInner::Image { dim, .. } => Kind::Texture {
                reg: digits(&name).unwrap_or(0),
                dim: match dim {
                    naga::ImageDimension::Cube => Dim::Cube,
                    naga::ImageDimension::D3 => Dim::D3,
                    _ => Dim::D2,
                },
            },
            naga::TypeInner::Sampler { .. } => Kind::Sampler,
            naga::TypeInner::Array {
                base,
                size: naga::ArraySize::Constant(n),
                stride,
            } => {
                if is_vec4f(*base) {
                    Kind::Float {
                        vec4s: n.get(),
                        regmap: None,
                    }
                } else {
                    Kind::Zero {
                        bytes: u64::from(n.get() * stride),
                    }
                }
            }
            naga::TypeInner::Struct { members, span } => match &m.types[members[0].ty].inner {
                naga::TypeInner::Array {
                    base,
                    size: naga::ArraySize::Constant(n),
                    ..
                } if is_vec4f(*base) => Kind::Float {
                    vec4s: n.get(),
                    regmap: (path == Path::Mojo).then(|| regmap.clone()),
                },
                _ => Kind::Zero {
                    bytes: u64::from(*span),
                },
            },
            other => return Err(format!("unsupported global {name}: {other:?}")),
        };
        res.push(Res {
            group: b.group,
            binding: b.binding,
            kind,
        });
    }
    res.sort_by_key(|r| (r.group, r.binding));
    let ep = m.entry_points.first().ok_or("no entry point")?;
    let mut vin = vec![];
    let mut locations: Vec<(u32, Option<String>)> = vec![];
    for a in &ep.function.arguments {
        match (&a.binding, &m.types[a.ty].inner) {
            (Some(naga::Binding::Location { location, .. }), _) => {
                locations.push((*location, a.name.clone()))
            }
            (None, naga::TypeInner::Struct { members, .. }) => {
                for mem in members {
                    if let Some(naga::Binding::Location { location, .. }) = &mem.binding {
                        locations.push((*location, mem.name.clone()));
                    }
                }
            }
            _ => {}
        }
    }
    let mut targets = 0;
    if tr.stage == Stage::Vertex {
        for (loc, name) in locations {
            let s = if path == Path::Ours {
                tr.reflection.inputs.iter().find(|s| s.location == loc)
            } else {
                // Reference modules name their inputs after the D3D input register.
                let reg = name.as_deref().and_then(digits).ok_or("unnamed VS input")?;
                tr.reflection.inputs.iter().find(|s| s.register == reg)
            };
            let s = s.ok_or_else(|| format!("VS input at location {loc} has no declaration"))?;
            vin.push((loc, s.usage, s.index));
        }
    } else if let Some(r) = &ep.function.result {
        match (&r.binding, &m.types[r.ty].inner) {
            (Some(naga::Binding::Location { location, .. }), _) => targets = location + 1,
            (None, naga::TypeInner::Struct { members, .. }) => {
                for mem in members {
                    if let Some(naga::Binding::Location { location, .. }) = &mem.binding {
                        targets = targets.max(location + 1);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(Prog {
        wgsl,
        res,
        vin,
        targets,
    })
}

/// vkd3d-shader narrows interpolants to the components the shader actually uses (a `float` at `Component 3` for a
/// colour whose only used channel is alpha) while the other stage uses vec4 whole; naga ignores the `Component`
/// decoration and Metal rejects the width mismatch at pipeline creation. Text pass: widen every narrow VS output member
/// / PS input parameter to vec4, placing it at its `comp` (location -> first component) and swizzling back at the use.
fn widen_interfaces(wgsl: &str, stage: Stage, comp: &HashMap<u32, u32>) -> Result<String, String> {
    let width = |t: &str| match t {
        "f32" => Some(1usize),
        "vec2<f32>" => Some(2),
        "vec3<f32>" => Some(3),
        _ => None,
    };
    let location = |l: &str| -> Option<u32> {
        let r = l.split("@location(").nth(1)?;
        r[..r.find(')')?].parse().ok()
    };
    let at = |l: &str| *location(l).and_then(|n| comp.get(&n)).unwrap_or(&0) as usize;
    if stage == Stage::Vertex {
        let mut widen: HashMap<usize, (usize, usize)> = HashMap::new(); // member -> (first component, width)
        let (mut member, mut in_struct) = (0usize, false);
        let mut lines: Vec<String> = vec![];
        for l in wgsl.lines() {
            in_struct |= l.starts_with("struct VertexOutput");
            if in_struct && l.starts_with('}') {
                in_struct = false;
            }
            if in_struct && l.trim_start().starts_with('@') {
                member += 1;
                if l.contains("@location(")
                    && let Some((head, ty)) = l.rsplit_once(": ")
                    && let Some(n) = width(ty.trim_end_matches(','))
                {
                    widen.insert(member - 1, (at(l), n));
                    lines.push(format!("{head}: vec4<f32>,"));
                    continue;
                }
            }
            lines.push(l.to_string());
        }
        let mut text = lines.join("\n");
        let ret = text
            .find("return VertexOutput(")
            .ok_or("no VertexOutput return")?;
        let args_start = ret + "return VertexOutput(".len();
        let args_end = text[args_start..].find(')').unwrap() + args_start;
        let args: Vec<String> = text[args_start..args_end]
            .split(", ")
            .map(str::to_owned)
            .collect();
        for (i, a) in args.iter().enumerate() {
            if let Some(&(c, n)) = widen.get(&i) {
                let pat = format!("let {a} = ");
                let p = text.find(&pat).ok_or("arg def not found")?;
                let e = p + text[p..].find(';').unwrap();
                let expr = text[p + pat.len()..e].to_string();
                let mut parts = vec!["0.0".to_owned(); c];
                parts.push(expr);
                parts.extend(vec!["0.0".to_owned(); 4 - c - n]);
                text.replace_range(p..e, &format!("{pat}vec4<f32>({})", parts.join(", ")));
            }
        }
        Ok(text + "\n")
    } else {
        let p = wgsl.find("fn main(").ok_or("no main")?;
        let e = p + wgsl[p..].find(") -> ").unwrap();
        let mut new_params = vec![];
        let mut fixes: Vec<(String, String)> = vec![];
        for prm in wgsl[p + 8..e].split(", ") {
            if let Some((a, ty)) = prm.rsplit_once(": ")
                && a.contains("@location(")
                && let Some(n) = width(ty)
            {
                let c = at(a);
                let sw: String = "xyzw"[c..c + n].to_owned();
                fixes.push((a.rsplit_once(' ').unwrap().1.to_string(), format!(".{sw}")));
                new_params.push(format!("{a}: vec4<f32>"));
                continue;
            }
            new_params.push(prm.to_string());
        }
        let mut text = format!("{}{}{}", &wgsl[..p + 8], new_params.join(", "), &wgsl[e..]);
        for (nm, sw) in fixes {
            let pat = format!("= {nm};");
            if !text.contains(&pat) {
                return Err(format!("cannot narrow use of {nm}"));
            }
            text = text.replacen(&pat, &format!("= {nm}{sw};"), 1);
        }
        Ok(text)
    }
}
