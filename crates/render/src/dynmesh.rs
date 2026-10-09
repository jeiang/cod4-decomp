// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (gfx_d3d/rb_tess.cpp: R_SetParticleCloudConstants, RB_CreateParticleCloud2dAxis; r_buffers.cpp: R_CreateParticleCloudBuffer; GPL-3.0, copyright the KisakCOD contributors and Activision).
//! Triangles the client builds every frame: effect sprites, decals clipped to the world. They are drawn with the same
//! `VertexKind::Model` pipelines as a skinned model surface, from the frame's dynamic vertex buffer, which is how the
//! original draws particles and marks of models (`GfxPackedVertex`).

use crate::skin::{VERTEX_SIZE, pack_unit};
use assets::zone::gfx::Material;
use glam::{Mat4, Vec2, Vec3, Vec3A};
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
    /// Set for a particle cloud, which has no `verts`: its sprites come from [`cloud_vertices`].
    pub cloud: Option<Cloud>,
}

impl DynMesh {
    pub fn new(material: Arc<Material>) -> Self {
        Self {
            material,
            verts: Vec::new(),
            light_origin: None,
            cloud: None,
        }
    }

    /// A particle cloud drawn with `material`.
    pub fn cloud(material: Arc<Material>, cloud: Cloud) -> Self {
        Self {
            cloud: Some(cloud),
            ..Self::new(material)
        }
    }

    /// Appends the quad `corners` (in order around it) as two triangles.
    pub fn push_quad(&mut self, v: [DynVertex; 4]) {
        self.verts.extend([v[0], v[1], v[2], v[0], v[2], v[3]]);
    }
}

/// A volume of soft sprites (`GfxParticleCloud`): the sprites of [`cloud_vertices`] fill a cube of half-width `scale`
/// around `origin`, turned by `axis`; the cloud's shader makes each face the camera with the `radius` it is given.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cloud {
    pub origin: Vec3,
    /// Forward, left, up.
    pub axis: [Vec3; 3],
    pub scale: f32,
    /// A point one unit back along the cloud's motion: the sprites stretch along the line to it.
    pub endpos: Vec3,
    /// The sprites' half width and height.
    pub radius: [f32; 2],
    /// RGBA.
    pub color: [u8; 4],
}

impl Cloud {
    /// The placement: the cube's unit corners to the world.
    pub fn world(&self) -> Mat4 {
        let a = |i: usize| (self.axis[i] * self.scale).extend(0.0);
        Mat4::from_cols(a(0), a(1), a(2), self.origin.extend(1.0))
    }

    /// `PARTICLE_CLOUD_MATRIX`: where a sprite's corner offsets go in view space, as the two columns of a 2x2.
    /// `view` takes world directions to view space (x right, y up, z ahead). Sprites of one radius are round; a
    /// moving cloud's are stretched along the motion as the camera sees it (`R_SetParticleCloudConstants`).
    pub fn matrix(&self, view: &Mat4) -> [f32; 4] {
        let [rx, ry] = self.radius;
        let round = [rx, 0.0, 0.0, ry];
        if rx == ry || (self.endpos - self.origin).abs().max_element() <= 0.001 {
            return round;
        }
        let along = view.transform_vector3((self.endpos - self.origin).normalize() * ry);
        if along.x < 0.001 && along.y < 0.001 {
            return round;
        }
        let across = Vec2::new(along.y, -along.x);
        let len = across.length();
        let side = across * (rx / len);
        let mut up = Vec2::new(along.x, along.y);
        if rx > len {
            up *= rx / len;
        }
        [side.x, side.y, up.x, up.y]
    }

    /// `PARTICLE_CLOUD_COLOR`.
    pub fn color_const(&self) -> [f32; 4] {
        self.color.map(|c| f32::from(c) / 255.0)
    }
}

/// Sprites in a cloud: a grid of 8 x 8 x 16 cells, one sprite in each at a random place.
pub const CLOUD_SPRITES: usize = 1024;
/// Vertices of a cloud as [`cloud_vertices`] lays them out: two triangles a sprite.
pub const CLOUD_VERTS: u32 = CLOUD_SPRITES as u32 * 6;
/// Bytes of one cloud vertex (`GfxPosTexVertex`): position, then texture coordinates.
pub const CLOUD_VERTEX_SIZE: usize = 20;

/// The vertices every particle cloud is drawn from (`R_CreateParticleCloudBuffer`): [`CLOUD_SPRITES`] sprites placed
/// at random in a cube of half-width one, each four corners at the same place that the shader pushes apart, as
/// two triangles of non-indexed vertices.
pub fn cloud_vertices() -> Vec<u8> {
    // A fixed sequence so every run draws the same cloud.
    let mut state = 0x2545_F491_u32;
    let mut next = move || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (state >> 8) as f32 / (1u32 << 24) as f32
    };
    const CORNERS: [[f32; 2]; 4] = [[0.0, 0.0], [0.0, 1.0], [1.0, 0.0], [1.0, 1.0]];
    let mut out = Vec::with_capacity(CLOUD_VERTS as usize * CLOUD_VERTEX_SIZE);
    for x in 0..8 {
        for y in 0..8 {
            for z in 0..16 {
                let pos = [
                    (next() + x as f32) * 0.25 - 1.0,
                    (next() + y as f32) * 0.25 - 1.0,
                    (next() + z as f32) * 0.125 - 1.0,
                ];
                for corner in [0, 1, 2, 2, 1, 3] {
                    for c in pos.iter().chain(&CORNERS[corner]) {
                        out.extend(c.to_le_bytes());
                    }
                }
            }
        }
    }
    out
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

    #[test]
    fn a_clouds_vertices_fill_its_cube_with_four_corners_a_sprite() {
        let v = cloud_vertices();
        assert_eq!(v.len(), CLOUD_VERTS as usize * CLOUD_VERTEX_SIZE);
        let f = |i: usize, k: usize| {
            let at = i * CLOUD_VERTEX_SIZE + k * 4;
            f32::from_le_bytes(v[at..at + 4].try_into().unwrap())
        };
        for i in 0..CLOUD_VERTS as usize {
            assert!((0..3).all(|k| (-1.0..=1.0).contains(&f(i, k))));
        }
        // The six vertices of a sprite share one position and take the corners 0, 1, 2, 2, 1, 3.
        let uv = |i: usize| (f(i, 3), f(i, 4));
        assert!((0..3).all(|k| f(0, k) == f(5, k)));
        assert_eq!(
            [0, 1, 2, 3, 4, 5].map(uv),
            [(0., 0.), (0., 1.), (1., 0.), (1., 0.), (0., 1.), (1., 1.)]
        );
    }

    #[test]
    fn a_cloud_of_round_sprites_or_no_motion_scales_the_view_axes_by_its_radius() {
        let c = Cloud {
            origin: Vec3::ZERO,
            axis: [Vec3::X, Vec3::Y, Vec3::Z],
            scale: 2.0,
            endpos: Vec3::Z,
            radius: [10.0, 10.0],
            color: [255, 128, 0, 255],
        };
        assert_eq!(c.matrix(&Mat4::IDENTITY), [10.0, 0.0, 0.0, 10.0]);
        let still = Cloud {
            endpos: Vec3::ZERO,
            radius: [10.0, 30.0],
            ..c
        };
        assert_eq!(still.matrix(&Mat4::IDENTITY), [10.0, 0.0, 0.0, 30.0]);
        assert_eq!(c.color_const(), [1.0, 128.0 / 255.0, 0.0, 1.0]);
        assert_eq!(c.world().transform_point3(Vec3::ONE), Vec3::splat(2.0));
    }

    #[test]
    fn a_moving_cloud_stretches_its_sprites_along_the_motion_seen_from_the_camera() {
        // View space is x right, y up: a cloud moving up the screen is tall and thin.
        let c = Cloud {
            origin: Vec3::ZERO,
            axis: [Vec3::X, Vec3::Y, Vec3::Z],
            scale: 1.0,
            endpos: Vec3::Y,
            radius: [10.0, 30.0],
            color: [255; 4],
        };
        let m = c.matrix(&Mat4::IDENTITY);
        assert!((m[0] - 10.0).abs() < 1e-4 && m[1].abs() < 1e-4, "{m:?}");
        assert!(m[2].abs() < 1e-4 && (m[3] - 30.0).abs() < 1e-4, "{m:?}");
    }
}
