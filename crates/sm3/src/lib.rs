// SPDX-License-Identifier: GPL-3.0-or-later
//! D3D9 SM3 bytecode to WGSL translation.
//!
//! [`translate`] parses a `vs_3_0` / `ps_3_0` token stream (with its CTAB comment) and emits WGSL that stays inside
//! the WebGL2-compatible floor: textures and samplers are separate bindings, there is no storage, and constants live
//! in one `array<vec4<f32>, 256>` uniform.
//!
//! Bind layout of the generated module (the caller builds matching bind group layouts from [`Translation`]):
//! * float constants `c`: `@group(0) @binding(0)` for a vertex shader, `@group(1) @binding(0)` for a pixel shader;
//!   index = D3D register. `def`'d constants are inlined and never read from the bank.
//! * boolean constants `bc` (only when read): binding 1 of the same group, `array<vec4<u32>, 2>`, register `r` is
//!   `bc[r / 4][r % 4]`.
//! * texture of sampler register `s`: `@group(2) @binding(s)`; its sampler: `@group(2) @binding(s + 16)`.
//! * vertex inputs / interpolants use `@location(semantic_location(usage, index))` so stages link by semantic,
//!   exactly like D3D9. Everything is widened to `vec4<f32>`.

mod ctab;
mod emit;
mod parse;

pub use ctab::{CtabEntry, RegisterSet};
pub use emit::{
    AlphaTest, Compare, Options, Reflection, SAMPLER_BINDING_OFFSET, SamplerDim, SamplerUse,
    Semantic, Translation, VertexFix, semantic_location,
};
pub use parse::{Dst, Inst, Reg, RegType, Shader, Src, Stage, blob_len, parse};

/// Why a blob could not be translated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not a well-formed SM2/SM3 token stream.
    Malformed(&'static str),
    /// Well-formed but uses something the emitter does not implement.
    Unsupported(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Malformed(m) => write!(f, "malformed shader: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// Parse `code` (little-endian token stream) and emit WGSL.
pub fn translate(code: &[u8], opts: &Options) -> Result<Translation, Error> {
    emit::emit(&parse(code)?, opts)
}
