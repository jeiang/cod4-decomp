// SPDX-License-Identifier: GPL-3.0-or-later
//! `shot <install> <map> <x> <y> <z> <yaw deg> <pitch deg> <out.png> [WxH]`: render one frame headless.
//!
//! `DUMP=<material substring>` also prints the translated binding table of the matching materials
//! (`TECH=<n>` picks the technique, default 8 = lit sun; `MODEL=1` prepares them for static-model vertices;
//! `WGSL=1` adds the translated shaders). `SHADOWS=off|color`, `NOFOG=1` and `NOLIGHTS=1` switch features off.

use assets::vfs::Vfs;
use render::material::VertexKind;
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
    let mut r = Renderer::new(
        gpu.clone(),
        scene,
        &data,
        TextureCache::new(Some(vfs), 0),
    );
    match std::env::var("SHADOWS").as_deref() {
        Ok("off") => r.settings.shadows = render::ShadowMode::Off,
        Ok("color") => r.settings.shadows = render::ShadowMode::Color,
        _ => {}
    }
    r.settings.fog &= std::env::var_os("NOFOG").is_none();
    r.settings.primary_lights &= std::env::var_os("NOLIGHTS").is_none();
    eprintln!(
        "loaded in {:?}; failures: {:?}",
        t.elapsed(),
        r.materials.failures
    );
    if let Ok(pat) = std::env::var("DUMP") {
        let tech = std::env::var("TECH")
            .ok()
            .and_then(|t| t.parse().ok())
            .unwrap_or(8);
        let kind = if std::env::var_os("MODEL").is_some() {
            VertexKind::Model
        } else {
            VertexKind::World
        };
        for m in r.scene.materials() {
            if m.name.as_deref().is_some_and(|n| n.contains(&pat))
                && let Some(p) = r.inspect(&m, &[tech], kind)
            {
                eprintln!("{}", p.describe());
                if std::env::var_os("WGSL").is_some() {
                    eprintln!("// ---- vs\n{}\n// ---- ps\n{}", p.vs_wgsl(), p.ps_wgsl());
                }
            }
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
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    let data = buf.slice(..).get_mapped_range().unwrap();
    let mut rgba = Vec::new();
    for y in 0..h {
        rgba.extend_from_slice(&data[(y * bpr) as usize..(y * bpr + w * 4) as usize]);
    }
    // The sky shader writes alpha 0; the screenshot is opaque.
    rgba.as_chunks_mut::<4>()
        .0
        .iter_mut()
        .for_each(|p| p[3] = 255);
    let file = std::io::BufWriter::new(std::fs::File::create(&a[8]).unwrap());
    let mut e = png::Encoder::new(file, w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(&rgba).unwrap();
}
