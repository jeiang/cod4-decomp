// SPDX-License-Identifier: GPL-3.0-only
//! `shot <install> <map> <x> <y> <z> <yaw deg> <pitch deg> <out.png> [WxH]`: render one frame headless.
//!
//! `DUMP=<material substring>` also prints the translated binding table of the matching materials
//! (`TECH=<n>` picks the technique, default 8 = lit sun; `MODEL=1` prepares them for static-model vertices;
//! `WGSL=1` adds the translated shaders). `SHADOWS=off|color`, `NOFOG=1` and `NOLIGHTS=1` switch features off.
//!
//! Post effects, each overriding the map's own values: `DOF=near_start,near_end,far_start,far_end,near_blur,far_blur`,
//! `GLOW=radius,intensity,cutoff,desaturation` (`GLOW=0` turns it off), `FILM=0|1|contrast,brightness,desaturation[,1 for no tint]`,
//! `BLUR=radius` (virtual 640x480 pixels) and `SHELLSHOCK=1` (a second frame draws the overlays over the first).
//! `TOUR=<n>` also renders n views from spawn points as `<out>-<i>.png`.
//! `TIMING=<frames>` renders that many frames and prints the mean GPU time of every pass.

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
    let mut r = Renderer::new(gpu.clone(), scene, &data, TextureCache::new(Some(vfs), 0));
    match std::env::var("SHADOWS").as_deref() {
        Ok("off") => r.settings.shadows = render::ShadowMode::Off,
        Ok("color") => r.settings.shadows = render::ShadowMode::Color,
        _ => {}
    }
    r.settings.fog &= std::env::var_os("NOFOG").is_none();
    r.settings.primary_lights &= std::env::var_os("NOLIGHTS").is_none();
    set_post(&mut r);
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
        roll: 0.0,
        fov_x: 90f32.to_radians(),
        time: 0.0,
    };
    let tv = tex.create_view(&Default::default());
    if std::env::var_os("SHELLSHOCK").is_some() {
        r.post.save_screen = true;
        r.render(&view, &tv, format, (w, h));
        r.post.shell_shock = Some(render::ShellShock {
            blur_alpha: 0.6,
            flash_screengrab: 0.3,
            flash_whiteout: 0.15,
        });
    }
    let stats = r.render(&view, &tv, format, (w, h));
    eprintln!("{stats:?}\nimage failures: {:?}", r.textures.failed);
    if let Some(n) = std::env::var("TIMING")
        .ok()
        .and_then(|n| n.parse::<u32>().ok())
    {
        let mut sums: Vec<(&str, f64)> = Vec::new();
        let mut total = 0.0;
        for _ in 0..n {
            r.render(&view, &tv, format, (w, h));
            let t = r.timer.as_mut().expect("timestamps");
            t.flush(&gpu);
            total += t.last_total_ms.unwrap_or(0.0);
            for &(name, ms) in &t.last {
                match sums.iter_mut().find(|s| s.0 == name) {
                    Some(s) => s.1 += ms,
                    None => sums.push((name, ms)),
                }
            }
        }
        for (name, ms) in sums {
            eprintln!("gpu {name:>16}: {:.3} ms", ms / f64::from(n));
        }
        eprintln!("gpu {:>16}: {:.3} ms", "frame", total / f64::from(n));
    }
    save(&gpu, &tex, w, h, &a[8]);
    if let Some(n) = std::env::var("TOUR")
        .ok()
        .and_then(|n| n.parse::<usize>().ok())
    {
        // `TOUR=n`: n more frames from evenly spread spawn points at eye height, as <out>-<i>.png.
        let spawns = data.spawn_points();
        for i in 0..n.min(spawns.len()) {
            let p = spawns[i * spawns.len() / n.min(spawns.len())];
            let v = View {
                origin: glam::Vec3::from(p) + glam::Vec3::Z * 56.0,
                yaw: i as f32 * 1.3,
                pitch: -0.05,
                roll: 0.0,
                fov_x: 90f32.to_radians(),
                time: 0.0,
            };
            r.render(&v, &tv, format, (w, h));
            save(
                &gpu,
                &tex,
                w,
                h,
                &a[8].replace(".png", &format!("-{i}.png")),
            );
        }
    }
}

fn save(gpu: &Gpu, tex: &wgpu::Texture, w: u32, h: u32, path: &str) {
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
    let file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut e = png::Encoder::new(file, w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(&rgba).unwrap();
}

/// The `DOF`, `GLOW`, `FILM` and `BLUR` overrides.
fn set_post(r: &mut Renderer) {
    let nums = |name: &str| -> Option<Vec<f32>> {
        let v = std::env::var(name).ok()?;
        Some(
            v.split(',')
                .map(|n| n.trim().parse().expect(name))
                .collect(),
        )
    };
    if let Some(n) = nums("DOF") {
        r.post.dof = Some(render::Dof {
            view_model_start: 0.0,
            view_model_end: 0.0,
            near_start: n[0],
            near_end: n[1],
            far_start: n[2],
            far_end: n[3],
            near_blur: n[4],
            far_blur: n[5],
        });
    }
    if let Some(n) = nums("GLOW") {
        let g = &mut r.post.glow;
        g.enabled = n[0] != 0.0;
        if n.len() >= 4 {
            (
                g.radius,
                g.bloom_intensity,
                g.bloom_cutoff,
                g.bloom_desaturation,
            ) = (n[0], n[1], n[2], n[3]);
        }
    }
    if let Some(n) = nums("FILM") {
        let f = &mut r.post.film;
        f.enabled = n[0] != 0.0;
        if n.len() >= 3 {
            (f.contrast, f.brightness, f.desaturation) = (n[0], n[1], n[2]);
        }
        if n.len() >= 4 && n[3] != 0.0 {
            (f.tint_light, f.tint_dark) = ([1.0; 3], [1.0; 3]);
        }
    }
    if let Some(n) = nums("BLUR") {
        r.post.blur_radius = n[0];
    }
}
