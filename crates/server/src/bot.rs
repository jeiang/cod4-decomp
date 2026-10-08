// SPDX-License-Identifier: GPL-3.0-or-later
//! Server-side bots: they inject usercmds directly (no netchan).
//!
//! A bot is a [`Brain`] that, once per tick, reads the world through the same state a player's
//! client would see (positions, line of sight) and answers with a usercmd. It roams along the
//! generated [`NavMesh`] toward spawn points and objectives, notices enemies in front of it
//! that it has a clear line to, turns toward them at a limited rate after a reaction delay and
//! shoots in bursts. Nothing here touches the netchan.

use std::sync::Arc;

use sim::Vec3;
use sim::contents;
use sim::pm::{ANGLE_UNIT, UserCmd, button, pmf};
use sim::weapon::OffhandClass;

use crate::client::{Session, Team};
use crate::game::Game;
use crate::nav::{NavMesh, NodeId, PathScratch, Steer};

/// Contents a sight line collides with (`sighttracepassed`'s mask).
const SIGHT_MASK: i32 = 0x0280_1803;
/// How often a bot looks for enemies (ms); each bot has its own phase.
const SCAN_MS: i32 = 200;
/// Enemies this close are noticed from any direction.
const NOTICE_RADIUS: f32 = 350.0;
/// Half angle of the field of view (degrees).
const FOV_HALF: f32 = 95.0;
/// Longest distance a bot shoots at.
const MAX_FIRE_DIST: f32 = 2200.0;

/// Working memory shared by all bots on the server (path searches run one at a time).
#[derive(Default)]
pub struct BotShared {
    pub scratch: Option<PathScratch>,
}

pub struct Brain {
    pub num: u16,
    rng: u32,
    reaction_ms: i32,
    aim_noise: f32,
    turn_rate: f32,
    yaw: f32,
    pitch: f32,
    enemy: Option<u16>,
    enemy_seen: i32,
    enemy_acquired: i32,
    last_known: Option<Vec3>,
    next_scan: i32,
    aim_err: [f32; 2],
    aim_err_until: i32,
    path: Vec<NodeId>,
    steer: Steer,
    next_path: i32,
    idle_until: i32,
    strafe: f32,
    strafe_until: i32,
    burst_until: i32,
    pause_until: i32,
    frag_until: i32,
    next_frag: i32,
    stuck_events: u32,
    jump_until: i32,
    spawn_time: i32,
    /// The objective trigger the bot is walking to, and the one it holds +activate on.
    obj: Option<u16>,
    using: Option<(u16, i32)>,
    /// A trigger the bot gave up on (it did nothing for the bot) and until when.
    gave_up: Option<(u16, i32)>,
}

fn xorshift(s: &mut u32) -> u32 {
    *s ^= *s << 13;
    *s ^= *s >> 17;
    *s ^= *s << 5;
    *s
}

fn frac(s: &mut u32) -> f32 {
    (xorshift(s) & 0xFFFF) as f32 / 65536.0
}

fn angle_diff(a: f32, b: f32) -> f32 {
    crate::client::angle_180(a - b)
}

fn dist3(a: Vec3, b: Vec3) -> f32 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

/// Pitch and yaw (degrees, original convention: negative pitch looks up) toward `d`.
fn angles_to(d: Vec3) -> (f32, f32) {
    let flat = (d[0] * d[0] + d[1] * d[1]).sqrt();
    let yaw = d[1].atan2(d[0]).to_degrees();
    let pitch = -d[2].atan2(flat).to_degrees();
    (pitch, yaw)
}

impl Brain {
    pub fn new(num: u16) -> Self {
        let mut rng = 0x9E37_79B9u32 ^ (u32::from(num) + 1).wrapping_mul(0x85EB_CA6B);
        xorshift(&mut rng);
        let skill = frac(&mut rng);
        Self {
            num,
            rng,
            reaction_ms: 250 + (frac(&mut rng) * 350.0) as i32,
            aim_noise: 1.5 + (1.0 - skill) * 4.0,
            turn_rate: 420.0 + skill * 360.0,
            yaw: 0.0,
            pitch: 0.0,
            enemy: None,
            enemy_seen: i32::MIN / 2,
            enemy_acquired: 0,
            last_known: None,
            next_scan: i32::from(num) * 7,
            aim_err: [0.0; 2],
            aim_err_until: 0,
            path: Vec::new(),
            steer: Steer::new(),
            next_path: 0,
            idle_until: 0,
            strafe: 0.0,
            strafe_until: 0,
            burst_until: 0,
            pause_until: 0,
            frag_until: 0,
            next_frag: 5000,
            stuck_events: 0,
            jump_until: 0,
            spawn_time: -1,
            obj: None,
            using: None,
            gave_up: None,
        }
    }

    /// The command the bot sends for the tick ending at `time`.
    pub fn usercmd(&mut self, g: &Game, sh: &mut BotShared, n: u16, time: i32) -> UserCmd {
        let mut cmd = UserCmd {
            server_time: time,
            ..UserCmd::default()
        };
        let Some(c) = g.client(n) else { return cmd };
        let delta = c.ps.delta_angles;
        let alive = c.session == Session::Playing
            && matches!(
                c.ps.pm_type,
                sim::pm::PmType::Normal | sim::pm::PmType::NormalLinked
            );
        if !alive {
            // Waiting to respawn: tap use on alternate ticks so the script sees a press.
            if (time / 33) % 2 == 0 {
                cmd.buttons |= button::USE;
            }
            self.spawn_time = -1;
            self.enemy = None;
            self.path.clear();
            self.yaw = c.ps.viewangles[1];
            self.pitch = c.ps.viewangles[0];
            self.finish(&mut cmd, delta);
            return cmd;
        }
        // `bot_idle`: bots stand where they are (a stage that needs a quiet round).
        if g.cvars.bool("bot_idle") {
            self.yaw = c.ps.viewangles[1];
            self.finish(&mut cmd, delta);
            return cmd;
        }
        if self.spawn_time < 0 {
            self.spawn_time = time;
            self.yaw = c.ps.viewangles[1];
            self.pitch = 0.0;
            self.path.clear();
            self.steer.reset();
            self.next_path = time;
        }
        let dt = 0.033;
        let pos = c.ps.origin;
        let eye = [pos[0], pos[1], pos[2] + c.ps.view_height_current];
        if time >= self.next_scan {
            self.next_scan = time + SCAN_MS;
            self.scan(g, n, eye, time);
        }
        let weapon = c.ps.weapon as u16;
        let info = g.weapons.info(weapon);
        let clip = c.inv.get_clip(&g.weapons, weapon);
        let stock = c.inv.get_stock(&g.weapons, weapon);

        // What to look at and where to walk.
        let mut want_dir = [0.0f32; 3];
        let mut sprint = false;
        let engaged = self.enemy.is_some() && time - self.enemy_seen < 400;
        let mut aim: Option<(f32, f32, f32)> = None; // pitch, yaw, distance
        // Standing in a use trigger the player's hint names: hold +activate, and stay on it
        // once begun, as a planter must.
        let holding =
            (!engaged || self.using.is_some()) && self.hold_use(c.ps.cursor_hint_ent_index, time);
        if holding {
            cmd.buttons |= button::USE;
        } else if let (Some(e), true) = (self.enemy, engaged)
            && let Some(te) = g.ent(e)
        {
            let h = te.maxs[2];
            let spot = [te.origin[0], te.origin[1], te.origin[2] + h * 0.78];
            if time >= self.aim_err_until {
                self.aim_err = [
                    (frac(&mut self.rng) - 0.5) * 2.0 * self.aim_noise,
                    (frac(&mut self.rng) - 0.5) * 2.0 * self.aim_noise,
                ];
                self.aim_err_until = time + 250;
            }
            let d = [spot[0] - eye[0], spot[1] - eye[1], spot[2] - eye[2]];
            let dist = dist3(spot, eye);
            let (p, y) = angles_to(d);
            aim = Some((p + self.aim_err[0], y + self.aim_err[1], dist));
            self.last_known = Some(te.origin);
            // Fight: keep distance by approaching far targets and backing off near ones.
            if time >= self.strafe_until {
                self.strafe = if frac(&mut self.rng) < 0.25 {
                    0.0
                } else if frac(&mut self.rng) < 0.5 {
                    -1.0
                } else {
                    1.0
                };
                self.strafe_until = time + 400 + (frac(&mut self.rng) * 900.0) as i32;
            }
            let flat = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1.0);
            let fwd = [d[0] / flat, d[1] / flat, 0.0];
            let right = [fwd[1], -fwd[0], 0.0];
            let approach = if dist > 900.0 {
                1.0
            } else if dist < 180.0 {
                -1.0
            } else {
                0.0
            };
            want_dir = [
                fwd[0] * approach + right[0] * self.strafe,
                fwd[1] * approach + right[1] * self.strafe,
                0.0,
            ];
        } else {
            self.navigate(g, sh, pos, time, &mut want_dir, &mut cmd, &mut sprint);
            // The path ends at the node nearest the trigger; walk the rest of the way in.
            if time < self.idle_until
                && let Some(t) = self.obj
                && let Some(mid) = g.use_center(t)
            {
                let d = [mid[0] - pos[0], mid[1] - pos[1]];
                let len = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1.0);
                want_dir = [d[0] / len, d[1] / len, 0.0];
            }
        }

        // View.
        let (tp, ty) = match aim {
            Some((p, y, _)) => (p, y),
            None => {
                let flat = (want_dir[0] * want_dir[0] + want_dir[1] * want_dir[1]).sqrt();
                if flat > 0.1 {
                    (0.0, want_dir[1].atan2(want_dir[0]).to_degrees())
                } else {
                    (0.0, self.yaw)
                }
            }
        };
        let step = self.turn_rate * dt;
        let dy = angle_diff(ty, self.yaw);
        self.yaw += dy.clamp(-step, step);
        let dp = tp - self.pitch;
        self.pitch += dp.clamp(-step, step);
        self.pitch = self.pitch.clamp(-80.0, 80.0);

        // Movement relative to the view.
        let yaw_r = self.yaw.to_radians();
        let (sy, cy) = yaw_r.sin_cos();
        let (fwd, right) = ([cy, sy], [sy, -cy]);
        let f = want_dir[0] * fwd[0] + want_dir[1] * fwd[1];
        let r = want_dir[0] * right[0] + want_dir[1] * right[1];
        cmd.forwardmove = (f * 127.0).clamp(-127.0, 127.0) as i8;
        cmd.rightmove = (r * 127.0).clamp(-127.0, 127.0) as i8;
        if sprint && f > 0.9 && !engaged {
            cmd.buttons |= button::SPRINT;
        }
        if time < self.jump_until {
            cmd.buttons |= button::JUMP;
        }

        // Shooting.
        if let (Some((_, _, dist)), true) = (aim, engaged) {
            let aimed = angle_diff(ty, self.yaw).abs()
                < (30.0 / dist.max(1.0)).atan().to_degrees().max(2.5)
                && (tp - self.pitch).abs() < 4.0;
            let ready = time - self.enemy_acquired >= self.reaction_ms;
            let usable = c.ps.weapon != 0
                && dist < MAX_FIRE_DIST
                && c.ps.weapon_flags & sim::pm::wf::DISABLED == 0;
            if clip <= 0 && stock > 0 {
                cmd.buttons |= button::RELOAD;
            } else if ready && aimed && usable && clip > 0 {
                if time >= self.burst_until && time >= self.pause_until {
                    self.burst_until = time + 300 + (frac(&mut self.rng) * 700.0) as i32;
                    self.pause_until =
                        self.burst_until + 150 + (frac(&mut self.rng) * 350.0) as i32;
                }
                if time < self.burst_until {
                    if info.is_semi_auto() {
                        if (time / 33) % 2 == 0 {
                            cmd.buttons |= button::ATTACK;
                        }
                    } else {
                        cmd.buttons |= button::ATTACK;
                    }
                }
            }
            if info.aim_down_sight && dist > 600.0 && usable {
                cmd.buttons |= button::ADS;
            }
            // Grenade at mid range targets.
            if time >= self.next_frag
                && dist > 350.0
                && dist < 900.0
                && c.inv
                    .first_available_offhand(&g.weapons, &c.ps, OffhandClass::Frag)
                    != 0
            {
                self.frag_until = time + 330;
                self.next_frag = time + 9000 + (frac(&mut self.rng) * 12000.0) as i32;
            }
        } else if clip < info.clip_size / 3 && stock > 0 && info.clip_size > 0 {
            cmd.buttons |= button::RELOAD;
        }
        if time < self.frag_until {
            cmd.buttons |= button::FRAG;
        } else if self.frag_until != 0 && time < self.frag_until + 33 {
            // Release throws.
        }
        if c.ps.pm_flags & pmf::MANTLE != 0 {
            cmd.forwardmove = 127;
        }
        // Picking a point on the map for an airstrike: somewhere on it, confirmed after a moment.
        if c.ps.loc_selection != 0 {
            cmd.buttons |= button::LOC_SELECTING;
            if time - self.spawn_time > 500 && (time / 33) % 2 == 0 {
                cmd.buttons |= button::LOC_CONFIRM;
                cmd.selected_location = [
                    ((frac(&mut self.rng) - 0.5) * 120.0) as i8,
                    ((frac(&mut self.rng) - 0.5) * 120.0) as i8,
                ];
            }
        }
        // A hardpoint in action slot 4 is called in by selecting its weapon, as the key would.
        let slot_weapon = c.ps.action_slot_param[3];
        if !engaged
            && c.ps.action_slot_type[3] == sim::pm::action_slot::WEAPON
            && slot_weapon != 0
            && c.inv.has(slot_weapon)
            && c.ps.weapon != u32::from(slot_weapon)
            && time - self.spawn_time > 4000
        {
            cmd.weapon = slot_weapon as u8;
        }
        self.finish(&mut cmd, delta);
        cmd
    }

    fn finish(&self, cmd: &mut UserCmd, delta: Vec3) {
        cmd.angles = [
            (angle_diff(self.pitch, delta[0]) / ANGLE_UNIT).round() as i32,
            (angle_diff(self.yaw, delta[1]) / ANGLE_UNIT).round() as i32,
            0,
        ];
    }

    /// Looks for the closest enemy in view with a clear line.
    fn scan(&mut self, g: &Game, n: u16, eye: Vec3, time: i32) {
        let Some(c) = g.client(n) else { return };
        let Some(world) = g.world.as_ref() else {
            return;
        };
        let my_team = c.team;
        let (sy, cy) = self.yaw.to_radians().sin_cos();
        let mut best: Option<(f32, u16)> = None;
        for (m, o) in g.connected_clients() {
            if m == n || o.session != Session::Playing || o.ps.pm_type != sim::pm::PmType::Normal {
                continue;
            }
            if my_team != Team::Free && o.team == my_team {
                continue;
            }
            let dist = dist3(o.ps.origin, eye);
            if dist > MAX_FIRE_DIST {
                continue;
            }
            let d = [o.ps.origin[0] - eye[0], o.ps.origin[1] - eye[1]];
            let flat = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1.0);
            let cos = (d[0] * cy + d[1] * sy) / flat;
            if dist > NOTICE_RADIUS && cos < FOV_HALF.to_radians().cos() {
                continue;
            }
            if best.is_some_and(|(b, _)| b <= dist) {
                continue;
            }
            let h = g.ent(m).map_or(60.0, |e| e.maxs[2]);
            let chest = [o.ps.origin[0], o.ps.origin[1], o.ps.origin[2] + h * 0.7];
            let head = [o.ps.origin[0], o.ps.origin[1], o.ps.origin[2] + h * 0.9];
            let clear =
                |to: Vec3| world.sight_trace(0, eye, to, [0.0; 3], [0.0; 3], n, m, SIGHT_MASK) == 0;
            if clear(chest) || clear(head) {
                best = Some((dist, m));
            }
        }
        // An enemy helicopter in the open is shot at too.
        for (m, e, v) in g.vehicles() {
            if v.owner == n
                || (my_team != Team::Free && g.client(v.owner).map(|c| c.team) == Some(my_team))
            {
                continue;
            }
            let dist = dist3(e.origin, eye);
            if dist > MAX_FIRE_DIST || best.is_some_and(|(b, _)| b <= dist) {
                continue;
            }
            if world.sight_trace(0, eye, e.origin, [0.0; 3], [0.0; 3], n, m, SIGHT_MASK) == 0 {
                best = Some((dist, m));
            }
        }
        match best {
            Some((_, m)) => {
                if self.enemy != Some(m) || time - self.enemy_seen > 1500 {
                    self.enemy_acquired = time;
                }
                self.enemy = Some(m);
                self.enemy_seen = time;
            }
            None => {
                if time - self.enemy_seen > 2500 {
                    self.enemy = None;
                }
            }
        }
    }

    /// Roams: follows the path toward the goal, picking a new goal on arrival or when stuck.
    #[allow(clippy::too_many_arguments)]
    fn navigate(
        &mut self,
        g: &Game,
        sh: &mut BotShared,
        pos: Vec3,
        time: i32,
        want_dir: &mut Vec3,
        cmd: &mut UserCmd,
        sprint: &mut bool,
    ) {
        let Some(mesh) = g.nav.as_ref().map(Arc::clone) else {
            return;
        };
        if time < self.idle_until {
            return;
        }
        let scratch = sh.scratch.get_or_insert_with(|| PathScratch::new(&mesh));
        // Head for where the enemy was last seen before roaming on.
        if self.path.is_empty() || time >= self.next_path {
            self.next_path = time + 1500;
            // A bot that is only roaming looks again for an objective now and then, so a
            // trigger that just came on for its team (a planted bomb) draws it.
            let reroll = self.obj.is_none() && frac(&mut self.rng) < 0.3;
            if self.path.is_empty() || reroll || self.steer_arrived(&mesh, pos) {
                self.pick_goal(g, &mesh, scratch, pos, time);
                self.steer.reset();
            }
        }
        if self.path.is_empty() {
            self.idle_until = time + 500;
            return;
        }
        let out = self.steer.update(&mesh, &self.path, pos);
        if out.arrived {
            self.path.clear();
            self.idle_until = time
                + if self.obj.is_some() {
                    2500
                } else {
                    (frac(&mut self.rng) * 600.0) as i32
                };
            return;
        }
        if out.stuck {
            self.stuck_events += 1;
            self.jump_until = time + 400;
            self.steer.reset();
            if self.stuck_events >= 2 {
                self.stuck_events = 0;
                self.path.clear();
            }
        }
        if out.jump && time >= self.jump_until {
            self.jump_until = time + 200;
        }
        if out.crouch {
            cmd.buttons |= button::CROUCH;
        }
        *want_dir = out.dir;
        *sprint = true;
    }

    /// Whether to keep +activate down: a use trigger is under the hint. A trigger that does
    /// nothing for the bot within 9 s is given up for 20 s.
    fn hold_use(&mut self, hint_ent: u16, time: i32) -> bool {
        if hint_ent == sim::cm::ENTITYNUM_NONE
            || self
                .gave_up
                .is_some_and(|(t, until)| t == hint_ent && time < until)
        {
            self.using = None;
            return false;
        }
        let start = match self.using {
            Some((t, s)) if t == hint_ent => s,
            _ => time,
        };
        self.using = Some((hint_ent, start));
        if time - start > 9000 {
            self.gave_up = Some((hint_ent, time + 20000));
            self.using = None;
            self.obj = None;
            self.path.clear();
            return false;
        }
        true
    }

    fn steer_arrived(&mut self, mesh: &NavMesh, pos: Vec3) -> bool {
        self.path.last().is_some_and(|l| {
            let p = mesh.node_pos(*l);
            ((p[0] - pos[0]).powi(2) + (p[1] - pos[1]).powi(2)).sqrt() < 40.0
        })
    }

    fn pick_goal(
        &mut self,
        g: &Game,
        mesh: &NavMesh,
        scratch: &mut PathScratch,
        pos: Vec3,
        time: i32,
    ) {
        let Some(from) = mesh.nearest_node(pos) else {
            self.path.clear();
            return;
        };
        let (objectives, n_use) = g.bot_objectives(self.num);
        for _ in 0..4 {
            self.obj = None;
            let to = if let Some(p) = self.last_known.take() {
                mesh.nearest_node(p)
            } else if !objectives.is_empty() && frac(&mut self.rng) < 0.85 {
                // Use triggers first: a team rushes the zones its carrier needs.
                let span = if n_use > 0 && frac(&mut self.rng) < 0.8 {
                    n_use
                } else {
                    objectives.len()
                };
                let i = xorshift(&mut self.rng) as usize % span;
                let (t, p) = objectives[i];
                if self
                    .gave_up
                    .is_some_and(|(g, until)| g == t && time < until)
                {
                    continue;
                }
                self.obj = Some(t);
                mesh.nearest_node(p)
            } else if !g.nav_goals.is_empty() && frac(&mut self.rng) < 0.7 {
                let i = xorshift(&mut self.rng) as usize % g.nav_goals.len();
                mesh.nearest_node(g.nav_goals[i])
            } else {
                let rng = &mut self.rng;
                Some(mesh.random_in_component(mesh.component(from), &mut || xorshift(rng)))
            };
            if let Some(to) = to
                && mesh.path(from, to, scratch, &mut self.path)
                && self.path.len() > 1
            {
                if let Some(w) = g.world.as_ref() {
                    mesh.smooth(w, &mut self.path);
                }
                return;
            }
        }
        self.path.clear();
    }
}

/// What the world offers bots to walk to: every spawn point and objective origin.
pub fn goals_from_map(entity_string: &[u8]) -> Vec<Vec3> {
    crate::nav::spawn_points(entity_string)
        .into_iter()
        .map(|s| s.origin)
        .collect()
}

#[allow(dead_code)]
const _: i32 = contents::PLAYER;
