// SPDX-License-Identifier: GPL-3.0-or-later
//! Builtin methods. The original binds them by name across five class tables (player, script
//! entity, hud element, helicopter, entity); each body checks its receiver's class.

use gsc::{EntClass, EntRef, Value, Vm};

use super::Impl::{self, Later, Real};
use super::{Args, MethFn, hud};
use crate::game::{EntKind, Game};
use crate::mover;
use sim::contents;

type R = Result<Value, String>;

const fn r(f: MethFn) -> Impl<MethFn> {
    Real(f)
}

const M6: &str = "M6 HUD, menus, killcam";
const M7: &str = "M7 audio";

pub const TABLE: &[(&str, Impl<MethFn>)] = &[
    // entity
    ("delete", r(delete)),
    ("setmodel", r(set_model)),
    ("show", r(|g, _, e, _| set_hidden(g, e, false))),
    ("hide", r(|g, _, e, _| set_hidden(g, e, true))),
    ("getorigin", r(get_origin)),
    (
        "getentitynumber",
        r(|_, _, e, _| Ok(Value::Int(i32::from(e.num)))),
    ),
    ("solid", r(|g, _, e, _| set_solid(g, e, true))),
    ("notsolid", r(|g, _, e, _| set_solid(g, e, false))),
    ("setcontents", r(set_contents)),
    ("moveto", r(|g, _, e, a| mover::move_to(g, e, a))),
    ("movex", r(|g, _, e, a| mover::move_axis(g, e, a, 0))),
    ("movey", r(|g, _, e, a| mover::move_axis(g, e, a, 1))),
    ("movez", r(|g, _, e, a| mover::move_axis(g, e, a, 2))),
    ("movegravity", r(|g, _, e, a| mover::move_gravity(g, e, a))),
    ("rotateto", r(|g, _, e, a| mover::rotate_to(g, e, a))),
    (
        "rotatepitch",
        r(|g, _, e, a| mover::rotate_axis(g, e, a, 0)),
    ),
    ("rotateyaw", r(|g, _, e, a| mover::rotate_axis(g, e, a, 1))),
    ("rotateroll", r(|g, _, e, a| mover::rotate_axis(g, e, a, 2))),
    (
        "rotatevelocity",
        r(|g, _, e, a| mover::rotate_velocity(g, e, a)),
    ),
    ("playsound", Later(M7)),
    ("playloopsound", Later(M7)),
    ("stoploopsound", Later(M7)),
    ("playsoundasmaster", Later(M7)),
    ("playsoundtoteam", Later(M7)),
    ("playsoundtoplayer", Later(M7)),
    ("logstring", r(|_, _, _, _| Ok(Value::Undefined))),
    ("fireweapon", Later("M8 vehicles")),
    // hud elements
    ("destroy", r(hud::destroy)),
    ("settext", Later(M6)),
    ("setshader", Later(M6)),
    ("settimer", Later(M6)),
    ("settimerup", Later(M6)),
    ("settenthstimer", Later(M6)),
    ("setvalue", Later(M6)),
    ("setwaypoint", Later(M6)),
    ("setplayernamestring", Later(M6)),
    ("fadeovertime", Later(M6)),
    ("moveovertime", Later(M6)),
    ("scaleovertime", Later(M6)),
    ("setpulsefx", Later(M6)),
    ("clearalltextafterhudelem", Later(M6)),
];

pub(super) fn live(g: &Game, e: EntRef) -> Result<(), String> {
    match e.class {
        EntClass::Entity if g.ent(e.num).is_some() => Ok(()),
        _ => Err("not an entity".into()),
    }
}

fn delete(g: &mut Game, vm: &mut Vm, e: EntRef, _: Args) -> R {
    live(g, e)?;
    if g.ent(e.num).is_some_and(|e| e.kind == EntKind::Client) {
        return Err("Cannot delete a client entity".into());
    }
    g.free_entity(vm, e.num);
    Ok(Value::Undefined)
}

fn set_model(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    live(g, e)?;
    let name = a.string(0)?;
    if g.content.model(name).is_none() {
        g.print(format!("setmodel: model '{name}' not found\n"));
    }
    if let Some(ent) = g.ent_mut(e.num) {
        ent.model = name.into();
    }
    Ok(Value::Undefined)
}

fn set_hidden(g: &mut Game, e: EntRef, hidden: bool) -> R {
    live(g, e)?;
    if let Some(ent) = g.ent_mut(e.num) {
        ent.hidden = hidden;
    }
    Ok(Value::Undefined)
}

/// `solid` / `notsolid`: scripted brush models are solid to everything, script models only to
/// weapon clips; `script_origin` has no extent and ignores both.
fn set_solid(g: &mut Game, e: EntRef, solid: bool) -> R {
    live(g, e)?;
    let Some(ent) = g.ent_mut(e.num) else {
        return Ok(Value::Undefined);
    };
    match &*ent.classname {
        "script_brushmodel" | "script_model" | "light" => {
            ent.contents = match (solid, &*ent.classname) {
                (false, _) => 0,
                (true, "script_model") => contents::MISSILECLIP | contents::CLIPSHOT,
                (true, _) => contents::SOLID,
            };
            g.relink(e.num);
        }
        "script_origin" => {
            g.print(format!(
                "cannot use the solid/notsolid commands on a script_origin entity( number {} )\n",
                e.num
            ));
        }
        _ => {
            return Err(format!(
                "entity {} is not a script_brushmodel, script_model, script_origin, or light",
                e.num
            ));
        }
    }
    Ok(Value::Undefined)
}

fn set_contents(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    live(g, e)?;
    let c = a.int(0)?;
    if let Some(ent) = g.ent_mut(e.num) {
        ent.contents = c;
    }
    g.relink(e.num);
    Ok(Value::Undefined)
}

fn get_origin(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    live(g, e)?;
    Ok(Value::Vector(g.ent(e.num).map_or([0.0; 3], |e| e.origin)))
}
