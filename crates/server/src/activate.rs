// SPDX-License-Identifier: GPL-3.0-or-later
//! The use button and the crosshair hint (`player_use_mp`): which use trigger a player is
//! looking at or standing in, the hint the client shows for it, and the `trigger` notify
//! pressing +activate sends to its scripts.

use crate::client::{Session, Team};
use crate::fire::view_origin;
use crate::game::Game;
use gsc::vm::Vm;
use net::ui::cs;
use sim::Vec3;
use sim::cm::ENTITYNUM_NONE;
use sim::contents;
use sim::pm::{PLAYER_MAXS, PLAYER_MINS, PmType, button, pmf, weapon_state, wf};

/// Farthest a use trigger is picked from the eye, and how far the hint scan reaches.
const USE_RADIUS: f32 = 128.0;
const SCAN_RADIUS: [f32; 3] = [192.0, 192.0, 96.0];
/// `requireLookAt` triggers need the view this close to their direction (cosine).
const LOOK_AT_DOT: f32 = 0.76;
const SCORE_SPAN: f32 = 256.0;

/// How many different use-trigger strings a level may have.
const MAX_HINT_STRINGS: u32 = 32;

impl Game {
    /// `G_GetHintStringIndex`: the use-trigger string slot holding `text`, filling a free one.
    pub fn hint_string_index(&mut self, text: &str) -> Result<usize, String> {
        let slot = (0..MAX_HINT_STRINGS).find(|i| {
            self.configstrings
                .get(&(u32::from(cs::USE_TRIG_STRINGS) + i))
                .is_none_or(|s| s.is_empty() || s == text)
        });
        let Some(i) = slot else {
            return Err(format!(
                "Too many different hintstring values. Max allowed is {MAX_HINT_STRINGS} different strings"
            ));
        };
        self.set_configstring(cs::USE_TRIG_STRINGS + i as u16, text);
        Ok(i as usize)
    }

    /// `Player_GetUseList`, trigger part: the use triggers in reach, best first.
    fn use_list(&self, n: u16) -> Vec<u16> {
        let (Some(c), Some(world)) = (self.client(n), self.world.as_ref()) else {
            return Vec::new();
        };
        let eye = view_origin(&c.ps);
        let (fwd, _, _) = sim::pm::math::angle_vectors(&c.ps.viewangles);
        let lo: Vec3 = std::array::from_fn(|i| eye[i] - SCAN_RADIUS[i]);
        let hi: Vec3 = std::array::from_fn(|i| eye[i] + SCAN_RADIUS[i]);
        let feet: Vec3 = std::array::from_fn(|i| c.ps.origin[i] + PLAYER_MINS[i]);
        let head: Vec3 = std::array::from_fn(|i| c.ps.origin[i] + PLAYER_MAXS[i]);
        let mut near = Vec::new();
        world.area_entities(lo, hi, contents::USE, |t| {
            near.push(t);
            true
        });
        let mut scored: Vec<(f32, u16)> = Vec::new();
        for t in near {
            let (Some(te), Some(le)) = (self.ent(t), world.entity(t)) else {
                continue;
            };
            if t == n {
                continue;
            }
            let touch = &*te.classname == "trigger_use_touch";
            if touch {
                let over = (0..3).all(|i| head[i] >= le.abs_min[i] && feet[i] <= le.abs_max[i]);
                if over && crate::script::entity_contact(self, feet, head, te) {
                    scored.push((-SCORE_SPAN, t));
                }
                continue;
            }
            let mid: Vec3 = std::array::from_fn(|i| (le.abs_min[i] + le.abs_max[i]) * 0.5);
            let mut d: Vec3 = std::array::from_fn(|i| mid[i] - eye[i]);
            let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            if dist > USE_RADIUS {
                continue;
            }
            if dist > 0.0 {
                d.iter_mut().for_each(|v| *v /= dist);
            }
            let dot = d[0] * fwd[0] + d[1] * fwd[1] + d[2] * fwd[2];
            if te.x.require_look_at && dot < LOOK_AT_DOT {
                continue;
            }
            // A look-at trigger beats a touch one the view is not on.
            let score = (1.0 - (dot + 1.0) * 0.5) * SCORE_SPAN - SCORE_SPAN + dist;
            scored.push((score, t));
        }
        scored.sort_by(|a, b| a.0.total_cmp(&b.0));
        // A trigger behind a wall is out of the running unless the player stands in it.
        scored
            .into_iter()
            .filter(|&(_, t)| {
                let (Some(te), Some(le)) = (self.ent(t), world.entity(t)) else {
                    return false;
                };
                if &*te.classname == "trigger_use_touch" {
                    return true;
                }
                let mid: Vec3 = std::array::from_fn(|i| (le.abs_min[i] + le.abs_max[i]) * 0.5);
                world.trace_passed(eye, mid, [0.0; 3], [0.0; 3], n, ENTITYNUM_NONE, 17)
            })
            .map(|(_, t)| t)
            .collect()
    }

    /// `Player_UpdateCursorHints`: what the crosshair hint shows this frame, and which trigger
    /// +activate would use.
    pub fn update_cursor_hints(&mut self, n: u16) {
        let Some(c) = self.client(n) else { return };
        let ps = &c.ps;
        let blocked = self.ent(n).is_none_or(|e| e.health <= 0)
            || c.session != Session::Playing
            || ps.pm_flags & (pmf::MANTLE | pmf::SPRINTING) != 0
            || (weapon_state::OFFHAND_INIT..=weapon_state::OFFHAND).contains(&ps.weapon_state)
            || ps.pm_type == PmType::LastStand
            || c.frozen;
        let mut hint = (0u8, -1i8, ENTITYNUM_NONE);
        if !blocked {
            for t in self.use_list(n) {
                let Some(te) = self.ent(t) else { continue };
                // Something else in reach that is not a use trigger shows no hint.
                if !matches!(&*te.classname, "trigger_use" | "trigger_use_touch") {
                    break;
                }
                if te.x.trigger_team != Team::Free && te.x.trigger_team != c.team {
                    continue;
                }
                if te.x.claimed_by.is_some_and(|who| who != n) {
                    continue;
                }
                // HINT_INHERIT (-1) has no icon of its own here.
                let kind = te.x.cursor_hint.max(1) as u8;
                let text = te.x.hint.map_or(-1, |i| i as i8);
                hint = (kind, text, t);
                break;
            }
        }
        if let Some(c) = self.client_mut(n) {
            c.ps.cursor_hint = hint.0;
            c.ps.cursor_hint_string = hint.1;
            c.ps.cursor_hint_ent_index = hint.2;
        }
    }

    /// `Player_UpdateActivate`: a fresh press picks the trigger the hint names; holding it
    /// notifies that trigger once the hold time is over.
    pub fn update_activate(&mut self, vm: &mut Vm, n: u16) {
        let time = self.level.time;
        let hold_ms = self.cvars.int("g_useholdtime");
        let spawn_delay = self.cvars.int("g_useholdspawndelay");
        let Some(c) = self.client_mut(n) else { return };
        let use_mask = button::USE | button::USE_RELOAD;
        let mut used = false;
        if c.use_hold_ent.is_some()
            && c.old_buttons & button::USE_RELOAD != 0
            && c.buttons & button::USE_RELOAD == 0
        {
            c.ps.weapon_flags |= wf::RELOAD_REQUESTED;
            return;
        }
        if c.latched_buttons & use_mask != 0 {
            c.use_hold_ent = None;
            let busy = c.ps.pm_flags & (pmf::MANTLE | pmf::SPRINTING) != 0
                || (weapon_state::OFFHAND_INIT..=weapon_state::OFFHAND_END)
                    .contains(&c.ps.weapon_state);
            if busy {
                used = true;
            } else if c.ps.cursor_hint != 0 && c.ps.cursor_hint_ent_index != ENTITYNUM_NONE {
                c.use_hold_ent = Some(c.ps.cursor_hint_ent_index);
                c.use_hold_time = time;
                used = true;
            }
        }
        if c.use_hold_ent.is_some() || used {
            if c.buttons & use_mask != 0
                && let Some(t) = c.use_hold_ent
                && time - c.last_spawn_time >= spawn_delay
                && time - c.use_hold_time >= hold_ms
            {
                c.use_hold_ent = None;
                if self.ent(t).is_some() {
                    let who = self.entity_value(vm, n);
                    vm.notify_entity(t, "trigger", &[who]);
                }
            }
            if let Some(c) = self.client_mut(n) {
                c.use_button_done = true;
            }
        } else if c.latched_buttons & button::USE_RELOAD != 0 {
            c.ps.weapon_flags |= wf::RELOAD_REQUESTED;
        }
    }
}

impl Game {
    /// A spot a player can stand on inside an entity's box: the first of a grid around its middle
    /// where the player's hull, dropped from the top of the box, lands on walkable floor; the middle
    /// of the box when there is none (a trigger with no floor in it).
    pub fn floor_in(&self, ent: u16) -> Option<Vec3> {
        use sim::cm::Collide;
        let world = self.world.as_ref()?;
        let e = world.entity(ent)?;
        let mid = [
            (e.abs_min[0] + e.abs_max[0]) * 0.5,
            (e.abs_min[1] + e.abs_max[1]) * 0.5,
        ];
        let mut offsets = vec![(0.0, 0.0)];
        for ring in 1..=6 {
            let r = ring as f32 * 20.0;
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                offsets.push((r * a.cos(), r * a.sin()));
            }
        }
        for (dx, dy) in offsets {
            let (x, y) = (mid[0] + dx, mid[1] + dy);
            if x < e.abs_min[0] || x > e.abs_max[0] || y < e.abs_min[1] || y > e.abs_max[1] {
                continue;
            }
            let top = [x, y, (e.abs_max[2] - 40.0).max(e.abs_min[2] + 8.0)];
            let low = [x, y, e.abs_min[2] - 8.0];
            let t = world.trace(
                top,
                low,
                PLAYER_MINS,
                PLAYER_MAXS,
                ENTITYNUM_NONE,
                contents::MASK_PLAYERSOLID,
            );
            if t.start_solid || t.all_solid || t.fraction >= 1.0 || !t.walkable {
                continue;
            }
            return Some([x, y, top[2] + (low[2] - top[2]) * t.fraction + 0.5]);
        }
        Some([mid[0], mid[1], (e.abs_min[2] + e.abs_max[2]) * 0.5])
    }

    /// Test hook: puts player `n` on the floor in the first live trigger named `name…` that is on for
    /// the player's team.
    pub fn teleport_to_named(&mut self, n: u16, name: &str) -> Result<(), String> {
        let team = self
            .client(n)
            .map(|c| c.team)
            .ok_or("devtele: no such client")?;
        let found = self.ents.iter().enumerate().find_map(|(i, e)| {
            let e = e.as_ref()?;
            (e.targetname.as_deref().is_some_and(|t| t.starts_with(name))
                && e.classname.starts_with("trigger_")
                && (e.x.trigger_team == Team::Free || e.x.trigger_team == team)
                && e.origin[2] > -1000.0
                && e.origin[2] < 5000.0)
                .then_some(i as u16)
        });
        let t = found.ok_or_else(|| format!("devtele: no live trigger {name:?} for this team"))?;
        let at = self
            .floor_in(t)
            .ok_or("devtele: the trigger is not linked")?;
        self.teleport(n, at);
        Ok(())
    }

    /// Test hook: puts player `n` at `origin`, standing still.
    pub fn teleport(&mut self, n: u16, origin: Vec3) {
        if let Some(c) = self.client_mut(n) {
            c.ps.origin = origin;
            c.ps.velocity = [0.0; 3];
        }
        if let Some(e) = self.ent_mut(n) {
            e.origin = origin;
        }
        self.relink(n);
    }

    /// The centre of an entity's bounds, for bots walking to a trigger.
    pub fn use_center(&self, t: u16) -> Option<Vec3> {
        let le = self.world.as_ref()?.entity(t)?;
        Some(std::array::from_fn(|i| {
            (le.abs_min[i] + le.abs_max[i]) * 0.5
        }))
    }

    /// What the gametype asks bot `n` to walk to: the use triggers it may use now and the
    /// pickup triggers (`*pickup*` targetnames) of carried objectives; the count is how many
    /// leading entries the bots should favour.
    pub fn bot_objectives(&self, n: u16) -> (Vec<(u16, Vec3)>, usize) {
        let Some(team) = self.client(n).map(|c| c.team) else {
            return (Vec::new(), 0);
        };
        let mut out = Vec::new();
        let mut pickups = Vec::new();
        let mut mine = Vec::new();
        for (i, e) in self.ents.iter().enumerate() {
            let Some(e) = e else { continue };
            let t = i as u16;
            let wanted = match &*e.classname {
                "trigger_use" | "trigger_use_touch" => {
                    e.x.trigger_team == Team::Free || e.x.trigger_team == team
                }
                "trigger_multiple" | "trigger_radius" => {
                    e.targetname.as_ref().is_some_and(|n| n.contains("pickup"))
                }
                _ => false,
            };
            if !wanted || e.hidden {
                continue;
            }
            // A trigger the script parked far below the map is switched off.
            if let Some(mid) = self.use_center(t)
                && mid[2] > -10000.0
            {
                if e.classname.starts_with("trigger_use") && e.x.trigger_team == team {
                    // Switched on for this team alone: the zone to plant at, the bomb to defuse.
                    mine.push((t, mid));
                } else if e.classname.starts_with("trigger_use") {
                    out.push((t, mid));
                } else if e.origin[2] < 5000.0 {
                    // A carried object parks its trigger 10000 units up.
                    pickups.push((t, mid));
                }
            }
        }
        // Triggers made for this team come first, then a pickup lying free, then the rest.
        let n_first = if mine.is_empty() {
            pickups.len()
        } else {
            mine.len()
        };
        mine.extend(pickups);
        mine.extend(out);
        (mine, n_first)
    }
}
