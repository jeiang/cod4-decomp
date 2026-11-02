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
    pub(super) fn scan_names(
        &mut self,
        st: i32,
        own: u16,
        eye: Vec3,
        (pitch, yaw): (f32, f32),
        dead: bool,
    ) {
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
            .interpolate_with_actors(st - net::view::INTERP_DELAY_MS, Some(own));
        let Some(ui) = self.net.ui_ref() else {
            self.scan = scan;
            return;
        };
        let (p, y) = (pitch.to_radians(), yaw.to_radians());
        let dir = Vec3::new(p.cos() * y.cos(), p.cos() * y.sin(), -p.sin());
        let team_of = |c: u16| ui.client(c).map_or(0, |i| i.team);
        let mine = team_of(viewer);
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
            let marked = e.head_icon != 0
                || e.eflags & (eflags::TALKING | eflags::CONNECTION_INTERRUPTED) != 0
                || you;
            // Only what could be drawn is traced to: teammates' names and the markers over anyone.
            let named = mine == 3 || (mine != 0 && team_of(e.client) == mine);
            let ahead = (Vec3::from(head) - eye).dot(dir) >= 0.0;
            let clear = (named || marked) && ahead && {
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
                ahead,
                clear,
                icon: (e.head_icon != 0).then_some((e.head_icon, e.head_icon_team)),
                talking: e.eflags & eflags::TALKING != 0,
                interrupted: e.eflags & eflags::CONNECTION_INTERRUPTED != 0,
                you,
            });
        }
        if !scan.flashed && !dead {
            let range = self
                .lib
                .content
                .weapon(self.weapons.name(weapon))
                .map_or(0.0, |d| d.enemy_crosshair_range);
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
                // `CG_ScanForCrosshairEntity` (KisakCOD `cg_draw_mp.cpp:324-402`; the same in the 1.7 binary): a
                // spectator names whoever is hit. A player on a side compares the team of the *previous* crosshair
                // player (`crosshairClientNum`) with their own: the same side marks a friend (bit 8), otherwise the
                // hit is an enemy, marked (0x10) only within the weapon's enemy range; out of it the scan stops and
                // the crosshair player stays as it was.
                if mine != 3 {
                    if mine != 0 && team_of(self.crosshair_client) == mine {
                        scan.weap_flags = sim::pm::wf::FRIEND_IN_CROSSHAIR;
                    } else if (Vec3::from(e.origin) - eye).length_squared() <= range * range {
                        scan.weap_flags = sim::pm::wf::ENEMY_IN_CROSSHAIR;
                    }
                }
                if mine == 3 || scan.weap_flags != 0 {
                    scan.crosshair = Some(hit);
                    self.crosshair_client = hit;
                }
            }
        }
        self.scan = scan;
    }
}
