// SPDX-License-Identifier: GPL-3.0-only
//! Melee aim assist for the playing client: the enemies the knife could lunge at, and what the swing does to the aim
//! and the command ([`crate::automelee`] has the rules).

use super::NetPlay;
use crate::automelee::{self, Target, View};
use glam::Vec3;
use server::netsv::eflags;
use sim::cm::{Collide, ENTITYNUM_NONE};
use sim::contents;
use sim::pm::{PLAYER_MAXS, pmf};

/// What blocks the view of an enemy.
const SIGHT: i32 = contents::SOLID | contents::CLIPSHOT;
/// How tall a crouched and a prone body are.
const CROUCHED: f32 = 50.0;
const PRONE: f32 = 30.0;

impl NetPlay {
    /// Tells the aim assist how wide the view is: the tangents of its half angles.
    pub fn set_projection(&mut self, tan_half_fov: [f32; 2]) {
        self.tan_half_fov = tan_half_fov;
    }

    /// The enemies in view of the player, nearest the crosshair first. `eye` and `feet` are the player's.
    fn melee_targets(&self, st: i32, own: u16, eye: Vec3, feet: Vec3) -> Vec<automelee::Screen> {
        let (Some(snap), Some(ui)) = (self.net.latest(), self.net.ui_ref()) else {
            return Vec::new();
        };
        let team = |c: u16| ui.client(c).map_or(0, |i| i.team);
        let mine = team(snap.own());
        let targets: Vec<Target> = self
            .net
            .snaps
            .interpolate(st - net::view::INTERP_DELAY_MS, Some(own))
            .iter()
            .filter(|e| e.etype == net::entity::etype::PLAYER && e.eflags & eflags::DEAD == 0)
            // Everyone is an enemy but the player's own team (and everyone is, in free for all).
            .filter(|e| mine == 0 || team(e.client) != mine)
            .map(|e| Target {
                number: e.number,
                origin: Vec3::from(e.origin),
                height: if e.pm_flags & pmf::PRONE != 0 {
                    PRONE
                } else if e.pm_flags & pmf::DUCKED != 0 {
                    CROUCHED
                } else {
                    PLAYER_MAXS[2]
                },
            })
            .collect();
        let view = View {
            eye,
            feet,
            angles: [self.angles[0], self.angles[1], 0.0],
            tan_half_fov: self.tan_half_fov,
        };
        let mut seen = automelee::screen_targets(&view, &targets);
        // Only what the eye can see (`AimTarget_IsTargetVisible`).
        seen.retain(|s| {
            self.boxes
                .world()
                .trace(
                    eye.to_array(),
                    s.aim.to_array(),
                    [0.0; 3],
                    [0.0; 3],
                    ENTITYNUM_NONE,
                    SIGHT,
                )
                .fraction
                >= 1.0
        });
        seen
    }

    /// The assist of one frame: sets the lunge the next command carries (`melee` is whether the knife is being
    /// pressed) and pulls the aim onto the target while the swing winds up.
    pub(super) fn melee_assist(&mut self, st: i32, own: u16, dt: f32, melee: bool) {
        self.melee_charge = (0.0, 0);
        let Some((eye, feet)) = self.last_eye.map(|e| (e, Vec3::from(self.c.end))) else {
            return;
        };
        let prone = self
            .net
            .latest()
            .is_some_and(|s| s.ps.pm_flags & pmf::PRONE != 0);
        if !melee && !self.meleeing {
            self.automelee = automelee::AutoMelee::default();
            return;
        }
        let screens = self.melee_targets(st, own, eye, feet);
        if melee && !prone {
            self.melee_charge = automelee::charge(&screens, eye, self.weapon_params.melee_range);
        }
        let pull = self.automelee.step(
            self.meleeing,
            &screens,
            eye,
            [self.angles[0], self.angles[1]],
            dt,
        );
        self.angles[0] = (self.angles[0] + pull[0]).clamp(self.pitch_limits.0, self.pitch_limits.1);
        self.angles[1] += pull[1];
    }
}
