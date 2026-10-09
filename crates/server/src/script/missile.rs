// SPDX-License-Identifier: GPL-3.0-only
//! Missile builtins: `detonate`, grenade touch damage, `missile_settarget`, `dropitem` and the
//! attractor and repulsor slots.
//!
//! Attractors and repulsors only steer guided missiles in the original (`GuidedMissileSteering`);
//! the slots are real (ids, limits, errors, release with their entity) but nothing reads them
//! yet. The multiplayer binary has no `missile_setflightmode*` builtins: `missile_settarget` is the
//! only way a script aims a guided rocket.

use gsc::{EntClass, EntRef, Value, Vm};
use sim::cm::{ENTITYNUM_NONE, ENTITYNUM_WORLD};
use sim::weapon::WeaponType;

use super::Impl::{self, Real};
use super::{Args, FuncFn, MethFn};
use crate::game::Game;
use crate::missile::{Attractor, Attractors, FL_GRENADE_TOUCH_DAMAGE};

type R = Result<Value, String>;

const FL_MISSILE_ATTRACTOR: i32 = 0x100_0000;

const fn m(f: MethFn) -> Impl<MethFn> {
    Real(f)
}

const fn f(f: FuncFn) -> Impl<FuncFn> {
    Real(f)
}

pub const METHODS: &[(&str, Impl<MethFn>)] = &[
    ("detonate", m(detonate)),
    (
        "enablegrenadetouchdamage",
        m(|g, _, e, _| touch_damage(g, e, true)),
    ),
    (
        "disablegrenadetouchdamage",
        m(|g, _, e, _| touch_damage(g, e, false)),
    ),
    ("missile_settarget", m(set_target)),
    ("dropitem", m(drop_item)),
];

pub const FUNCS: &[(&str, Impl<FuncFn>)] = &[
    (
        "missile_createattractorent",
        f(|g, _, a| create(g, &a, true, true)),
    ),
    (
        "missile_createattractororigin",
        f(|g, _, a| create(g, &a, true, false)),
    ),
    (
        "missile_createrepulsorent",
        f(|g, _, a| create(g, &a, false, true)),
    ),
    (
        "missile_createrepulsororigin",
        f(|g, _, a| create(g, &a, false, false)),
    ),
    ("missile_deleteattractor", f(delete_attractor)),
];

fn entity(g: &Game, e: EntRef) -> Result<u16, String> {
    if e.class != EntClass::Entity || g.ent(e.num).is_none() {
        return Err("not an entity".into());
    }
    Ok(e.num)
}

fn param_err(i: usize, msg: &str) -> String {
    format!("parameter {}: {msg}", i + 1)
}

/// `GScr_Detonate`: a grenade goes off now, credited to the player given (or to the world).
fn detonate(g: &mut Game, vm: &mut Vm, e: EntRef, a: Args) -> R {
    let n = entity(g, e)?;
    let is_grenade = g
        .ent(n)
        .and_then(|e| e.missile.as_ref())
        .is_some_and(|m| m.info.weap_type == WeaponType::Grenade);
    if !is_grenade {
        return Err("entity is not a grenade".into());
    }
    if !a.is_empty() {
        let parent = match a.entity_or_undefined(0)? {
            Some(p) if g.is_client(p.num) => p.num,
            Some(_) => return Err(param_err(0, "Entity is not a player")),
            None => ENTITYNUM_WORLD,
        };
        if let Some(m) = g.ent_mut(n).and_then(|e| e.missile.as_mut()) {
            m.parent = Some(parent);
        }
    }
    g.detonate_missile(vm, n);
    Ok(Value::Undefined)
}

/// `GScr_EnableGrenadeTouchDamage` and `GScr_DisableGrenadeTouchDamage`.
fn touch_damage(g: &mut Game, e: EntRef, on: bool) -> R {
    let n = entity(g, e)?;
    let ent = g.ent_mut(n).ok_or("not an entity")?;
    if &*ent.classname != "trigger_damage" {
        return Err("Currently on supported on damage triggers".into());
    }
    if on {
        ent.flags |= FL_GRENADE_TOUCH_DAMAGE;
    } else {
        ent.flags &= !FL_GRENADE_TOUCH_DAMAGE;
    }
    Ok(Value::Undefined)
}

/// `GScr_MissileSetTarget`: a rocket homes on `target` (nothing for `undefined`), aiming at
/// the optional offset from it.
fn set_target(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = entity(g, e)?;
    let target = a.entity_or_undefined(0)?.map(|t| t.num);
    let offset = if a.len() > 1 { a.vector(1)? } else { [0.0; 3] };
    let is_rocket = g.ent(n).is_some_and(|e| &*e.classname == "rocket");
    if !is_rocket {
        return Err(format!("Entity {n} is not a rocket\n"));
    }
    if let Some(m) = g.ent_mut(n).and_then(|e| e.missile.as_mut()) {
        m.target = target;
        m.target_offset = offset;
    }
    Ok(Value::Undefined)
}

/// `PlayerCmd_dropItem`: the player drops a weapon they hold; returns the item entity, or
/// `undefined` when nothing was left to drop.
fn drop_item(g: &mut Game, vm: &mut Vm, e: EntRef, a: Args) -> R {
    let n = entity(g, e)?;
    if !g.is_client(n) {
        return Err(format!("entity {n} is not a player"));
    }
    let weapon = g.weapons.index(a.string(0)?);
    if weapon == 0 {
        return Ok(Value::Undefined);
    }
    let model = g.clients[usize::from(n)].inv.model(weapon);
    Ok(match g.drop_weapon(vm, n, weapon, model) {
        Some(item) => g.entity_value(vm, item),
        None => Value::Undefined,
    })
}

fn create(g: &mut Game, a: &Args, attractor: bool, by_entity: bool) -> R {
    let (entity, origin) = if by_entity {
        let e = a.entity(0)?;
        (Some(entity_num(g, e)?), [0.0; 3])
    } else {
        (None, a.vector(0)?)
    };
    let strength = a.float(1)?;
    let max_dist = a.float(2)?;
    if max_dist <= 0.0 {
        return Err(param_err(2, "maxDist must be greater than zero"));
    }
    let slot = g
        .attractors
        .add(Attractor {
            attractor,
            entity,
            origin,
            strength,
            max_dist,
        })
        .ok_or_else(|| {
            format!(
                "Ran out of attractor/repulsors.  Max allowed: {}",
                Attractors::MAX
            )
        })?;
    if let (Some(n), true) = (entity, attractor)
        && let Some(e) = g.ent_mut(n)
    {
        e.flags |= FL_MISSILE_ATTRACTOR;
    }
    Ok(Value::Int(slot as i32))
}

fn entity_num(g: &Game, e: EntRef) -> Result<u16, String> {
    if g.ent(e.num).is_none() || e.num == ENTITYNUM_NONE {
        return Err("not an entity".into());
    }
    Ok(e.num)
}

fn delete_attractor(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let i = a.int(0)?;
    if !(0..Attractors::MAX as i32).contains(&i) {
        return Err(param_err(0, "Invalid attractor or repulsor"));
    }
    g.attractors.remove(i as usize);
    Ok(Value::Undefined)
}
