// SPDX-License-Identifier: GPL-3.0-or-later
//! Damage builtins: `radiusdamage`, `setcandamage`, the cone traces, `positionwouldtelefrag`,
//! `obituary` and the player counts.

use gsc::{EntRef, Value, Vm};
use sim::cm::{Collide, ENTITYNUM_NONE};
use sim::contents;

use super::Impl::{self, Real};
use super::{Args, FuncFn, MethFn};
use crate::client::{Session, Team};
use crate::combat::{MOD_UNKNOWN, mod_from_name};
use crate::game::Game;

type R = Result<Value, String>;

pub const FUNCS: &[(&str, Impl<FuncFn>)] = &[
    (
        "radiusdamage",
        Real(|g, vm, a| radius_damage(g, vm, a, None)),
    ),
    ("positionwouldtelefrag", Real(position_would_telefrag)),
    ("obituary", Real(|_, _, _| Ok(Value::Undefined))),
    (
        "setplayerignoreradiusdamage",
        Real(|g, _, a| {
            g.level.ignore_radius_damage = a.int(0)? != 0;
            Ok(Value::Undefined)
        }),
    ),
    ("getteamplayersalive", Real(team_players_alive)),
];

pub const METHODS: &[(&str, Impl<MethFn>)] = &[
    (
        "radiusdamage",
        Real(|g, vm, e, a| radius_damage(g, vm, a, Some(e.num))),
    ),
    ("setcandamage", Real(set_can_damage)),
    (
        "damageconetrace",
        Real(|g, _, e, a| cone_trace(g, e, a, contents::MASK_SOLID)),
    ),
    (
        "sightconetrace",
        Real(|g, _, e, a| cone_trace(g, e, a, contents::MASK_SOLID | contents::FOLIAGE)),
    ),
];

fn entity_num(v: Option<&Value>) -> Option<u16> {
    match v {
        Some(Value::Object(o)) => o.entity().map(|e| e.num),
        _ => None,
    }
}

/// `radiusDamage(origin, range, maxDamage, minDamage[, attacker[, mod]])`.
fn radius_damage(g: &mut Game, vm: &mut Vm, a: Args, inflictor: Option<u16>) -> R {
    let origin = a.vector(0)?;
    let (range, max, min) = (a.float(1)?, a.float(2)?, a.float(3)?);
    let attacker = entity_num(a.opt(4));
    let mean = match a.opt(5) {
        Some(Value::Str(s)) => mod_from_name(s).unwrap_or(MOD_UNKNOWN),
        _ => crate::combat::MOD_EXPLOSIVE,
    };
    if g.level.ignore_radius_damage {
        return Ok(Value::Undefined);
    }
    g.radius_damage(
        vm, origin, range, max as i32, min as i32, attacker, inflictor, mean, 0,
    );
    Ok(Value::Undefined)
}

fn set_can_damage(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let on = a.int(0)? != 0;
    match g.ent_mut(e.num) {
        Some(ent) => ent.takedamage = on,
        None => return Err("not an entity".into()),
    }
    Ok(Value::Undefined)
}

/// `positionWouldTeleFrag(origin)`: a player stands where a player would spawn.
fn position_would_telefrag(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let o = a.vector(0)?;
    let Some(w) = g.world.as_ref() else {
        return Ok(Value::Int(0));
    };
    let hit = w.box_in_solid(
        o,
        sim::pm::PLAYER_MINS,
        sim::pm::PLAYER_MAXS,
        ENTITYNUM_NONE,
        contents::PLAYER,
    );
    Ok(Value::Int(i32::from(hit.is_some())))
}

fn team_players_alive(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let team = Team::from_name(a.string(0)?).ok_or("getteamplayersalive: bad team")?;
    let n = g
        .connected_clients()
        .filter(|(n, c)| {
            c.team == team
                && c.session == Session::Playing
                && g.ent(*n).is_some_and(|e| e.health > 0)
        })
        .count();
    Ok(Value::Int(n as i32))
}

/// The fraction of five sample points of the target (centre and four corners of its box)
/// the origin sees; `damageConeTrace(origin[, entity])` with the player as the target.
fn cone_trace(g: &mut Game, e: EntRef, a: Args, mask: i32) -> R {
    let origin = a.vector(0)?;
    let ignore = entity_num(a.opt(1)).unwrap_or(ENTITYNUM_NONE);
    let Some(w) = g.world.as_ref() else {
        return Ok(Value::Float(0.0));
    };
    let Some(t) = g.ent(e.num) else {
        return Err("not an entity".into());
    };
    let c = [
        t.origin[0] + (t.mins[0] + t.maxs[0]) * 0.5,
        t.origin[1] + (t.mins[1] + t.maxs[1]) * 0.5,
        t.origin[2] + (t.mins[2] + t.maxs[2]) * 0.5,
    ];
    let pts = [
        c,
        [c[0] + 12.0, c[1], c[2]],
        [c[0] - 12.0, c[1], c[2]],
        [c[0], c[1] + 12.0, c[2]],
        [c[0], c[1] - 12.0, c[2]],
    ];
    let seen = pts
        .iter()
        .filter(|p| {
            w.trace(origin, **p, [0.0; 3], [0.0; 3], ignore, mask)
                .fraction
                >= 1.0
        })
        .count();
    Ok(Value::Float(seen as f32 / pts.len() as f32))
}
