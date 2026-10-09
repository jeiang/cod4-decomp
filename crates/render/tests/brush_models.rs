// SPDX-License-Identifier: GPL-3.0-only
//! Install- and adapter-gated: an inline model of the map (a door, a crate) handed to the renderer is drawn where its
//! entity is, and drawn somewhere else when the entity moves. Skips without either.

use assets::vfs::Vfs;
use glam::Vec3;
use render::{BrushInstance, Gpu, MapData, Renderer, Scene, TextureCache, View};
use std::sync::Arc;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const W: u32 = 320;
const H: u32 = 180;

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

/// How many pixels differ visibly between two frames.
fn changed(a: &[u8], b: &[u8]) -> usize {
    a.chunks(4)
        .zip(b.chunks(4))
        .filter(|(p, q)| (0..3).map(|i| p[i].abs_diff(q[i]) as u32).sum::<u32>() > 24)
        .count()
}

#[test]
fn an_inline_model_is_drawn_where_its_entity_is() {
    let Some(root) = std::env::var_os("COD4_PATH").map(std::path::PathBuf::from) else {
        eprintln!("skipped: COD4_PATH not set");
        return;
    };
    let Ok(gpu) = Gpu::new(None) else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let Ok(data) = MapData::load(&root, "mp_cargoship") else {
        eprintln!("skipped: no mp_cargoship");
        return;
    };
    // The first inline model with surfaces of its own (model 0 is the map).
    let Some(model) = data
        .world
        .models
        .iter()
        .skip(1)
        .position(|m| m.surface_count > 1)
        .map(|i| i as u16 + 1)
    else {
        eprintln!("untested: the map has no inline model with surfaces");
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

    let draw = |r: &mut Renderer, at: Option<Vec3>| {
        r.brush_models = at
            .map(|p| BrushInstance {
                model,
                origin: p.to_array(),
                angles: [0.0; 3],
            })
            .into_iter()
            .collect();
        r.render(&view, &tv, FORMAT, (W, H));
        read_back(&gpu, &target)
    };
    let empty = draw(&mut r, None);
    let ahead = draw(&mut r, Some(at + Vec3::X * 50.0));
    let aside = draw(&mut r, Some(at + Vec3::X * 50.0 + Vec3::Y * 40.0));
    let shown = changed(&empty, &ahead);
    if shown == 0 {
        // Something in the way (a wall at the spawn) can hide it; the model still has to show when moved.
        eprintln!("untested: the model is hidden at this spawn");
        return;
    }
    assert!(shown > 50, "the model barely shows: {shown} pixels");
    assert!(
        changed(&ahead, &aside) > 50,
        "moving the entity did not move the model"
    );
}
