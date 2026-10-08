// SPDX-License-Identifier: GPL-3.0-or-later
//! `--viewmodel-tour <n>`: the first-person weapon and hands, drawn headless from `n` spawn points of the map and the
//! points on the way between opposite ones (two headings each), once with and once without them. The pixels the two frames
//! differ in are the view model: the report lists their share of the picture, mean colour and the share of pixels one channel dominates per view. A tint that does not belong to the lighting (a
//! reflection probe of another room, a stale lighting entry) shows as one channel dominating the mean.

use crate::models::Library;
use crate::viewmodel::ViewModel;
use glam::Vec3;
use render::{Gpu, MapData, Renderer, Scene, TextureCache, View};
use serde_json::{Value, json};
use sim::pm::{PlayerState, weapon_state};
use std::path::Path;
use std::sync::Arc;

const SIZE: (u32, u32) = (1280, 720);
const WEAPON: &str = "m16_gl_mp";
const EYE_HEIGHT: f32 = 60.0;
/// The most pixels of a view one channel may dominate: the stock gun and hands are neutral metal and tan camouflage; a
/// default (solid red) reflection probe turned 24-68% of them red.
const MAX_TINTED: f64 = 0.10;
/// Mean brightness of a view model drawn at a spawn point (0..255): neither black nor blown out.
const SPAWN_BRIGHTNESS: std::ops::RangeInclusive<f64> = 8.0..=220.0;
/// The least share of the picture the weapon and hands cover; they are always in front of the camera.
const MIN_SHARE: f64 = 0.03;
/// A pixel belongs to the view model when it differs from the frame without it by more than this in some channel.
const DELTA: i32 = 12;

fn read(gpu: &Gpu, tex: &wgpu::Texture) -> Result<Vec<u8>, String> {
    let (w, h) = SIZE;
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
        .map_err(|e| e.to_string())?;
    let data = buf
        .slice(..)
        .get_mapped_range()
        .map_err(|e| e.to_string())?;
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        rgba.extend_from_slice(&data[(y * bpr) as usize..(y * bpr + w * 4) as usize]);
    }
    Ok(rgba)
}

fn save(path: &Path, rgba: &[u8]) -> Result<(), String> {
    let file = std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| e.to_string())?);
    let mut e = png::Encoder::new(file, SIZE.0, SIZE.1);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header()
        .and_then(|mut w| w.write_image_data(rgba))
        .map_err(|e| e.to_string())
}

/// The view model's share of the picture, its mean colour, and the share of its pixels one channel dominates (bright
/// and over 1.5 times both others) in the frame with it and the frame without it.
fn viewmodel_colour(with: &[u8], without: &[u8]) -> (f64, [f64; 3], f64) {
    let (mut n, mut sum, mut tinted) = (0u64, [0u64; 3], 0u64);
    for (a, b) in with
        .as_chunks::<4>()
        .0
        .iter()
        .zip(without.as_chunks::<4>().0)
    {
        if (0..3).any(|c| (i32::from(a[c]) - i32::from(b[c])).abs() > DELTA) {
            n += 1;
            for c in 0..3 {
                sum[c] += u64::from(a[c]);
            }
            let [r, g, bl] = [a[0], a[1], a[2]].map(u32::from);
            let dominant = |x: u32, y: u32, z: u32| x > 40 && 2 * x > 3 * y.max(z);
            tinted += u64::from(dominant(r, g, bl) || dominant(g, r, bl) || dominant(bl, r, g));
        }
    }
    let mean = sum.map(|s| s as f64 / n.max(1) as f64);
    (
        n as f64 / (with.len() / 4) as f64,
        mean,
        tinted as f64 / n.max(1) as f64,
    )
}

/// Renders `views` evenly spread spawn points of `map`. `out` also gets each view as `<map>-<i>.png`.
/// Returns the report and what is wrong with it.
pub fn run(
    install: &Path,
    map: &str,
    views: usize,
    out: Option<&Path>,
) -> Result<(Value, Vec<String>), String> {
    let lib = Library::load(install, map)?;
    let data = MapData::load(install, map).map_err(|e| format!("{e:?}"))?;
    let def = lib
        .content
        .weapon(WEAPON)
        .cloned()
        .ok_or_else(|| format!("weapon {WEAPON} not loaded"))?;
    let mut vm = ViewModel::new(&lib.content, &def, None)?;
    let spawns = data.spawn_points();
    if spawns.is_empty() {
        return Err(format!("{map} has no spawn points"));
    }
    let gpu = Arc::new(Gpu::new(None).map_err(|e| format!("{e:?}"))?);
    let scene = Scene::new(&gpu, &data);
    let vfs = assets::vfs::Vfs::open_stock(install, 0).map_err(|e| e.to_string())?;
    let mut r = Renderer::new(gpu.clone(), scene, &data, TextureCache::new(Some(vfs), 0));
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: SIZE.0,
            height: SIZE.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let tv = tex.create_view(&Default::default());
    let mut samples = Vec::new();
    let mut bad = Vec::new();
    // The spawn points, and the way from each to the one across the map in quarters: where a player crosses it.
    let n = views.min(spawns.len());
    let picked: Vec<[f32; 3]> = (0..n).map(|i| spawns[i * spawns.len() / n]).collect();
    let mut points = picked.clone();
    for i in 0..n {
        let (a, b) = (picked[i], picked[(i + n / 2) % n]);
        for t in [0.25, 0.5, 0.75] {
            points.push([0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * t));
        }
    }
    for (i, &p) in points.iter().enumerate() {
        for heading in 0..2 {
            let yaw = i as f32 * 1.3 + heading as f32 * std::f32::consts::PI;
            let ps = PlayerState {
                origin: p,
                view_height_current: EYE_HEIGHT,
                viewangles: [0.0, yaw.to_degrees(), 0.0],
                weapon_state: weapon_state::READY,
                ..PlayerState::default()
            };
            let view = View {
                origin: Vec3::from(p) + Vec3::Z * EYE_HEIGHT,
                yaw,
                pitch: 0.0,
                fov_x: 90f32.to_radians(),
                time: 0.0,
            };
            // The first update poses the weapon; the second settles the idle animation.
            vm.update(&ps, 0.05);
            r.dynamic_models = vm.update(&ps, 0.05);
            r.render(&view, &tv, format, SIZE);
            let with = read(&gpu, &tex)?;
            r.dynamic_models = Vec::new();
            r.render(&view, &tv, format, SIZE);
            let without = read(&gpu, &tex)?;
            let (share, mean, tinted) = viewmodel_colour(&with, &without);
            if let Some(dir) = out {
                save(&dir.join(format!("{map}-{i}-{heading}.png")), &with)?;
            }
            let spawn = i < n;
            let name = format!("{map} view {i}-{heading} at {p:?}");
            if tinted > MAX_TINTED {
                bad.push(format!(
                    "{name}: {:.0}% of the view model is one colour",
                    tinted * 100.0
                ));
            }
            if share < MIN_SHARE {
                bad.push(format!(
                    "{name}: the view model covers only {:.1}% of the picture",
                    share * 100.0
                ));
            }
            let brightness = mean.iter().sum::<f64>() / 3.0;
            if spawn && !SPAWN_BRIGHTNESS.contains(&brightness) {
                bad.push(format!("{name}: mean brightness {brightness:.0}"));
            }
            samples.push(json!({"view": format!("{i}-{heading}"), "spawn": spawn, "origin": p, "share": share, "mean": mean, "tinted": tinted}));
        }
    }
    Ok((
        json!({"map": map, "samples": samples, "problems": bad}),
        bad,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(rgb: [u8; 3]) -> Vec<u8> {
        std::iter::repeat_n([rgb[0], rgb[1], rgb[2], 255], 100)
            .flatten()
            .collect()
    }

    #[test]
    fn a_red_view_model_is_tinted_and_a_neutral_or_tan_one_is_not() {
        let bare = frame([10, 10, 10]);
        assert!(viewmodel_colour(&frame([190, 80, 70]), &bare).2 > MAX_TINTED);
        assert!(viewmodel_colour(&frame([60, 62, 58]), &bare).2 <= MAX_TINTED);
        // Tan camouflage: red leads, but by less than 1.5 times the others.
        assert!(viewmodel_colour(&frame([120, 100, 74]), &bare).2 <= MAX_TINTED);
    }

    #[test]
    fn only_the_pixels_the_view_model_changes_count() {
        let bare = frame([190, 80, 70]);
        let (share, _, tinted) = viewmodel_colour(&bare, &bare);
        assert_eq!((share, tinted), (0.0, 0.0));
    }
}
