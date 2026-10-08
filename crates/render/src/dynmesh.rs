// SPDX-License-Identifier: GPL-3.0-only
//! Triangles the client builds every frame: effect sprites, decals clipped to the world. They are drawn with the same
//! `VertexKind::Model` pipelines as a skinned model surface, from the frame's dynamic vertex buffer, which is how the
//! original draws particles and marks of models (`GfxPackedVertex`).

use crate::skin::{VERTEX_SIZE, pack_unit};
use assets::zone::gfx::Material;
use glam::Vec3A;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DynVertex {
    pub pos: [f32; 3],
    /// RGBA.
    pub color: [u8; 4],
    pub uv: [f32; 2],
    pub normal: [f32; 3],
    pub tangent: [f32; 3],
}

/// A triangle list drawn with one material.
#[derive(Clone)]
pub struct DynMesh {
    pub material: Arc<Material>,
    /// Three vertices per triangle.
    pub verts: Vec<DynVertex>,
    /// Where the model-lighting volume is sampled for techniques that light the surface; `None` uses the sun's.
    pub light_origin: Option<[f32; 3]>,
}

impl DynMesh {
    pub fn new(material: Arc<Material>) -> Self {
        Self {
            material,
            verts: Vec::new(),
            light_origin: None,
        }
    }

    /// Appends the quad `corners` (in order around it) as two triangles.
    pub fn push_quad(&mut self, v: [DynVertex; 4]) {
        self.verts.extend([v[0], v[1], v[2], v[0], v[2], v[3]]);
    }
}

/// Most vertices of one draw: the index buffer of dynamic draws counts up from zero in 16 bits.
pub const MAX_DRAW_VERTS: usize = 65535 / 3 * 3;

/// Half-precision float bits of `f` (the original's texture-coordinate format: flush-to-zero below the normal
/// range, saturating above it).
pub fn f16_bits(f: f32) -> u16 {
    let b = f.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32 - 127 + 15;
    if exp <= 0 {
        return sign;
    }
    if exp >= 31 {
        return sign | 0x7bff;
    }
    // Round to nearest on the dropped mantissa bits.
    let m = b & 0x007f_ffff;
    let h = (exp as u32) << 10 | m >> 13;
    let h = h + ((m >> 12) & 1);
    sign | h.min(0x7bff) as u16
}

/// Appends the 32-byte `GfxPackedVertex` of `v`.
pub fn pack(v: &DynVertex, out: &mut Vec<u8>) {
    let at = out.len();
    out.resize(at + VERTEX_SIZE, 0);
    let o = &mut out[at..];
    for (i, c) in v.pos.iter().enumerate() {
        o[i * 4..i * 4 + 4].copy_from_slice(&c.to_le_bytes());
    }
    // The binormal's sign.
    o[12..16].copy_from_slice(&1.0f32.to_le_bytes());
    // D3DCOLOR: blue, green, red, alpha.
    o[16..20].copy_from_slice(&[v.color[2], v.color[1], v.color[0], v.color[3]]);
    // Packed texture coordinates: v in the low half, u in the high.
    o[20..22].copy_from_slice(&f16_bits(v.uv[1]).to_le_bytes());
    o[22..24].copy_from_slice(&f16_bits(v.uv[0]).to_le_bytes());
    pack_unit(Vec3A::from(v.normal), &mut o[24..28]);
    pack_unit(Vec3A::from(v.tangent), &mut o[28..32]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_match_the_known_encodings() {
        assert_eq!(f16_bits(0.0), 0);
        assert_eq!(f16_bits(1.0), 0x3c00);
        assert_eq!(f16_bits(-2.0), 0xc000);
        assert_eq!(f16_bits(0.5), 0x3800);
        assert_eq!(f16_bits(0.333_333_34), 0x3555);
        assert_eq!(f16_bits(1e9), 0x7bff);
        assert_eq!(f16_bits(1e-9), 0);
    }

    #[test]
    fn a_packed_vertex_puts_each_field_where_the_model_pipelines_read_it() {
        let mut out = Vec::new();
        pack(
            &DynVertex {
                pos: [1.0, 2.0, 3.0],
                color: [10, 20, 30, 40],
                uv: [1.0, 0.5],
                normal: [0.0, 0.0, 1.0],
                tangent: [1.0, 0.0, 0.0],
            },
            &mut out,
        );
        assert_eq!(out.len(), VERTEX_SIZE);
        assert_eq!(f32::from_le_bytes(out[8..12].try_into().unwrap()), 3.0);
        assert_eq!(&out[16..20], &[30, 20, 10, 40]);
        // v = 0.5 low, u = 1.0 high.
        assert_eq!(
            u32::from_le_bytes(out[20..24].try_into().unwrap()),
            0x3c00_3800
        );
        assert_eq!(&out[24..28], &[127, 127, 254, 63]);
        assert_eq!(&out[28..32], &[254, 127, 127, 63]);
    }
}
