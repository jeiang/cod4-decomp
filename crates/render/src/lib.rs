// SPDX-License-Identifier: GPL-3.0-or-later
//! wgpu renderer: draws a map's `GfxWorld` with the original techsets (translated from D3D9 SM3 at load), lightmaps,
//! reflection probes, the model-lighting volume, sky, and static models.

pub mod codeconst;
pub mod cull;
pub mod gpu;
pub mod lightgrid;
pub mod material;
mod renderer;
pub mod scene;
pub mod state;
pub mod texture;

pub use gpu::{Gpu, GpuError, GpuInfo};
pub use renderer::{FrameStats, Renderer, View};
pub use scene::{MapData, Scene};
pub use texture::TextureCache;
