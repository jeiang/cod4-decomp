// SPDX-License-Identifier: GPL-3.0-or-later
//! Dynamic (skinned) models: the instances the client hands the renderer each frame and the CPU skinning of their
//! surfaces.
//!
//! The original skins on the CPU too (`R_SkinXModelCmd` writes `GfxPackedVertex`es into a per-frame cache that the
//! ordinary model techniques then draw), so there are no skinned-vertex techsets or bone constants to translate: a
//! skinned surface is drawn with the same `VertexKind::Model` pipelines as a static model, from a vertex buffer
//! rewritten every frame. The skin of a bone is `pose * inverse(base)`: the model's bind pose (`base_mat`) takes a
//! vertex into bone space and the animated bone matrix places it. An XSurface is skinned one of two ways:
//!
//! * *rigid* (`deformed == false`): the vertices are runs (`vert_list`), each bound to one bone;
//! * *blended* (`deformed == true`): the first `blend_counts[0]` vertices have one bone, the next `blend_counts[1]`
//!   two, then three and four. `blends` holds, per vertex, the first bone's byte offset into the skin matrices (64
//!   bytes per bone, so `offset / 64` is the bone), then for every further bone its offset and a 16-bit weight; the
//!   first bone takes the weight left over.
//!
//! Positions blend over all bones; the normal and tangent follow the first bone only, as in the original.

use assets::zone::xmodel::{Surface, XModel};
use glam::{Affine3A, Quat, Vec3, Vec3A};
use sim::skel::BoneMat;
use std::sync::Arc;

/// Bytes of one packed model vertex (`GfxPackedVertex`).
pub const VERTEX_SIZE: usize = 32;

/// Which projection and shadowing a model gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelKind {
    /// A model in the world: a player body, a dropped weapon. Casts sun shadows.
    World,
    /// The first-person weapon and hands: its own field of view, drawn in a depth range in front of the world, no
    /// shadow.
    ViewModel,
}

/// One skinned model to draw this frame.
#[derive(Clone)]
pub struct ModelInstance {
    pub model: Arc<XModel>,
    /// Game-space position of the model's origin.
    pub origin: [f32; 3],
    /// Pitch, yaw, roll in degrees, as the engine's `angles`.
    pub angles: [f32; 3],
    /// One matrix per bone of `model` in model space (what `sim::skel::Rig::pose` yields for this model's slice of the
    /// rig). Missing bones keep the bind pose.
    pub bones: Vec<BoneMat>,
    pub kind: ModelKind,
    /// LOD to draw; `None` picks by distance to the eye.
    pub lod: Option<usize>,
    /// Bones whose surfaces are hidden (`DObjSetHidePartBits`), most significant bit first like the surfaces' part
    /// bits.
    pub hidden_parts: [u32; 4],
    /// Where the model-lighting volume is sampled.
    pub light_origin: [f32; 3],
}

impl ModelInstance {
    pub fn new(model: Arc<XModel>, kind: ModelKind) -> Self {
        Self {
            model,
            origin: [0.0; 3],
            angles: [0.0; 3],
            bones: Vec::new(),
            kind,
            lod: None,
            hidden_parts: [0; 4],
            light_origin: [0.0; 3],
        }
    }

    /// World matrix: the model's origin and angles.
    pub fn world_matrix(&self) -> glam::Mat4 {
        let q = sim::skel::quat::from_angles(&self.angles);
        glam::Mat4::from_rotation_translation(
            Quat::from_xyzw(q[0], q[1], q[2], q[3]),
            Vec3::from(self.origin),
        )
    }
}

fn pose_affine(m: &BoneMat) -> Affine3A {
    Affine3A::from_rotation_translation(
        Quat::from_xyzw(m.quat[0], m.quat[1], m.quat[2], m.quat[3]).normalize(),
        Vec3::from(m.trans),
    )
}

/// The skin matrix of every bone: `pose * inverse(bind)`. A bone without a pose keeps the identity.
pub fn skin_matrices(model: &XModel, bones: &[BoneMat]) -> Vec<Affine3A> {
    model
        .base_mat
        .iter()
        .enumerate()
        .map(|(i, b)| match bones.get(i) {
            Some(p) => {
                let base = Affine3A::from_rotation_translation(
                    Quat::from_xyzw(b.quat[0], b.quat[1], b.quat[2], b.quat[3]).normalize(),
                    Vec3::from(b.trans),
                );
                pose_affine(p) * base.inverse()
            }
            None => Affine3A::IDENTITY,
        })
        .collect()
}

fn unpack_unit(b: &[u8]) -> Vec3A {
    let scale = (f32::from(b[3]) + 192.0) / 32385.0;
    Vec3A::new(
        (f32::from(b[0]) - 127.0) * scale,
        (f32::from(b[1]) - 127.0) * scale,
        (f32::from(b[2]) - 127.0) * scale,
    )
}

pub(crate) fn pack_unit(v: Vec3A, out: &mut [u8]) {
    for (o, c) in out.iter_mut().zip(v.to_array()) {
        *o = (c * 127.0 + 127.5).clamp(0.0, 255.0) as u8;
    }
    out[3] = 63;
}

/// Copies vertex `i` of `src` to `dst` with its position moved to `pos` and its basis rotated by `m`.
fn write_vertex(src: &[u8], dst: &mut [u8], pos: Vec3A, m: &Affine3A) {
    dst.copy_from_slice(src);
    for (k, c) in pos.to_array().into_iter().enumerate() {
        dst[4 * k..4 * k + 4].copy_from_slice(&c.to_le_bytes());
    }
    pack_unit(
        m.transform_vector3a(unpack_unit(&src[24..28])),
        &mut dst[24..28],
    );
    pack_unit(
        m.transform_vector3a(unpack_unit(&src[28..32])),
        &mut dst[28..32],
    );
}

fn position(v: &[u8]) -> Vec3A {
    let f = |i: usize| f32::from_le_bytes([v[i], v[i + 1], v[i + 2], v[i + 3]]);
    Vec3A::new(f(0), f(4), f(8))
}

/// Skins a blended surface's vertices (`verts` packed vertices, `blends` and `counts` as in the XSurface). Appends
/// to `out`. Bone offsets outside `mats` read as the identity.
pub fn skin_blended(
    verts: &[u8],
    blends: &[u16],
    counts: [i16; 4],
    mats: &[Affine3A],
    out: &mut Vec<u8>,
) {
    let mat = |off: u16| {
        mats.get(usize::from(off) / 64)
            .copied()
            .unwrap_or(Affine3A::IDENTITY)
    };
    let start = out.len();
    out.resize(start + verts.len(), 0);
    let mut at = 0usize;
    let mut v = 0usize;
    for (extra, &count) in counts.iter().enumerate() {
        for _ in 0..count.max(0) {
            let stride = 1 + 2 * extra;
            let Some(b) = blends.get(at..at + stride) else {
                return;
            };
            let Some(src) = verts.get(v * VERTEX_SIZE..(v + 1) * VERTEX_SIZE) else {
                return;
            };
            let first = mat(b[0]);
            let p = position(src);
            let mut pos = Vec3A::ZERO;
            let mut left = 1.0f32;
            for k in 0..extra {
                let w = f32::from(b[2 + 2 * k]) / 65536.0;
                pos += mat(b[1 + 2 * k]).transform_point3a(p) * w;
                left -= w;
            }
            pos += first.transform_point3a(p) * left;
            let dst = &mut out[start + v * VERTEX_SIZE..start + (v + 1) * VERTEX_SIZE];
            write_vertex(src, dst, pos, &first);
            at += stride;
            v += 1;
        }
    }
}

/// Skins a rigid surface: each run of vertices follows one bone. Appends to `out`.
pub fn skin_rigid(
    verts: &[u8],
    runs: impl Iterator<Item = (u16, u16)>,
    mats: &[Affine3A],
    out: &mut Vec<u8>,
) {
    let start = out.len();
    out.resize(start + verts.len(), 0);
    let mut v = 0usize;
    for (bone_offset, count) in runs {
        let m = mats
            .get(usize::from(bone_offset) / 64)
            .copied()
            .unwrap_or(Affine3A::IDENTITY);
        for _ in 0..count {
            let Some(src) = verts.get(v * VERTEX_SIZE..(v + 1) * VERTEX_SIZE) else {
                return;
            };
            let pos = m.transform_point3a(position(src));
            let dst = &mut out[start + v * VERTEX_SIZE..start + (v + 1) * VERTEX_SIZE];
            write_vertex(src, dst, pos, &m);
            v += 1;
        }
    }
}

/// Skins `surf`, appending `vert_count * 32` bytes to `out`.
pub fn skin_surface(surf: &Surface, mats: &[Affine3A], out: &mut Vec<u8>) {
    if surf.deformed {
        skin_blended(&surf.verts, &surf.blends, surf.blend_counts, mats, out);
    } else {
        skin_rigid(
            &surf.verts,
            surf.vert_list.iter().map(|l| (l.bone_offset, l.vert_count)),
            mats,
            out,
        );
    }
}

/// Whether a surface is hidden by `hidden` (any bone it needs is hidden).
pub fn is_hidden(surf: &Surface, hidden: &[u32; 4]) -> bool {
    surf.part_bits
        .iter()
        .zip(hidden)
        .any(|(s, h)| (*s as u32) & h != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vertex(p: [f32; 3]) -> Vec<u8> {
        let mut v = vec![0u8; VERTEX_SIZE];
        for (i, c) in p.iter().enumerate() {
            v[i * 4..i * 4 + 4].copy_from_slice(&c.to_le_bytes());
        }
        // normal +z, tangent +x, packed with scale 1/127.
        v[24..28].copy_from_slice(&[127, 127, 254, 63]);
        v[28..32].copy_from_slice(&[254, 127, 127, 63]);
        v[16..24].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        v
    }

    fn pos(v: &[u8]) -> [f32; 3] {
        position(v).to_array()
    }

    fn shift(x: f32) -> Affine3A {
        Affine3A::from_translation(Vec3::new(x, 0.0, 0.0))
    }

    #[test]
    fn bone_pose_times_inverse_bind_is_identity_at_rest() {
        let m = BoneMat {
            quat: [0.0, 0.0, 0.70710677, 0.70710677],
            trans: [3.0, 4.0, 5.0],
        };
        let base = Affine3A::from_rotation_translation(
            Quat::from_xyzw(m.quat[0], m.quat[1], m.quat[2], m.quat[3]),
            Vec3::from(m.trans),
        );
        let skin = pose_affine(&m) * base.inverse();
        assert!(skin.abs_diff_eq(Affine3A::IDENTITY, 1e-5));
    }

    #[test]
    fn rigid_runs_follow_their_own_bone_and_keep_colour_and_uv() {
        let mut verts = vertex([1.0, 0.0, 0.0]);
        verts.extend(vertex([0.0, 2.0, 0.0]));
        let mats = [shift(10.0), shift(-5.0)];
        let mut out = Vec::new();
        skin_rigid(&verts, [(0u16, 1u16), (64, 1)].into_iter(), &mats, &mut out);
        assert_eq!(pos(&out[0..]), [11.0, 0.0, 0.0]);
        assert_eq!(pos(&out[32..]), [-5.0, 2.0, 0.0]);
        assert_eq!(&out[16..24], &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn rotation_turns_the_normal_and_translation_does_not() {
        let verts = vertex([0.0; 3]);
        let rot = Affine3A::from_rotation_translation(
            Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            Vec3::new(9.0, 0.0, 0.0),
        );
        let mut out = Vec::new();
        skin_rigid(&verts, [(0u16, 1u16)].into_iter(), &[rot], &mut out);
        // +z rotated a quarter turn about y is +x.
        let n = unpack_unit(&out[24..28]);
        assert!((n - Vec3A::X).length() < 0.02, "{n:?}");
        assert_eq!(out[27], 63);
    }

    #[test]
    fn blends_weigh_bones_and_the_first_takes_the_rest() {
        // One 1-bone vertex, then one 2-bone vertex with half weight on bone 1.
        let mut verts = vertex([0.0; 3]);
        verts.extend(vertex([0.0; 3]));
        let blends = [64u16, 0, 128, 32768];
        let mats = [shift(0.0), shift(10.0), shift(20.0)];
        let mut out = Vec::new();
        skin_blended(&verts, &blends, [1, 1, 0, 0], &mats, &mut out);
        assert_eq!(pos(&out[0..]), [10.0, 0.0, 0.0]);
        // 0.5 * bone 2 (20) + 0.5 * bone 0 (0).
        assert_eq!(pos(&out[32..]), [10.0, 0.0, 0.0]);
        let blends3 = [0u16, 64, 16384, 128, 16384];
        let mut out = Vec::new();
        skin_blended(&vertex([0.0; 3]), &blends3, [0, 0, 1, 0], &mats, &mut out);
        // 0.25 * 10 + 0.25 * 20 + 0.5 * 0.
        assert_eq!(pos(&out), [7.5, 0.0, 0.0]);
    }

    #[test]
    fn truncated_blend_data_does_not_panic() {
        let mut out = Vec::new();
        skin_blended(&vertex([0.0; 3]), &[], [1, 0, 0, 0], &[], &mut out);
        assert_eq!(out.len(), VERTEX_SIZE);
    }
}
