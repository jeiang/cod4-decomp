// SPDX-License-Identifier: GPL-3.0-only
//! Damage builtins: `radiusdamage`, `setcandamage`, the cone traces, `positionwouldtelefrag`,
//! `obituary` and the player counts.

use gsc::{EntRef, Value, Vm};
use sim::cm::{ENTITYNUM_NONE, ENTITYNUM_WORLD};
use sim::contents;

use super::Impl::{self, Real};
use super::{Args, FuncFn, MethFn};
use crate::client::{Session, Team};
use crate::combat::{Blast, MOD_UNKNOWN, RADIUS_DAMAGE_MASK, mod_from_name};
use crate::game::Game;

type R = Result<Value, String>;

pub const FUNCS: &[(&str, Impl<FuncFn>)] = &[
    (
        "radiusdamage",
        Real(|g, vm, a| radius_damage(g, vm, a, None)),
    ),
    ("positionwouldtelefrag", Real(position_would_telefrag)),
    ("obituary", Real(obituary)),
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
        Real(|g, _, e, a| cone_trace(g, e, a, RADIUS_DAMAGE_MASK)),
    ),
    (
        "sightconetrace",
        Real(|g, _, e, a| cone_trace(g, e, a, contents::SOLID | contents::SKY)),
    ),
];

fn entity_num(v: Option<&Value>) -> Option<u16> {
    match v {
        Some(Value::Object(o)) => o.entity().map(|e| e.num),
        _ => None,
    }
}

/// `radiusDamage(origin, range, maxDamage, minDamage[, attacker[, mod[, weapon]]])`; the attacker is the world
/// when none is given. `setplayerignoreradiusdamage` spares players from it.
fn radius_damage(g: &mut Game, vm: &mut Vm, a: Args, inflictor: Option<u16>) -> R {
    let origin = a.vector(0)?;
    let (radius, inner, outer) = (a.float(1)?, a.float(2)?, a.float(3)?);
    let attacker = entity_num(a.opt(4)).or(Some(ENTITYNUM_WORLD));
    let mean = match a.opt(5) {
        Some(Value::Str(s)) => mod_from_name(s).unwrap_or(MOD_UNKNOWN),
        _ => crate::combat::MOD_EXPLOSIVE,
    };
    let weapon = match a.opt(6) {
        Some(Value::Str(s)) => g.weapon_index(s),
        _ => 0,
    };
    g.blast(
        vm,
        &Blast {
            origin,
            radius,
            inner,
            outer,
            attacker,
            inflictor,
            cone: None,
            ignore: inflictor,
            skip_clients: g.level.ignore_radius_damage,
            mean,
            weapon,
        },
    );
    Ok(Value::Undefined)
}

/// `obituary(victim, attacker, weapon, mod)`: one line of the kill feed, with the arguments the script chose (it
/// may name another attacker, or the victim itself, and a means of death the engine did not see).
fn obituary(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let victim = entity_num(a.opt(0)).ok_or("not an entity")?;
    let killer = entity_num(a.opt(1))
        .filter(|k| g.is_client(*k))
        .unwrap_or(net::ui::NO_ENTITY);
    let weapon = g.weapon_name(g.weapon_index(a.string(2)?)).to_owned();
    let mean = a.string(3)?;
    mod_from_name(mean).ok_or_else(|| format!("Unknown means of death \"{mean}\"\n"))?;
    g.send(
        crate::ui::Dest::All,
        net::ui::ServerCmd::Obituary(net::ui::Obituary {
            killer,
            victim,
            weapon,
            headshot: mean == "MOD_HEAD_SHOT",
            mean: mean.to_owned(),
        }),
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

/// How much of the entity the origin reaches (`CanDamage`): `damageConeTrace(origin[, ignored entity])`.
fn cone_trace(g: &mut Game, e: EntRef, a: Args, mask: i32) -> R {
    let origin = a.vector(0)?;
    let ignore = entity_num(a.opt(1)).unwrap_or(ENTITYNUM_NONE);
    if g.ent(e.num).is_none() {
        return Err("not an entity".into());
    }
    Ok(Value::Float(
        g.damage_coverage(e.num, origin, None, ignore, mask),
    ))
}
