// SPDX-License-Identifier: GPL-3.0-or-later
//! Post-pass for MojoShader/vkd3d SPIR-V: split combined image+sampler uniform-constant variables
//! (`OpTypeSampledImage` variables, valid Vulkan SPIR-V but rejected by naga) into a separate
//! texture variable (keeps the original id/decorations) and a sampler variable
//! (binding = texture binding + SAMPLER_BINDING_OFFSET), and rebuild the sampled image with OpSampledImage.
use std::collections::HashMap;

pub const SAMPLER_BINDING_OFFSET: u32 = 16;

struct Inst {
    op: u32,
    w: Vec<u32>,
} // w includes the header word

fn parse(words: &[u32]) -> Result<(Vec<u32>, Vec<Inst>), String> {
    if words.len() < 5 || words[0] != 0x0723_0203 {
        return Err("not SPIR-V".into());
    }
    let hdr = words[..5].to_vec();
    let mut i = 5;
    let mut out = vec![];
    while i < words.len() {
        let n = (words[i] >> 16) as usize;
        let op = words[i] & 0xFFFF;
        if n == 0 || i + n > words.len() {
            return Err(format!("bad instruction word count at word {i}"));
        }
        out.push(Inst {
            op,
            w: words[i..i + n].to_vec(),
        });
        i += n;
    }
    Ok((hdr, out))
}

fn mk(op: u32, args: &[u32]) -> Inst {
    let mut w = vec![((args.len() as u32 + 1) << 16) | op];
    w.extend_from_slice(args);
    Inst { op, w }
}

fn str_words(s: &str) -> Vec<u32> {
    let mut b = s.as_bytes().to_vec();
    b.push(0);
    while !b.len().is_multiple_of(4) {
        b.push(0);
    }
    b.chunks(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Returns the rewritten module and the number of combined samplers split.
pub fn split_samplers(words: &[u32]) -> Result<(Vec<u32>, usize), String> {
    const OP_NAME: u32 = 5;
    const OP_TYPE_IMAGE: u32 = 25;
    const OP_TYPE_SAMPLER: u32 = 26;
    const OP_TYPE_SAMPLED_IMAGE: u32 = 27;
    const OP_TYPE_POINTER: u32 = 32;
    const OP_VARIABLE: u32 = 59;
    const OP_LOAD: u32 = 61;
    const OP_DECORATE: u32 = 71;
    const OP_SAMPLED_IMAGE: u32 = 86;
    const UNIFORM_CONSTANT: u32 = 0;
    const DEC_BINDING: u32 = 33;
    const DEC_DESC_SET: u32 = 34;
    let (mut hdr, insts) = parse(words)?;
    let mut bound = hdr[3];
    let mut si: HashMap<u32, u32> = HashMap::new(); // sampled-image type -> image type
    let mut sampler_ty: Option<u32> = None;
    let mut ptr_ty: HashMap<(u32, u32), u32> = HashMap::new(); // (storage, pointee) -> id
    let mut ptr_of: HashMap<u32, (u32, u32)> = HashMap::new();
    let mut names: HashMap<u32, String> = HashMap::new();
    let mut deco: HashMap<u32, (Option<u32>, Option<u32>)> = HashMap::new(); // id -> (set, binding)
    for ins in &insts {
        match ins.op {
            OP_TYPE_SAMPLED_IMAGE => {
                si.insert(ins.w[1], ins.w[2]);
            }
            OP_TYPE_SAMPLER => {
                sampler_ty = Some(ins.w[1]);
            }
            OP_TYPE_POINTER => {
                ptr_ty.insert((ins.w[2], ins.w[3]), ins.w[1]);
                ptr_of.insert(ins.w[1], (ins.w[2], ins.w[3]));
            }
            OP_NAME => {
                let mut b = vec![];
                for &x in &ins.w[2..] {
                    b.extend_from_slice(&x.to_le_bytes());
                }
                let e = b.iter().position(|&c| c == 0).unwrap_or(b.len());
                names.insert(ins.w[1], String::from_utf8_lossy(&b[..e]).into());
            }
            OP_DECORATE => {
                let e = deco.entry(ins.w[1]).or_default();
                if ins.w[2] == DEC_DESC_SET {
                    e.0 = Some(ins.w[3]);
                }
                if ins.w[2] == DEC_BINDING {
                    e.1 = Some(ins.w[3]);
                }
            }
            _ => {}
        }
    }
    let _ = OP_TYPE_IMAGE;
    // combined variables
    let mut combined: HashMap<u32, (u32, u32, u32)> = HashMap::new(); // var -> (S type, image type, sampler var)
    for ins in &insts {
        if ins.op == OP_VARIABLE
            && let Some(&(sc, pointee)) = ptr_of.get(&ins.w[1])
            && sc == UNIFORM_CONSTANT
            && let Some(&img) = si.get(&pointee)
        {
            combined.insert(ins.w[2], (pointee, img, 0));
        }
    }
    if combined.is_empty() {
        return Ok((words.to_vec(), 0));
    }
    let mut out: Vec<Inst> = vec![];
    let mut new_decor: Vec<Inst> = vec![];
    let mut new_names: Vec<Inst> = vec![];
    let mut emitted_types = false;
    let mut first_type_at: Option<usize> = None;
    let mut sampler_ptr = 0u32;
    let mut smp_ty = sampler_ty.unwrap_or(0);
    let mut img_ptr: HashMap<u32, u32> = HashMap::new();
    let mut order: Vec<u32> = combined.keys().copied().collect();
    order.sort();
    for v in &order {
        bound += 1;
        combined.get_mut(v).unwrap().2 = bound;
    }
    for ins in insts {
        if first_type_at.is_none() && (19..=39).contains(&ins.op) {
            first_type_at = Some(out.len());
        }
        match ins.op {
            OP_VARIABLE if combined.contains_key(&ins.w[2]) => {
                let (s_ty, img, smp_var) = combined[&ins.w[2]];
                if !emitted_types {
                    emitted_types = true;
                    if smp_ty == 0 {
                        bound += 1;
                        smp_ty = bound;
                        out.push(mk(OP_TYPE_SAMPLER, &[smp_ty]));
                    }
                    bound += 1;
                    sampler_ptr = bound;
                    out.push(mk(
                        OP_TYPE_POINTER,
                        &[sampler_ptr, UNIFORM_CONSTANT, smp_ty],
                    ));
                }
                let ip = *img_ptr.entry(img).or_insert_with(|| {
                    if let Some(&p) = ptr_ty.get(&(UNIFORM_CONSTANT, img)) {
                        return p;
                    }
                    bound += 1;
                    let p = bound;
                    out.push(mk(OP_TYPE_POINTER, &[p, UNIFORM_CONSTANT, img]));
                    p
                });
                let _ = s_ty;
                out.push(mk(OP_VARIABLE, &[ip, ins.w[2], UNIFORM_CONSTANT]));
                out.push(mk(OP_VARIABLE, &[sampler_ptr, smp_var, UNIFORM_CONSTANT]));
                // decorations for the sampler variable
                let (set, bind) = deco.get(&ins.w[2]).copied().unwrap_or((None, None));
                if let Some(s) = set {
                    new_decor.push(mk(OP_DECORATE, &[smp_var, DEC_DESC_SET, s]));
                }
                if let Some(b) = bind {
                    new_decor.push(mk(
                        OP_DECORATE,
                        &[smp_var, DEC_BINDING, b + SAMPLER_BINDING_OFFSET],
                    ));
                }
                let nm = names
                    .get(&ins.w[2])
                    .cloned()
                    .unwrap_or_else(|| format!("s{}", ins.w[2]));
                let mut a = vec![smp_var];
                a.extend(str_words(&format!("{nm}_smp")));
                new_names.push(mk(OP_NAME, &a));
            }
            OP_LOAD if combined.contains_key(&ins.w[3]) => {
                let (s_ty, img, smp_var) = combined[&ins.w[3]];
                bound += 2;
                let (t1, t2) = (bound - 1, bound);
                out.push(mk(OP_LOAD, &[img, t1, ins.w[3]]));
                out.push(mk(OP_LOAD, &[smp_ty, t2, smp_var]));
                out.push(mk(OP_SAMPLED_IMAGE, &[s_ty, ins.w[2], t1, t2]));
            }
            _ => out.push(ins),
        }
    }
    // insert new decorations/names before the first type declaration
    let at = first_type_at.unwrap_or(0);
    let tail = out.split_off(at);
    out.extend(new_decor);
    out.extend(tail);
    let nat = out
        .iter()
        .position(|i| i.op == OP_DECORATE || (19..=39).contains(&i.op))
        .unwrap_or(0);
    let tail = out.split_off(nat);
    out.extend(new_names);
    out.extend(tail);
    hdr[3] = bound;
    let mut w = hdr;
    for i in out {
        w.extend(i.w);
    }
    Ok((w, combined.len()))
}

/// vkd3d-shader declares a `BuiltIn PointSize` vertex output that naga's WGSL writer cannot express.
/// Re-decorate it as an ordinary `Location 15` varying (harmless: nothing in the pixel stage reads it).
pub fn pointsize_to_location(words: &[u32]) -> Result<(Vec<u32>, usize), String> {
    let mut w = words.to_vec();
    let mut i = 5;
    let mut n = 0;
    while i < w.len() {
        let c = (w[i] >> 16) as usize;
        let op = w[i] & 0xFFFF;
        if c == 0 {
            return Err("bad word count".into());
        }
        if op == 71 && c == 4 && w[i + 2] == 11 && w[i + 3] == 1 {
            w[i + 2] = 30;
            w[i + 3] = 15;
            n += 1;
        }
        i += c;
    }
    Ok((w, n))
}

/// Rewrite `Location` decorations of `Input` (1) or `Output` (3) variables. vkd3d-shader numbers
/// interpolants by D3D register index while D3D9 links stages by semantic, so the viewer remaps both
/// stages to a semantic-derived location.
pub fn remap_locations(
    words: &[u32],
    storage: u32,
    f: &dyn Fn(u32) -> u32,
) -> Result<Vec<u32>, String> {
    let mut w = words.to_vec();
    let mut vars = std::collections::HashSet::new();
    let mut i = 5;
    while i < w.len() {
        let c = (w[i] >> 16) as usize;
        if c == 0 {
            return Err("bad word count".into());
        }
        if w[i] & 0xFFFF == 59 && w[i + 3] == storage {
            vars.insert(w[i + 2]);
        }
        i += c;
    }
    let mut i = 5;
    while i < w.len() {
        let c = (w[i] >> 16) as usize;
        if w[i] & 0xFFFF == 71 && c >= 4 && w[i + 2] == 30 && vars.contains(&w[i + 1]) {
            w[i + 3] = f(w[i + 3]);
        }
        i += c;
    }
    Ok(w)
}

/// Add `delta` to every DescriptorSet decoration.
pub fn shift_sets(words: &[u32], delta: u32) -> Result<Vec<u32>, String> {
    let mut w = words.to_vec();
    let mut i = 5;
    while i < w.len() {
        let c = (w[i] >> 16) as usize;
        if c == 0 {
            return Err("bad word count".into());
        }
        if w[i] & 0xFFFF == 71 && c >= 4 && w[i + 2] == 34 {
            w[i + 3] += delta;
        }
        i += c;
    }
    Ok(w)
}

/// `Location` -> first `Component` of the `Input` (1) / `Output` (3) variables that carry a `Component` decoration.
pub fn component_offsets(words: &[u32], storage: u32) -> Result<HashMap<u32, u32>, String> {
    let mut vars = std::collections::HashSet::new();
    let (mut loc, mut comp) = (HashMap::new(), HashMap::new());
    let mut i = 5;
    while i < words.len() {
        let c = (words[i] >> 16) as usize;
        if c == 0 {
            return Err("bad word count".into());
        }
        match words[i] & 0xFFFF {
            59 if words[i + 3] == storage => {
                vars.insert(words[i + 2]);
            }
            71 if c >= 4 && words[i + 2] == 30 => {
                loc.insert(words[i + 1], words[i + 3]);
            }
            71 if c >= 4 && words[i + 2] == 31 => {
                comp.insert(words[i + 1], words[i + 3]);
            }
            _ => {}
        }
        i += c;
    }
    Ok(vars
        .iter()
        .filter_map(|v| Some((*loc.get(v)?, *comp.get(v)?)))
        .collect())
}
