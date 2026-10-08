// SPDX-License-Identifier: GPL-3.0-only
//! Install- and adapter-gated: draws rects, a scissored rect, an alpha-blended material and a line of stock
//! `fonts/normalFont` text into an offscreen target and reads the pixels back. Skips without either.

use assets::vfs::Vfs;
use assets::zone::text::Font;
use assets::zone::{Asset, KeepAll, Zone};
use render::ui2d::{TextStyle, Ui2d};
use render::{Gpu, TextureCache};
use std::sync::Arc;

const W: u32 = 256;
const H: u32 = 96;

fn read_back(gpu: &Gpu, tex: &wgpu::Texture) -> Vec<u8> {
    let row = W * 4; // 1024, already a multiple of the copy alignment
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

fn px(img: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2], img[i + 3]]
}

fn lit(img: &[u8], x0: u32, y0: u32, x1: u32, y1: u32) -> usize {
    (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| (x, y)))
        .filter(|&(x, y)| px(img, x, y)[..3] != [0, 0, 0])
        .count()
}

fn normal_font(root: &std::path::Path) -> Option<Arc<Font>> {
    let file = std::fs::File::open(root.join("zone/english/code_post_gfx.ff")).ok()?;
    let zone = Zone::open(std::io::BufReader::new(file)).ok()?;
    let mut found = None;
    zone.decode(&KeepAll, |a| {
        if let Asset::Font(f) = a
            && f.name.as_deref() == Some("fonts/normalFont")
        {
            found = Some(f);
        }
    })
    .ok()?;
    found
}

#[test]
fn ui2d_rects_scissor_blend_and_text_reach_the_target() {
    let Some(root) = std::env::var_os("COD4_PATH").map(std::path::PathBuf::from) else {
        eprintln!("skipped: COD4_PATH not set");
        return;
    };
    let Ok(gpu) = Gpu::new(None) else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let Some(font) = normal_font(&root) else {
        eprintln!("skipped: no code_post_gfx.ff with fonts/normalFont");
        return;
    };
    let gpu = Arc::new(gpu);
    let vfs = Vfs::open_stock(&root, 0).unwrap();
    let mut cache = TextureCache::new(Some(vfs), 0);
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut ui = Ui2d::new(gpu.clone(), format);
    let face = ui.image(&mut cache, font.material.as_ref().unwrap());
    let glow = ui.image(&mut cache, font.glow_material.as_ref().unwrap());
    assert!(ui.missing.is_empty(), "missing: {:?}", ui.missing);
    assert_eq!(face.blend(), render::ui2d::Blend::Alpha);
    assert_eq!(glow.blend(), render::ui2d::Blend::Additive);

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
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let white = ui.white();
    let full = [0.0, 0.0, 1.0, 1.0];

    ui.begin((W, H));
    ui.quad(&white, [8.0, 8.0, 40.0, 40.0], full, [1.0, 0.0, 0.0, 1.0]);
    ui.quad(&white, [60.0, 8.0, 40.0, 40.0], full, [0.0, 1.0, 0.0, 0.5]);
    ui.scissor(Some([110.0, 8.0, 30.0, 40.0]));
    ui.quad(&white, [110.0, 8.0, 60.0, 40.0], full, [1.0, 1.0, 1.0, 1.0]);
    ui.scissor(None);
    // a blue backdrop, then the font sheet over it with the font's alpha-blended material
    ui.quad(&white, [180.0, 8.0, 64.0, 64.0], full, [0.0, 0.0, 1.0, 1.0]);
    ui.quad(&face, [180.0, 8.0, 64.0, 64.0], full, [1.0, 1.0, 0.0, 1.0]);
    let scale = font.pixel_height as f32 / 48.0; // one font pixel per pixel
    ui.draw_text(
        &font,
        &face,
        Some(&glow),
        "Hello",
        8.0,
        80.0,
        scale,
        [1.0; 4],
        TextStyle::from_menu_style(3, None),
        0,
    );
    ui.flush(&view, Some(wgpu::Color::BLACK));
    let img = read_back(&gpu, &target);

    assert_eq!(px(&img, 20, 20), [255, 0, 0, 255]);
    let g = px(&img, 80, 20);
    assert!(
        g[0] == 0 && (126..=130).contains(&g[1]) && g[2] == 0,
        "{g:?}"
    );
    assert_eq!(px(&img, 125, 20), [255, 255, 255, 255]);
    assert_eq!(px(&img, 150, 20), [0, 0, 0, 255]);

    let region: Vec<_> = (8..72)
        .flat_map(|y| (180..244).map(|x| (x, y)).collect::<Vec<_>>())
        .map(|(x, y)| px(&img, x, y))
        .collect();
    assert!(region.contains(&[0, 0, 255, 255]), "no backdrop left");
    assert!(
        region.iter().any(|p| p[0] > 0),
        "the alpha-blended material drew nothing"
    );

    let text = lit(&img, 8, 66, 80, 90);
    assert!(text > 40, "text pixels: {text}");
    assert_eq!(
        lit(&img, 8, 90, 80, 96),
        0,
        "nothing below the baseline's descenders"
    );
    assert_eq!(lit(&img, 100, 66, 180, 90), 0);

    // a second frame on the same Ui2d loads over the first and draws only what it queues
    ui.begin((W, H));
    ui.quad(
        &white,
        [200.0, 80.0, 20.0, 10.0],
        full,
        [0.0, 1.0, 1.0, 1.0],
    );
    ui.flush(&view, None);
    let img = read_back(&gpu, &target);
    assert_eq!(px(&img, 210, 85), [0, 255, 255, 255]);
    assert_eq!(px(&img, 20, 20), [255, 0, 0, 255]);

    // a loose image with no material
    let grad = ui.image_named(&mut cache, "gradient_fadein");
    let nope = ui.image_named(&mut cache, "no_such_image_xyz");
    assert_eq!(ui.missing, ["no_such_image_xyz"]);
    assert!(nope == ui.image_named(&mut cache, "NO_SUCH_IMAGE_XYZ"));
    ui.begin((W, H));
    ui.quad(
        &white,
        [0.0, 0.0, W as f32, H as f32],
        full,
        [0.0, 0.0, 0.0, 1.0],
    );
    ui.quad(&grad, [0.0, 0.0, W as f32, 32.0], full, [1.0; 4]);
    ui.flush(&view, None);
    let img = read_back(&gpu, &target);
    assert!(lit(&img, 0, 0, W, 32) > 100, "gradient_fadein drew nothing");
    assert_eq!(lit(&img, 0, 40, W, H), 0);
}
