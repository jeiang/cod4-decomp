// SPDX-License-Identifier: GPL-3.0-or-later
//! Weapon builtins: the player's inventory (`giveweapon`, ammo, switching) and the
//! weapon-definition queries (`weaponclass`, `weaponclipsize`, ...).

use std::rc::Rc;

use gsc::{Array, EntClass, EntRef, Key, Value, Vm};
use sim::pm::wf;
use sim::weapon::{InventoryType, OffhandClass};

use super::Impl::{self, Real};
use super::{Args, FuncFn, MethFn};
use crate::game::Game;

type R = Result<Value, String>;

const fn m(f: MethFn) -> Impl<MethFn> {
    Real(f)
}

const fn f(f: FuncFn) -> Impl<FuncFn> {
    Real(f)
}

pub const METHODS: &[(&str, Impl<MethFn>)] = &[
    ("giveweapon", m(give_weapon)),
    ("takeweapon", m(take_weapon)),
    ("takeallweapons", m(take_all_weapons)),
    ("hasweapon", m(has_weapon)),
    ("getcurrentweapon", m(get_current_weapon)),
    ("getcurrentoffhand", m(get_current_offhand)),
    ("switchtoweapon", m(switch_to_weapon)),
    ("switchtooffhand", m(switch_to_offhand)),
    ("setweaponammoclip", m(set_clip)),
    ("setweaponammostock", m(set_stock)),
    ("getweaponammoclip", m(get_clip)),
    ("getweaponammostock", m(get_stock)),
    ("getammocount", m(get_ammo_count)),
    ("givestartammo", m(give_start_ammo)),
    ("givemaxammo", m(give_max_ammo)),
    ("getfractionstartammo", m(fraction_start)),
    ("getfractionmaxammo", m(fraction_max)),
    ("anyammoforweaponmodes", m(any_ammo)),
    ("getweaponslist", m(|g, _, e, _| weapons_list(g, e, false))),
    (
        "getweaponslistprimaries",
        m(|g, _, e, _| weapons_list(g, e, true)),
    ),
    ("setspawnweapon", m(set_spawn_weapon)),
    ("setoffhandsecondaryclass", m(set_offhand_secondary)),
    ("getoffhandsecondaryclass", m(get_offhand_secondary)),
    (
        "disableweapons",
        m(|g, _, e, _| weapons_enabled(g, e, false)),
    ),
    ("enableweapons", m(|g, _, e, _| weapons_enabled(g, e, true))),
    ("playerads", m(player_ads)),
    (
        "setactionslot",
        m(|g, _, e, _| client(g, e).map(|_| Value::Undefined)),
    ),
    (
        "setspreadoverride",
        m(|g, _, e, _| client(g, e).map(|_| Value::Undefined)),
    ),
    (
        "resetspreadoverride",
        m(|g, _, e, _| client(g, e).map(|_| Value::Undefined)),
    ),
    (
        "getviewmodel",
        m(|g, _, e, _| client(g, e).map(|_| Value::str("viewmodel_base_viewhands"))),
    ),
    (
        "setviewmodel",
        m(|g, _, e, _| client(g, e).map(|_| Value::Undefined)),
    ),
    ("itemweaponsetammo", m(|_, _, _, _| Ok(Value::Undefined))),
];

pub const FUNCS: &[(&str, Impl<FuncFn>)] = &[
    (
        "weaponfiretime",
        f(|g, _, a| def(g, &a, |w| Value::Float(w.fire_time as f32 * 0.001))),
    ),
    (
        "weaponclass",
        f(|g, _, a| def(g, &a, |w| Value::str(w.weap_class.name()))),
    ),
    (
        "weapontype",
        f(|g, _, a| def(g, &a, |w| Value::str(w.weap_type.name()))),
    ),
    (
        "weaponclipsize",
        f(|g, _, a| def(g, &a, |w| Value::Int(w.clip_size))),
    ),
    (
        "weaponstartammo",
        f(|g, _, a| def(g, &a, |w| Value::Int(w.start_ammo))),
    ),
    (
        "weaponmaxammo",
        f(|g, _, a| def(g, &a, |w| Value::Int(w.max_ammo))),
    ),
    (
        "weaponisboltaction",
        f(|g, _, a| def(g, &a, |w| bool_v(w.bolt_action))),
    ),
    (
        "weaponissemiauto",
        f(|g, _, a| def(g, &a, |w| bool_v(w.is_semi_auto()))),
    ),
    (
        "isweaponcliponly",
        f(|g, _, a| def(g, &a, |w| bool_v(w.clip_only))),
    ),
    (
        "isweapondetonationtimed",
        f(|g, _, a| def(g, &a, |w| bool_v(w.timed_detonation))),
    ),
    (
        "weaponinventorytype",
        f(|g, _, a| {
            def(g, &a, |w| {
                Value::str(match w.inventory_type {
                    InventoryType::Primary => "primary",
                    InventoryType::Offhand => "offhand",
                    InventoryType::Item => "item",
                    InventoryType::AltMode => "altmode",
                })
            })
        }),
    ),
    ("weaponaltweaponname", f(weapon_alt_name)),
    ("getweaponmodel", f(get_weapon_model)),
];

fn bool_v(b: bool) -> Value {
    Value::Int(i32::from(b))
}

fn client(g: &Game, e: EntRef) -> Result<u16, String> {
    if e.class != EntClass::Entity {
        return Err("not an entity".into());
    }
    if g.is_client(e.num) {
        Ok(e.num)
    } else {
        Err(format!("entity {} is not a player", e.num))
    }
}

/// The weapon named by argument `i`: 0 for `"none"`, an error for a name no zone defines.
fn weapon(g: &Game, a: &Args, i: usize) -> Result<u16, String> {
    let name = a.string(i)?;
    let idx = g.weapons.index(name);
    if idx == 0 && !name.eq_ignore_ascii_case("none") && !name.is_empty() {
        return Err(format!("unknown weapon '{name}'"));
    }
    Ok(idx)
}

fn def(g: &Game, a: &Args, f: impl Fn(&sim::weapon::WeaponInfo) -> Value) -> R {
    let idx = weapon(g, a, 0)?;
    Ok(f(g.weapons.info(idx)))
}

fn weapon_alt_name(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let w = g.weapons.info(weapon(g, &a, 0)?);
    Ok(match g.weapons.get(w.alt_weapon) {
        Some(alt) if w.alt_weapon != 0 => Value::str(&alt.name),
        _ => Value::str("none"),
    })
}

fn get_weapon_model(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let name = a.string(0)?;
    let model = g
        .content
        .weapon(name)
        .and_then(|w| w.world_models.first().cloned().flatten())
        .and_then(|m| m.name.clone());
    Ok(Value::str(model.as_deref().unwrap_or("")))
}

fn give_weapon(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client(g, e)?;
    let w = weapon(g, &a, 0)?;
    let model = if a.len() > 1 {
        a.int(1)?.clamp(0, 255) as u8
    } else {
        0
    };
    let c = &mut g.clients[usize::from(n)];
    c.inv.give(&g.weapons, &mut c.ps, w, model);
    Ok(Value::Undefined)
}

fn take_weapon(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client(g, e)?;
    let w = weapon(g, &a, 0)?;
    let c = &mut g.clients[usize::from(n)];
    c.inv.take(&g.weapons, &mut c.ps, w, true);
    Ok(Value::Undefined)
}

fn take_all_weapons(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let n = client(g, e)?;
    let c = &mut g.clients[usize::from(n)];
    c.inv.take_all(&g.weapons, &mut c.ps);
    Ok(Value::Undefined)
}

fn has_weapon(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client(g, e)?;
    let w = weapon(g, &a, 0)?;
    Ok(bool_v(g.clients[usize::from(n)].inv.has(w)))
}

fn get_current_weapon(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let n = client(g, e)?;
    let w = g.clients[usize::from(n)].ps.weapon;
    Ok(Value::str(g.weapons.name(w as u16)))
}

fn get_current_offhand(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let n = client(g, e)?;
    let w = g.clients[usize::from(n)].ps.offhand_index;
    Ok(Value::str(g.weapons.name(w)))
}

fn switch_to_weapon(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client(g, e)?;
    let w = weapon(g, &a, 0)?;
    g.clients[usize::from(n)].inv.select(w);
    Ok(Value::Undefined)
}

fn switch_to_offhand(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client(g, e)?;
    let w = weapon(g, &a, 0)?;
    let c = &mut g.clients[usize::from(n)];
    if c.inv.has(w) {
        c.ps.offhand_index = w;
    }
    Ok(Value::Undefined)
}

fn set_clip(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client(g, e)?;
    let w = weapon(g, &a, 0)?;
    let count = a.int(1)?;
    g.clients[usize::from(n)].inv.set_clip(&g.weapons, w, count);
    Ok(Value::Undefined)
}

fn set_stock(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client(g, e)?;
    let w = weapon(g, &a, 0)?;
    let count = a.int(1)?;
    g.clients[usize::from(n)]
        .inv
        .set_stock(&g.weapons, w, count);
    Ok(Value::Undefined)
}

fn get_clip(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client(g, e)?;
    let w = weapon(g, &a, 0)?;
    Ok(Value::Int(
        g.clients[usize::from(n)].inv.get_clip(&g.weapons, w),
    ))
}

fn get_stock(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client(g, e)?;
    let w = weapon(g, &a, 0)?;
    Ok(Value::Int(
        g.clients[usize::from(n)].inv.get_stock(&g.weapons, w),
    ))
}

fn get_ammo_count(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client(g, e)?;
    let w = weapon(g, &a, 0)?;
    Ok(Value::Int(
        g.clients[usize::from(n)].inv.weapon_ammo(&g.weapons, w),
    ))
}

fn give_start_ammo(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client(g, e)?;
    let w = weapon(g, &a, 0)?;
    let c = &mut g.clients[usize::from(n)];
    c.inv.give_start_ammo(&g.weapons, &mut c.ps, w);
    Ok(Value::Undefined)
}

fn give_max_ammo(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client(g, e)?;
    let w = weapon(g, &a, 0)?;
    let c = &mut g.clients[usize::from(n)];
    c.inv.give_max_ammo(&g.weapons, &mut c.ps, w);
    Ok(Value::Undefined)
}

fn fraction_start(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client(g, e)?;
    let w = weapon(g, &a, 0)?;
    Ok(Value::Float(
        g.clients[usize::from(n)]
            .inv
            .fraction_start_ammo(&g.weapons, w),
    ))
}

fn fraction_max(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client(g, e)?;
    let w = weapon(g, &a, 0)?;
    Ok(Value::Float(
        g.clients[usize::from(n)]
            .inv
            .fraction_max_ammo(&g.weapons, w),
    ))
}

fn any_ammo(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client(g, e)?;
    let w = weapon(g, &a, 0)?;
    Ok(bool_v(
        g.clients[usize::from(n)]
            .inv
            .any_ammo_for_weapon_modes(&g.weapons, w),
    ))
}

fn weapons_list(g: &mut Game, e: EntRef, primaries: bool) -> R {
    let n = client(g, e)?;
    let c = &g.clients[usize::from(n)];
    let mut out = Array::new();
    let names: Vec<&str> = if primaries {
        c.inv
            .list_primaries(&g.weapons)
            .map(|w| g.weapons.name(w))
            .collect()
    } else {
        c.inv.list(&g.weapons).map(|w| g.weapons.name(w)).collect()
    };
    for (i, name) in names.into_iter().enumerate() {
        out.set(Key::Int(i as i32), Value::str(name));
    }
    Ok(Value::Array(Rc::new(out)))
}

fn set_spawn_weapon(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client(g, e)?;
    let w = weapon(g, &a, 0)?;
    let c = &mut g.clients[usize::from(n)];
    if c.inv.spawn_weapon(&mut c.ps, w) {
        c.inv.select(w);
    }
    Ok(Value::Undefined)
}

fn set_offhand_secondary(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client(g, e)?;
    let class = a.string(0)?;
    let v = match class {
        "flash" => 1,
        "smoke" => 0,
        c => {
            return Err(format!(
                "Unknown offhand secondary class '{c}', must be 'smoke' or 'flash'"
            ));
        }
    };
    g.clients[usize::from(n)].ps.offhand_secondary = v;
    Ok(Value::Undefined)
}

fn get_offhand_secondary(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let n = client(g, e)?;
    let v = g.clients[usize::from(n)].ps.offhand_secondary;
    let _ = OffhandClass::default();
    Ok(Value::str(if v == 1 { "flash" } else { "smoke" }))
}

fn weapons_enabled(g: &mut Game, e: EntRef, on: bool) -> R {
    let n = client(g, e)?;
    let ps = &mut g.clients[usize::from(n)].ps;
    if on {
        ps.weapon_flags &= !wf::DISABLED;
    } else {
        ps.weapon_flags |= wf::DISABLED;
    }
    Ok(Value::Undefined)
}

fn player_ads(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let n = client(g, e)?;
    Ok(Value::Float(g.clients[usize::from(n)].ps.weapon_pos_frac))
}
