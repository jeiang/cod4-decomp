// SPDX-License-Identifier: GPL-3.0-only
//! Install- and adapter-gated: a heat-haze material drawn over a map makes the frame copy the scene first and then
//! shows that scene rather than white, and a z-feathered smoke sprite makes the frame draw the float-Z target. Skips
//! without either.

use assets::vfs::Vfs;
use assets::zone::fx::FxVisuals;
use assets::zone::gfx::Material;
use assets::zone::{Asset, KeepAll, Zone};
use glam::Vec3;
use render::{DynMesh, DynVertex, Gpu, MapData, Renderer, Scene, TextureCache, View};
use std::sync::Arc;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const W: u32 = 320;
const H: u32 = 180;

fn material(root: &std::path::Path, name: &str) -> Option<Arc<Material>> {
    let file = std::fs::File::open(root.join("zone/english/common_mp.ff")).ok()?;
    let zone = Zone::open(std::io::BufReader::new(file)).ok()?;
    let mut found = None;
    zone.decode(&KeepAll, |a| {
        // Effect materials are only reachable through the effects that use them.
        if let Asset::Fx(f) = a {
            for e in f.elems.iter() {
                if let FxVisuals::Materials(ms) = &e.visuals {
                    for m in ms.iter().flatten() {
                        if m.name.as_deref() == Some(name) {
                            found = Some(m.clone());
                        }
                    }
                }
            }
        }
    })
    .ok()?;
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

/// Mean of the red, green and blue channels over the middle third of the picture.
fn centre_mean(img: &[u8]) -> f64 {
    let (mut sum, mut n) = (0u64, 0u64);
    for y in H / 3..2 * H / 3 {
        for x in W / 3..2 * W / 3 {
            let i = ((y * W + x) * 4) as usize;
            sum += u64::from(img[i]) + u64::from(img[i + 1]) + u64::from(img[i + 2]);
            n += 3;
        }
    }
    sum as f64 / n as f64
}

/// A square of side `2 * half` facing the camera `ahead` units in front of it (looking along +x).
fn quad_ahead(mat: Arc<Material>, at: Vec3, ahead: f32, half: f32) -> DynMesh {
    let v = |y: f32, z: f32, u: f32, t: f32| DynVertex {
        pos: [at.x + ahead, at.y + y, at.z + z],
        color: [255; 4],
        uv: [u, t],
        normal: [-1.0, 0.0, 0.0],
        tangent: [0.0, 1.0, 0.0],
    };
    let mut m = DynMesh::new(mat);
    m.push_quad([
        v(half, half, 0.0, 0.0),
        v(-half, half, 1.0, 0.0),
        v(-half, -half, 1.0, 1.0),
        v(half, -half, 0.0, 1.0),
    ]);
    m
}

#[test]
fn distortion_samples_the_scene_copy_and_soft_particles_draw_float_z() {
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
    let (Some(haze), Some(smoke)) = (
        material(&root, "gfx_distortion_heat"),
        material(&root, "gfx_smk_white_atlas"),
    ) else {
        eprintln!("skipped: no effect materials in common_mp");
        return;
    };
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

    let plain = r.render(&view, &tv, FORMAT, (W, H));
    assert!(!plain.distortion_copy && !plain.floatz, "{plain:?}");
    let scene_mean = centre_mean(&read_back(&gpu, &target));
    if !(8.0..200.0).contains(&scene_mean) {
        eprintln!("untested: the view is empty or white (mean {scene_mean:.0}) at this spawn");
        return;
    }

    r.dynamic_meshes = vec![quad_ahead(haze, at, 100.0, 400.0)];
    let hazy = r.render(&view, &tv, FORMAT, (W, H));
    assert!(hazy.distortion_copy, "{hazy:?}");
    let hazy_mean = centre_mean(&read_back(&gpu, &target));
    assert!(
        hazy_mean < 220.0,
        "the haze shows white: mean {hazy_mean:.0}, scene {scene_mean:.0}"
    );

    r.settings.distortion = false;
    let off = r.render(&view, &tv, FORMAT, (W, H));
    assert!(!off.distortion_copy, "{off:?}");
    r.settings.distortion = true;

    r.dynamic_meshes = vec![quad_ahead(smoke, at, 100.0, 400.0)];
    let soft = r.render(&view, &tv, FORMAT, (W, H));
    assert!(soft.floatz, "{soft:?}");
}
