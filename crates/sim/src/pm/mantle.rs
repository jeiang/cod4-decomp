// SPDX-License-Identifier: GPL-3.0-or-later
//! Ledge mantling (`Mantle_*`).
//!
//! A mantle is found by tracing forward against `MANTLE` contents, then probing ledge heights
//! 60, 40 and 20 units up. Starting one picks the animation pair whose height is closest and
//! moves the player along that animation's root motion, frame by frame, until its length has
//! elapsed. The root-motion tracks come from the install's `mp_mantle_*` animations, see
//! [`MantleAnims`].

use super::math::{self, dot, mad, normalize, sub};
use super::state::{PmType, ef, ev, pmf};
use super::{Pml, Pmove, PLAYER_MAXS, PLAYER_MINS};
use crate::Vec3;
use crate::cm::Collide;
use crate::contents;
use assets::zone::xanim::{Indices, TransFrames, XAnimParts};

/// Animation slots: root, 7 "up", 3 "over".
pub const MANTLE_ANIM_COUNT: usize = 11;
const UP_FIRST: usize = 1;
const OVER_LOW: usize = 10;

/// Names of the mantle animation assets, in slot order (multiplayer set; the third "over"
/// animation really is `player_mantle_over_low` in the original).
pub const MANTLE_ANIM_NAMES: [&str; MANTLE_ANIM_COUNT] = [
    "mp_mantle_root",
    "mp_mantle_up_57",
    "mp_mantle_up_51",
    "mp_mantle_up_45",
    "mp_mantle_up_39",
    "mp_mantle_up_33",
    "mp_mantle_up_27",
    "mp_mantle_up_21",
    "mp_mantle_over_high",
    "mp_mantle_over_mid",
    "player_mantle_over_low",
];

/// One up/over animation pair and the ledge height it climbs (`s_mantleTrans`).
#[derive(Debug, Clone, Copy)]
pub struct MantleTransition {
    pub up_anim: usize,
    pub over_anim: usize,
    pub height: f32,
}

pub const TRANSITIONS: [MantleTransition; 7] = [
    MantleTransition { up_anim: 1, over_anim: 8, height: 57.0 },
    MantleTransition { up_anim: 2, over_anim: 8, height: 51.0 },
    MantleTransition { up_anim: 3, over_anim: 9, height: 45.0 },
    MantleTransition { up_anim: 4, over_anim: 9, height: 39.0 },
    MantleTransition { up_anim: 5, over_anim: 9, height: 33.0 },
    MantleTransition { up_anim: 6, over_anim: 10, height: 27.0 },
    MantleTransition { up_anim: 7, over_anim: 10, height: 21.0 },
];

/// Root-motion track of one mantle animation: the accumulated translation at every frame.
#[derive(Debug, Clone, Default)]
pub struct MantleAnim {
    /// `(int)(numframes / framerate * 1000)`.
    pub length_msec: i32,
    /// Translation after frame `i`, for `i` in `0..=numframes`.
    pub samples: Vec<Vec3>,
}

impl MantleAnim {
    /// Translation reached at `frac` (0..=1) of the animation (`XAnimGetAbsDelta`).
    pub fn abs_delta(&self, frac: f32) -> Vec3 {
        let n = self.samples.len();
        if n == 0 {
            return [0.0; 3];
        }
        if frac >= 1.0 || n == 1 {
            return self.samples[n - 1];
        }
        let frames = (n - 1) as f32;
        let at = frames * frac;
        let i = (at as usize).min(n - 2);
        math::lerp(&self.samples[i], &self.samples[i + 1], at - i as f32)
    }

    /// Samples the root-delta translation of decoded animation parts.
    pub fn from_xanim(parts: &XAnimParts) -> Self {
        let frames = usize::from(parts.num_frames);
        let length = (f64::from(parts.num_frames) / f64::from(parts.frame_rate)) as f32;
        let length_msec = (f64::from(length) * 1000.0) as i32;
        let trans = parts.delta.as_ref().and_then(|d| d.trans.as_ref());
        let Some(t) = trans else {
            return Self { length_msec, samples: vec![[0.0; 3]; frames + 1] };
        };
        if t.size == 0 {
            return Self { length_msec, samples: vec![t.frame0; frames + 1] };
        }
        let n = usize::from(t.size) + 1;
        let key_frame = |i: usize| -> f32 {
            match &t.indices {
                Indices::Byte(b) => f32::from(b[i]),
                Indices::Short(s) => f32::from(s[i]),
                Indices::None => i as f32,
            }
        };
        let key_value = |i: usize| -> Vec3 {
            let q = match &t.frames {
                TransFrames::Byte(f) => f[i].map(f32::from),
                TransFrames::Short(f) => f[i].map(f32::from),
                TransFrames::None => [0.0; 3],
            };
            [
                t.extent[0] * q[0] + t.mins[0],
                t.extent[1] * q[1] + t.mins[1],
                t.extent[2] * q[2] + t.mins[2],
            ]
        };
        let mut samples = Vec::with_capacity(frames + 1);
        let mut k = 0;
        for f in 0..=frames {
            let f = f as f32;
            while k + 1 < n - 1 && key_frame(k + 1) <= f {
                k += 1;
            }
            let (a, b) = (key_frame(k), key_frame(k + 1));
            samples.push(if f >= key_frame(n - 1) {
                key_value(n - 1)
            } else {
                math::lerp(&key_value(k), &key_value(k + 1), (f - a) / (b - a))
            });
        }
        Self { length_msec, samples }
    }
}

/// The mantle animation set (index 0, the root, is unused).
#[derive(Debug, Clone, Default)]
pub struct MantleAnims {
    anims: [MantleAnim; MANTLE_ANIM_COUNT],
}

impl MantleAnims {
    pub fn new(anims: [MantleAnim; MANTLE_ANIM_COUNT]) -> Self {
        Self { anims }
    }

    /// Builds the set from decoded animations looked up by [`MANTLE_ANIM_NAMES`]; `None` when
    /// one is missing.
    pub fn from_xanims<'p>(mut find: impl FnMut(&str) -> Option<&'p XAnimParts>) -> Option<Self> {
        let mut anims: [MantleAnim; MANTLE_ANIM_COUNT] = Default::default();
        for (slot, name) in MANTLE_ANIM_NAMES.iter().enumerate().skip(UP_FIRST) {
            anims[slot] = MantleAnim::from_xanim(find(name)?);
        }
        Some(Self { anims })
    }

    pub fn anim(&self, slot: usize) -> &MantleAnim {
        &self.anims[slot]
    }

    fn up_length(&self, trans_index: i32) -> i32 {
        self.anims[TRANSITIONS[trans_index as usize].up_anim].length_msec
    }

    fn over_length(&self, trans_index: i32, flags: u32) -> i32 {
        if flags & FLAG_OVER != 0 {
            self.anims[TRANSITIONS[trans_index as usize].over_anim].length_msec
        } else {
            0
        }
    }

    /// Total mantle duration for a transition (`Mantle_GetUpLength + Mantle_GetOverLength`).
    pub fn duration(&self, trans_index: i32, over: bool) -> i32 {
        self.up_length(trans_index) + self.over_length(trans_index, if over { FLAG_OVER } else { 0 })
    }

    /// Translation `time` ms into the mantle, in world orientation (`Mantle_GetAnimDelta`).
    fn delta(&self, trans_index: i32, flags: u32, yaw: f32, time: i32) -> Vec3 {
        let t = &TRANSITIONS[trans_index as usize];
        let up_time = self.up_length(trans_index);
        let over_time = self.over_length(trans_index, flags);
        let mut d = if time > up_time {
            let frac = (time - up_time) as f32 / over_time as f32;
            let up = self.anims[t.up_anim].abs_delta(1.0);
            let over = self.anims[t.over_anim].abs_delta(frac);
            math::add(&over, &up)
        } else {
            self.anims[t.up_anim].abs_delta(time as f32 / up_time as f32)
        };
        let (s, c) = math::sincos_deg(yaw);
        let x = d[0] * c - d[1] * s;
        d[1] = d[1] * c + d[0] * s;
        d[0] = x;
        d
    }
}

// MantleState.flags
const FLAG_OVER: u32 = 1;
const FLAG_FORCE_CROUCH: u32 = 2;
const FLAG_STAND_AFTER: u32 = 4;
const FLAG_HINT: u32 = 8;
const FLAG_DONE: u32 = 0x10;

/// Surface flags the mantle brushes carry.
const SURF_MANTLEON: i32 = 0x0200_0000;
const SURF_MANTLEOVER: i32 = 0x0400_0000;

/// What a successful probe found (`MantleResults`).
struct Found {
    dir: Vec3,
    start_pos: Vec3,
    ledge_pos: Vec3,
    end_pos: Vec3,
    flags: u32,
}

/// `Mantle_ClearHint`.
pub(super) fn clear_hint(pm: &mut Pmove<'_>) {
    pm.ps.mantle_state.flags &= !FLAG_HINT;
}

/// `Mantle_IsWeaponInactive`: the weapon is lowered while climbing over a high ledge.
pub fn weapon_inactive(pm: &Pmove<'_>) -> bool {
    pm.params.mantle_enable
        && pm.ps.pm_flags & pmf::MANTLE != 0
        && TRANSITIONS[pm.ps.mantle_state.trans_index as usize].over_anim != OVER_LOW
}

/// `Mantle_Check`: starts a mantle when the player is facing a ledge and pressing jump.
pub(super) fn check(pm: &mut Pmove<'_>, world: &dyn Collide, pml: &Pml) {
    pm.ps.mantle_state.flags &= !FLAG_DONE;
    if !pm.params.mantle_enable {
        return;
    }
    clear_hint(pm);
    if pm.ps.pm_type >= PmType::Dead || pm.ps.pm_flags & pmf::MANTLE != 0 {
        return;
    }
    if pm.ps.e_flags & (ef::CROUCH | ef::PRONE) != 0 {
        return;
    }
    let Some((dir, surface_flags)) = find_surface(pm, world, pml) else {
        return;
    };
    let mut found = Found {
        dir,
        start_pos: pm.ps.origin,
        ledge_pos: [0.0; 3],
        end_pos: [0.0; 3],
        flags: if surface_flags & SURF_MANTLEOVER != 0 { FLAG_OVER } else { 0 },
    };
    let _ = check_ledge(pm, world, &mut found, 60.0)
        || check_ledge(pm, world, &mut found, 40.0)
        || check_ledge(pm, world, &mut found, 20.0);
}

/// `Mantle_FindMantleSurface`: the direction of a mantle brush in front of the player and its
/// surface flags.
fn find_surface(pm: &Pmove<'_>, world: &dyn Collide, pml: &Pml) -> Option<(Vec3, i32)> {
    let radius = pm.params.mantle_check_radius;
    let mins = [-radius, -radius, 0.0];
    let maxs = [radius, radius, 70.0];
    let inner = 15.0 - radius;
    let dist = pm.params.mantle_check_range + inner;
    let mut trace_dir = [pml.forward[0], pml.forward[1], 0.0];
    normalize(&mut trace_dir);
    let start = mad(&pm.ps.origin, -inner, &trace_dir);
    let end = mad(&pm.ps.origin, dist, &trace_dir);
    let t = pm.trace(world, start, mins, maxs, end, contents::MANTLE);
    if t.start_solid || t.all_solid || t.fraction == 1.0 {
        return None;
    }
    if t.surface_flags & (SURF_MANTLEOVER | SURF_MANTLEON) == 0 {
        return None;
    }
    let mut dir = [-t.normal[0], -t.normal[1], 0.0];
    if normalize(&mut dir) < 0.0001 {
        return None;
    }
    if pm.params.mantle_check_angle >= math::acos_deg(dot(&trace_dir, &dir)) {
        Some((dir, t.surface_flags))
    } else {
        None
    }
}

/// `Mantle_CheckLedge`: probes a ledge `height` up; on a jump press, starts the mantle.
fn check_ledge(pm: &mut Pmove<'_>, world: &dyn Collide, f: &mut Found, height: f32) -> bool {
    let mins = [-15.0, -15.0, 0.0];
    let mut maxs = [15.0, 15.0, 30.0];
    let mut start = f.start_pos;
    start[2] += height;
    let mut end = mad(&start, 16.0, &f.dir);
    let t = pm.trace(world, start, mins, maxs, end, pm.tracemask);
    if t.start_solid || t.fraction < 1.0 {
        return false;
    }
    start = end;
    end[2] = f.start_pos[2] + 18.0;
    let t = pm.trace(world, start, mins, maxs, end, pm.tracemask);
    if t.start_solid || t.fraction == 1.0 || !t.walkable {
        return false;
    }
    f.ledge_pos = [end[0], end[1], (end[2] - start[2]) * t.fraction + start[2]];
    maxs[2] = 50.0;
    if pm.trace(world, f.ledge_pos, mins, maxs, f.ledge_pos, pm.tracemask).start_solid {
        return false;
    }
    pm.ps.mantle_state.flags |= FLAG_HINT;
    f.flags |= FLAG_HINT;
    if pm.cmd.buttons & super::button::JUMP != 0 {
        calc_end_pos(pm, world, f);
        if pm.ps.e_flags & ef::CROUCH == 0 {
            let at_ledge = pm.trace(world, f.ledge_pos, PLAYER_MINS, PLAYER_MAXS, f.ledge_pos, pm.tracemask);
            if at_ledge.start_solid {
                f.flags |= FLAG_FORCE_CROUCH;
            }
            let at_end = pm.trace(world, f.end_pos, PLAYER_MINS, PLAYER_MAXS, f.end_pos, pm.tracemask);
            if !at_end.start_solid {
                f.flags |= FLAG_STAND_AFTER;
            }
        }
        start_mantle(pm, f);
    }
    true
}

/// `Mantle_CalcEndPos`: where an "over" mantle lands, or the ledge itself.
fn calc_end_pos(pm: &Pmove<'_>, world: &dyn Collide, f: &mut Found) {
    f.end_pos = f.ledge_pos;
    if f.flags & FLAG_OVER == 0 {
        return;
    }
    let mins = [-15.0, -15.0, 0.0];
    let maxs = [15.0, 15.0, 50.0];
    let mut end = mad(&f.ledge_pos, 31.0, &f.dir);
    let t = pm.trace(world, f.ledge_pos, mins, maxs, end, pm.tracemask);
    if t.start_solid || t.fraction < 1.0 {
        f.flags &= !FLAG_OVER;
        return;
    }
    let start = end;
    end[2] -= 18.0;
    let t = pm.trace(world, start, mins, maxs, end, pm.tracemask);
    if t.start_solid || t.fraction < 1.0 {
        f.flags &= !FLAG_OVER;
        return;
    }
    f.end_pos = [end[0], end[1], (end[2] - start[2]) * t.fraction + start[2]];
}

/// `Mantle_FindTransition`: the animation pair whose height is nearest the climb.
fn find_transition(cur_height: f32, goal_height: f32) -> i32 {
    let height = goal_height - cur_height;
    let mut best = 0;
    let mut best_diff = (TRANSITIONS[0].height - height).abs();
    for (i, t) in TRANSITIONS.iter().enumerate().skip(1) {
        let d = (t.height - height).abs();
        if best_diff > d {
            best = i;
            best_diff = d;
        }
    }
    best as i32
}

/// `Mantle_Start`: places the player at the animation's start offset and flags the mantle.
fn start_mantle(pm: &mut Pmove<'_>, f: &Found) {
    let Some(anims) = pm.params.mantle_anims.as_ref() else {
        return;
    };
    let yaw = math::vec_to_yaw(&f.dir);
    let trans_index = find_transition(f.start_pos[2], f.ledge_pos[2]);
    let flags = f.flags;
    let duration = anims.up_length(trans_index) + anims.over_length(trans_index, flags);
    let total = anims.delta(trans_index, flags, yaw, duration);
    pm.ps.mantle_state.yaw = yaw;
    pm.ps.mantle_state.timer = 0;
    pm.ps.mantle_state.trans_index = trans_index;
    pm.ps.mantle_state.flags = flags;
    pm.ps.origin = sub(&f.end_pos, &total);
    pm.ps.pm_flags |= pmf::MANTLE;
    pm.ps.e_flags |= ef::MANTLE;
    pm.mantle_end_pos = f.end_pos;
    pm.mantle_duration = duration;
    pm.mantle_started = true;
}

/// `Mantle_Move`: advances the climb by this step's time.
pub(super) fn advance(pm: &mut Pmove<'_>, pml: &Pml) {
    if !pm.params.mantle_enable {
        return;
    }
    let Some(anims) = pm.params.mantle_anims.as_ref() else {
        return;
    };
    let ms = pm.ps.mantle_state;
    pm.ps.mantle_state.flags &= !FLAG_HINT;
    if ms.flags & FLAG_FORCE_CROUCH != 0 {
        pm.ps.add_event(ev::STANCE_FORCE_CROUCH, 0);
    }
    let length = anims.up_length(ms.trans_index) + anims.over_length(ms.trans_index, ms.flags);
    let prev_time = ms.timer;
    let timer = (ms.timer + pml.msec).min(length);
    let delta_time = timer - prev_time;
    let prev = anims.delta(ms.trans_index, ms.flags, ms.yaw, prev_time);
    let cur = anims.delta(ms.trans_index, ms.flags, ms.yaw, timer);
    let step = sub(&cur, &prev);
    pm.ps.origin = math::add(&step, &pm.ps.origin);
    let inv = 1.0 / (delta_time as f32 * math::EQUAL_EPSILON);
    pm.ps.velocity = math::scale(&step, inv);
    pm.ps.mantle_state.timer = timer;
    if timer == length {
        pm.ps.pm_flags &= !pmf::MANTLE;
        pm.mantle_started = false;
        if ms.flags & FLAG_STAND_AFTER != 0 {
            pm.ps.add_event(ev::STANCE_FORCE_STAND, 0);
            pm.ps.e_flags &= !ef::MANTLE;
        }
        pm.ps.mantle_state.flags |= FLAG_DONE;
    }
}

/// `Mantle_CapView`: keeps the view within `mantle_view_yawcap` of the climb direction.
pub(super) fn cap_view(pm: &mut Pmove<'_>) {
    if !pm.params.mantle_enable {
        return;
    }
    let cap = pm.params.mantle_view_yawcap;
    let mut delta = math::angle_delta(pm.ps.mantle_state.yaw, pm.ps.viewangles[1]);
    if delta < -cap || cap < delta {
        while delta < -cap {
            delta += cap;
        }
        while cap < delta {
            delta -= cap;
        }
        let value = if delta <= 0.0 { cap } else { -cap };
        pm.ps.delta_angles[1] += delta;
        pm.ps.viewangles[1] = math::angle_normalize_360(pm.ps.mantle_state.yaw + value);
    }
}
