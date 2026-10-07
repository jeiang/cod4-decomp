// SPDX-License-Identifier: GPL-3.0-or-later
//! Builtin methods. The original binds them by name across five class tables (player, script
//! entity, hud element, helicopter, entity); each body checks its receiver's class.

use gsc::{EntClass, EntRef, Value, Vm};

use super::Impl::{self, Later, Real};
use super::{Args, MethFn, hud};
use crate::game::{EntKind, Game};

type R = Result<Value, String>;

const fn r(f: MethFn) -> Impl<MethFn> {
    Real(f)
}

const M3: &str = "M3 bots, pmove, traces, weapons";
const M5: &str = "M5 connect and play";
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
    ("setcandamage", Later(M3)),
    ("istouching", r(|_, _, _, _| Ok(Value::Int(0)))),
    ("linkto", Later(M3)),
    ("unlink", Later(M3)),
    ("usetriggerrequirelookat", Later(M5)),
    ("sethintstring", Later(M5)),
    ("setcursorhint", Later(M5)),
    ("setteamfortrigger", Later(M5)),
    ("playsound", Later(M7)),
    ("playloopsound", Later(M7)),
    ("stoploopsound", Later(M7)),
    ("playsoundasmaster", Later(M7)),
    ("playsoundtoteam", Later(M7)),
    ("playsoundtoplayer", Later(M7)),
    ("placespawnpoint", Later(M3)),
    ("hidepart", Later(M5)),
    ("showpart", Later(M5)),
    ("showallparts", Later(M5)),
    ("logstring", r(|_, _, _, _| Ok(Value::Undefined))),
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

fn live(g: &Game, e: EntRef) -> Result<(), String> {
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

fn set_solid(g: &mut Game, e: EntRef, solid: bool) -> R {
    live(g, e)?;
    if let Some(ent) = g.ent_mut(e.num) {
        ent.contents = if solid { 1 } else { 0 };
    }
    Ok(Value::Undefined)
}

fn set_contents(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    live(g, e)?;
    let c = a.int(0)?;
    if let Some(ent) = g.ent_mut(e.num) {
        ent.contents = c;
    }
    Ok(Value::Undefined)
}

fn get_origin(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    live(g, e)?;
    Ok(Value::Vector(g.ent(e.num).map_or([0.0; 3], |e| e.origin)))
}
