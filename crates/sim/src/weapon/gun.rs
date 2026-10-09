// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (GPL-3.0, KisakCOD contributors): `bgame/bg_weapons.cpp`
// (`BG_CalculateWeaponAngles`, `BG_CalculateWeaponPosition_*`, `BG_CalculateViewAngles`, `BG_CalculateView_*`),
// `cgame/cg_weapons.cpp` (`CalculateWeaponPosition*`) and `game_mp/g_active_mp.cpp` (`ClientThink_real`).
//! How the held weapon moves: the gun's orientation (stance rotation, lean, idle breathing, bob, damage kick, the
//! recoil spring, view-delta sway) and the view's own effects, which the server composes into the direction shots
//! leave in and the client draws the view model with.
//!
//! One [`GunState`] is a player's springs and clocks. The client keeps one for the weapon it draws and the server
//! one per player, each advanced once per frame (client) or per usercmd (server) with the same arithmetic, so the
//! aim the client shows is the aim the server judges.

use crate::Vec3;
use crate::pm::bob::{bob_cycle, horizontal_bob, vertical_bob};
use crate::pm::math::{angle_delta, angle_vectors, angle_wrap_180, diff_track, get_lean_fraction};
use crate::pm::{PlayerState, VIEW_CROUCH, VIEW_PRONE, ef, weapon_state};
use assets::zone::weapon::WeaponDef;

/// The bob of the gun and of a scoped view is capped at this, degrees.
const BOB_MAX: f32 = 10.0;
/// The aimed view bob of any weapon is capped at this, degrees.
const ADS_VIEW_BOB_MAX: f32 = 45.0;
/// Ground speed to angle bob speed.
const BOB_SPEED: f32 = 0.16;
/// The integration step of the recoil spring, seconds.
const RECOIL_STEP: f32 = 0.005;
/// How fast the idle sway follows the stance's steadiness, per second.
const IDLE_FACTOR_SPEED: f32 = 0.5;
/// `MYLERP_START` and `MYLERP_END`: the part of a night vision change the gun is lowered over.
const NV_LERP_START: f32 = 0.3;
const NV_LERP_END: f32 = 0.1;

/// The numbers of a weapon file that move the gun.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GunParams {
    pub aim_down_sight: bool,
    pub overlay_reticle: bool,
    pub ads_aim_pitch: f32,
    pub ads_bob_factor: f32,
    pub ads_view_bob_mult: f32,
    pub ads_idle_amount: f32,
    pub hip_idle_amount: f32,
    pub ads_idle_speed: f32,
    pub hip_idle_speed: f32,
    pub idle_crouch_factor: f32,
    pub idle_prone_factor: f32,
    /// Hip then ADS: `fGunKickAccel`, `SpeedMax`, `SpeedDecay`, `StaticDecay`.
    pub gun_kick_accel: [f32; 2],
    pub gun_kick_speed_max: [f32; 2],
    pub gun_kick_speed_decay: [f32; 2],
    pub gun_kick_static_decay: [f32; 2],
    pub gun_max_pitch: f32,
    pub gun_max_yaw: f32,
    /// Hip then ADS.
    pub sway_max_angle: [f32; 2],
    pub sway_lerp_speed: [f32; 2],
    pub sway_pitch_scale: [f32; 2],
    pub sway_yaw_scale: [f32; 2],
    pub sway_horiz_scale: [f32; 2],
    pub sway_vert_scale: [f32; 2],
    pub sway_shell_shock_scale: f32,
    /// Stand, ducked, prone.
    pub rot: [[f32; 3]; 3],
    pub rot_min_speed: [f32; 3],
    pub move_ofs: [[f32; 3]; 3],
    pub move_min_speed: [f32; 3],
    /// Ducked and prone positions at rest.
    pub ofs: [[f32; 3]; 2],
    /// Stand-and-duck and prone rates.
    pub pos_rot_rate: [f32; 2],
    pub pos_move_rate: [f32; 2],
    pub night_vision_wear_time: i32,
}

impl GunParams {
    pub fn from_def(d: &WeaponDef) -> Self {
        Self {
            aim_down_sight: d.aim_down_sight != 0,
            overlay_reticle: d.overlay_reticle != 0,
            ads_aim_pitch: d.ads_aim_pitch,
            ads_bob_factor: d.ads_bob_factor,
            ads_view_bob_mult: d.ads_view_bob_mult,
            ads_idle_amount: d.ads_idle_amount,
            hip_idle_amount: d.hip_idle_amount,
            ads_idle_speed: d.ads_idle_speed,
            hip_idle_speed: d.hip_idle_speed,
            idle_crouch_factor: d.idle_crouch_factor,
            idle_prone_factor: d.idle_prone_factor,
            gun_kick_accel: [d.hip_gun_kick_accel, d.ads_gun_kick_accel],
            gun_kick_speed_max: [d.hip_gun_kick_speed_max, d.ads_gun_kick_speed_max],
            gun_kick_speed_decay: [d.hip_gun_kick_speed_decay, d.ads_gun_kick_speed_decay],
            gun_kick_static_decay: [d.hip_gun_kick_static_decay, d.ads_gun_kick_static_decay],
            gun_max_pitch: d.gun_max_pitch,
            gun_max_yaw: d.gun_max_yaw,
            sway_max_angle: [d.sway_max_angle, d.ads_sway_max_angle],
            sway_lerp_speed: [d.sway_lerp_speed, d.ads_sway_lerp_speed],
            sway_pitch_scale: [d.sway_pitch_scale, d.ads_sway_pitch_scale],
            sway_yaw_scale: [d.sway_yaw_scale, d.ads_sway_yaw_scale],
            sway_horiz_scale: [d.sway_horiz_scale, d.ads_sway_horiz_scale],
            sway_vert_scale: [d.sway_vert_scale, d.ads_sway_vert_scale],
            sway_shell_shock_scale: d.sway_shell_shock_scale,
            rot: [d.v_stand_rot, d.v_ducked_rot, d.v_prone_rot],
            rot_min_speed: [
                d.stand_rot_min_speed,
                d.ducked_rot_min_speed,
                d.prone_rot_min_speed,
            ],
            move_ofs: [d.v_stand_move, d.v_ducked_move, d.v_prone_move],
            move_min_speed: [
                d.stand_move_min_speed,
                d.ducked_move_min_speed,
                d.prone_move_min_speed,
            ],
            ofs: [d.v_ducked_ofs, d.v_prone_ofs],
            pos_rot_rate: [d.pos_rot_rate, d.pos_prone_rot_rate],
            pos_move_rate: [d.pos_move_rate, d.pos_prone_move_rate],
            night_vision_wear_time: d.night_vision_wear_time,
        }
    }
}

/// What one step needs besides the state: the player and the weapon, how fast the player moves over the ground
/// (`BG_GetSpeed`), the step in seconds and the clock the damage kick is timed by.
#[derive(Debug, Clone, Copy)]
pub struct GunFrame<'a> {
    pub ps: &'a PlayerState,
    pub p: &'a GunParams,
    pub xyspeed: f32,
    pub frametime: f32,
    pub time: i32,
    /// The last hit (see [`ViewFx::hit`]).
    pub hit: Hit,
}

/// A player's gun springs and clocks.
#[derive(Debug, Clone, PartialEq)]
pub struct GunState {
    /// Where the stance rotation and position have got to (`vLastMoveAng`, `vLastMoveOrg`).
    pub last_move_ang: Vec3,
    pub last_move_org: Vec3,
    /// How much of the stance's steadiness the idle sway has reached (`fLastIdleFactor`).
    pub last_idle_factor: f32,
    /// The idle clock, ms (`weapIdleTime`); the view's scope sway and the gun's idle share it.
    pub idle_time: i32,
    /// The recoil spring: offset in degrees and speed in degrees per second (`vGunOffset`, `vGunSpeed`).
    pub offset: Vec3,
    pub speed: Vec3,
    /// The view the sway last saw, and what the gun lags by (`swayViewAngles`, `swayOffset`, `swayAngles`).
    pub sway_view: Vec3,
    pub sway_offset: Vec3,
    pub sway_angles: Vec3,
}

impl Default for GunState {
    fn default() -> Self {
        Self {
            last_move_ang: [0.0; 3],
            last_move_org: [0.0; 3],
            last_idle_factor: 1.0,
            idle_time: 0,
            offset: [0.0; 3],
            speed: [0.0; 3],
            sway_view: [0.0; 3],
            sway_offset: [0.0; 3],
            sway_angles: [0.0; 3],
        }
    }
}

fn blend(pair: [f32; 2], ads: f32) -> f32 {
    (pair[1] - pair[0]) * ads + pair[0]
}

/// `DiffTrackAngle`.
fn diff_track_angle(mut tgt: f32, cur: f32, rate: f32, dt: f32) -> f32 {
    while tgt - cur > 180.0 {
        tgt -= 360.0;
    }
    while tgt - cur < -180.0 {
        tgt += 360.0;
    }
    angle_wrap_180(diff_track(tgt, cur, rate, dt))
}

/// 0 stand, 1 ducked, 2 prone: what the entity flags say.
fn stance(ps: &PlayerState) -> usize {
    if ps.e_flags & ef::PRONE != 0 {
        2
    } else if ps.e_flags & ef::CROUCH != 0 {
        1
    } else {
        0
    }
}

/// Moves `last` towards `target` by `rate` of the way per second, but never slower than 0.1 per second.
fn track_toward(last: &mut f32, target: f32, dt: f32, rate: f32) {
    if target == *last {
        return;
    }
    let mut delta = (target - *last) * dt * rate;
    if target <= *last {
        delta = delta.min(dt * -0.1);
        *last += delta;
        if target > *last {
            *last = target;
        }
    } else {
        delta = delta.max(dt * 0.1);
        *last += delta;
        if target < *last {
            *last = target;
        }
    }
}

impl GunState {
    /// Gives the recoil spring the speed of a shot (`BG_WeaponFireRecoil`'s gun part).
    pub fn kick(&mut self, speed: [f32; 2]) {
        self.speed[0] += speed[0];
        self.speed[1] += speed[1];
    }

    /// `BG_CalculateWeaponAngles`: the gun's angles relative to the view, degrees.
    pub fn weapon_angles(&mut self, f: &GunFrame<'_>) -> Vec3 {
        let mut a = [0.0; 3];
        if f.ps.leanf != 0.0 {
            a[2] -= get_lean_fraction(f.ps.leanf) * 2.0;
        }
        self.base_angles(f, &mut a);
        self.idle_angles(f, &mut a);
        let bob = bob_angles(f.ps, f.xyspeed, f.p, true);
        add_to(&mut a, &bob);
        self.damage_kick(f, &mut a);
        self.gun_recoil(f, &mut a);
        a[0] = angle_delta(a[0], self.sway_angles[0]);
        a[1] = angle_delta(a[1], self.sway_angles[1]);
        a
    }

    /// `BG_CalculateWeaponPosition_BaseAngles` and `_BasePosition_angles`: the aimed pitch and the stance's turn of
    /// the gun while moving.
    fn base_angles(&mut self, f: &GunFrame<'_>, a: &mut Vec3) {
        let (ps, p) = (f.ps, f.p);
        if p.aim_down_sight {
            a[0] += ps.weapon_pos_frac * p.ads_aim_pitch;
        }
        let s = stance(ps);
        let min = p.rot_min_speed[s];
        let mut target = [0.0; 3];
        if min < f.xyspeed && ps.weapon_state != weapon_state::RELOADING {
            let scale = ((f.xyspeed - min) / (ps.speed as f32 - min)).clamp(0.0, 1.0);
            target = p.rot[s].map(|v| v * scale);
        }
        if ps.weapon_pos_frac != 0.0 {
            let keep = 1.0 - ps.weapon_pos_frac;
            target = target.map(|v| v * keep);
        }
        let rate = if ps.view_height_current == 11.0 {
            p.pos_rot_rate[1]
        } else {
            p.pos_rot_rate[0]
        };
        for (last, t) in self.last_move_ang.iter_mut().zip(target) {
            track_toward(last, t, f.frametime, rate);
        }
        let k = match ps.weapon_pos_frac {
            0.0 => 1.0,
            x if x < 0.5 => 1.0 - x * 2.0,
            _ => return,
        };
        for (a, m) in a.iter_mut().zip(self.last_move_ang) {
            *a += k * m;
        }
    }

    /// `BG_CalculateWeaponPosition_IdleAngles`: the gun breathes, wider the more the weapon file says, aimed or not.
    fn idle_angles(&mut self, f: &GunFrame<'_>, a: &mut Vec3) {
        let (ps, p) = (f.ps, f.p);
        let ads = ps.weapon_pos_frac;
        let (amount, speed) = idle_amount_speed(p, ads);
        let target = idle_stance_factor(ps, p);
        if (!p.overlay_reticle || ads == 0.0) && self.last_idle_factor != target {
            self.last_idle_factor = approach(
                self.last_idle_factor,
                target,
                f.frametime * IDLE_FACTOR_SPEED,
            );
        }
        let mut size = amount * self.last_idle_factor;
        if p.overlay_reticle {
            size *= 1.0 - ads;
        }
        self.idle_time = self
            .idle_time
            .wrapping_add((f.frametime * 1000.0 * speed) as i32);
        let t = f64::from(self.idle_time);
        let wave = |k: f64| (t * k).sin() as f32 * size * 0.01;
        a[2] += wave(0.0005);
        a[1] += wave(0.0007);
        a[0] += wave(0.001);
    }

    /// `BG_CalculateWeaponPosition_DamageKick`: a hit throws the gun a little the way it throws the view.
    fn damage_kick(&self, f: &GunFrame<'_>, a: &mut Vec3) {
        if f.hit.time == 0 {
            return;
        }
        let (ps, p) = (f.ps, f.p);
        let ads = ps.weapon_pos_frac;
        let mut factor = ads * 0.5 + 0.5;
        let (deflect, back) = (factor * 100.0, factor * 400.0);
        if ads != 0.0 && p.overlay_reticle {
            factor *= 1.0 - ads * 0.75;
        }
        let since = (f.time - f.hit.time) as f32;
        let k = if since >= deflect {
            let left = 1.0 - (since - deflect) / back;
            if left <= 0.0 {
                return;
            }
            (1.0 - get_lean_fraction(1.0 - left)) * factor
        } else {
            get_lean_fraction(since / deflect) * factor
        };
        a[0] += k * f.hit.pitch * 0.5;
        a[1] -= k * f.hit.roll;
        a[2] += k * f.hit.roll * 0.5;
    }

    /// `BG_CalculateWeaponPosition_GunRecoil`: steps the spring over the frame and adds its offset. A weapon that
    /// cannot aim down sights has none.
    fn gun_recoil(&mut self, f: &GunFrame<'_>, a: &mut Vec3) {
        let p = f.p;
        if !p.aim_down_sight {
            return;
        }
        let ads = f.ps.weapon_pos_frac;
        let accel = blend(p.gun_kick_accel, ads);
        let max = blend(p.gun_kick_speed_max, ads);
        let decay = blend(p.gun_kick_speed_decay, ads);
        let rest = blend(p.gun_kick_static_decay, ads);
        let mut left = f.frametime;
        while left > 0.0 {
            let ft = left.min(RECOIL_STEP);
            left -= ft;
            let [o0, o1, _] = &mut self.offset;
            let [s0, s1, _] = &mut self.speed;
            let pitch_done = spring(o0, s0, ft, [p.gun_max_pitch, accel, max, decay, rest]);
            let yaw_done = spring(o1, s1, ft, [p.gun_max_yaw, accel, max, decay, rest]);
            if pitch_done && yaw_done {
                break;
            }
        }
        add_to(a, &self.offset);
    }

    /// `BG_CalculateWeaponPosition_Sway`: the gun lags behind a turning view. `ss_scale` is the shell shock's scale
    /// of it, `msec` the frame. A scoped weapon does not sway while aimed.
    pub fn sway(&mut self, ps: &PlayerState, p: &GunParams, ss_scale: f32, msec: i32) {
        if msec == 0 {
            return;
        }
        let f = ps.weapon_pos_frac;
        let dt = msec as f32 * 0.001;
        let at = |pair: [f32; 2]| {
            if p.aim_down_sight {
                blend(pair, f)
            } else {
                pair[0]
            }
        };
        if p.aim_down_sight && f > 0.0 && p.overlay_reticle {
            return;
        }
        let (max, lerp) = (at(p.sway_max_angle), at(p.sway_lerp_speed));
        let pitch = at(p.sway_pitch_scale) * ss_scale;
        let yaw = at(p.sway_yaw_scale) * ss_scale;
        let horiz = at(p.sway_horiz_scale) * ss_scale;
        let vert = at(p.sway_vert_scale) * ss_scale;
        let per_frame = 1.0 / (dt * 60.0);
        let delta = |i: usize| {
            let d = angle_delta(ps.viewangles[i], self.sway_view[i]) * per_frame;
            // The cap, written the way the original's comparisons fall when the cap is not positive.
            let top = if d - max < 0.0 { d } else { max };
            if -max - d < 0.0 { top } else { -max }
        };
        let (d0, d1) = (delta(0), delta(1));
        self.sway_offset[1] = diff_track(d1 * horiz, self.sway_offset[1], lerp, dt);
        self.sway_offset[2] = diff_track(d0 * vert, self.sway_offset[2], lerp, dt);
        self.sway_angles[0] = diff_track_angle(d0 * pitch, self.sway_angles[0], lerp, dt);
        self.sway_angles[1] = diff_track_angle(d1 * yaw, self.sway_angles[1], lerp, dt);
        self.sway_view = ps.viewangles;
    }

    /// Where the gun is relative to the eye, in the view's own axes (forward, left, up), units
    /// (`CalculateWeaponPosition`): the lean's shift, the stance's resting and moving positions and the sway's
    /// offset.
    pub fn position(&mut self, f: &GunFrame<'_>) -> Vec3 {
        let ps = f.ps;
        let mut o = [0.0; 3];
        if ps.leanf != 0.0 && ps.weapon_pos_frac < 1.0 {
            let lean = get_lean_fraction(ps.leanf);
            let (_, right, _) = angle_vectors(&[0.0, 0.0, -(lean + lean)]);
            let dist = (1.0 - ps.weapon_pos_frac) * lean * 1.6;
            for (o, r) in o.iter_mut().zip(right) {
                *o += dist * r;
            }
        }
        self.base_position(f, &mut o);
        o[1] -= self.sway_offset[1];
        o[2] += self.sway_offset[2];
        o
    }

    /// `CalculateWeaponPosition_BasePosition_movement`.
    fn base_position(&mut self, f: &GunFrame<'_>, o: &mut Vec3) {
        let (ps, p) = (f.ps, f.p);
        let s = stance(ps);
        let min = p.move_min_speed[s];
        let moving = min < f.xyspeed;
        let nv = matches!(
            ps.weapon_state,
            weapon_state::NIGHTVISION_WEAR | weapon_state::NIGHTVISION_REMOVE
        );
        let prone = ps.view_height_target == VIEW_PRONE;
        let crouched = ps.view_height_target == VIEW_CROUCH;
        let mut target = [0.0; 3];
        if moving && ps.weapon_state != weapon_state::RELOADING && !nv {
            let scale = ((f.xyspeed - min) / (ps.speed as f32 - min)).clamp(0.0, 1.0);
            target = p.move_ofs[s].map(|v| v * scale);
        }
        if (!moving || !nv || !prone) && (crouched || prone) {
            let mut lerp = 1.0;
            if nv {
                let t = ps.weapon_time as f32 / p.night_vision_wear_time as f32;
                lerp = if t < NV_LERP_END {
                    1.0
                } else if t >= NV_LERP_START {
                    0.0
                } else {
                    1.0 - (t - NV_LERP_END) / (NV_LERP_START - NV_LERP_END)
                };
            }
            let ofs = p.ofs[usize::from(prone)];
            for (t, v) in target.iter_mut().zip(ofs) {
                *t += lerp * v;
            }
        }
        let rate = if ps.view_height_current == 11.0 {
            p.pos_move_rate[1]
        } else {
            p.pos_move_rate[0]
        };
        for (last, t) in self.last_move_org.iter_mut().zip(target) {
            track_toward(last, t, f.frametime, rate);
        }
        let k = match ps.weapon_pos_frac {
            0.0 => 1.0,
            x if x < 0.5 => 1.0 - x * 2.0,
            _ => return,
        };
        for (o, m) in o.iter_mut().zip(self.last_move_org) {
            *o += k * m;
        }
    }
}

/// The last hit the view felt: when (the player state's `command_time` it was first seen at, 0 for none) and the kick
/// at full strength, degrees.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Hit {
    pub time: i32,
    pub pitch: f32,
    pub roll: f32,
}

/// The longest step the view effects take in one go, seconds (`ClientThink_real` caps `msec` at 200).
const MAX_STEP: f32 = 0.2;
/// How long a shell shock takes to fade the sway's scale back, ms (`ClientThink_real`).
const SHOCK_FADE_MS: i32 = 3000;

/// `ssSwayScale`: how much the gun sways more under a shell shock with `left` ms still to run.
pub fn shell_shock_sway_scale(sway_shell_shock_scale: f32, left: i32) -> f32 {
    if left <= 0 {
        return 1.0;
    }
    let t = (left as f32 / SHOCK_FADE_MS as f32).min(1.0);
    let t = (3.0 - t * 2.0) * t * t;
    (sway_shell_shock_scale - 1.0) * t + 1.0
}

/// What the view does besides the aim (`BG_CalculateViewAngles`): the kick of a hit and a scope's idle sway, whose
/// clocks follow the player state's `command_time`. The server steps one per player by each usercmd and the client
/// one by each predicted state, so given the same commands both come to the same offset: shots leave along the view
/// plus these, and the client draws the camera with them.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewFx {
    seen_event: Option<u8>,
    who: u16,
    pub hit: Hit,
    last_time: Option<i32>,
    idle_time: f64,
    idle_factor: f32,
}

impl Default for ViewFx {
    fn default() -> Self {
        Self {
            seen_event: None,
            who: u16::MAX,
            hit: Hit::default(),
            last_time: None,
            idle_time: 0.0,
            idle_factor: 1.0,
        }
    }
}

impl ViewFx {
    /// Takes in a hit `ps` newly shows (`P_DamageFeedback`, `CG_DamageFeedback`): each change of `damage_event` is
    /// seen once, and the first look only learns the count. The kick is the hit's percent of the health pushed along
    /// the direction the state tells, against the state's view.
    pub fn observe(&mut self, ps: &PlayerState) {
        if self.who != ps.client_num {
            // Another player's state: learn it, do not kick for it.
            *self = Self {
                who: ps.client_num,
                ..Self::default()
            };
        }
        let Some(last) = self.seen_event.replace(ps.damage_event) else {
            return;
        };
        if last == ps.damage_event || ps.damage_count == 0 {
            return;
        }
        let kick = crate::pm::damage::view_kick(ps.damage_count);
        let (pitch, roll) = match crate::pm::damage::direction_of(ps.damage_pitch, ps.damage_yaw) {
            None => (-kick, 0.0),
            Some(dir) => {
                let (forward, right, _) = angle_vectors(&ps.viewangles);
                (
                    crate::pm::math::dot(&dir, &forward) * kick,
                    crate::pm::math::dot(&dir, &right) * -kick,
                )
            }
        };
        self.hit = Hit {
            time: ps.command_time.max(1),
            pitch,
            roll,
        };
    }

    /// Advances the scope's idle clock to `ps.command_time`. A state that is not later than the last one moves
    /// nothing.
    pub fn step(&mut self, ps: &PlayerState, p: &GunParams) {
        let dt = match self.last_time {
            Some(t) if ps.command_time > t => ((ps.command_time - t) as f32 * 0.001).min(MAX_STEP),
            _ => 0.0,
        };
        if self.last_time.is_none_or(|t| ps.command_time > t) {
            self.last_time = Some(ps.command_time);
        }
        if !p.overlay_reticle {
            return;
        }
        let ads = ps.weapon_pos_frac;
        let target = idle_stance_factor(ps, p);
        if ads != 0.0 && self.idle_factor != target {
            self.idle_factor = approach(self.idle_factor, target, dt * IDLE_FACTOR_SPEED);
        }
        let (_, speed) = idle_amount_speed(p, ads);
        self.idle_time += f64::from(ps.hold_breath_scale * dt * 1000.0 * speed);
    }

    /// Forgets the hit and the clock (a new life, a view that is not the player's).
    pub fn clear(&mut self) {
        *self = Self {
            who: self.who,
            ..Self::default()
        };
    }

    /// The hit's kick on the view now, pitch and roll degrees (`BG_CalculateView_DamageKick`).
    pub fn damage_kick(&self, ps: &PlayerState, p: &GunParams) -> [f32; 2] {
        if self.hit.time == 0 {
            return [0.0; 2];
        }
        let ads = ps.weapon_pos_frac;
        let mut factor = 1.0 - ads * 0.5;
        if ads != 0.0 && p.overlay_reticle {
            factor *= ads * 0.5 + 1.0;
        }
        let since = (ps.command_time - self.hit.time) as f32;
        let k = if since >= 100.0 {
            let left = 1.0 - (since - 100.0) / 400.0;
            if left <= 0.0 {
                return [0.0; 2];
            }
            (1.0 - get_lean_fraction(1.0 - left)) * factor
        } else {
            get_lean_fraction(since / 100.0) * factor
        };
        [k * self.hit.pitch, k * self.hit.roll]
    }

    /// A scope's idle sway (`BG_CalculateView_IdleAngles`), still while the breath is held: pitch and yaw, degrees.
    pub fn idle(&self, ps: &PlayerState, p: &GunParams) -> [f32; 2] {
        if !p.overlay_reticle {
            return [0.0; 2];
        }
        let ads = ps.weapon_pos_frac;
        let (amount, _) = idle_amount_speed(p, ads);
        let size = amount * self.idle_factor * ads * ps.hold_breath_scale * 0.01;
        [
            (self.idle_time * 0.001).sin() as f32 * size,
            (self.idle_time * 0.0007).sin() as f32 * size,
        ]
    }

    /// The stateful part of `BG_CalculateViewAngles`: the hit's kick and the idle sway, pitch, yaw, roll degrees.
    pub fn angles(&self, ps: &PlayerState, p: &GunParams) -> Vec3 {
        let [pitch, roll] = self.damage_kick(ps, p);
        let [ip, iy] = self.idle(ps, p);
        [pitch + ip, iy, roll]
    }

    /// All of `BG_CalculateViewAngles`: [`angles`](Self::angles) and the bob of the view ([`view_bob`]). Shots leave
    /// along the player's angles plus these.
    pub fn view_angles(&self, ps: &PlayerState, p: &GunParams, xyspeed: f32) -> Vec3 {
        let mut a = self.angles(ps, p);
        add_to(&mut a, &view_bob(ps, xyspeed, p));
        a
    }
}

/// The bob of the view (`BG_CalculateView_BobAngles`, `BG_CalculateView_Velocity`): a scoped weapon bobs by angle
/// while aimed, and an aimed weapon with an `adsViewBobMult` moves with the steps. Degrees.
pub fn view_bob(ps: &PlayerState, xyspeed: f32, p: &GunParams) -> Vec3 {
    let ads = ps.weapon_pos_frac;
    let mut a = [0.0; 3];
    if p.overlay_reticle && ads != 0.0 {
        a = bob_angles(ps, xyspeed, p, false).map(|v| v * ads);
    }
    if ps.e_flags & ef::TURRET_ACTIVE == 0 && ads != 0.0 && p.ads_view_bob_mult != 0.0 {
        let cycle = bob_cycle(ps);
        let k = ads * p.ads_view_bob_mult;
        a[0] -= k * vertical_bob(ps, cycle, xyspeed, ADS_VIEW_BOB_MAX);
        a[1] -= k * horizontal_bob(ps, cycle, xyspeed, ADS_VIEW_BOB_MAX);
    }
    a
}

/// The weapon's idle amount and speed at `ads` aimed.
fn idle_amount_speed(p: &GunParams, ads: f32) -> (f32, f32) {
    if p.aim_down_sight {
        (
            (p.ads_idle_amount - p.hip_idle_amount) * ads + p.hip_idle_amount,
            (p.ads_idle_speed - p.hip_idle_speed) * ads + p.hip_idle_speed,
        )
    } else if p.hip_idle_amount == 0.0 {
        (80.0, 1.0)
    } else {
        (p.hip_idle_amount, p.hip_idle_speed)
    }
}

/// How steady the stance holds the weapon.
fn idle_stance_factor(ps: &PlayerState, p: &GunParams) -> f32 {
    match stance(ps) {
        2 => p.idle_prone_factor,
        1 => p.idle_crouch_factor,
        _ => 1.0,
    }
}

fn approach(cur: f32, target: f32, step: f32) -> f32 {
    if cur >= target {
        (cur - step).max(target)
    } else {
        (cur + step).min(target)
    }
}

fn add_to(a: &mut Vec3, b: &Vec3) {
    for (a, b) in a.iter_mut().zip(b) {
        *a += b;
    }
}

/// The stance's bob as angles (`BG_CalculateWeaponPosition_BobOffset`, and the scoped view's
/// `BG_CalculateView_BobAngles` for `gun` false): the bob amplitude times the ground speed, capped, scaled by the
/// weapon's `adsBobFactor` while aimed. The gun fades it out entirely for a scoped weapon; the view's caller scales it
/// by the aim.
pub fn bob_angles(ps: &PlayerState, xyspeed: f32, p: &GunParams, gun: bool) -> Vec3 {
    use std::f32::consts::{FRAC_PI_4, TAU};
    let ads = ps.weapon_pos_frac;
    let cycle = bob_cycle(ps) + FRAC_PI_4 + TAU;
    let speed = xyspeed * BOB_SPEED;
    let roll = horizontal_bob(ps, cycle - 0.471_238_9, speed * 1.5, BOB_MAX).min(0.0);
    let mut out = [
        -vertical_bob(ps, cycle, speed, BOB_MAX),
        -horizontal_bob(ps, cycle, speed, BOB_MAX),
        roll,
    ];
    if ads != 0.0 {
        let scale = 1.0 - (1.0 - p.ads_bob_factor) * ads;
        out = out.map(|v| v * scale);
    }
    if gun && p.overlay_reticle {
        out = out.map(|v| v * (1.0 - ads));
    }
    out
}

/// One axis of the recoil spring for `dt` seconds (`..._GunRecoil_SingleAngle`); `[cap, accel, speed max, speed
/// decay, static decay]`. Returns whether the axis is at rest.
fn spring(
    offset: &mut f32,
    speed: &mut f32,
    dt: f32,
    [cap, accel, max, decay, rest]: [f32; 5],
) -> bool {
    if offset.abs() < 0.25 && speed.abs() < 1.0 {
        *offset = 0.0;
        *speed = 0.0;
        return true;
    }
    *offset += *speed * dt;
    if *offset > cap {
        *offset = cap;
        *speed = speed.min(0.0);
    } else if *offset < -cap {
        *offset = -cap;
        *speed = speed.max(0.0);
    }
    if *offset > 0.0 {
        *speed -= accel * dt;
    } else if *offset < 0.0 {
        *speed += accel * dt;
    }
    *speed -= *speed * decay * dt;
    if *speed <= 0.0 {
        *speed = (*speed + rest * dt).min(0.0);
    } else {
        *speed = (*speed - rest * dt).max(0.0);
    }
    *speed = speed.clamp(-max, max);
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rifle() -> GunParams {
        GunParams {
            aim_down_sight: true,
            ads_bob_factor: 0.5,
            ads_idle_amount: 40.0,
            hip_idle_amount: 80.0,
            ads_idle_speed: 1.0,
            hip_idle_speed: 1.0,
            idle_crouch_factor: 0.5,
            idle_prone_factor: 0.2,
            gun_kick_accel: [50.0, 50.0],
            gun_kick_speed_max: [400.0, 400.0],
            gun_kick_speed_decay: [3.0, 3.0],
            gun_kick_static_decay: [20.0, 20.0],
            gun_max_pitch: 8.0,
            gun_max_yaw: 8.0,
            sway_max_angle: [30.0, 10.0],
            sway_lerp_speed: [6.0, 10.0],
            sway_pitch_scale: [1.0, 0.5],
            sway_yaw_scale: [1.0, 0.5],
            sway_horiz_scale: [0.2, 0.1],
            sway_vert_scale: [0.2, 0.1],
            sway_shell_shock_scale: 3.0,
            rot: [[0.0; 3]; 3],
            rot_min_speed: [100.0; 3],
            pos_rot_rate: [10.0; 2],
            pos_move_rate: [10.0; 2],
            night_vision_wear_time: 1000,
            ..GunParams::default()
        }
    }

    fn frame<'a>(ps: &'a PlayerState, p: &'a GunParams) -> GunFrame<'a> {
        GunFrame {
            ps,
            p,
            xyspeed: 0.0,
            frametime: 0.016,
            time: 1000,
            hit: Hit::default(),
        }
    }

    /// A kick moves the gun back, the spring pulls it home and it comes to rest exactly at zero; the cap holds.
    #[test]
    fn the_gun_recoil_spring_kicks_and_settles() {
        let params = [8.0, 50.0, 400.0, 3.0, 20.0];
        let (mut offset, mut speed) = (0.0f32, 300.0f32);
        let mut peak = 0.0f32;
        let mut at_rest = false;
        for _ in 0..2000 {
            at_rest = spring(&mut offset, &mut speed, RECOIL_STEP, params);
            peak = peak.max(offset);
            assert!(offset <= params[0]);
            if at_rest {
                break;
            }
        }
        assert!(peak > 1.0, "the gun moved: {peak}");
        assert!(at_rest && offset == 0.0 && speed == 0.0);
    }

    #[test]
    fn a_shot_pitches_the_gun_and_it_returns() {
        let (p, ps) = (rifle(), PlayerState::default());
        let mut g = GunState::default();
        g.kick([-300.0, 40.0]);
        let mut peak = 0.0f32;
        for _ in 0..40 {
            peak = peak.max(g.weapon_angles(&frame(&ps, &p))[0].abs());
        }
        assert!(peak > 1.0, "kicked: {peak}");
        for _ in 0..600 {
            g.weapon_angles(&frame(&ps, &p));
        }
        assert_eq!(g.offset, [0.0; 3], "and settled");
    }

    #[test]
    fn the_frame_rate_does_not_change_where_the_spring_ends() {
        let (p, ps) = (rifle(), PlayerState::default());
        let run = |ft: f32, n: usize| {
            let mut g = GunState::default();
            g.kick([-200.0, 30.0]);
            for _ in 0..n {
                g.weapon_angles(&GunFrame {
                    frametime: ft,
                    ..frame(&ps, &p)
                });
            }
            g.offset
        };
        let (a, b) = (run(0.005, 40), run(0.020, 10));
        for i in 0..2 {
            assert!((a[i] - b[i]).abs() < 1e-3, "{a:?} {b:?}");
        }
    }

    #[test]
    fn a_weapon_that_cannot_aim_has_no_recoil_spring() {
        let p = GunParams {
            aim_down_sight: false,
            ..rifle()
        };
        let ps = PlayerState::default();
        let mut g = GunState::default();
        g.kick([-300.0, 0.0]);
        g.weapon_angles(&frame(&ps, &p));
        assert_eq!(g.offset, [0.0; 3]);
    }

    #[test]
    fn the_gun_breathes_at_the_hip_and_less_aimed_and_crouched() {
        let p = rifle();
        let widest = |ps: &PlayerState| {
            let mut g = GunState::default();
            let mut w = 0.0f32;
            for _ in 0..2000 {
                let a = g.weapon_angles(&frame(ps, &p));
                w = w.max(a[0].abs()).max(a[1].abs()).max(a[2].abs());
            }
            w
        };
        let hip = widest(&PlayerState::default());
        assert!(hip > 0.1, "breathes: {hip}");
        let aimed = widest(&PlayerState {
            weapon_pos_frac: 1.0,
            ..PlayerState::default()
        });
        assert!(aimed < hip, "aimed {aimed} < hip {hip}");
        // Crouched, the factor eases from 1 to the weapon's, so look at the end of the ease.
        let mut g = GunState::default();
        let ps = PlayerState {
            e_flags: ef::CROUCH,
            ..PlayerState::default()
        };
        for _ in 0..300 {
            g.weapon_angles(&frame(&ps, &p));
        }
        assert_eq!(g.last_idle_factor, p.idle_crouch_factor);
    }

    #[test]
    fn a_turning_view_drags_the_gun_behind_and_it_settles() {
        let p = rifle();
        let mut ps = PlayerState::default();
        let mut g = GunState::default();
        let mut lag = 0.0f32;
        for i in 0..20 {
            ps.viewangles[1] = i as f32 * 3.0;
            g.sway(&ps, &p, 1.0, 16);
            lag = lag.max(g.sway_angles[1].abs());
        }
        assert!(lag > 0.5, "the gun lags the turn: {lag}");
        assert!(g.sway_offset[1] != 0.0);
        for _ in 0..100 {
            g.sway(&ps, &p, 1.0, 16);
        }
        assert_eq!(g.sway_angles, [0.0, 0.0, 0.0], "and catches up");
        assert_eq!(g.sway_offset[1], 0.0);
    }

    #[test]
    fn the_sway_is_capped_and_a_shell_shock_scales_it() {
        let p = rifle();
        let sway = |scale: f32, turn: f32| {
            let mut ps = PlayerState::default();
            let mut g = GunState::default();
            ps.viewangles[1] = turn;
            g.sway(&ps, &p, scale, 16);
            g.sway_angles[1].abs()
        };
        // A huge snap turn lags no more than the cap allows; a small one is below it.
        assert_eq!(sway(1.0, 90.0), sway(1.0, 170.0));
        assert!(sway(3.0, 0.2) > sway(1.0, 0.2));
    }

    #[test]
    fn a_scope_aimed_does_not_sway_and_a_frame_of_no_time_changes_nothing() {
        let p = GunParams {
            overlay_reticle: true,
            ..rifle()
        };
        let mut ps = PlayerState {
            weapon_pos_frac: 1.0,
            ..PlayerState::default()
        };
        ps.viewangles[1] = 40.0;
        let mut g = GunState::default();
        g.sway(&ps, &p, 1.0, 16);
        assert_eq!(g, GunState::default());
        ps.weapon_pos_frac = 0.0;
        g.sway(&ps, &p, 1.0, 0);
        assert_eq!(g, GunState::default());
    }

    #[test]
    fn a_hit_turns_the_gun_then_lets_go() {
        let (p, ps) = (rifle(), PlayerState::default());
        let hit = |time: i32| GunFrame {
            time,
            hit: Hit {
                time: 1000,
                pitch: 10.0,
                roll: 6.0,
            },
            ..frame(&ps, &p)
        };
        let g = GunState::default();
        let mut gun = [0.0; 3];
        g.damage_kick(&hit(1050), &mut gun);
        assert!(gun[0] > 0.0 && gun[1] < 0.0 && gun[2] > 0.0, "{gun:?}");
        for time in [999 + 600, 5000] {
            let mut v = [0.0; 3];
            g.damage_kick(&hit(time), &mut v);
            assert_eq!(v, [0.0; 3], "{time}");
        }
    }

    #[test]
    fn a_moving_stance_turns_the_gun_and_a_lean_rolls_and_shifts_it() {
        let p = GunParams {
            rot: [[2.0, -3.0, 4.0]; 3],
            rot_min_speed: [10.0; 3],
            ..rifle()
        };
        let ps = PlayerState {
            speed: 190,
            ..PlayerState::default()
        };
        let mut g = GunState::default();
        for _ in 0..200 {
            g.weapon_angles(&GunFrame {
                xyspeed: 190.0,
                ..frame(&ps, &p)
            });
        }
        assert_eq!(g.last_move_ang, [2.0, -3.0, 4.0]);
        let leaning = PlayerState {
            leanf: 0.5,
            ..ps.clone()
        };
        let rolled = GunState::default().weapon_angles(&frame(&leaning, &rifle()))[2];
        assert!(rolled < -1.0, "leaning right rolls the gun: {rolled}");
        let shift = GunState::default().position(&frame(&leaning, &rifle()));
        assert!(shift[1].abs() > 0.5, "and moves it sideways: {shift:?}");
    }

    #[test]
    fn the_stance_lowers_the_gun_when_crouched() {
        let p = GunParams {
            ofs: [[0.0, 0.0, -2.0], [0.0; 3]],
            ..rifle()
        };
        let ps = PlayerState {
            view_height_target: VIEW_CROUCH,
            ..PlayerState::default()
        };
        let mut g = GunState::default();
        let mut at = [0.0; 3];
        for _ in 0..300 {
            at = g.position(&frame(&ps, &p));
        }
        assert_eq!(at[2], -2.0);
    }

    fn scoped() -> GunParams {
        GunParams {
            overlay_reticle: true,
            ..rifle()
        }
    }

    fn aimed(time: i32, breath: f32) -> PlayerState {
        PlayerState {
            command_time: time,
            weapon_pos_frac: 1.0,
            hold_breath_scale: breath,
            ..PlayerState::default()
        }
    }

    #[test]
    fn a_scoped_view_sways_unless_the_breath_is_held_and_a_plain_one_does_not() {
        let (p, plain) = (scoped(), rifle());
        let mut fx = ViewFx::default();
        let mut widest = 0.0f32;
        for i in 0..600 {
            let ps = aimed(i * 16, 1.0);
            fx.step(&ps, &p);
            let a = fx.angles(&ps, &p);
            widest = widest.max(a[0].abs()).max(a[1].abs());
        }
        assert!(widest > 0.1, "{widest}");
        let held = aimed(9600, 0.0);
        assert_eq!(fx.angles(&held, &p), [0.0; 3]);
        assert_eq!(fx.angles(&aimed(9600, 1.0), &plain), [0.0; 3]);
    }

    fn hit_ps(time: i32, event: u8) -> PlayerState {
        PlayerState {
            command_time: time,
            damage_event: event,
            damage_count: 40,
            // From straight ahead: the yaw and pitch of -x, 1/255 turns.
            damage_pitch: 0,
            damage_yaw: 128,
            ..PlayerState::default()
        }
    }

    #[test]
    fn a_hit_is_seen_once_kicks_the_view_and_lets_go() {
        let p = rifle();
        let mut fx = ViewFx::default();
        fx.observe(&hit_ps(1000, 3));
        assert_eq!(
            fx.hit,
            Hit::default(),
            "the first look only learns the count"
        );
        fx.observe(&hit_ps(2000, 4));
        let time = fx.hit.time;
        assert_eq!(time, 2000);
        fx.observe(&hit_ps(2100, 4));
        assert_eq!(fx.hit.time, time, "the same hit again is not a new one");
        let mid = fx.damage_kick(&hit_ps(2050, 4), &p);
        assert!(mid[0] != 0.0, "{mid:?}");
        assert_eq!(fx.damage_kick(&hit_ps(2700, 4), &p), [0.0; 2]);
    }

    /// The server steps by every command, the client by the states it predicts or is told: the same commands make
    /// the same offset.
    #[test]
    fn the_server_and_the_client_come_to_the_same_view_offset() {
        let p = scoped();
        let states: Vec<PlayerState> = (0..120)
            .map(|i| {
                let mut ps = aimed(i * 8, 0.8);
                ps.damage_event = u8::from(i >= 40);
                ps.damage_count = 30;
                ps.damage_pitch = 0;
                ps.damage_yaw = 64;
                ps
            })
            .collect();
        let (mut server, mut client) = (ViewFx::default(), ViewFx::default());
        for (i, ps) in states.iter().enumerate() {
            server.observe(ps);
            server.step(ps, &p);
            // The client sees every third state, and the one the hit first shows in.
            if i % 3 == 0 || i == 40 {
                client.observe(ps);
                client.step(ps, &p);
                if i % 3 == 0 {
                    let (a, b) = (
                        server.view_angles(ps, &p, 0.0),
                        client.view_angles(ps, &p, 0.0),
                    );
                    for k in 0..3 {
                        assert!((a[k] - b[k]).abs() < 1e-4, "{i}: {a:?} {b:?}");
                    }
                }
            }
        }
        assert!(server.hit.time != 0);
    }

    #[test]
    fn a_shell_shock_swings_the_gun_more_and_fades() {
        assert_eq!(shell_shock_sway_scale(3.0, 0), 1.0);
        assert_eq!(shell_shock_sway_scale(3.0, 5000), 3.0);
        let mid = shell_shock_sway_scale(3.0, 1500);
        assert!(mid > 1.0 && mid < 3.0, "{mid}");
    }
}
