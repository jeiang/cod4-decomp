// SPDX-License-Identifier: GPL-3.0-or-later
//! Script movers: `moveto`, `rotateto` and friends as trajectories the frame loop advances.
//!
//! A move is up to three phases: accelerate for `accel` seconds, cruise, decelerate for
//! `decel` seconds. [`setup_move`] starts the first phase and records the phase boundaries in
//! [`Channel`]; [`update_move`] starts the next phase when one ends. Position and angles are
//! two channels with the same machinery.

use gsc::{EntClass, EntRef, Value, Vm};
use sim::Vec3;
use sim::traj::{TrType, Trajectory};

use crate::game::Game;
use crate::script::Args;

/// One animated quantity of a mover (its origin or its angles).
#[derive(Debug, Clone, Copy, Default)]
pub struct Channel {
    pub tr: Trajectory,
    pub speed: f32,
    /// Seconds of the cruise phase.
    pub mid_time: f32,
    pub decel_time: f32,
    /// Start of the cruise, end of the cruise, final target.
    pub p1: Vec3,
    pub p2: Vec3,
    pub p3: Vec3,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Mover {
    pub pos: Channel,
    pub ang: Channel,
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: Vec3, s: f32) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn length(a: Vec3) -> f32 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

fn normalized(a: Vec3) -> Vec3 {
    let l = length(a);
    if l == 0.0 {
        [0.0; 3]
    } else {
        scale(a, 1.0 / l)
    }
}

fn ms(seconds: f32) -> i32 {
    (seconds * 1000.0) as i32
}

/// The `total [accel [decel]]` arguments of a move command, from argument `first`.
pub fn command_times(args: &Args, first: usize) -> Result<(f32, f32, f32), String> {
    let err = |i: usize, m: &str| format!("parameter {}: {m}", first + i + 1);
    let mut total = args.float(first)?;
    if total <= 0.0 {
        return Err(err(0, "total time must be positive"));
    }
    if total < 0.001 {
        total = 0.001;
    }
    let accel = if args.len() > first + 1 {
        args.float(first + 1)?
    } else {
        0.0
    };
    if accel < 0.0 {
        return Err(err(1, "accel time must be nonnegative"));
    }
    let decel = if args.len() > first + 2 {
        args.float(first + 2)?
    } else {
        0.0
    };
    if decel < 0.0 {
        return Err(err(2, "decel time must be nonnegative"));
    }
    if total < accel + decel {
        let up = total * 1.000_000_5;
        if up < accel + decel {
            return Err("accel time plus decel time is greater than total time".into());
        }
        total = up;
    }
    Ok((total, accel, decel))
}

/// `ScriptMover_SetupMove`: start moving `curr` to `to` at level time `now`.
pub fn setup_move(
    ch: &mut Channel,
    to: Vec3,
    (total, accel, decel): (f32, f32, f32),
    curr: &mut Vec3,
    now: i32,
) {
    let mv = sub(to, *curr);
    if ch.tr.kind != TrType::Stationary {
        *curr = ch.tr.evaluate(now);
    }
    if accel == 0.0 && decel == 0.0 {
        ch.tr.time = now;
        ch.tr.duration = ms(total);
        ch.mid_time = total;
        ch.decel_time = 0.0;
        ch.p3 = to;
        ch.tr.base = *curr;
        ch.tr.delta = scale(mv, 1000.0 / ch.tr.duration as f32);
        ch.tr.kind = TrType::LinearStop;
        *curr = ch.tr.evaluate(now);
        return;
    }
    ch.mid_time = total - accel - decel;
    ch.decel_time = decel;
    ch.speed = length(mv) * 2.0 / (total * 2.0 - accel - decel);
    let max_speed = scale(normalized(mv), ch.speed);
    if accel == 0.0 {
        ch.p1 = *curr;
        ch.tr.time = now;
        ch.tr.base = *curr;
        if ch.mid_time == 0.0 {
            ch.tr.duration = ms(decel);
            ch.tr.delta = max_speed;
            ch.tr.kind = TrType::Decelerate;
        } else {
            ch.tr.duration = ms(ch.mid_time);
            ch.tr.delta = scale(
                scale(max_speed, ch.mid_time),
                1000.0 / ch.tr.duration as f32,
            );
            ch.tr.kind = TrType::LinearStop;
        }
    } else {
        ch.tr.time = now;
        ch.tr.duration = ms(accel);
        ch.tr.base = *curr;
        ch.tr.delta = max_speed;
        ch.tr.kind = TrType::Accelerate;
        ch.p1 = ch.tr.evaluate(ch.tr.duration + now);
    }
    ch.p2 = [
        ch.p1[0] + ch.mid_time * max_speed[0],
        ch.p1[1] + ch.mid_time * max_speed[1],
        ch.p1[2] + ch.mid_time * max_speed[2],
    ];
    ch.p3 = to;
    *curr = ch.tr.evaluate(now);
}

/// `ScriptMover_SetupMoveSpeed` (`rotatevelocity`): move at constant `velocity` for `total`
/// seconds, with optional ramp up and down.
pub fn setup_move_speed(
    ch: &mut Channel,
    velocity: Vec3,
    (total, accel, decel): (f32, f32, f32),
    curr: &mut Vec3,
    now: i32,
) {
    if ch.tr.kind != TrType::Stationary {
        *curr = ch.tr.evaluate(now);
    }
    if accel == 0.0 && decel == 0.0 {
        ch.tr.time = now;
        ch.tr.duration = ms(total);
        ch.mid_time = total;
        ch.decel_time = 0.0;
        ch.tr.base = *curr;
        ch.tr.delta = velocity;
        ch.tr.kind = TrType::LinearStop;
        *curr = ch.tr.evaluate(now);
        ch.p3 = ch.tr.evaluate(ch.tr.duration + now);
        return;
    }
    ch.mid_time = total - accel - decel;
    ch.decel_time = decel;
    ch.speed = length(velocity);
    ch.tr.time = now;
    ch.tr.base = *curr;
    ch.tr.delta = velocity;
    if accel == 0.0 {
        ch.p1 = *curr;
        if ch.mid_time == 0.0 {
            ch.tr.duration = ms(decel);
            ch.tr.kind = TrType::Decelerate;
        } else {
            ch.tr.duration = ms(ch.mid_time);
            ch.tr.kind = TrType::LinearStop;
        }
    } else {
        ch.tr.duration = ms(accel);
        ch.tr.kind = TrType::Accelerate;
        ch.p1 = ch.tr.evaluate(ch.tr.duration + now);
    }
    ch.p2 = [
        ch.p1[0] + ch.mid_time * velocity[0],
        ch.p1[1] + ch.mid_time * velocity[1],
        ch.p1[2] + ch.mid_time * velocity[2],
    ];
    ch.p3 = if decel == 0.0 {
        ch.p2
    } else {
        Trajectory {
            kind: TrType::Decelerate,
            time: now,
            duration: ms(decel),
            base: ch.p2,
            delta: velocity,
        }
        .evaluate(ms(decel) + now)
    };
    *curr = ch.tr.evaluate(now);
}

/// `ScriptMover_UpdateMove`, called once the current phase has run its duration. Returns true
/// when the whole move is finished.
pub fn update_move(ch: &mut Channel, now: i32) -> bool {
    let mid = ms(ch.mid_time);
    if ch.tr.kind == TrType::Accelerate && mid > 0 {
        ch.tr.time = now;
        ch.tr.duration = mid;
        ch.tr.base = ch.p1;
        ch.tr.delta = scale(sub(ch.p2, ch.p1), 1000.0 / mid as f32);
        ch.tr.kind = TrType::LinearStop;
        return false;
    }
    if matches!(ch.tr.kind, TrType::Accelerate | TrType::LinearStop) && ch.decel_time > 0.0 {
        ch.tr.time = now;
        ch.tr.duration = ms(ch.decel_time);
        ch.tr.base = ch.p2;
        ch.tr.delta = scale(normalized(sub(ch.p3, ch.p2)), ch.speed);
        ch.tr.kind = TrType::Decelerate;
        return false;
    }
    ch.tr.base = if ch.tr.kind == TrType::Gravity {
        ch.tr.evaluate(now)
    } else {
        ch.p3
    };
    ch.tr.time = now;
    ch.tr.kind = TrType::Stationary;
    true
}

/// `ScriptMover_GravityMove`: fly with `velocity` under gravity for `seconds`.
pub fn gravity_move(ch: &mut Channel, curr: &mut Vec3, velocity: Vec3, seconds: f32, now: i32) {
    ch.tr = Trajectory {
        kind: TrType::Gravity,
        time: now,
        duration: ms(seconds),
        base: *curr,
        delta: velocity,
    };
    *curr = ch.tr.evaluate(now);
}

impl Channel {
    /// True when the trajectory has run out at `now`.
    pub fn finished(&self, now: i32) -> bool {
        self.tr.kind != TrType::Stationary && now >= self.tr.time + self.tr.duration
    }
}

// ---- script commands ----

type R = Result<Value, String>;

/// The receiver check every mover command makes.
fn mover_ent(g: &Game, e: EntRef) -> Result<u16, String> {
    if e.class != EntClass::Entity {
        return Err("not an entity".into());
    }
    let ent = g.ent(e.num).ok_or("not an entity")?;
    match &*ent.classname {
        "script_brushmodel" | "script_model" | "script_origin" | "light" => Ok(e.num),
        _ => Err(format!(
            "entity {} is not a script_brushmodel, script_model, script_origin, or light",
            e.num
        )),
    }
}

fn start(g: &mut Game, n: u16, to: Vec3, t: (f32, f32, f32), rotate: bool) {
    let now = g.level.time;
    let Some(ent) = g.ent_mut(n) else { return };
    if rotate {
        let mut cur = ent.angles;
        setup_move(&mut ent.mv.ang, to, t, &mut cur, now);
        ent.angles = cur;
    } else {
        let mut cur = ent.origin;
        setup_move(&mut ent.mv.pos, to, t, &mut cur, now);
        ent.origin = cur;
    }
    g.relink(n);
}

pub fn move_to(g: &mut Game, e: EntRef, a: Args) -> R {
    let n = mover_ent(g, e)?;
    let to = a.vector(0)?;
    start(g, n, to, command_times(&a, 1)?, false);
    Ok(Value::Undefined)
}

pub fn move_axis(g: &mut Game, e: EntRef, a: Args, axis: usize) -> R {
    let n = mover_ent(g, e)?;
    let d = a.float(0)?;
    let t = command_times(&a, 1)?;
    let mut to = g.ent(n).map_or([0.0; 3], |e| e.origin);
    to[axis] += d;
    start(g, n, to, t, false);
    Ok(Value::Undefined)
}

pub fn move_gravity(g: &mut Game, e: EntRef, a: Args) -> R {
    let n = mover_ent(g, e)?;
    let v = a.vector(0)?;
    if v.iter().any(|c| !c.is_finite()) {
        return Err(format!(
            "invalid velocity parameter in movegravity command: {} {} {}",
            v[0], v[1], v[2]
        ));
    }
    let secs = a.float(1)?;
    let now = g.level.time;
    if let Some(ent) = g.ent_mut(n) {
        let mut cur = ent.origin;
        gravity_move(&mut ent.mv.pos, &mut cur, v, secs, now);
        ent.origin = cur;
    }
    g.relink(n);
    Ok(Value::Undefined)
}

/// `AngleNormalize360`.
fn angle_360(a: f32) -> f32 {
    let v = a * (1.0 / 360.0);
    (v - v.floor()) * 360.0
}

/// `AngleDelta(to, from)`: the signed shortest way round.
fn angle_delta(to: f32, from: f32) -> f32 {
    let d = angle_360(to - from);
    if d > 180.0 { d - 360.0 } else { d }
}

pub fn rotate_to(g: &mut Game, e: EntRef, a: Args) -> R {
    let n = mover_ent(g, e)?;
    let dest = a.vector(0)?;
    let t = command_times(&a, 1)?;
    let cur = g.ent(n).map_or([0.0; 3], |e| e.angles);
    let to = [0, 1, 2].map(|i| angle_delta(dest[i], cur[i]) + cur[i]);
    start(g, n, to, t, true);
    Ok(Value::Undefined)
}

pub fn rotate_axis(g: &mut Game, e: EntRef, a: Args, axis: usize) -> R {
    let n = mover_ent(g, e)?;
    let d = a.float(0)?;
    let t = command_times(&a, 1)?;
    let mut to = g.ent(n).map_or([0.0; 3], |e| e.angles);
    to[axis] += d;
    start(g, n, to, t, true);
    Ok(Value::Undefined)
}

pub fn rotate_velocity(g: &mut Game, e: EntRef, a: Args) -> R {
    let n = mover_ent(g, e)?;
    let v = a.vector(0)?;
    let t = command_times(&a, 1)?;
    let now = g.level.time;
    if let Some(ent) = g.ent_mut(n) {
        let mut cur = ent.angles;
        setup_move_speed(&mut ent.mv.ang, v, t, &mut cur, now);
        ent.angles = cur;
    }
    g.relink(n);
    Ok(Value::Undefined)
}

// ---- frame ----

impl Game {
    /// `G_RunMover` for a script mover whose trajectory ran out or is still running: advance
    /// origin and angles to the level time and fire `movedone` / `rotatedone`.
    pub fn run_mover(&mut self, vm: &mut Vm, n: u16) {
        if self.is_linked(n) {
            self.follow_link(vm, n);
            return;
        }
        if self.ent(n).is_some_and(|e| e.x.corpse.is_some()) {
            self.run_corpse(n);
            return;
        }
        let now = self.level.time;
        let Some(ent) = self.ent_mut(n) else { return };
        if ent.mv.pos.tr.kind == TrType::Stationary && ent.mv.ang.tr.kind == TrType::Stationary {
            return;
        }
        if ent.mv.pos.tr.kind != TrType::Stationary {
            ent.origin = ent.mv.pos.tr.evaluate(now);
        }
        if ent.mv.ang.tr.kind != TrType::Stationary {
            ent.angles = ent.mv.ang.tr.evaluate(now);
        }
        self.relink(n);
        if self.ent(n).is_some_and(|e| e.mv.pos.finished(now)) {
            let ent = self.ent_mut(n).expect("checked above");
            let done = update_move(&mut ent.mv.pos, now);
            ent.origin = ent.mv.pos.tr.evaluate(now);
            self.relink(n);
            if done {
                vm.notify_entity(n, "movedone", &[]);
            }
        }
        if self.ent(n).is_some_and(|e| e.mv.ang.finished(now)) {
            let ent = self.ent_mut(n).expect("checked above");
            let done = update_move(&mut ent.mv.ang, now);
            ent.angles = ent.mv.ang.tr.evaluate(now);
            if done {
                let a = &mut ent.angles;
                a[0] = angle_180(a[0]);
                a[1] = angle_360(a[1]);
                a[2] = angle_180(a[2]);
            }
            self.relink(n);
            if done {
                vm.notify_entity(n, "rotatedone", &[]);
            }
        }
    }
}

/// The original's pitch/roll wrap at the end of a rotation: into [-180, 180).
fn angle_180(a: f32) -> f32 {
    let v = a * (1.0 / 360.0);
    (v - (v + 0.5).floor()) * 360.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(ch: &mut Channel, curr: &mut Vec3, from: i32, to: i32, step: i32) -> (Vec<f32>, i32) {
        let mut xs = Vec::new();
        let mut t = from;
        let mut done_at = -1;
        while t <= to {
            *curr = ch.tr.evaluate(t);
            if ch.finished(t) && update_move(ch, t) && done_at < 0 {
                done_at = t;
                *curr = ch.tr.evaluate(t);
            }
            xs.push(curr[0]);
            t += step;
        }
        (xs, done_at)
    }

    #[test]
    fn linear_move_arrives_exactly_when_the_time_is_up() {
        let mut ch = Channel::default();
        let mut p = [0.0; 3];
        setup_move(&mut ch, [100.0, 0.0, 0.0], (2.0, 0.0, 0.0), &mut p, 1000);
        let (xs, done) = run(&mut ch, &mut p, 1000, 4000, 100);
        assert_eq!(done, 3000);
        assert!(
            (xs[10] - 50.0).abs() < 1e-3,
            "half way after 1 s, got {}",
            xs[10]
        );
        assert_eq!(p, [100.0, 0.0, 0.0]);
    }

    #[test]
    fn accel_cruise_decel_covers_the_distance_in_the_total_time() {
        let mut ch = Channel::default();
        let mut p = [0.0; 3];
        // 1 s accelerating, 2 s cruising, 1 s decelerating over 300 units.
        setup_move(&mut ch, [0.0, 300.0, 0.0], (4.0, 1.0, 1.0), &mut p, 0);
        assert!((ch.speed - 100.0).abs() < 1e-3, "{}", ch.speed);
        let (_, done) = run(&mut ch, &mut p, 0, 5000, 50);
        assert_eq!(done, 4000);
        assert_eq!(p, [0.0, 300.0, 0.0]);
    }

    #[test]
    fn a_new_move_starts_from_where_the_old_one_is() {
        let mut ch = Channel::default();
        let mut p = [0.0; 3];
        setup_move(&mut ch, [100.0, 0.0, 0.0], (1.0, 0.0, 0.0), &mut p, 0);
        p = ch.tr.evaluate(500);
        setup_move(&mut ch, [0.0; 3], (1.0, 0.0, 0.0), &mut p, 500);
        assert_eq!(ch.tr.base, [50.0, 0.0, 0.0]);
        assert_eq!(ch.tr.evaluate(1500), [0.0; 3]);
    }

    #[test]
    fn command_times_follow_the_original_rules() {
        use gsc::Value;
        let v = [Value::Float(1.0), Value::Float(0.6), Value::Float(0.6)];
        let a = crate::script::Args::new("moveto", &v);
        // 0.6 + 0.6 > 1.0 and the 1.0000005 slack does not cover it.
        assert!(command_times(&a, 0).is_err());
        let v = [Value::Float(1.0), Value::Float(0.5), Value::Float(0.5)];
        let a = crate::script::Args::new("moveto", &v);
        assert_eq!(command_times(&a, 0).unwrap().1, 0.5);
        let v = [Value::Float(0.0)];
        assert!(command_times(&crate::script::Args::new("moveto", &v), 0).is_err());
    }
}
