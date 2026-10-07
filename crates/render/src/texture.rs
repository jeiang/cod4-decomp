// SPDX-License-Identifier: GPL-3.0-or-later
//! Images to GPU textures: IWI files from the VFS and the images a zone carries inline (lightmaps, probes).

use crate::gpu::Gpu;
use assets::iwi::{self, Texels};
use assets::vfs::Vfs;
use assets::zone::gfx::GfxImage;
use sm3::SamplerDim;
use std::collections::HashMap;
use std::sync::Arc;
use wgpu::util::DeviceExt;

pub struct Tex {
    pub view: wgpu::TextureView,
    pub dim: SamplerDim,
}

const MAPTYPE_3D: u32 = 4;
const MAPTYPE_CUBE: u32 = 5;
const D3DFMT_A8R8G8B8: i32 = 21;
const D3DFMT_X8R8G8B8: i32 = 22;
const D3DFMT_L8: i32 = 50;

fn view_dim(d: SamplerDim) -> wgpu::TextureViewDimension {
    match d {
        SamplerDim::D2 => wgpu::TextureViewDimension::D2,
        SamplerDim::Cube => wgpu::TextureViewDimension::Cube,
        SamplerDim::D3 => wgpu::TextureViewDimension::D3,
    }
}

/// A texture from tightly packed layer-major data (each layer: its mips, largest first).
pub fn upload(
    gpu: &Gpu,
    label: &str,
    dim: SamplerDim,
    size: [u32; 3],
    mips: u32,
    format: wgpu::TextureFormat,
    data: &[u8],
) -> Tex {
    let layers = if dim == SamplerDim::Cube { 6 } else { 1 };
    let tex = gpu.device.create_texture_with_data(
        &gpu.queue,
        &wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: if dim == SamplerDim::D3 {
                    size[2]
                } else {
                    layers
                },
            },
            mip_level_count: mips,
            sample_count: 1,
            dimension: if dim == SamplerDim::D3 {
                wgpu::TextureDimension::D3
            } else {
                wgpu::TextureDimension::D2
            },
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        data,
    );
    Tex {
        view: tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(view_dim(dim)),
            ..Default::default()
        }),
        dim,
    }
}

pub struct TextureCache {
    vfs: Option<Vfs>,
    cache: HashMap<String, Option<Arc<Tex>>>,
    placeholders: HashMap<(SamplerDim, [u8; 4]), Arc<Tex>>,
    picmip: usize,
    /// Names of images that could not be loaded.
    pub failed: Vec<String>,
}

impl TextureCache {
    pub fn new(vfs: Option<Vfs>, picmip: usize) -> Self {
        TextureCache {
            vfs,
            cache: HashMap::new(),
            placeholders: HashMap::new(),
            picmip,
            failed: Vec::new(),
        }
    }

    /// A 1x1 texel of `rgba` in the given dimensionality, for samplers the renderer has no real image for.
    pub fn solid(&mut self, gpu: &Gpu, dim: SamplerDim, rgba: [u8; 4]) -> Arc<Tex> {
        self.placeholders
            .entry((dim, rgba))
            .or_insert_with(|| {
                let n = if dim == SamplerDim::Cube { 6 } else { 1 };
                let data: Vec<u8> = rgba.repeat(n);
                Arc::new(upload(
                    gpu,
                    "solid",
                    dim,
                    [1, 1, 1],
                    1,
                    wgpu::TextureFormat::Rgba8Unorm,
                    &data,
                ))
            })
            .clone()
    }

    /// The image as a texture; `None` when it cannot be loaded (callers fall back to a placeholder).
    pub fn image(&mut self, gpu: &Gpu, img: &GfxImage) -> Option<Arc<Tex>> {
        let name = img.name.as_deref()?.trim_start_matches(',');
        if let Some(t) = self.cache.get(name) {
            return t.clone();
        }
        let tex = match &img.load_def {
            Some(d) if !d.data.is_empty() => load_inline(gpu, img),
            _ => self.load_iwi(gpu, name),
        }
        .map(Arc::new);
        if tex.is_none() {
            self.failed.push(name.to_owned());
        }
        self.cache.insert(name.to_owned(), tex.clone());
        tex
    }

    fn load_iwi(&self, gpu: &Gpu, name: &str) -> Option<Tex> {
        let vfs = self.vfs.as_ref()?;
        let image = iwi::load_picmip(vfs, name, self.picmip).ok()?;
        let (w, h) = (
            image.level(0, 0).width as u32,
            image.level(0, 0).height as u32,
        );
        let cube = image.faces == 6;
        let dim = if cube {
            SamplerDim::Cube
        } else {
            SamplerDim::D2
        };
        let mips = image.mip_count();
        let block = !matches!(image.texels, Texels::Rgba8);
        let bc_ok = gpu.bc && block && w % 4 == 0 && h % 4 == 0;
        let (format, convert) = match (image.texels, bc_ok) {
            (Texels::Bc1, true) => (wgpu::TextureFormat::Bc1RgbaUnorm, false),
            (Texels::Bc2, true) => (wgpu::TextureFormat::Bc2RgbaUnorm, false),
            (Texels::Bc3, true) => (wgpu::TextureFormat::Bc3RgbaUnorm, false),
            _ => (wgpu::TextureFormat::Rgba8Unorm, true),
        };
        let mut data = Vec::new();
        for face in 0..image.faces {
            for mip in 0..mips {
                let l = image.level(mip, face);
                if convert {
                    data.extend_from_slice(&l.to_rgba8(image.texels));
                } else {
                    data.extend_from_slice(&l.data);
                }
            }
        }
        Some(upload(
            gpu,
            name,
            dim,
            [w, h, 1],
            mips as u32,
            format,
            &data,
        ))
    }
}

/// Images stored in the zone: `GfxImageLoadDef` pixels in D3D formats, face-major then mip-major.
fn load_inline(gpu: &Gpu, img: &GfxImage) -> Option<Tex> {
    let def = img.load_def.as_ref()?;
    let (w, h) = (u32::from(img.width), u32::from(img.height));
    let cube = img.map_type == MAPTYPE_CUBE;
    if img.map_type == MAPTYPE_3D {
        return None;
    }
    let mips = u32::from(def.level_count).max(1);
    let bytes_per = match def.format {
        D3DFMT_A8R8G8B8 | D3DFMT_X8R8G8B8 => 4,
        D3DFMT_L8 => 1,
        _ => return None,
    };
    let mut out = Vec::new();
    let mut off = 0usize;
    for _face in 0..if cube { 6 } else { 1 } {
        for m in 0..mips {
            let n = ((w >> m).max(1) * (h >> m).max(1)) as usize;
            let src = def.data.get(off..off + n * bytes_per)?;
            off += n * bytes_per;
            match def.format {
                D3DFMT_L8 => out.extend(src.iter().flat_map(|&l| [l, l, l, 255])),
                _ => out.extend(
                    src.as_chunks::<4>()
                        .0
                        .iter()
                        .flat_map(|p| [p[2], p[1], p[0], p[3]]),
                ),
            }
        }
    }
    let dim = if cube {
        SamplerDim::Cube
    } else {
        SamplerDim::D2
    };
    Some(upload(
        gpu,
        img.name.as_deref().unwrap_or("inline"),
        dim,
        [w, h, 1],
        mips,
        wgpu::TextureFormat::Rgba8Unorm,
        &out,
    ))
}
