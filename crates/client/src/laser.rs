// SPDX-License-Identifier: GPL-3.0-only
// The laser's reach, fade, widening and post light follow KisakCOD (GPL-3.0): cgame/cg_laser.cpp (CG_Laser_Add,
// CG_Laser_Add_Core), cgame/cg_ents.cpp (the script model's laser) and the cg_laser* dvars of cgame_mp/cg_main_mp.cpp;
// the one-segment unwiggled beam is EffectsCore/fx_beam.cpp (FX_Beam_GenerateVerts); copyright holders of the Call of
// Duty 4 source reconstruction and its contributors.
//! Laser sights (`laseron`): a thin beam from a model's `tag_laser` (or `tag_flash`) along its forward axis to the
//! first thing it strikes within [`RANGE`], white at the source and fading with the distance it covers, widening the
//! farther its end is from the viewer. With `cg_laserLight` on, each laser also casts a post light (`gfx_laser_light`)
//! along its last stretch that lights the surface it strikes. Clients draw a script model's laser only through night
//! vision.

use glam::Vec3;
use render::{DynMesh, DynVertex, PostLight, dynmesh::MAX_POST_LIGHTS};
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
/// `cg_laserLightBeginOffset`: how far from the source the post light starts.
const LIGHT_BEGIN_OFFSET: f32 = 13.0;
/// `cg_laserLightEndOffset`: how far short of what the laser strikes the post light ends (negative: past it).
const LIGHT_END_OFFSET: f32 = -3.0;
/// `cg_laserLightBodyTweak`: how much farther the post light reaches into a player or actor it strikes.
const LIGHT_BODY_TWEAK: f32 = 15.0;
/// `cg_laserLightRadius`: the post light's radius before it widens with distance like the beam's end.
const LIGHT_RADIUS: f32 = 3.0;
/// The shortest post light: a shorter one is stretched to this about its middle.
const LIGHT_MIN_LENGTH: f32 = 4.0;

/// One laser this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Beam {
    pub begin: Vec3,
    pub end: Vec3,
    /// The end's opacity, 0..=255: the share of [`RANGE`] the beam did not cover.
    pub end_alpha: u8,
    /// The unit direction from the source.
    pub forward: Vec3,
    /// How far the laser reached before it struck something.
    pub length: f32,
    /// What it struck is a player or an actor: the post light reaches into it.
    pub on_body: bool,
}

impl Beam {
    /// The laser from `origin` along the unit `forward` whose trace over [`RANGE`] stopped at `fraction`, in what has
    /// `contents`.
    pub fn new(origin: Vec3, forward: Vec3, fraction: f32, contents: i32) -> Self {
        let length = fraction * RANGE;
        Self {
            begin: origin,
            end: origin + forward * (length - END_OFFSET),
            end_alpha: ((1.0 - fraction) * 255.0).clamp(0.0, 255.0) as u8,
            forward,
            length,
            on_body: contents & (sim::contents::PLAYER | sim::contents::ACTOR) != 0,
        }
    }

    /// How much the beam's end (and the post light) widens seen from `eye`.
    fn end_widen(&self, eye: Vec3) -> f32 {
        eye.distance(self.end) * FLARE_PCT * 0.01 + 1.0
    }

    /// The post light the laser casts seen from `eye`: from [`LIGHT_BEGIN_OFFSET`] along it to just past what it
    /// strikes (`CG_Laser_Add_Core`).
    pub fn light(&self, eye: Vec3) -> PostLight {
        let mut begin = LIGHT_BEGIN_OFFSET;
        let mut end = self.length - LIGHT_END_OFFSET;
        if self.on_body {
            end += LIGHT_BODY_TWEAK;
        }
        if end - begin < LIGHT_MIN_LENGTH {
            let mid = (begin + end) * 0.5;
            (begin, end) = (mid - LIGHT_MIN_LENGTH * 0.5, mid + LIGHT_MIN_LENGTH * 0.5);
        }
        PostLight {
            begin: self.begin + self.forward * begin,
            end: self.begin + self.forward * end,
            radius: LIGHT_RADIUS * self.end_widen(eye),
            color: [255; 4],
        }
    }
}

/// The post lights of the beams seen from `eye`, one mesh each (each has its own code mesh arguments), at most
/// [`MAX_POST_LIGHTS`].
pub fn light_meshes(material: &Arc<Material>, beams: &[Beam], eye: Vec3) -> Vec<DynMesh> {
    beams
        .iter()
        .take(MAX_POST_LIGHTS)
        .filter_map(|b| DynMesh::post_light(material.clone(), b.light(eye)))
        .collect()
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
        let end_radius = RADIUS * b.end_widen(eye);
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
        let b = Beam::new(Vec3::ZERO, Vec3::X, 0.25, 0);
        assert!((b.end.x - (375.0 - END_OFFSET)).abs() < 1e-3, "{b:?}");
        assert_eq!(b.end_alpha, 191);
        assert_eq!(Beam::new(Vec3::ZERO, Vec3::X, 1.0, 0).end_alpha, 0);
    }

    #[test]
    fn the_post_light_runs_from_past_the_source_to_past_what_the_laser_strikes() {
        let b = Beam::new(Vec3::ZERO, Vec3::X, 0.1, sim::contents::SOLID);
        // Seen from the struck point's side: no widening beyond the distance to the end.
        let eye = b.end;
        let l = b.light(eye);
        assert_eq!(l.begin, Vec3::new(13.0, 0.0, 0.0));
        assert_eq!(l.end, Vec3::new(153.0, 0.0, 0.0));
        assert_eq!(l.radius, 3.0);
        assert_eq!(l.color, [255; 4]);
        // A player struck takes the light deeper; a far viewer sees it wider.
        let body = Beam::new(Vec3::ZERO, Vec3::X, 0.1, sim::contents::PLAYER);
        assert_eq!(body.light(eye).end, Vec3::new(168.0, 0.0, 0.0));
        let far = b.light(b.end + Vec3::Y * 500.0);
        assert!((far.radius - 6.0).abs() < 1e-4, "{far:?}");
        // A laser striking right at its source still lights a short stretch about the middle.
        let close = Beam::new(Vec3::ZERO, Vec3::X, 0.0, 0).light(Vec3::ZERO);
        assert_eq!((close.begin.x, close.end.x), (6.0, 10.0));
    }
}
