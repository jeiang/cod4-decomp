// SPDX-License-Identifier: GPL-3.0-only
//! wgpu renderer: draws a map's `GfxWorld` with the original techsets (translated from D3D9 SM3 at load), lightmaps,
//! reflection probes, the model-lighting volume, sky, and static models.

pub mod art;
pub mod codeconst;
pub mod cull;
pub mod dynmesh;
pub mod gpu;
pub mod lightgrid;
pub mod material;
pub mod post;
mod renderer;
pub mod scene;
pub mod skin;
pub mod state;
pub mod sunshadow;
pub mod texture;
pub mod timing;
pub mod ui2d;
pub mod water;

pub use dynmesh::{DynMesh, DynVertex};
pub use gpu::{Gpu, GpuError, GpuInfo};
pub use post::{Dof, PostParams, ShellShock};
pub use renderer::{FrameStats, Progress, Renderer, Settings, ShadowMode, View};
pub use scene::{MapData, Scene};
pub use skin::{ModelInstance, ModelKind};
pub use texture::TextureCache;
