// SPDX-License-Identifier: GPL-3.0-or-later
//! `shot <install> <map> <x> <y> <z> <yaw deg> <pitch deg> <out.png> [WxH]`: render one frame headless.

use assets::vfs::Vfs;
use render::{Gpu, MapData, Renderer, Scene, TextureCache, View};
use std::sync::Arc;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let num = |i: usize| a[i].parse::<f32>().unwrap();
    let install = std::path::Path::new(&a[1]);
    let (w, h) = a.get(9).map_or((1280, 720), |s| {
        let (w, h) = s.split_once('x').unwrap();
        (w.parse().unwrap(), h.parse().unwrap())
    });
    let gpu = Arc::new(Gpu::new(None).unwrap());
    let t = std::time::Instant::now();
    let data = MapData::load(install, &a[2]).unwrap();
    let scene = Scene::new(&gpu, &data);
    let vfs = Vfs::open_stock(install, 0).unwrap();
    let mut r = Renderer::new(gpu.clone(), scene, &data.techsets, TextureCache::new(Some(vfs), 0));
    eprintln!("loaded in {:?}; failures: {:?}", t.elapsed(), r.materials.failures);
    eprintln!("bounds {:?} {:?}", r.scene.world.mins, r.scene.world.maxs);
    eprintln!("sun dir {:?} color {:?}", r.scene.sun_dir, r.scene.sun_color);
    for l in &r.scene.world.lightmaps {
        for i in [&l.primary, &l.secondary].into_iter().flatten() {
            let d = i.load_def.as_ref().unwrap();
            let mean = d.data.iter().map(|&b| u64::from(b)).sum::<u64>() as f64 / d.data.len() as f64;
            eprintln!("lightmap {:?} {}x{} fmt {} mips {} bytes {} mean {mean:.1}", i.name, i.width, i.height, d.format, d.level_count, d.data.len());
        }
    }
    if let Some(pat) = std::env::var_os("DUMP") {
        let pat = pat.to_string_lossy().into_owned();
        for m in r.scene.materials() {
            if m.name.as_deref().is_some_and(|n| n.contains(&pat)) {
                if let Some(p) = r.prepare_for_debug(&m, &[std::env::var("TECH").ok().and_then(|t| t.parse().ok()).unwrap_or(8)], if std::env::var_os("MODEL").is_some() { render::material::VertexKind::Model } else { render::material::VertexKind::World }) {
                    eprintln!("{}", p.describe());
                    if std::env::var_os("WGSL").is_some() {
                        eprintln!("{}", p.vs_wgsl());
                    }
                }
            }
        }
    }
    eprintln!("sky_start_surfs {:?}", r.scene.world.sky_start_surfs);
    if std::env::var_os("WIND").is_some() {
        let w = &r.scene.world;
        let (mut pos, mut neg) = (0, 0);
        let (mut up_pos, mut up_neg, mut side_pos, mut side_neg) = (0, 0, 0, 0);
        for s in &w.dpvs.surfaces {
            for t in 0..s.tri_count as usize {
                let idx: Vec<usize> = (0..3).map(|k| (w.indices[s.base_index as usize + t * 3 + k] as i32 + s.first_vertex) as usize).collect();
                let v = |i: usize| -> ([f32; 3], [f32; 3]) {
                    let b = &w.vertices[i * 44..i * 44 + 44];
                    let f = |o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
                    let sc = (f32::from(b[39]) + 192.0) / 32385.0;
                    ([f(0), f(4), f(8)], [(f32::from(b[36]) - 127.0) * sc, (f32::from(b[37]) - 127.0) * sc, (f32::from(b[38]) - 127.0) * sc])
                };
                let (a, na) = v(idx[0]);
                let (b, _) = v(idx[1]);
                let (c, _) = v(idx[2]);
                let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
                let n = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
                let d = n[0] * na[0] + n[1] * na[1] + n[2] * na[2];
                if d > 0.0 { pos += 1; } else { neg += 1; }
                let up = na[2].abs() > 0.7;
                match (up, d > 0.0) { (true, true) => up_pos += 1, (true, false) => up_neg += 1, (false, true) => side_pos += 1, _ => side_neg += 1 }
            }
        }
        eprintln!("winding: geometric normal agrees with vertex normal {pos} / disagrees {neg}; up {up_pos}/{up_neg} side {side_pos}/{side_neg}");
    }
    if std::env::var_os("MV").is_some() {
        let m = r.scene.world.dpvs.smodel_draw_insts.iter().filter_map(|m| m.model.clone()).find(|m| m.surfs.iter().any(|s| s.verts.len() >= 32 * 8)).unwrap();
        let s = m.surfs.iter().find(|s| s.verts.len() >= 32 * 8).unwrap();
        eprintln!("model {:?} verts {}", m.name, s.vert_count);
        for i in 0..6 {
            let b = &s.verts[i * 32..i * 32 + 32];
            eprintln!("v{i}: pos {:?} w {:?} col {:?} uv {:02x?} n {:02x?} t {:02x?}", [f32::from_le_bytes(b[0..4].try_into().unwrap()), f32::from_le_bytes(b[4..8].try_into().unwrap()), f32::from_le_bytes(b[8..12].try_into().unwrap())], f32::from_le_bytes(b[12..16].try_into().unwrap()), &b[16..20], &b[20..24], &b[24..28], &b[28..32]);
        }
    }
    if std::env::var_os("TREES").is_some() {
        for (ci, c) in r.scene.world.cells.iter().enumerate().take(3) {
            eprintln!("cell {ci} trees {} portals {}", c.aabb_trees.len(), c.portals.len());
            for (i, t) in c.aabb_trees.iter().enumerate().take(12) {
                eprintln!("  tree {i}: children {} off {} surf {} start {} nodecal {} {} smodels {}", t.child_count, t.children_offset, t.surface_count, t.start_surf_index, t.surface_count_no_decal, t.start_surf_index_no_decal, t.smodel_indexes.len());
            }
        }
    }
    if std::env::var_os("COVER").is_some() {
        let w = &r.scene.world;
        let n = w.dpvs.surfaces.len();
        let mut leaf = vec![0u32; n];
        let mut any = vec![0u32; n];
        for c in &w.cells {
            for (i, t) in c.aabb_trees.iter().enumerate() {
                for k in t.start_surf_index as usize..(t.start_surf_index as usize + t.surface_count as usize) {
                    if k < n { any[k] += 1; if t.child_count == 0 { leaf[k] += 1; } }
                }
                let _ = i;
            }
        }
        eprintln!("cover: surfaces {n}, in some leaf {}, in some node {}, in >1 leaf {}", leaf.iter().filter(|&&x| x > 0).count(), any.iter().filter(|&&x| x > 0).count(), leaf.iter().filter(|&&x| x > 1).count());
    }
    if std::env::var_os("BOXES").is_some() {
        let w = &r.scene.world;
        let mut bad = 0;
        for (ci, c) in w.cells.iter().enumerate() {
            for (ti, t) in c.aabb_trees.iter().enumerate() {
                for k in t.start_surf_index as usize..(t.start_surf_index as usize + t.surface_count as usize) {
                    let b = &w.dpvs.surfaces[k].bounds;
                    if t.child_count != 0 { continue; }
                    let outside = (0..3).any(|a| b[0][a] < t.mins[a] - 1.0 || b[1][a] > t.maxs[a] + 1.0);
                    if outside { bad += 1; if bad < 6 { eprintln!("cell {ci} tree {ti} box {:?} {:?} surf {k} bounds {:?}", t.mins, t.maxs, b); } }
                }
            }
        }
        eprintln!("surfaces outside their tree box: {bad}");
    }
    if std::env::var_os("BIG").is_some() {
        let mut v: Vec<_> = r.scene.world.dpvs.surfaces.iter().enumerate().map(|(i, s)| {
            let d = [s.bounds[1][0] - s.bounds[0][0], s.bounds[1][1] - s.bounds[0][1], s.bounds[1][2] - s.bounds[0][2]];
            (d[0] * d[1], i, s)
        }).collect();
        v.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (a, i, s) in v.iter().take(15) {
            eprintln!("big {i} area {a} {:?} tris {} tech {:?} sk {} flags {}", s.material.as_ref().and_then(|m| m.name.clone()), s.tri_count, s.material.as_ref().and_then(|m| m.technique_set.as_ref()).and_then(|t| t.name.clone()), s.material.as_ref().map_or(0, |m| m.sort_key), s.flags);
        }
    }
    if let Some(pat) = std::env::var_os("SURFS") {
        let pat = pat.to_string_lossy().into_owned();
        for (i, s) in r.scene.world.dpvs.surfaces.iter().enumerate() {
            let n = s.material.as_ref().and_then(|m| m.name.clone()).unwrap_or_default();
            if n.contains(&pat) {
                eprintln!("surf {i} {n} base {} tris {} fv {} vc {} lm {} probe {} pl {} flags {} bounds {:?}", s.base_index, s.tri_count, s.first_vertex, s.vertex_count, s.lightmap_index, s.reflection_probe_index, s.primary_light_index, s.flags, s.bounds);
            }
        }
    }
    if std::env::var_os("DUMPLM").is_some() {
        for (n, i) in [("lm_primary", &r.scene.world.lightmaps[0].primary), ("lm_secondary", &r.scene.world.lightmaps[0].secondary)] {
            let i = i.as_ref().unwrap();
            let d = i.load_def.as_ref().unwrap();
            let (w, h) = (u32::from(i.width), u32::from(i.height));
            let rgba: Vec<u8> = if d.format == 50 {
                d.data.iter().flat_map(|&l| [l, l, l, 255]).collect()
            } else {
                d.data.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]).collect()
            };
            let mut e = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(format!("/tmp/{n}.png")).unwrap()), w, h);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&rgba).unwrap();
        }
    }
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = View {
        origin: glam::Vec3::new(num(3), num(4), num(5)),
        yaw: num(6).to_radians(),
        pitch: num(7).to_radians(),
        fov_x: 90f32.to_radians(),
        time: 0.0,
    };
    let tv = tex.create_view(&Default::default());
    let stats = r.render(&view, &tv, format, (w, h));
    eprintln!("{stats:?}\nimage failures: {:?}", r.textures.failed);
    let bpr = (w * 4).next_multiple_of(256);
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(bpr * h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        tex.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = buf.slice(..).get_mapped_range().unwrap();
    let mut rgba = Vec::new();
    for y in 0..h {
        rgba.extend_from_slice(&data[(y * bpr) as usize..(y * bpr + w * 4) as usize]);
    }
    let mut e = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&a[8]).unwrap()), w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(&rgba).unwrap();
}
