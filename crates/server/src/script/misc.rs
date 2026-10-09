// SPDX-License-Identifier: GPL-3.0-only
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
const M7: &str = "M7 audio";
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
    ("playrumbleonentity", Later(M7)),
    ("playrumblelooponentity", Later(M7)),
    ("stoprumble", Later(M7)),
    ("enableaimassist", Later(M5)),
    ("disableaimassist", Later(M5)),
    ("laseron", Later(M8)),
    ("laseroff", Later(M8)),
    ("showtoplayer", Later(M5)),
    ("devaddpitch", Later(M5)),
    ("devaddyaw", Later(M5)),
    ("devaddroll", Later(M5)),
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
