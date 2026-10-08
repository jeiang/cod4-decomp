// SPDX-License-Identifier: GPL-3.0-or-later
//! The helicopter's builtin methods (`CMD_VEH_*`, `CMD_Heli_*`): they set the goals and limits the vehicle's think
//! (`crate::vehicle`) flies by.

use gsc::{EntRef, Value, Vm};

use super::Impl::{self, Real};
use super::{Args, MethFn};
use crate::game::Game;
use crate::vehicle::{MPH, MoveState, Target, Vehicle};

type R = Result<Value, String>;

const fn m(f: MethFn) -> Impl<MethFn> {
    Real(f)
}

pub const METHODS: &[(&str, Impl<MethFn>)] = &[
    ("freehelicopter", m(free_helicopter)),
    ("setspeed", m(set_speed)),
    (
        "getspeed",
        m(|g, _, e, _| get(g, e, |v| Value::Float(v.speed))),
    ),
    (
        "getspeedmph",
        m(|g, _, e, _| get(g, e, |v| Value::Float(v.speed / MPH))),
    ),
    ("resumespeed", m(resume_speed)),
    ("setyawspeed", m(set_yaw_speed)),
    ("setmaxpitchroll", m(set_max_pitch_roll)),
    (
        "setturningability",
        m(|g, _, e, a| {
            let x = a.float(0)?;
            with(g, e, |v| v.turning_ability = x)
        }),
    ),
    (
        "setairresistance",
        m(|g, _, e, a| {
            let x = a.float(0)? * MPH;
            with(g, e, |v| v.max_drag_speed = x)
        }),
    ),
    ("sethoverparams", m(set_hover_params)),
    (
        "setneargoalnotifydist",
        m(|g, _, e, a| {
            let x = a.float(0)?;
            with(g, e, |v| v.near_goal_dist = x)
        }),
    ),
    ("setvehgoalpos", m(set_goal_pos)),
    (
        "setgoalyaw",
        m(|g, _, e, a| {
            let x = a.float(0)?;
            with(g, e, |v| v.goal_yaw = Some(x))
        }),
    ),
    (
        "cleargoalyaw",
        m(|g, _, e, _| with(g, e, |v| v.goal_yaw = None)),
    ),
    (
        "settargetyaw",
        m(|g, _, e, a| {
            let x = a.float(0)?;
            with(g, e, |v| v.target_yaw = Some(x))
        }),
    ),
    (
        "cleartargetyaw",
        m(|g, _, e, _| with(g, e, |v| v.target_yaw = None)),
    ),
    ("setlookatent", m(set_look_at)),
    (
        "clearlookatent",
        m(|g, _, e, _| with(g, e, |v| v.look_at = None)),
    ),
    ("setvehweapon", m(set_weapon)),
    ("fireweapon", m(fire_weapon)),
    ("setturrettargetvec", m(set_turret_target_vec)),
    ("setturrettargetent", m(set_turret_target_ent)),
    (
        "clearturrettarget",
        m(|g, _, e, _| with(g, e, |v| v.target = Target::None)),
    ),
    ("setvehicleteam", m(set_team)),
    ("setdamagestage", m(set_damage_stage)),
];

/// The vehicle of the receiver.
fn veh(g: &mut Game, e: EntRef) -> Result<&mut Vehicle, String> {
    super::methods::live(g, e)?;
    g.ent_mut(e.num)
        .and_then(|x| x.veh.as_deref_mut())
        .ok_or_else(|| "entity is not a helicopter".to_owned())
}

fn with(g: &mut Game, e: EntRef, f: impl FnOnce(&mut Vehicle)) -> R {
    f(veh(g, e)?);
    Ok(Value::Undefined)
}

fn get(g: &mut Game, e: EntRef, f: impl FnOnce(&Vehicle) -> Value) -> R {
    Ok(f(veh(g, e)?))
}

fn free_helicopter(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    veh(g, e)?;
    if let Some(x) = g.ent_mut(e.num) {
        x.veh = None;
    }
    Ok(Value::Undefined)
}

/// `setspeed(mph, accel[, decel])`.
fn set_speed(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let speed = a.float(0)? * MPH;
    if speed < 0.0 {
        return Err("Cannot set negative speed on vehicle".into());
    }
    let accel = if a.len() > 1 {
        Some(a.float(1)? * MPH)
    } else {
        None
    };
    let decel = if a.len() > 2 {
        Some(a.float(2)? * MPH)
    } else {
        None
    };
    let v = veh(g, e)?;
    v.manual_speed = speed;
    if let Some(x) = accel {
        v.manual_accel = x;
    }
    // A helicopter cannot accelerate faster than it can reach its speed in a second.
    if v.speed < v.manual_speed && v.manual_accel > v.manual_speed {
        v.manual_accel = v.manual_speed;
    }
    v.manual_decel = decel.unwrap_or(v.manual_accel * 0.5);
    if v.manual_accel <= 0.0 || v.manual_decel <= 0.0 {
        v.manual_accel = MPH;
        v.manual_decel = MPH;
        return Err("Acceleration/deceleration must be > 0".into());
    }
    Ok(Value::Undefined)
}

fn resume_speed(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let accel = a.float(0)? * MPH;
    if accel < 0.0 {
        return Err("Cannot set negative acceleration on vehicle".into());
    }
    with(g, e, |v| v.manual_accel = accel)
}

/// `setyawspeed(degrees/s, accel[, decel[, overshoot]])`.
fn set_yaw_speed(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let (speed, accel) = (a.float(0)?, a.float(1)?);
    let decel = if a.len() > 2 { a.float(2)? } else { accel };
    let overshoot = if a.len() > 3 { Some(a.float(3)?) } else { None };
    if speed < 0.0 {
        return Err("Cannot set negative yaw speed on vehicle".into());
    }
    if accel < 0.0 {
        return Err("Cannot set negative yaw acceleration on vehicle".into());
    }
    if overshoot.is_some_and(|o| !(0.0..=1.0).contains(&o)) {
        return Err("Overshoot must be in 0 to 1 range".into());
    }
    with(g, e, |v| {
        v.max_angle_vel[1] = speed;
        v.yaw_accel = accel;
        v.yaw_decel = decel;
        if let Some(o) = overshoot {
            v.yaw_overshoot = o;
        }
    })
}

fn set_max_pitch_roll(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let (pitch, roll) = (a.float(0)?, a.float(1)?);
    if pitch < 0.0 {
        return Err("Cannot set negative max pitch".into());
    }
    if roll < 0.0 {
        return Err("Cannot set negative max roll".into());
    }
    with(g, e, |v| {
        v.max_pitch = pitch;
        v.max_roll = roll;
    })
}

fn set_hover_params(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let radius = a.float(0)?;
    let speed = if a.len() > 1 { Some(a.float(1)?) } else { None };
    let accel = if a.len() > 2 { Some(a.float(2)?) } else { None };
    with(g, e, |v| {
        v.hover_radius = radius;
        if let Some(s) = speed {
            v.hover_speed = s;
            if let Some(x) = accel {
                v.hover_accel = x;
            }
        }
    })
}

/// `setvehgoalpos(origin[, stop])`.
fn set_goal_pos(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let goal = a.vector(0)?;
    let stop = a.len() > 1 && a.int(1)? != 0;
    let v = veh(g, e)?;
    if v.manual_speed == 0.0 || v.manual_accel == 0.0 || v.manual_decel == 0.0 {
        return Err("Speed and acceleration must not be zero before setting goal pos".into());
    }
    v.goal = goal;
    v.stop_at_goal = stop;
    v.state = MoveState::Move;
    v.stopping = false;
    Ok(Value::Undefined)
}

fn set_look_at(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let t = a.entity(0).map_err(|_| "Invalid entity".to_owned())?;
    if g.ent(t.num).is_none() {
        return Err("Invalid entity".into());
    }
    let alive = g.ent(e.num).is_some_and(|x| x.health > 0);
    if !alive {
        return Err("Vehicle must have health to control".into());
    }
    with(g, e, |v| v.look_at = Some(t.num))
}

fn set_weapon(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let w = g.weapons.index(a.string(0)?);
    with(g, e, |v| v.weapon = w)
}

/// `fireWeapon([tag[, target[, offset]]])`: a bullet or a projectile; the missile entity comes back.
fn fire_weapon(g: &mut Game, vm: &mut Vm, e: EntRef, a: Args) -> R {
    veh(g, e)?;
    let tag = if a.is_empty() {
        None
    } else {
        Some(a.string(0)?)
    };
    let target = match a.opt(1) {
        Some(Value::Object(o)) => o.entity().map(|t| {
            let off = a.vector(2).unwrap_or([0.0; 3]);
            (t.num, off)
        }),
        _ => None,
    };
    match g.vehicle_fire(vm, e.num, tag, target)? {
        Some(missile) => Ok(g.entity_value(vm, missile)),
        None => Ok(Value::Undefined),
    }
}

fn health_check(g: &Game, e: EntRef) -> Result<(), String> {
    if g.ent(e.num).is_some_and(|x| x.health > 0) {
        Ok(())
    } else {
        Err("Vehicle must have health to control the turret".into())
    }
}

fn set_turret_target_vec(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let p = a.vector(0)?;
    health_check(g, e)?;
    with(g, e, |v| v.target = Target::Point(p))
}

/// `setTurretTargetEnt(entity[, offset])`.
fn set_turret_target_ent(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    health_check(g, e)?;
    let t = a.entity_or_undefined(0)?;
    let off = if a.len() > 1 { a.vector(1)? } else { [0.0; 3] };
    with(g, e, |v| {
        v.target = match t {
            Some(t) => Target::Ent(t.num, off),
            None => Target::None,
        }
    })
}

fn set_team(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let team = match a.string(0)?.to_ascii_lowercase().as_str() {
        "axis" => 1,
        "allies" => 2,
        "none" => 0,
        _ => {
            return Err(
                "setVehicleTeam: invalid team used must be 'axis', 'allies', or 'none'\n".into(),
            );
        }
    };
    with(g, e, |v| v.team = team)
}

/// `setDamageStage(n)`: the smoke the clients draw; stage 0 is the crash.
fn set_damage_stage(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let stage = a.int(0)?.clamp(0, 3) as u8;
    let v = veh(g, e)?;
    let crashed = v.stage != 0 && stage == 0;
    v.stage = stage;
    if crashed {
        g.stats.heli_crashes += 1;
    }
    Ok(Value::Undefined)
}
