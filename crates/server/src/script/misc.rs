// SPDX-License-Identifier: GPL-3.0-only
// `devaddpitch`, `devaddyaw` and `devaddroll` translated from KisakCOD (game/g_scr_mover.cpp; GPL-3.0, copyright the KisakCOD contributors and Activision).
//! The remaining builtins: server administration, the developer file functions, queries with
//! fixed answers on a dedicated server, and the names that only matter once their milestone
//! lands (accepted and logged once).

use gsc::{EntRef, Value, Vm};

use super::Impl::{self, Later, Real};
use super::methods::live;
use super::{Args, FuncFn, MethFn};
use crate::game::Game;
use crate::missile::FL_STABLE_MISSILES;

type R = Result<Value, String>;

const fn f(f: FuncFn) -> Impl<FuncFn> {
    Real(f)
}

const fn m(f: MethFn) -> Impl<MethFn> {
    Real(f)
}

const M5: &str = "M5 connect and play";
const M8: &str = "M8 FX, decals, post";
const VEH: &str = "M8 vehicles";

pub const FUNCS: &[(&str, Impl<FuncFn>)] = &[
    ("kick", f(kick)),
    ("ban", f(kick)),
    ("map", f(load_map)),
    ("soundexists", f(sound_exists)),
    (
        "getassignedteam",
        f(|_, _, a| a.entity(0).map(|_| Value::Int(0))),
    ),
    // The script file functions exist only in developer builds of the original; a release
    // build answers every call with the failure value.
    ("openfile", f(|_, _, _| Ok(Value::Int(-1)))),
    ("closefile", f(|_, _, _| Ok(Value::Int(-1)))),
    ("fprintln", f(|_, _, _| Ok(Value::Int(-1)))),
    ("fprintfields", f(|_, _, _| Ok(Value::Int(-1)))),
    ("freadln", f(|_, _, _| Ok(Value::Int(-1)))),
    ("fgetarg", f(|_, _, _| Ok(Value::str("")))),
    ("quitlobby", Later(M5)),
    ("quitparty", Later(M5)),
    ("startparty", Later(M5)),
    ("startprivatematch", Later(M5)),
    ("searchforonlinegames", Later(M5)),
];

pub const METHODS: &[(&str, Impl<MethFn>)] = &[
    ("setstablemissile", m(set_stable_missile)),
    ("sendleaderboards", Later(M5)),
    // Null subs in the original, which has no rumble.
    ("playrumbleonentity", m(|_, _, _, _| Ok(Value::Undefined))),
    (
        "playrumblelooponentity",
        m(|_, _, _, _| Ok(Value::Undefined)),
    ),
    ("stoprumble", m(|_, _, _, _| Ok(Value::Undefined))),
    ("enableaimassist", Later(M5)),
    ("disableaimassist", Later(M5)),
    ("laseron", Later(M8)),
    ("laseroff", Later(M8)),
    ("devaddpitch", m(|g, _, e, a| dev_add_rotate(g, e, a, 1))),
    ("devaddyaw", m(|g, _, e, a| dev_add_rotate(g, e, a, 2))),
    ("devaddroll", m(|g, _, e, a| dev_add_rotate(g, e, a, 0))),
    ("buttonpressed", m(|_, _, _, _| Ok(Value::Int(0)))),
    ("settargetent", Later(VEH)),
    ("cleartargetent", Later(VEH)),
    ("setleftarc", Later(VEH)),
    ("setrightarc", Later(VEH)),
    ("settoparc", Later(VEH)),
    ("setbottomarc", Later(VEH)),
];

/// `kick(n)` / `ban(n)`: the client in slot `n` is dropped. There is no persistent ban list,
/// so a ban lasts as long as a kick.
fn kick(g: &mut Game, vm: &mut Vm, a: Args) -> R {
    if a.is_empty() {
        return Ok(Value::Undefined);
    }
    let n = a.int(0)?;
    if let Ok(n) = u16::try_from(n) {
        g.disconnect_client(vm, n);
    }
    Ok(Value::Undefined)
}

/// `map(name[, savepersist])`: asks the server to change the map after this frame.
fn load_map(g: &mut Game, _: &mut Vm, a: Args) -> R {
    if a.is_empty() {
        return Ok(Value::Undefined);
    }
    let name = a.string(0)?;
    if !g.map_exists(name) {
        return Ok(Value::Undefined);
    }
    if g.level.map_requested.is_some() {
        return Err("map already called".into());
    }
    g.level.save_persist = a.len() > 1 && a.int(1)? != 0;
    g.level.map_requested = Some(name.to_owned());
    Ok(Value::Undefined)
}

/// `soundexists(alias)`: whether the loaded zones have an alias of that name.
pub(super) fn sound_exists(g: &mut Game, _: &mut Vm, a: Args) -> R {
    Ok(Value::Int(i32::from(g.content.sound_exists(a.string(0)?))))
}

/// `ScriptEnt_DevAddRotate`: turns a script model, brush model, origin or light by `degrees` about one of its own
/// axes (0 forward, 1 left, 2 up) at once, ending any rotation in progress.
fn dev_add_rotate(g: &mut Game, e: EntRef, a: Args, axis: usize) -> R {
    live(g, e)?;
    let class = g
        .ent(e.num)
        .map(|e| e.classname.clone())
        .unwrap_or_default();
    if !matches!(
        &*class,
        "script_brushmodel" | "script_model" | "script_origin" | "light"
    ) {
        return Err(format!(
            "entity {} is not a script_brushmodel, script_model, script_origin, or light",
            e.num
        ));
    }
    if a.len() != 1 {
        return Err("ScriptEnt_ProtoAddRotate: expect exactly one parameter.".into());
    }
    let (s, c) = f64::from(a.float(0)?).to_radians().sin_cos();
    let (sin, cos) = (s as f32, c as f32);
    let Some(ent) = g.ent_mut(e.num) else {
        return Ok(Value::Undefined);
    };
    let from = crate::tags::angles_to_axis(ent.angles);
    let (b, c2) = ((axis + 1) % 3, (axis + 2) % 3);
    let mut out = from;
    for i in 0..3 {
        out[b][i] = from[b][i] * cos + from[c2][i] * sin;
        out[c2][i] = from[c2][i] * cos - from[b][i] * sin;
    }
    ent.angles = crate::tags::axis_to_angles(&out);
    ent.mv.ang.tr.kind = sim::traj::TrType::Stationary;
    ent.mv.ang.tr.base = ent.angles;
    g.relink(e.num);
    Ok(Value::Undefined)
}

fn set_stable_missile(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    live(g, e)?;
    let on = a.int(0)? != 0;
    if let Some(ent) = g.ent_mut(e.num) {
        if on {
            ent.flags |= FL_STABLE_MISSILES;
        } else {
            ent.flags &= !FL_STABLE_MISSILES;
        }
    }
    Ok(Value::Undefined)
}
