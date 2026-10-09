// SPDX-License-Identifier: GPL-3.0-only
//! Install- and adapter-gated: a particle cloud of a stock effect material is drawn over a map, where the cloud's own
//! vertex layout and the constants of the object (`PARTICLE_CLOUD_MATRIX` and `PARTICLE_CLOUD_COLOR`) make it appear in
//! the picture, in front of where it is placed and not anywhere else. Skips without either.

use assets::vfs::Vfs;
use assets::zone::fx::{FxVisuals, elem};
use assets::zone::gfx::Material;
use assets::zone::{Asset, KeepAll, Zone};
use glam::Vec3;
use render::{Cloud, DynMesh, Gpu, MapData, Renderer, Scene, TextureCache, View};
use std::sync::Arc;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const W: u32 = 320;
const H: u32 = 180;

/// The materials of the cloud elements of the stock effects that have a technique set.
fn cloud_materials(root: &std::path::Path) -> Vec<Arc<Material>> {
    let Ok(file) = std::fs::File::open(root.join("zone/english/common_mp.ff")) else {
        return Vec::new();
    };
    let Ok(zone) = Zone::open(std::io::BufReader::new(file)) else {
        return Vec::new();
    };
    let mut found: Vec<Arc<Material>> = Vec::new();
    let _ = zone.decode(&KeepAll, |a| {
        if let Asset::Fx(f) = a {
            for e in f.elems.iter().filter(|e| e.elem_type == elem::CLOUD) {
                if let FxVisuals::Materials(ms) = &e.visuals {
                    for m in ms.iter().flatten().filter(|m| m.technique_set.is_some()) {
                        if !found.iter().any(|f| f.name == m.name) {
                            found.push(m.clone());
                        }
                    }
                }
            }
        }
    });
    found
}

fn read_back(gpu: &Gpu, tex: &wgpu::Texture) -> Vec<u8> {
    let row = W * 4;
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * H),
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
                bytes_per_row: Some(row),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([enc.finish()]);
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |r| r.unwrap());
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    slice.get_mapped_range().unwrap().to_vec()
}

/// Mean absolute difference of two pictures over the pixels `x0..x1` of every row.
fn diff(a: &[u8], b: &[u8], x0: u32, x1: u32) -> f64 {
    let (mut sum, mut n) = (0u64, 0u64);
    for y in 0..H {
        for x in x0..x1 {
            let i = ((y * W + x) * 4) as usize;
            for c in 0..3 {
                sum += u64::from(a[i + c].abs_diff(b[i + c]));
            }
            n += 3;
        }
    }
    sum as f64 / n as f64
}

#[test]
fn a_particle_cloud_is_drawn_where_it_is_placed() {
    let Some(root) = std::env::var_os("COD4_PATH").map(std::path::PathBuf::from) else {
        eprintln!("skipped: COD4_PATH not set");
        return;
    };
    let Ok(gpu) = Gpu::new(None) else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let Ok(data) = MapData::load(&root, "mp_crash") else {
        eprintln!("skipped: no mp_crash");
        return;
    };
    let materials = cloud_materials(&root);
    if materials.is_empty() {
        eprintln!("skipped: no cloud materials in common_mp");
        return;
    }
    let gpu = Arc::new(gpu);
    let scene = Scene::new(&gpu, &data);
    let vfs = Vfs::open_stock(&root, 0).unwrap();
    let mut r = Renderer::new(gpu.clone(), scene, &data, TextureCache::new(Some(vfs), 0));
    r.settings.aa_samples = 1;
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let tv = target.create_view(&Default::default());
    let at = data
        .spawn_points()
        .first()
        .map_or(Vec3::ZERO, |p| Vec3::from(*p) + Vec3::Z * 60.0);
    let view = View {
        origin: at,
        yaw: 0.0,
        pitch: 0.0,
        roll: 0.0,
        fov_x: 1.5,
        time: 0.0,
    };
    r.warm_step(FORMAT, std::time::Duration::from_secs(600));
    let plain_stats = r.render(&view, &tv, FORMAT, (W, H));
    let plain = read_back(&gpu, &target);

    // A big cloud to the left of straight ahead, in every cloud material the stock effects use.
    let mut drawn = 0;
    for material in &materials {
        let cloud = Cloud {
            origin: at + Vec3::new(160.0, 90.0, 0.0),
            axis: [Vec3::X, Vec3::Y, Vec3::Z],
            scale: 60.0,
            endpos: at + Vec3::new(160.0, 90.0, 0.0),
            radius: [40.0, 40.0],
            color: [255; 4],
        };
        r.dynamic_meshes = vec![DynMesh::cloud(material.clone(), cloud)];
        let stats = r.render(&view, &tv, FORMAT, (W, H));
        let with = read_back(&gpu, &target);
        let (left, right) = (diff(&plain, &with, 0, W / 2), diff(&plain, &with, W / 2, W));
        eprintln!(
            "{:?}: {} draws against {}, picture changed {left:.2} left, {right:.2} right",
            material.name, stats.draws, plain_stats.draws
        );
        if stats.draws > plain_stats.draws && left > 0.3 {
            drawn += 1;
            assert!(
                left > 4.0 * right,
                "the cloud is left of the view's middle but changed {left:.2} there and {right:.2} right of it"
            );
        }
    }
    assert!(
        drawn > 0,
        "no stock cloud material drew anything: {} tried",
        materials.len()
    );
}
