// SPDX-License-Identifier: GPL-3.0-only
// The laser's reach, fade and widening follow KisakCOD (GPL-3.0): cgame/cg_laser.cpp (CG_Laser_Add, CG_Laser_Add_Core),
// cgame/cg_ents.cpp (the script model's laser) and the cg_laser* dvars of cgame_mp/cg_main_mp.cpp; the one-segment
// unwiggled beam is EffectsCore/fx_beam.cpp (FX_Beam_GenerateVerts); copyright holders of the Call of Duty 4 source
// reconstruction and its contributors.
//! Laser sights (`laseron`): a thin beam from a model's `tag_laser` (or `tag_flash`) along its forward axis to the
//! first thing it strikes within [`RANGE`], white at the source and fading with the distance it covers, widening the
//! farther its end is from the viewer. Clients draw a script model's laser only through night vision.

use glam::Vec3;
use render::{DynMesh, DynVertex};
use std::sync::Arc;

use assets::zone::gfx::Material;

/// `cg_laserRange`: how far a laser reaches.
pub const RANGE: f32 = 1500.0;
/// `cg_laserEndOffset`: how far short of what it strikes the beam ends.
const END_OFFSET: f32 = 0.5;
/// `cg_laserRadius`: the beam's half width at its source.
const RADIUS: f32 = 0.8;
/// `cg_laserFlarePct`: the percentage the beam's end widens per unit of distance from the viewer.
const FLARE_PCT: f32 = 0.2;

/// One laser this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Beam {
    pub begin: Vec3,
    pub end: Vec3,
    /// The end's opacity, 0..=255: the share of [`RANGE`] the beam did not cover.
    pub end_alpha: u8,
}

impl Beam {
    /// The laser from `origin` along the unit `forward` whose trace over [`RANGE`] stopped at `fraction`.
    pub fn new(origin: Vec3, forward: Vec3, fraction: f32) -> Self {
        let length = fraction * RANGE;
        Self {
            begin: origin,
            end: origin + forward * (length - END_OFFSET),
            end_alpha: ((1.0 - fraction) * 255.0).clamp(0.0, 255.0) as u8,
        }
    }
}

/// The beams as one camera-facing quad each, seen from `eye`.
pub fn mesh(material: &Arc<Material>, beams: &[Beam], eye: Vec3) -> Option<DynMesh> {
    let mut mesh = DynMesh::new(material.clone());
    for b in beams {
        let Some(dir) = (b.end - b.begin).try_normalize() else {
            continue;
        };
        let Some(side) = dir.cross((b.begin + b.end) * 0.5 - eye).try_normalize() else {
            continue;
        };
        let end_radius = RADIUS * (eye.distance(b.end) * FLARE_PCT * 0.01 + 1.0);
        let vertex = |at: Vec3, s: f32, radius: f32, alpha: u8, v: f32| DynVertex {
            pos: (at + side * (radius * s)).to_array(),
            color: [255, 255, 255, alpha],
            uv: [(s + 1.0) * 0.5, v],
            normal: (-dir.cross(side)).to_array(),
            tangent: dir.to_array(),
        };
        mesh.push_quad([
            vertex(b.begin, -1.0, RADIUS, 255, 0.0),
            vertex(b.begin, 1.0, RADIUS, 255, 0.0),
            vertex(b.end, 1.0, end_radius, b.end_alpha, 1.0),
            vertex(b.end, -1.0, end_radius, b.end_alpha, 1.0),
        ]);
    }
    (!mesh.verts.is_empty()).then_some(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_laser_stops_short_of_what_it_strikes_and_fades_with_its_reach() {
        let b = Beam::new(Vec3::ZERO, Vec3::X, 0.25);
        assert!((b.end.x - (375.0 - END_OFFSET)).abs() < 1e-3, "{b:?}");
        assert_eq!(b.end_alpha, 191);
        assert_eq!(Beam::new(Vec3::ZERO, Vec3::X, 1.0).end_alpha, 0);
    }
}
