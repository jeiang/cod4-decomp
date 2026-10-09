// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (cgame_mp/cg_draw_mp.cpp CG_ScanForCrosshairEntity, CG_CanSeeFriendlyHead; cgame_mp/cg_players_mp.cpp CG_AddAllPlayerSpriteDrawSurfs; GPL-3.0, copyright the KisakCOD contributors and Activision).
//! What the world says about the other players for the names and icons over their heads: where each head is, whether
//! the eye can see it, the icon the scripts gave it, and who the crosshair is on. The HUD turns this into fades and
//! pictures ([`crate::hud::names`]).

use super::NetPlay;
use crate::hud::{NameScan, NearPlayer};
use glam::Vec3;
use server::netsv::eflags;
use sim::cm::{Collide, ENTITYNUM_NONE};
use sim::contents;

/// `MASK_PLAYER_VISIBILITY`: what blocks a look at a player.
const VISIBILITY: i32 = contents::SOLID
    | contents::AI_NOSIGHT
    | contents::CLIPSHOT
    | contents::VEHICLE
    | contents::PLAYER;
/// How far the crosshair looks for a player (`CG_ScanForCrosshairEntity`).
const CROSSHAIR_REACH: f32 = 8192.0;
/// The head bone's height above the feet of a body with no skeleton yet.
const HEAD_HEIGHT: f32 = 72.0;
/// The client numbers a trace can name as a player.
const MAX_CLIENTS: u16 = 64;

impl NetPlay {
    /// The players around the viewer at `eye` looking along `(pitch, yaw)` degrees, for this frame's HUD. `st` is
    /// the shell time of the frame, `own` the viewer.
    pub(super) fn scan_names(&mut self, st: i32, own: u16, eye: Vec3, (pitch, yaw): (f32, f32)) {
        let mut scan = NameScan {
            flashed: self.look.flashed(st),
            ..NameScan::default()
        };
        let Some(snap) = self.net.latest() else {
            self.scan = scan;
            return;
        };
        let killcam = snap.killcam();
        let viewer = snap.own();
        let weapon = snap.ps.weapon as u16;
        let ents = self
            .net
            .snaps
            .interpolate(st - net::view::INTERP_DELAY_MS, Some(own));
        let Some(ui) = self.net.ui_ref() else {
            self.scan = scan;
            return;
        };
        for e in ents
            .iter()
            .filter(|e| e.etype == net::entity::etype::PLAYER)
        {
            let you = killcam && e.client == viewer;
            if e.eflags & eflags::DEAD != 0 && !you {
                continue;
            }
            let head = self
                .remotes
                .get(&e.client)
                .and_then(|r| r.player.head_pos(e.origin))
                .unwrap_or([e.origin[0], e.origin[1], e.origin[2] + HEAD_HEIGHT]);
            let clear = {
                let t = self.boxes.world().trace(
                    eye.to_array(),
                    head,
                    [0.0; 3],
                    [0.0; 3],
                    own,
                    VISIBILITY,
                );
                t.hit_id == ENTITYNUM_NONE || t.hit_id == e.client
            };
            scan.near.push(NearPlayer {
                client: e.client,
                head,
                clear,
                icon: Some(e.head_icon)
                    .filter(|i| *i != 0)
                    .map(|i| (ui.material(i).to_owned(), e.head_icon_team))
                    .filter(|(name, _)| !name.is_empty()),
                talking: e.eflags & eflags::TALKING != 0,
                interrupted: e.eflags & eflags::CONNECTION_INTERRUPTED != 0,
                you,
            });
        }
        if !scan.flashed {
            let team_of = |c: u16| ui.client(c).map_or(0, |i| i.team);
            let range = self
                .lib
                .content
                .weapon(self.weapons.name(weapon))
                .map_or(0.0, |d| d.enemy_crosshair_range);
            let (p, y) = (pitch.to_radians(), yaw.to_radians());
            let dir = Vec3::new(p.cos() * y.cos(), p.cos() * y.sin(), -p.sin());
            let end = eye + dir * CROSSHAIR_REACH;
            let hit = self
                .boxes
                .world()
                .trace(
                    eye.to_array(),
                    end.to_array(),
                    [0.0; 3],
                    [0.0; 3],
                    own,
                    VISIBILITY,
                )
                .hit_id;
            if hit < MAX_CLIENTS
                && let Some(e) = ents
                    .iter()
                    .find(|e| e.client == hit && e.etype == net::entity::etype::PLAYER)
            {
                let mine = team_of(viewer);
                // A spectator sees every name; a player on a side sees friends always and enemies in the weapon's
                // enemy range.
                let friend = mine != 0 && team_of(hit) == mine;
                let in_range = (Vec3::from(e.origin) - eye).length_squared() <= range * range;
                if mine == 3 || friend || in_range {
                    scan.crosshair = Some(hit);
                }
            }
        }
        self.scan = scan;
    }
}
