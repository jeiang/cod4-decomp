//! THROWAWAY viewer for ticket #18: draws the real mp_crash world (decoded from the original zone) with SM3 shaders
//! translated to WGSL by one of three paths, real IWI textures from the IWDs, and prints/titles the techset + path +
//! constant bindings. See README.md for keys and flags.
use glam::{Mat4, Vec3};
use serde_json::Value;
use sm3_wgsl_proto::{codeconst_names::*, engine::*, iwi, paths::*, sm3, world::*};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use wgpu::util::DeviceExt;

const TECH_PREF: [usize; 4] = [8, 7, 4, 5];
const TECH_NAMES: [&str; 34] = ["depth_prepass", "build_floatz", "build_shadowmap_depth", "build_shadowmap_color", "unlit", "emissive", "emissive_shadow", "lit", "lit_sun", "lit_sun_shadow", "lit_spot", "lit_spot_shadow", "lit_omni", "lit_omni_shadow", "lit_instanced", "lit_instanced_sun", "lit_instanced_sun_shadow", "lit_instanced_spot", "lit_instanced_spot_shadow", "lit_instanced_omni", "lit_instanced_omni_shadow", "light_spot", "light_omni", "light_spot_shadow", "fakelight_normal", "fakelight_view", "sunlight_preview", "case_texture", "wireframe_solid", "wireframe_shaded", "shadowcookie_caster", "shadowcookie_receiver", "debug_bumpmap", "debug_bumpmap_instanced"];

#[derive(Clone)]
enum Src { Lit([f32; 4]), Code(u32, u32), MatConst(u32) }
struct Write { reg: u32, src: Src }

struct Bank { buf: wgpu::Buffer, n: u32, regmap: Option<HashMap<u32, u32>>, writes: Vec<Write>, group: u32 }

struct Prepared {
    pipeline: Option<wgpu::RenderPipeline>,
    groups: Vec<(u32, wgpu::BindGroup)>,
    banks: Vec<Bank>,
    info: Vec<String>,
    error: Option<String>,
}

struct Gfx { device: wgpu::Device, queue: wgpu::Queue, color_format: wgpu::TextureFormat, bc: bool }

struct App {
    gfx: Gfx,
    world: World,
    work: PathBuf,
    iwd: iwi::Iwd,
    tex_cache: HashMap<String, Option<wgpu::TextureView>>,
    place: HashMap<&'static str, wgpu::TextureView>,
    vbuf: wgpu::Buffer, ibuf: wgpu::Buffer,
    prepared: HashMap<(usize, PathId), Prepared>,
    prog_cache: HashMap<(PathId, String, String, String), Result<Prog, String>>,
    draw_order: Vec<usize>,     // surface indices sorted
    focus_list: Vec<usize>,     // material indices, most surfaces first
    focus: usize,
    path: PathId,
    only_focus: bool,
    cam: Vec3, yaw: f32, pitch: f32,
    depth: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
    time: f32,
}

fn blend_factor(s: &str) -> wgpu::BlendFactor {
    use wgpu::BlendFactor as B;
    match s { "zero" => B::Zero, "one" => B::One, "srcColor" => B::Src, "invSrcColor" => B::OneMinusSrc, "srcAlpha" => B::SrcAlpha, "invSrcAlpha" => B::OneMinusSrcAlpha, "dstAlpha" => B::DstAlpha, "invDstAlpha" => B::OneMinusDstAlpha, "dstColor" => B::Dst, "invDstColor" => B::OneMinusDst, _ => B::One }
}

impl App {
    fn new(gfx: Gfx, work: PathBuf, cod4: PathBuf) -> App {
        let world = World::load(&work);
        let vbuf = gfx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("verts"), contents: &world.verts, usage: wgpu::BufferUsages::VERTEX });
        let mut idx = world.indices.clone(); while idx.len() % 4 != 0 { idx.push(0); }
        let ibuf = gfx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("idx"), contents: &idx, usage: wgpu::BufferUsages::INDEX });
        let mut order: Vec<usize> = (0..world.surfs.len()).collect();
        order.sort_by_key(|&i| (world.mats[world.surfs[i].mat].sort_key, world.surfs[i].mat));
        let mut focus_list: Vec<usize> = (0..world.mats.len()).filter(|&m| world.surf_count_by_mat[m] > 0).collect();
        focus_list.sort_by_key(|&m| std::cmp::Reverse(world.surf_count_by_mat[m]));
        let mut app = App { gfx, world, work, iwd: iwi::Iwd::open(&cod4.join("main")), tex_cache: HashMap::new(), place: HashMap::new(), vbuf, ibuf, prepared: HashMap::new(), prog_cache: HashMap::new(),
            draw_order: order, focus_list, focus: 0, path: PathId::Custom, only_focus: false, cam: Vec3::ZERO, yaw: 0.0, pitch: 0.0, depth: None, time: 0.0 };
        app.make_placeholders();
        let (mn, mx) = (app.world.mins, app.world.maxs);
        app.cam = Vec3::new((mn[0] + mx[0]) * 0.5 - 2200.0, (mn[1] + mx[1]) * 0.5, 900.0);
        app.pitch = -0.25;
        app
    }

    fn make_tex(&self, dim: wgpu::TextureDimension, size: [u32; 3], layers: u32, rgba: [u8; 4], vd: wgpu::TextureViewDimension) -> wgpu::TextureView {
        let n = (size[0] * size[1] * size[2] * layers) as usize;
        let data: Vec<u8> = std::iter::repeat(rgba).take(n).flatten().collect();
        let t = self.gfx.device.create_texture_with_data(&self.gfx.queue, &wgpu::TextureDescriptor { label: None, size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: if dim == wgpu::TextureDimension::D3 { size[2] } else { layers } }, mip_level_count: 1, sample_count: 1, dimension: dim, format: wgpu::TextureFormat::Rgba8Unorm, usage: wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[] }, wgpu::util::TextureDataOrder::LayerMajor, &data);
        t.create_view(&wgpu::TextureViewDescriptor { dimension: Some(vd), ..Default::default() })
    }
    fn make_placeholders(&mut self) {
        use wgpu::{TextureDimension as D, TextureViewDimension as V};
        let p = [
            ("white2d", self.make_tex(D::D2, [1, 1, 1], 1, [255; 4], V::D2)), ("black2d", self.make_tex(D::D2, [1, 1, 1], 1, [0, 0, 0, 255], V::D2)),
            ("normal2d", self.make_tex(D::D2, [1, 1, 1], 1, [128, 128, 255, 255], V::D2)), ("lm2d", self.make_tex(D::D2, [1, 1, 1], 1, [120, 120, 120, 255], V::D2)),
            ("white3d", self.make_tex(D::D3, [1, 1, 1], 1, [255; 4], V::D3)), ("black3d", self.make_tex(D::D3, [1, 1, 1], 1, [0, 0, 0, 255], V::D3)),
            ("cube", self.make_tex(D::D2, [1, 1, 1], 6, [60, 70, 80, 255], V::Cube)),
        ];
        for (k, v) in p { self.place.insert(k, v); }
    }

    fn load_image(&mut self, name: &str) -> Option<wgpu::TextureView> {
        if let Some(t) = self.tex_cache.get(name) { return t.clone(); }
        let r = (|| {
            let im = self.iwd.load(name)?;
            if im.cube || im.vol { return None; }
            let mips = iwi::mips_2d(&im)?;
            let (fmt, conv): (wgpu::TextureFormat, bool) = match im.format {
                11 if self.gfx.bc && im.w % 4 == 0 && im.h % 4 == 0 => (wgpu::TextureFormat::Bc1RgbaUnorm, false),
                12 if self.gfx.bc && im.w % 4 == 0 && im.h % 4 == 0 => (wgpu::TextureFormat::Bc2RgbaUnorm, false),
                13 if self.gfx.bc && im.w % 4 == 0 && im.h % 4 == 0 => (wgpu::TextureFormat::Bc3RgbaUnorm, false),
                1 | 2 | 3 | 4 | 5 => (wgpu::TextureFormat::Rgba8Unorm, true), _ => return None };
            let mut data = vec![];
            for (w, h, b) in &mips {
                if !conv { data.extend_from_slice(b); continue; }
                for i in 0..(w * h) as usize { let px: [u8; 4] = match im.format {
                    1 => [b[i * 4 + 2], b[i * 4 + 1], b[i * 4], b[i * 4 + 3]], 2 => [b[i * 3 + 2], b[i * 3 + 1], b[i * 3], 255],
                    3 => [b[i * 2], b[i * 2], b[i * 2], b[i * 2 + 1]], 4 => [b[i], b[i], b[i], 255], _ => [0, 0, 0, b[i]] }; data.extend_from_slice(&px); }
            }
            let t = self.gfx.device.create_texture_with_data(&self.gfx.queue, &wgpu::TextureDescriptor { label: Some(name), size: wgpu::Extent3d { width: im.w, height: im.h, depth_or_array_layers: 1 }, mip_level_count: mips.len() as u32, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: fmt, usage: wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[] }, wgpu::util::TextureDataOrder::LayerMajor, &data);
            Some(t.create_view(&Default::default()))
        })();
        self.tex_cache.insert(name.to_string(), r.clone());
        r
    }

    fn prog(&mut self, path: PathId, kind: &str, hash: &str, vs: &str) -> Result<Prog, String> {
        let key = (path, kind.to_string(), hash.to_string(), if path == PathId::Mojo && kind == "ps" { vs.to_string() } else { String::new() });
        if let Some(p) = self.prog_cache.get(&key) { return p.clone(); }
        let r = build(path, kind, hash, vs, &self.work);
        self.prog_cache.insert(key, r.clone());
        r
    }

    fn technique(&self, mat: usize) -> Option<(usize, &Value)> {
        let ts = self.world.techsets.get(&self.world.mats[mat].techset)?;
        for &s in &TECH_PREF { let t = &ts["techniques"][s]; if t.get("passes").is_some() { return Some((s, t)); } }
        None
    }

    fn prepare(&mut self, mat: usize, path: PathId) {
        if self.prepared.contains_key(&(mat, path)) { return; }
        let p = self.prepare_inner(mat, path).unwrap_or_else(|e| Prepared { pipeline: None, groups: vec![], banks: vec![], info: vec![], error: Some(e) });
        self.prepared.insert((mat, path), p);
    }

    fn prepare_inner(&mut self, mi: usize, path: PathId) -> Result<Prepared, String> {
        let (slot, tech) = self.technique(mi).ok_or_else(|| format!("no usable technique in techset {}", self.world.mats[mi].techset))?;
        let tech = tech.clone();
        let pass = &tech["passes"][0];
        let vsh = pass["vs"]["hash"].as_str().ok_or("unresolved vs ref")?.to_string();
        let psh = pass["ps"]["hash"].as_str().ok_or("unresolved ps ref")?.to_string();
        let vs = self.prog(path, "vs", &vsh, &vsh)?;
        let ps = self.prog(path, "ps", &psh, &vsh)?;
        if std::env::var("SM3_DUMP_IO").is_ok() { for (n, p) in [("VS", &vs), ("PS", &ps)] { println!("--- {n} {:?} dcl_in {:?} dcl_out {:?}", path, p.dcl_in, p.dcl_out); for l in p.wgsl.lines().filter(|l| l.contains("@location") || l.contains("@builtin")) { println!("   {}", l.trim()); } } }
        let mut info = vec![format!("techset {} / technique[{}] {} ({}) pass 0", self.world.mats[mi].techset, slot, TECH_NAMES[slot], tech["name"].as_str().unwrap_or("?")),
            format!("path {}  vs={} ps={}", path.label(), self.world.shaders[&format!("vs_{vsh}")]["names"][0].as_str().unwrap_or("?"), self.world.shaders[&format!("ps_{psh}")]["names"][0].as_str().unwrap_or("?"))];
        // ---- argument resolution
        let mat = self.world.mats[mi].clone(); let mat = &mat;
        let ctab_name = |p: &Prog, set: u16, reg: u32| p.ctab.iter().find(|c| c.regset == set && (c.reg as u32) <= reg && reg < (c.reg + c.count.max(1)) as u32).map(|c| c.name.clone()).unwrap_or_default();
        let mut vs_w = vec![]; let mut ps_w = vec![];
        let mut samp_src: HashMap<u32, String> = HashMap::new();          // sampler reg -> description
        let mut samp_img: HashMap<u32, Option<String>> = HashMap::new();  // sampler reg -> material image
        let mut samp_code: HashMap<u32, u32> = HashMap::new();
        for a in pass["args"].as_array().unwrap() {
            let ty = a["type"].as_u64().unwrap(); let dest = a["dest"].as_u64().unwrap() as u32;
            match ty {
                0 | 6 => {
                    let h = a["hash"].as_u64().unwrap() as u32;
                    let (w, p) = if ty == 0 { (&mut vs_w, &vs) } else { (&mut ps_w, &ps) };
                    let nm = mat.constants.iter().find(|c| c.0 == h).map(|c| format!("{}={:?}", c.1, c.2)).unwrap_or(format!("(hash {h}, not in material -> 0)"));
                    info.push(format!("{} c{dest} [{}] <- material const {nm}", if ty == 0 { "VS" } else { "PS" }, ctab_name(p, 2, dest)));
                    w.push(Write { reg: dest, src: Src::MatConst(h) });
                }
                1 | 7 => { let l = a["lit"].as_array().map(|v| [v[0].as_f64().unwrap() as f32, v[1].as_f64().unwrap() as f32, v[2].as_f64().unwrap() as f32, v[3].as_f64().unwrap() as f32]).unwrap_or([0.0; 4]);
                    (if ty == 1 { &mut vs_w } else { &mut ps_w }).push(Write { reg: dest, src: Src::Lit(l) }); }
                3 | 5 => {
                    let (idx, fr, rc) = (a["index"].as_u64().unwrap() as u32, a["firstRow"].as_u64().unwrap() as u32, a["rowCount"].as_u64().unwrap().max(1) as u32);
                    let nm = CODE_CONST_NAMES.get(idx as usize).copied().unwrap_or("?");
                    let (w, p, st) = if ty == 3 { (&mut vs_w, &vs, "VS") } else { (&mut ps_w, &ps, "PS") };
                    info.push(format!("{st} c{dest}{} [{}] <- code const {nm} (0x{idx:x}) rows {fr}..{}", if rc > 1 { format!("-c{}", dest + rc - 1) } else { String::new() }, ctab_name(p, 2, dest), fr + rc - 1));
                    for r in 0..rc { w.push(Write { reg: dest + r, src: Src::Code(idx, fr + r) }); }
                }
                2 => { let h = a["hash"].as_u64().unwrap() as u32; let t = mat.textures.iter().find(|t| t.hash == h);
                    samp_src.insert(dest, format!("s{dest} [{}] <- material {} -> {}", ctab_name(&ps, 3, dest), t.map(|t| t.semantic.as_str()).unwrap_or("(missing in material)"), t.map(|t| t.image.as_str()).unwrap_or("white")));
                    samp_img.insert(dest, t.map(|t| t.image.clone())); }
                4 => { let i = a["sampler"].as_u64().unwrap() as u32; samp_code.insert(dest, i);
                    samp_src.insert(dest, format!("s{dest} [{}] <- code sampler {} (placeholder texture)", ctab_name(&ps, 3, dest), CODE_TEXTURE_NAMES.get(i as usize).copied().unwrap_or("?"))); }
                _ => {}
            }
        }
        let mut sl: Vec<_> = samp_src.iter().collect(); sl.sort(); for (_, s) in sl { info.push(format!("PS {s}")); }
        // ---- bind group layouts
        let dev = self.gfx.device.clone();
        let max_group = vs.res.iter().chain(ps.res.iter()).map(|r| r.group).max().unwrap_or(0);
        let mut layouts = vec![]; let mut bgs = vec![]; let mut banks = vec![];
        let mut tex_views: Vec<(u32, u32, wgpu::TextureView)> = vec![]; // (group, binding, view) kept alive
        let mut sampler_objs: Vec<(u32, u32, wgpu::Sampler)> = vec![];
        for g in 0..=max_group {
            let mut ents = vec![]; let mut res_entries: Vec<(u32, u32, &Resource, bool)> = vec![];
            for (prog, is_vs) in [(&vs, true), (&ps, false)] { for r in prog.res.iter().filter(|r| r.group == g) { res_entries.push((r.binding, if is_vs { 1 } else { 2 }, r, is_vs)); } }
            for (binding, vis, r, _) in &res_entries {
                let visibility = if *vis == 1 { wgpu::ShaderStages::VERTEX } else { wgpu::ShaderStages::FRAGMENT };
                let ty = match &r.kind {
                    ResKind::ConstBank { .. } | ResKind::Other(_) => wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    ResKind::Texture { dim, .. } => wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: *dim, multisampled: false },
                    ResKind::Sampler { .. } => wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                };
                ents.push(wgpu::BindGroupLayoutEntry { binding: *binding, visibility, ty, count: None });
            }
            let layout = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries: &ents });
            let mut entries_res: Vec<(u32, BindRes)> = vec![];
            for (binding, _, r, is_vs) in &res_entries {
                match &r.kind {
                    ResKind::ConstBank { n, regmap } => {
                        let buf = dev.create_buffer(&wgpu::BufferDescriptor { label: Some(&r.name), size: (*n as u64 * 16).max(16), usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
                        banks.push(Bank { buf: buf.clone(), n: *n, regmap: regmap.clone(), writes: if *is_vs { vs_w.iter().map(|w| Write { reg: w.reg, src: w.src.clone() }).collect() } else { ps_w.iter().map(|w| Write { reg: w.reg, src: w.src.clone() }).collect() }, group: g });
                        entries_res.push((*binding, BindRes::Buf(buf)));
                    }
                    ResKind::Other(_) => { let buf = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: &[0u8; 32], usage: wgpu::BufferUsages::UNIFORM }); entries_res.push((*binding, BindRes::Buf(buf))); }
                    ResKind::Texture { reg, dim } => {
                        let v = self.resolve_texture(*reg, *dim, &samp_img, &samp_code, &ps);
                        entries_res.push((*binding, BindRes::Tex(tex_views.len()))); tex_views.push((g, *binding, v));
                    }
                    ResKind::Sampler { reg } => {
                        let t = mat.textures.iter().find(|t| samp_img.get(reg).and_then(|i| i.as_ref()).map_or(false, |i| *i == t.image));
                        let (cu, cv) = t.map(|t| (t.clamp_u, t.clamp_v)).unwrap_or((false, false));
                        let am = |c: bool| if c { wgpu::AddressMode::ClampToEdge } else { wgpu::AddressMode::Repeat };
                        let s = dev.create_sampler(&wgpu::SamplerDescriptor { address_mode_u: am(cu), address_mode_v: am(cv), address_mode_w: wgpu::AddressMode::Repeat, mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, mipmap_filter: wgpu::MipmapFilterMode::Linear, anisotropy_clamp: 8, ..Default::default() });
                        entries_res.push((*binding, BindRes::Smp(sampler_objs.len()))); sampler_objs.push((g, *binding, s));
                    }
                }
            }
            let entries: Vec<wgpu::BindGroupEntry> = entries_res.iter().map(|(b, r)| wgpu::BindGroupEntry { binding: *b, resource: match r { BindRes::Buf(buf) => buf.as_entire_binding(), BindRes::Tex(i) => wgpu::BindingResource::TextureView(&tex_views[*i].2), BindRes::Smp(i) => wgpu::BindingResource::Sampler(&sampler_objs[*i].2) } }).collect();
            let bg = dev.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &layout, entries: &entries });
            layouts.push(layout); bgs.push((g, bg));
        }
        // ---- vertex layout from routing
        let routing: Vec<(u64, u64)> = pass["decl"]["routing"].as_array().map(|a| a.iter().map(|p| (p[0].as_u64().unwrap(), p[1].as_u64().unwrap())).collect()).unwrap_or_default();
        let mut attrs = vec![];
        for (loc, reg) in &vs.vin {
            let (usage, index) = *vs.dcl_in.get(reg).ok_or(format!("VS input v{reg} has no dcl"))?;
            let dst = match usage { 0 => 0, 3 => 1, 10 => 2 + index as u64, 5 => 4 + index as u64, _ => return Err(format!("unsupported VS input semantic {usage}/{index}")) };
            let src = routing.iter().find(|r| r.1 == dst).map(|r| r.0).ok_or(format!("decl routes nothing to dest slot {dst} (usage {usage}/{index})"))?;
            let (off, fmt) = match src { 0 => (0, wgpu::VertexFormat::Float32x3), 1 => (16, wgpu::VertexFormat::Unorm8x4), 2 => (20, wgpu::VertexFormat::Float32x2), 3 => (36, wgpu::VertexFormat::Unorm8x4), 4 => (40, wgpu::VertexFormat::Unorm8x4), 5 => (28, wgpu::VertexFormat::Float32x2), s => return Err(format!("stream source {s} not in world vertices")) };
            attrs.push(wgpu::VertexAttribute { format: fmt, offset: off, shader_location: *loc });
        }
        let bgl_refs: Vec<Option<&wgpu::BindGroupLayout>> = layouts.iter().map(Some).collect();
        let pl = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &bgl_refs, immediate_size: 0 });
        let g = dev.push_error_scope(wgpu::ErrorFilter::Validation); let g2 = dev.push_error_scope(wgpu::ErrorFilter::Internal);
        let vsm = dev.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("vs"), source: wgpu::ShaderSource::Wgsl(vs.wgsl.as_str().into()) });
        let psm = dev.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("ps"), source: wgpu::ShaderSource::Wgsl(ps.wgsl.as_str().into()) });
        // state bits
        let sb = mat.state_entry.get(slot).copied().filter(|&i| (i as usize) < mat.state_bits.len()).map(|i| &mat.state_bits[i as usize]);
        let (blend, depth_write, depth_cmp) = match sb {
            Some(s) => {
                let sf = s["srcBlendRgb"].as_str().unwrap_or("one"); let df = s["dstBlendRgb"].as_str().unwrap_or("zero");
                let blend = if sf == "one" && df == "zero" { None } else { Some(wgpu::BlendState { color: wgpu::BlendComponent { src_factor: blend_factor(sf), dst_factor: blend_factor(df), operation: wgpu::BlendOperation::Add }, alpha: wgpu::BlendComponent { src_factor: blend_factor(s["srcBlendAlpha"].as_str().unwrap_or("one")), dst_factor: blend_factor(s["dstBlendAlpha"].as_str().unwrap_or("zero")), operation: wgpu::BlendOperation::Add } }) };
                (blend, s["depthWrite"].as_u64().unwrap_or(1) != 0, match s["depthTest"].as_str().unwrap_or("lessEqual") { "equal" => wgpu::CompareFunction::Equal, "less" => wgpu::CompareFunction::Less, _ => wgpu::CompareFunction::LessEqual })
            }
            None => (None, true, wgpu::CompareFunction::LessEqual),
        };
        info.push(format!("state: blend={} depthWrite={} depth={:?} (cull disabled in prototype)", blend.is_some(), depth_write, depth_cmp));
        let pipeline = dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None, layout: Some(&pl),
            vertex: wgpu::VertexState { module: &vsm, entry_point: Some("main"), compilation_options: Default::default(), buffers: &[Some(wgpu::VertexBufferLayout { array_stride: 44, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attrs })] },
            fragment: Some(wgpu::FragmentState { module: &psm, entry_point: Some("main"), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format: self.gfx.color_format, blend, write_mask: wgpu::ColorWrites::ALL })] }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: wgpu::TextureFormat::Depth32Float, depth_write_enabled: Some(depth_write), depth_compare: Some(depth_cmp), stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(), multiview_mask: None, cache: None,
        });
        if let Some(e) = pollster::block_on(g2.pop()) { return Err(format!("wgpu/Metal internal pipeline error: {}", e.to_string().lines().filter(|l| l.contains("localizedDescription") || l.contains("Internal")).take(2).collect::<Vec<_>>().join(" | ").chars().take(300).collect::<String>())); }
        if let Some(e) = pollster::block_on(g.pop()) { return Err(format!("wgpu pipeline validation: {}", e.to_string().lines().take(3).collect::<Vec<_>>().join(" | "))); }
        let _ = (tex_views, sampler_objs); // bind groups hold the resources alive
        Ok(Prepared { pipeline: Some(pipeline), groups: bgs, banks, info, error: None })
    }

    fn resolve_texture(&mut self, reg: u32, dim: wgpu::TextureViewDimension, samp_img: &HashMap<u32, Option<String>>, samp_code: &HashMap<u32, u32>, ps: &Prog) -> wgpu::TextureView {
        use wgpu::TextureViewDimension as V;
        if let Some(Some(img)) = samp_img.get(&reg) { if dim == V::D2 { if let Some(v) = self.load_image(img) { return v; } } }
        let cname = ps.ctab.iter().find(|c| c.regset == 3 && c.reg as u32 == reg).map(|c| c.name.to_lowercase()).unwrap_or_default();
        let code = samp_code.get(&reg).map(|&i| CODE_TEXTURE_NAMES.get(i as usize).copied().unwrap_or("")).unwrap_or("");
        let key = match dim {
            V::Cube => "cube", V::D3 => if cname.contains("modellighting") { "black3d" } else { "white3d" },
            _ => if code == "BLACK" || cname.contains("floatz") { "black2d" } else if code == "IDENTITY_NORMAL_MAP" || cname.contains("normal") { "normal2d" } else if cname.contains("lightmap") || code.starts_with("LIGHTMAP") { "lm2d" } else { "white2d" },
        };
        self.place[key].clone()
    }

    fn frame_state(&self, aspect: f32) -> FrameState {
        // CoD world (x fwd, y left, z up) -> left-handed y-up camera space
        let conv = Mat4::from_cols(glam::Vec4::new(0.0, 0.0, 1.0, 0.0), glam::Vec4::new(-1.0, 0.0, 0.0, 0.0), glam::Vec4::new(0.0, 1.0, 0.0, 0.0), glam::Vec4::W);
        let dir = Vec3::new(self.yaw.cos() * self.pitch.cos(), self.yaw.sin() * self.pitch.cos(), self.pitch.sin());
        let eye_l = conv.transform_point3(self.cam); let dir_l = conv.transform_vector3(dir);
        let view = look_to_lh(eye_l, dir_l, Vec3::Y) * conv;
        let proj = perspective_lh(75f32.to_radians(), aspect, 4.0, 30000.0);
        FrameState { world: Mat4::IDENTITY, view, proj, eye: self.cam, sun_dir: Vec3::from(self.world.sun_dir), sun_color: Vec3::from(self.world.sun_color) * 1.0, time: self.time }
    }

    fn fill_banks(&self, p: &Prepared, mi: usize, st: &FrameState) {
        let mat = self.world.mats[mi].clone(); let mat = &mat;
        for b in &p.banks {
            let mut data = vec![0f32; b.n as usize * 4];
            for w in &b.writes {
                let v = match &w.src { Src::Lit(l) => *l, Src::Code(i, r) => code_const(*i, *r, st), Src::MatConst(h) => mat.constants.iter().find(|c| c.0 == *h).map(|c| c.2).unwrap_or([0.0; 4]) };
                let idx = match &b.regmap { Some(m) => match m.get(&w.reg) { Some(i) => *i, None => continue }, None => w.reg };
                if (idx as usize) < b.n as usize { data[idx as usize * 4..idx as usize * 4 + 4].copy_from_slice(&v); }
            }
            self.gfx.queue.write_buffer(&b.buf, 0, bytemuck::cast_slice(&data));
        }
    }

    fn ensure_depth(&mut self, size: (u32, u32)) {
        if self.depth.as_ref().map_or(true, |d| d.2 != size) {
            let t = self.gfx.device.create_texture(&wgpu::TextureDescriptor { label: Some("depth"), size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Depth32Float, usage: wgpu::TextureUsages::RENDER_ATTACHMENT, view_formats: &[] });
            let v = t.create_view(&Default::default()); self.depth = Some((t, v, size));
        }
    }

    fn render(&mut self, target: &wgpu::TextureView, size: (u32, u32)) -> (usize, usize) {
        self.ensure_depth(size);
        let st = self.frame_state(size.0 as f32 / size.1 as f32);
        let focus_mat = self.focus_list[self.focus];
        let order = self.draw_order.clone();
        let mut mats_needed: Vec<usize> = vec![]; for &si in &order { let m = self.world.surfs[si].mat; if !mats_needed.contains(&m) { mats_needed.push(m); } }
        for &m in &mats_needed { if !self.only_focus || m == focus_mat { self.prepare(m, self.path); } }
        let mut enc = self.gfx.device.create_command_encoder(&Default::default());
        let (mut drawn, mut failed) = (0, 0);
        {
            for &m in &mats_needed { if let Some(p) = self.prepared.get(&(m, self.path)) { if p.pipeline.is_some() { self.fill_banks(p, m, &st); } } }
            let depth_view = &self.depth.as_ref().unwrap().1;
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor { label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: target, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.36, g: 0.45, b: 0.55, a: 1.0 }), store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: depth_view, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }), stencil_ops: None }),
                timestamp_writes: None, occlusion_query_set: None, multiview_mask: None });
            rp.set_vertex_buffer(0, self.vbuf.slice(..)); rp.set_index_buffer(self.ibuf.slice(..), wgpu::IndexFormat::Uint16);
            let mut cur = usize::MAX;
            for &si in &order {
                let s = &self.world.surfs[si];
                if self.only_focus && s.mat != focus_mat { continue; }
                let Some(p) = self.prepared.get(&(s.mat, self.path)) else { continue };
                let Some(pl) = &p.pipeline else { failed += 1; continue };
                if cur != s.mat { rp.set_pipeline(pl); for (g, bg) in &p.groups { rp.set_bind_group(*g, bg, &[]); } cur = s.mat; }
                rp.draw_indexed(s.first_index..s.first_index + s.tri_count * 3, s.first_vertex, 0..1); drawn += 1;
            }
        }
        self.gfx.queue.submit([enc.finish()]);
        (drawn, failed)
    }

    fn title(&mut self) -> String {
        let m = self.focus_list[self.focus];
        self.prepare(m, self.path);
        let p = &self.prepared[&(m, self.path)];
        let mat = &self.world.mats[m];
        let status = p.error.as_deref().map(|e| format!("FAILED: {e}")).unwrap_or_else(|| format!("{} const bindings", p.info.iter().filter(|l| l.contains(" <- ")).count()));
        format!("[{}/{}] mat {} | techset {} | path {} | {}{}", self.focus + 1, self.focus_list.len(), mat.name, mat.techset, self.path.short(), status, if self.only_focus { " | ONLY-FOCUS" } else { "" })
    }
    fn dump(&mut self) {
        let m = self.focus_list[self.focus]; self.prepare(m, self.path);
        let p = &self.prepared[&(m, self.path)];
        println!("\n=== material {} ({} surfaces) ===", self.world.mats[m].name, self.world.surf_count_by_mat[m]);
        if let Some(e) = &p.error { println!("  path {} FAILED for this material: {e}", self.path.label()); }
        for l in &p.info { println!("  {l}"); }
    }
}

enum BindRes { Buf(wgpu::Buffer), Tex(usize), Smp(usize) }

// ------------------------------------------------------------------ windowed app
struct Win { app: App, window: Option<Arc<winit::window::Window>>, surface: Option<wgpu::Surface<'static>>, config: Option<wgpu::SurfaceConfiguration>, keys: std::collections::HashSet<winit::keyboard::KeyCode>, drag: bool, last: std::time::Instant, instance: wgpu::Instance, adapter: wgpu::Adapter }

impl winit::application::ApplicationHandler for Win {
    fn resumed(&mut self, el: &winit::event_loop::ActiveEventLoop) {
        if self.window.is_some() { return; }
        let w = Arc::new(el.create_window(winit::window::Window::default_attributes().with_title("sm3-wgsl").with_inner_size(winit::dpi::LogicalSize::new(1280, 720))).unwrap());
        let surf = self.instance.create_surface(w.clone()).unwrap();
        let caps = surf.get_capabilities(&self.adapter);
        let fmt = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        let sz = w.inner_size();
        let cfg = wgpu::SurfaceConfiguration { usage: wgpu::TextureUsages::RENDER_ATTACHMENT, format: fmt, width: sz.width.max(1), height: sz.height.max(1), present_mode: wgpu::PresentMode::AutoVsync, desired_maximum_frame_latency: 2, alpha_mode: caps.alpha_modes[0], view_formats: vec![], color_space: Default::default() };
        surf.configure(&self.app.gfx.device, &cfg);
        self.app.gfx.color_format = fmt;
        self.surface = Some(surf); self.config = Some(cfg); self.window = Some(w);
        self.app.dump();
        self.window.as_ref().unwrap().request_redraw();
    }
    fn window_event(&mut self, el: &winit::event_loop::ActiveEventLoop, _id: winit::window::WindowId, ev: winit::event::WindowEvent) {
        use winit::{event::*, keyboard::{KeyCode, PhysicalKey}};
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(s) => { if let (Some(c), Some(sf)) = (self.config.as_mut(), self.surface.as_ref()) { c.width = s.width.max(1); c.height = s.height.max(1); sf.configure(&self.app.gfx.device, c); } }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => self.drag = state == ElementState::Pressed,
            WindowEvent::KeyboardInput { event: KeyEvent { physical_key: PhysicalKey::Code(k), state, .. }, .. } => {
                if state == ElementState::Pressed {
                    let n = self.app.focus_list.len();
                    let shift = self.keys.contains(&KeyCode::ShiftLeft);
                    match k {
                        KeyCode::Escape => el.exit(),
                        KeyCode::Tab | KeyCode::KeyT => { self.app.focus = if shift { (self.app.focus + n - 1) % n } else { (self.app.focus + 1) % n }; self.app.dump(); }
                        KeyCode::KeyP => { let i = PathId::ALL.iter().position(|p| *p == self.app.path).unwrap(); self.app.path = PathId::ALL[(i + 1) % 3]; self.app.dump(); }
                        KeyCode::KeyF => { self.app.only_focus = !self.app.only_focus; }
                        _ => {}
                    }
                    self.keys.insert(k);
                } else { self.keys.remove(&k); }
            }
            WindowEvent::RedrawRequested => {
                let now = std::time::Instant::now(); let dt = (now - self.last).as_secs_f32().min(0.1); self.last = now;
                self.app.time += dt;
                let speed = if self.keys.contains(&KeyCode::ShiftLeft) { 2400.0 } else { 600.0 } * dt;
                let a = &mut self.app;
                let fwd = Vec3::new(a.yaw.cos(), a.yaw.sin(), 0.0); let left = Vec3::new(-a.yaw.sin(), a.yaw.cos(), 0.0);
                for k in self.keys.iter() { match k { KeyCode::KeyW => a.cam += fwd * speed, KeyCode::KeyS => a.cam -= fwd * speed, KeyCode::KeyA => a.cam += left * speed, KeyCode::KeyD => a.cam -= left * speed, KeyCode::KeyE => a.cam.z += speed, KeyCode::KeyQ => a.cam.z -= speed,
                    KeyCode::ArrowLeft => a.yaw += dt * 1.5, KeyCode::ArrowRight => a.yaw -= dt * 1.5, KeyCode::ArrowUp => a.pitch = (a.pitch + dt).min(1.5), KeyCode::ArrowDown => a.pitch = (a.pitch - dt).max(-1.5), _ => {} } }
                let (sf, cfg) = (self.surface.as_ref().unwrap(), self.config.as_ref().unwrap());
                match sf.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(frame) | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                        let view = frame.texture.create_view(&Default::default());
                        self.app.render(&view, (cfg.width, cfg.height));
                        let t = self.app.title(); self.window.as_ref().unwrap().set_title(&t);
                        self.app.gfx.queue.present(frame);
                    }
                    _ => { sf.configure(&self.app.gfx.device, cfg); }
                }
                self.window.as_ref().unwrap().request_redraw();
            }
            _ => {}
        }
    }
    fn device_event(&mut self, _el: &winit::event_loop::ActiveEventLoop, _id: winit::event::DeviceId, ev: winit::event::DeviceEvent) {
        if let winit::event::DeviceEvent::MouseMotion { delta } = ev { if self.drag { self.app.yaw -= delta.0 as f32 * 0.003; self.app.pitch = (self.app.pitch - delta.1 as f32 * 0.003).clamp(-1.5, 1.5); } }
    }
}

// ------------------------------------------------------------------ offscreen helpers
fn render_to_rgba(app: &mut App, size: (u32, u32)) -> (Vec<u8>, (usize, usize)) {
    let tex = app.gfx.device.create_texture(&wgpu::TextureDescriptor { label: Some("shot"), size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Rgba8Unorm, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC, view_formats: &[] });
    let view = tex.create_view(&Default::default());
    let stats = app.render(&view, size);
    let bpr = (size.0 * 4 + 255) / 256 * 256;
    let buf = app.gfx.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (bpr * size.1) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
    let mut enc = app.gfx.device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All }, wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(size.1) } }, wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 });
    app.gfx.queue.submit([enc.finish()]);
    let sl = buf.slice(..); sl.map_async(wgpu::MapMode::Read, |_| {});
    app.gfx.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = sl.get_mapped_range().unwrap();
    let mut out = Vec::with_capacity((size.0 * size.1 * 4) as usize);
    for y in 0..size.1 { out.extend_from_slice(&data[(y * bpr) as usize..(y * bpr + size.0 * 4) as usize]); }
    (out, stats)
}

fn save_png(path: &str, size: (u32, u32), rgba: &[u8]) {
    let f = std::fs::File::create(path).unwrap();
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), size.0, size.1); e.set_color(png::ColorType::Rgba); e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(rgba).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let has = |k: &str| args.iter().any(|a| a == k);
    let work = PathBuf::from(get("--work").unwrap_or("/tmp/sm3wgsl-work".into()));
    let cod4 = PathBuf::from(get("--cod4").unwrap_or("/Users/aidanp/Projects/cod4-decomp/COD4".into()));
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).expect("no adapter");
    let bc = adapter.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { required_features: if bc { wgpu::Features::TEXTURE_COMPRESSION_BC } else { wgpu::Features::empty() }, required_limits: wgpu::Limits::default().using_resolution(adapter.limits()), ..Default::default() })).expect("device");
    println!("adapter {:?}, BC textures: {bc}", adapter.get_info().name);
    let gfx = Gfx { device, queue, color_format: wgpu::TextureFormat::Rgba8Unorm, bc };
    let mut app = App::new(gfx, work, cod4);
    if let Some(p) = get("--path") { app.path = match p.as_str() { "a" => PathId::Mojo, "b" => PathId::Vkd3d, _ => PathId::Custom }; }
    if let Some(f) = get("--focus") { app.focus = f.parse::<usize>().ok().or_else(|| app.focus_list.iter().position(|&m| app.world.mats[m].name.contains(&f))).unwrap_or(0).min(app.focus_list.len() - 1); }
    if has("--only-focus") { app.only_focus = true; }
    if let Some(c) = get("--cam") { let v: Vec<f32> = c.split(',').map(|x| x.parse().unwrap()).collect(); app.cam = Vec3::new(v[0], v[1], v[2]); app.yaw = v[3].to_radians(); app.pitch = v[4].to_radians(); }
    let size = get("--size").map(|s| { let (w, h) = s.split_once('x').unwrap(); (w.parse().unwrap(), h.parse().unwrap()) }).unwrap_or((1280u32, 720u32));
    if let Some(out) = get("--shot") {
        app.dump();
        let (rgba, (drawn, failed)) = render_to_rgba(&mut app, size);
        println!("drew {drawn} surfaces, {failed} skipped (shader/pipeline failure) with path {}", app.path.label());
        save_png(&out, size, &rgba); println!("wrote {out}"); return;
    }
    if has("--survey") {
        // pipeline-creation pass/fail over all world materials for every path (real wgpu/Metal pipeline validation)
        for p in PathId::ALL { let mut ok = 0; let mut bad: HashMap<String, usize> = HashMap::new(); let n = app.focus_list.len();
            for i in 0..n { let m = app.focus_list[i]; app.prepare(m, p); let pr = &app.prepared[&(m, p)]; match &pr.error { None => ok += 1, Some(e) => *bad.entry(e.chars().take(130).collect()).or_default() += 1 } }
            println!("path {}: {ok}/{n} world materials build a valid wgpu render pipeline", p.label()); let mut b: Vec<_> = bad.into_iter().collect(); b.sort_by_key(|x| std::cmp::Reverse(x.1)); for (e, c) in b.iter().take(5) { println!("    {c:3}  {e}"); } }
        return;
    }
    if has("--diff") {
        // render the same view with each path and compare pixels
        let mut imgs = vec![];
        for p in PathId::ALL { app.path = p; let (rgba, (d, f)) = render_to_rgba(&mut app, size); println!("path {}: drew {d}, skipped {f}", p.short()); imgs.push(rgba); }
        for (i, j) in [(0, 1), (0, 2), (1, 2)] { let (a, b) = (&imgs[i], &imgs[j]); let mut sum = 0u64; let mut big = 0; let mut any = 0usize; let mut mx = 0i32; for k in 0..a.len() / 4 { let mut m = 0i32; for c in 0..3 { m = m.max((a[k * 4 + c] as i32 - b[k * 4 + c] as i32).abs()); sum += (a[k * 4 + c] as i32 - b[k * 4 + c] as i32).unsigned_abs() as u64; } if m > 8 { big += 1; } if m > 0 { any += 1; } mx = mx.max(m); }
            println!("diff {} vs {}: mean abs channel diff {:.4}/255, max channel diff {mx}, pixels differing at all {:.2}%, by >8: {:.3}%", PathId::ALL[i].short(), PathId::ALL[j].short(), sum as f64 / (a.len() / 4 * 3) as f64, 100.0 * any as f64 / (a.len() / 4) as f64, 100.0 * big as f64 / (a.len() / 4) as f64); }
        for (n, im) in imgs.iter().enumerate() { save_png(&format!("/tmp/sm3wgsl-diff-{}.png", PathId::ALL[n].short()), size, im); }
        return;
    }
    let el = winit::event_loop::EventLoop::new().unwrap();
    let mut win = Win { app, window: None, surface: None, config: None, keys: Default::default(), drag: false, last: std::time::Instant::now(), instance, adapter };
    el.run_app(&mut win).unwrap();
}

/// left-handed look-to view matrix (D3D style)
fn look_to_lh(eye: Vec3, dir: Vec3, up: Vec3) -> Mat4 {
    let f = dir.normalize(); let s = up.cross(f).normalize(); let u = f.cross(s);
    Mat4::from_cols(glam::Vec4::new(s.x, u.x, f.x, 0.0), glam::Vec4::new(s.y, u.y, f.y, 0.0), glam::Vec4::new(s.z, u.z, f.z, 0.0), glam::Vec4::new(-s.dot(eye), -u.dot(eye), -f.dot(eye), 1.0))
}
/// left-handed perspective, depth 0..1
fn perspective_lh(fovy: f32, aspect: f32, n: f32, f: f32) -> Mat4 {
    let h = 1.0 / (fovy * 0.5).tan(); let w = h / aspect; let r = f / (f - n);
    Mat4::from_cols(glam::Vec4::new(w, 0.0, 0.0, 0.0), glam::Vec4::new(0.0, h, 0.0, 0.0), glam::Vec4::new(0.0, 0.0, r, 1.0), glam::Vec4::new(0.0, 0.0, -r * n, 0.0))
}
