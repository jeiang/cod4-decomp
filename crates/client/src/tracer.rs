// SPDX-License-Identifier: GPL-3.0-only
// Tracer flight and the beam's shape follow KisakCOD (GPL-3.0): cgame/cg_weapons.cpp (CG_SpawnTracer, CG_DrawTracer),
// cgame/cg_localents.cpp (CG_AddMovingTracer) and EffectsCore/fx_beam.cpp (FX_Beam_GenerateVerts); copyright holders
// of the Call of Duty 4 source reconstruction and its contributors.
//! Bullet tracers (`LE_MOVING_TRACER`): a short bright streak that flies from the muzzle to where the bullet struck at
//! [`SPEED`], drawn as a camera-facing beam that corkscrews slightly (`FX_Beam`).
//!
//! The client decides which shots are tracers and where each ends (`crate::effects`); this module is the flight and
//! the geometry.

use glam::Vec3;
use render::{DynMesh, DynVertex};
use std::sync::Arc;

use assets::zone::gfx::Material;

/// `cg_tracerSpeed`: units per second.
pub const SPEED: f32 = 7500.0;
/// `cg_tracerlength`: the visible streak.
pub const LENGTH: f32 = 160.0;
/// `cg_tracerwidth`: the beam's half width at both ends.
pub const WIDTH: f32 = 4.0;
/// `cg_tracerScrewDist`: how far a tracer goes in one turn of its corkscrew.
const SCREW_DIST: f32 = 100.0;
/// `cg_tracerScrewRadius`.
const SCREW_RADIUS: f32 = 0.5;
/// `FX_Beam_Add` keeps 96 beams a frame.
const MAX_BEAMS: usize = 96;
/// The corkscrew's eight steps (`wiggle`): a unit circle.
const WIGGLE: [[f32; 2]; 8] = [
    [0.0, 1.0],
    [0.71, 0.71],
    [1.0, 0.0],
    [0.71, -0.71],
    [0.0, -1.0],
    [-0.71, -0.71],
    [-1.0, 0.0],
    [-0.71, 0.71],
];

/// What the player has set about tracers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cvars {
    /// `cg_tracerchance`: the probability another player's bullet is a tracer.
    pub chance: f32,
    /// `cg_firstPersonTracerChance`: the same for the player's own.
    pub own_chance: f32,
    /// `cg_tracerScale`, `cg_tracerScaleMinDist`, `cg_tracerScaleDistRange`: how much wider a far tracer is drawn, from
    /// what distance, and over what further distance it reaches the full scale.
    pub scale: f32,
    pub scale_min_dist: f32,
    pub scale_dist_range: f32,
}

impl Default for Cvars {
    fn default() -> Self {
        Self {
            chance: 0.2,
            own_chance: 0.5,
            scale: 1.0,
            scale_min_dist: 5000.0,
            scale_dist_range: 25000.0,
        }
    }
}

impl Cvars {
    /// `ScaleTracer` for one end of a beam `dist` from the eye: its half width (`CalcTracerFinalScale`).
    fn width(&self, dist: f32) -> f32 {
        if self.scale == 1.0 {
            return WIDTH;
        }
        let d = if self.scale_min_dist != 0.0 {
            dist - self.scale_min_dist
        } else {
            dist
        };
        if d <= 0.0 {
            return WIDTH;
        }
        let k = if self.scale_dist_range <= 0.0 {
            self.scale
        } else {
            let v = self.scale * (d / self.scale_dist_range);
            if v < 1.0 { 1.0 } else { v.min(self.scale) }
        };
        WIDTH * k
    }
}

struct Tracer {
    start: Vec3,
    dir: Vec3,
    /// How far it flies before it is gone: the distance to where the bullet struck.
    dist: f32,
    born: i32,
}

/// The tracers in flight.
#[derive(Default)]
pub struct Tracers {
    live: Vec<Tracer>,
}

impl Tracers {
    /// Launches a tracer from `start` toward `end` at `now` (server milliseconds).
    pub fn spawn(&mut self, now: i32, start: Vec3, end: Vec3) {
        let d = end - start;
        let dist = d.length();
        if dist < 1.0 || !dist.is_finite() {
            return;
        }
        if self.live.len() >= MAX_BEAMS {
            self.live.remove(0);
        }
        self.live.push(Tracer {
            start,
            dir: d / dist,
            dist,
            born: now,
        });
    }

    /// The streak of every tracer in flight at `now`, as (tail, head): the tracers that reached their end are gone
    /// (`CG_AddLocalEntityTracerBeams`). A streak is [`LENGTH`] long, or shorter as it reaches the end.
    pub fn beams(&mut self, now: i32) -> Vec<(Vec3, Vec3)> {
        self.live
            .retain(|t| now >= t.born && (now - t.born) as f32 * SPEED < t.dist * 1000.0);
        self.live
            .iter()
            .map(|t| {
                let flown = (now - t.born) as f32 * SPEED / 1000.0;
                let tail = t.start + t.dir * flown;
                (tail, tail + t.dir * LENGTH.min(t.dist - flown))
            })
            .collect()
    }

    /// How many tracers are in flight.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.live.len()
    }
}

/// The beam from `begin` to `end` as camera-facing quads seen from `eye`, a quad per eighth of [`SCREW_DIST`] with the
/// corkscrew's wiggle on each joint (`FX_Beam_GenerateVerts`). The texture runs across the beam in `u` and along it in
/// `v`.
pub fn beam_mesh(
    material: &Arc<Material>,
    beams: &[(Vec3, Vec3)],
    eye: Vec3,
    cvars: &Cvars,
) -> Option<DynMesh> {
    let mut mesh = DynMesh::new(material.clone());
    for &(begin, end) in beams {
        let axis = end - begin;
        let len = axis.length();
        let Some(dir) = axis.try_normalize() else {
            continue;
        };
        // Across the beam, facing the viewer; the other perpendicular is for the corkscrew.
        let Some(side) = dir.cross((begin + end) * 0.5 - eye).try_normalize() else {
            continue;
        };
        let other = side.cross(dir);
        let segments = ((len * 8.0 / SCREW_DIST) as usize).max(1);
        let (w0, w1) = (
            cvars.width(eye.distance(begin)),
            cvars.width(eye.distance(end)),
        );
        let joint = |i: usize| {
            let a = i as f32 / segments as f32;
            let w = WIGGLE[i % 8];
            let centre =
                begin.lerp(end, a) + side * (SCREW_RADIUS * w[0]) + other * (SCREW_RADIUS * w[1]);
            let width = w0 + (w1 - w0) * a;
            let vertex = |s: f32, u: f32| DynVertex {
                pos: (centre + side * (width * s)).to_array(),
                color: [255; 4],
                uv: [u, a],
                normal: (-dir.cross(side)).to_array(),
                tangent: dir.to_array(),
            };
            [vertex(-1.0, 0.0), vertex(1.0, 1.0)]
        };
        let mut prev = joint(0);
        for i in 1..=segments {
            let next = joint(i);
            mesh.push_quad([prev[0], prev[1], next[1], next[0]]);
            prev = next;
        }
    }
    (!mesh.verts.is_empty()).then_some(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tracer_flies_to_the_end_and_its_streak_shortens_there() {
        let mut t = Tracers::default();
        t.spawn(1000, Vec3::ZERO, Vec3::new(1000.0, 0.0, 0.0));
        // 40 ms is 300 units on.
        let b = t.beams(1040);
        assert_eq!(b.len(), 1);
        assert!((b[0].0.x - 300.0).abs() < 0.01, "{:?}", b[0]);
        assert!((b[0].1.x - 460.0).abs() < 0.01, "a full streak: {:?}", b[0]);
        // 120 ms is 900 units on: only 100 left to fly.
        let b = t.beams(1120);
        assert!(
            (b[0].1.x - 1000.0).abs() < 0.01,
            "clipped at the end: {:?}",
            b[0]
        );
        // 140 ms is past the end.
        assert!(t.beams(1140).is_empty());
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn a_tracer_does_not_exist_before_it_is_launched() {
        let mut t = Tracers::default();
        t.spawn(1000, Vec3::ZERO, Vec3::X * 500.0);
        assert!(t.beams(900).is_empty());
    }

    #[test]
    fn far_tracers_are_drawn_wider_only_when_scaled() {
        let c = Cvars::default();
        assert_eq!(c.width(20000.0), WIDTH);
        let c = Cvars {
            scale: 4.0,
            ..Cvars::default()
        };
        assert_eq!(c.width(1000.0), WIDTH, "inside the minimum distance");
        assert_eq!(c.width(5000.0 + 25000.0), WIDTH * 4.0, "at the full scale");
        let mid = c.width(5000.0 + 12500.0);
        assert!(mid > WIDTH && mid < WIDTH * 4.0, "{mid}");
    }

    #[test]
    fn nothing_flies_from_a_point_to_itself() {
        let mut t = Tracers::default();
        t.spawn(0, Vec3::ONE, Vec3::ONE);
        assert_eq!(t.len(), 0);
    }
}
