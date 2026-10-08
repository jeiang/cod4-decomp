// SPDX-License-Identifier: GPL-3.0-only
//! The ocean simulation behind `TS_WATER_MAP` textures.
//!
//! A water material carries a Phillips-spectrum snapshot (`h0`) and the dispersion term `w` per frequency cell of an
//! `m x n` grid (centred: cell `(m/2, n/2)` is the zero frequency). Each frame the height field is
//! `h(x, t) = IFFT[ h0(k) e^{iwt} + conj(h0(-k)) e^{-iwt} ]`, written as an L8 image the water techset samples as its
//! height map (`normalMapSampler`): the pixel shader differences three octaves of it into the surface normal.

use crate::gpu::Gpu;
use crate::texture::Tex;
use assets::zone::gfx::Water;
use sm3::SamplerDim;
use std::sync::Arc;

/// Heights span `+-RANGE_RMS` standard deviations around mid-grey before clamping.
const RANGE_RMS: f32 = 3.0;

/// The height field of one water, on the CPU.
pub struct WaterField {
    water: Arc<Water>,
    /// Texels per byte step: `127.5 / (RANGE_RMS * rms)`.
    scale: f32,
    spectrum: Vec<[f32; 2]>,
    pixels: Vec<u8>,
}

impl WaterField {
    /// `None` when the water has no spectrum (a server-side load) or a grid that is not a power of two.
    pub fn new(water: Arc<Water>) -> Option<Self> {
        let (m, n) = (
            usize::try_from(water.m).ok()?,
            usize::try_from(water.n).ok()?,
        );
        if !m.is_power_of_two() || !n.is_power_of_two() || water.h0.len() != m * n {
            return None;
        }
        let mut f = WaterField {
            water,
            scale: 1.0,
            spectrum: vec![[0.0; 2]; m * n],
            pixels: vec![128; m * n],
        };
        // Fix the scale from the field at t = 0: waves keep their strength through the whole match.
        let heights = f.heights(0.0);
        let rms = (heights.iter().map(|h| h * h).sum::<f32>() / heights.len() as f32).sqrt();
        f.scale = if rms > 0.0 {
            127.5 / (RANGE_RMS * rms)
        } else {
            0.0
        };
        f.update(0.0);
        Some(f)
    }

    /// (columns, rows) of the image.
    pub fn size(&self) -> (u32, u32) {
        (self.water.n as u32, self.water.m as u32)
    }

    /// The height of every grid point at `time` seconds, row-major, in the spectrum's units.
    fn heights(&mut self, time: f32) -> Vec<f32> {
        let w = &*self.water;
        let (m, n) = (w.m as usize, w.n as usize);
        for r in 0..m {
            // The cell of the opposite frequency; row/column 0 is the unpaired edge of the centred grid.
            let rr = (m - r) % m;
            for c in 0..n {
                let cc = (n - c) % n;
                let (a, b) = (w.h0[r * n + c], w.h0[rr * n + cc]);
                let (s, co) = (w.w_term[r * n + c] * time).sin_cos();
                // a e^{iwt} + conj(b) e^{-iwt}
                self.spectrum[r * n + c] = [
                    a[0] * co - a[1] * s + b[0] * co - b[1] * s,
                    a[0] * s + a[1] * co - b[1] * co - b[0] * s,
                ];
            }
        }
        ifft2(&mut self.spectrum, m, n);
        // The centred spectrum shifts the result by half a grid: flip the sign of odd cells.
        self.spectrum
            .iter()
            .enumerate()
            .map(|(i, h)| {
                if (i / n + i % n) & 1 == 1 {
                    -h[0]
                } else {
                    h[0]
                }
            })
            .collect()
    }

    /// Recompute the height image for `time` seconds.
    pub fn update(&mut self, time: f32) {
        let heights = self.heights(time);
        let scale = self.scale;
        for (p, h) in self.pixels.iter_mut().zip(&heights) {
            *p = (128.0 + h * scale).clamp(0.0, 255.0) as u8;
        }
    }

    /// The L8 image of the last [`update`](Self::update), row-major.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
}

/// A [`WaterField`] and the GPU texture it feeds.
pub struct WaterSim {
    field: WaterField,
    pub tex: Arc<Tex>,
    texture: wgpu::Texture,
}

impl WaterSim {
    pub fn new(gpu: &Gpu, water: Arc<Water>) -> Option<Self> {
        let field = WaterField::new(water)?;
        let (n, m) = field.size();
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("water height"),
            size: wgpu::Extent3d {
                width: n,
                height: m,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let tex = Arc::new(Tex {
            view: texture.create_view(&Default::default()),
            dim: SamplerDim::D2,
            width: n,
        });
        let sim = WaterSim {
            field,
            tex,
            texture,
        };
        sim.upload(gpu);
        Some(sim)
    }

    /// Advance to `time` seconds and upload.
    pub fn update(&mut self, gpu: &Gpu, time: f32) {
        self.field.update(time);
        self.upload(gpu);
    }

    fn upload(&self, gpu: &Gpu) {
        let (n, m) = self.field.size();
        gpu.queue.write_texture(
            self.texture.as_image_copy(),
            self.field.pixels(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(n),
                rows_per_image: Some(m),
            },
            wgpu::Extent3d {
                width: n,
                height: m,
                depth_or_array_layers: 1,
            },
        );
    }
}

/// In-place radix-2 inverse transform (unnormalised) of `data`, `len` a power of two.
fn ifft(data: &mut [[f32; 2]]) {
    let len = data.len();
    let bits = len.trailing_zeros();
    for i in 0..len {
        let j = (i.reverse_bits() >> (usize::BITS - bits)) % len;
        if j > i {
            data.swap(i, j);
        }
    }
    let mut half = 1;
    while half < len {
        let step = std::f32::consts::PI / half as f32;
        for start in (0..len).step_by(half * 2) {
            for k in 0..half {
                let (s, c) = (step * k as f32).sin_cos();
                let (a, b) = (data[start + k], data[start + k + half]);
                let t = [b[0] * c - b[1] * s, b[0] * s + b[1] * c];
                data[start + k] = [a[0] + t[0], a[1] + t[1]];
                data[start + k + half] = [a[0] - t[0], a[1] - t[1]];
            }
        }
        half *= 2;
    }
}

fn ifft2(data: &mut [[f32; 2]], rows: usize, cols: usize) {
    for row in data.chunks_exact_mut(cols) {
        ifft(row);
    }
    let mut col = vec![[0.0; 2]; rows];
    for c in 0..cols {
        for r in 0..rows {
            col[r] = data[r * cols + c];
        }
        ifft(&mut col);
        for r in 0..rows {
            data[r * cols + c] = col[r];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ifft_of_a_single_frequency_is_a_cosine() {
        let mut d = vec![[0.0f32; 2]; 8];
        d[1] = [1.0, 0.0];
        ifft(&mut d);
        for (i, v) in d.iter().enumerate() {
            let want = (std::f32::consts::TAU * i as f32 / 8.0).cos();
            assert!((v[0] - want).abs() < 1e-5, "{i}: {v:?}");
        }
    }
}
