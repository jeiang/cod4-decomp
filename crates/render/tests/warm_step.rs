// SPDX-License-Identifier: GPL-3.0-only
//! Install- and adapter-gated: with a spent pipeline budget a frame skips draws whose pipeline is not built and counts
//! them, `warm_step` builds the demanded ones first and in the end every pipeline, and then a frame misses none.

use assets::vfs::Vfs;
use glam::Vec3;
use render::{Gpu, MapData, Renderer, Scene, TextureCache, View};
use std::sync::Arc;
use std::time::Duration;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

#[test]
fn spent_budget_skips_draws_until_warm_step_has_built_them() {
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
    let gpu = Arc::new(gpu);
    let scene = Scene::new(&gpu, &data);
    let vfs = Vfs::open_stock(&root, 0).unwrap();
    let mut r = Renderer::new(gpu.clone(), scene, &data, TextureCache::new(Some(vfs), 0));
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 320,
            height: 180,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let tour = data.spawn_points();
    let at = tour
        .first()
        .map_or(Vec3::ZERO, |p| Vec3::from(*p) + Vec3::Z * 60.0);
    let v = View {
        origin: at,
        yaw: 0.0,
        pitch: 0.0,
        roll: 0.0,
        fov_x: 1.5,
        time: 0.0,
    };

    // A zero budget builds exactly one pipeline per step (progress is guaranteed) and the frame builds none.
    let p0 = r.warm_step(FORMAT, Duration::ZERO);
    assert_eq!(p0.done, 1);
    assert!(p0.total > 100, "{p0:?}");
    let blocked = r.render(&v, &view, FORMAT, (320, 180));
    assert!(blocked.pipelines_missing > 0, "{blocked:?}");
    let p1 = r.warm_step(FORMAT, Duration::ZERO);
    assert_eq!(p1.done, 2);

    // A long budget finishes the enumeration, and a frame then draws everything.
    let end = r.warm_step(FORMAT, Duration::from_secs(600));
    assert_eq!(end.done, end.total);
    let full = r.render(&v, &view, FORMAT, (320, 180));
    assert_eq!(full.pipelines_missing, 0, "{full:?}");
    assert!(full.draws > blocked.draws, "{full:?} vs {blocked:?}");
}
