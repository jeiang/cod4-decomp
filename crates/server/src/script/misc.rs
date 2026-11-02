// SPDX-License-Identifier: GPL-3.0-only
// `devaddpitch`, `devaddyaw` and `devaddroll` translated from KisakCOD (game/g_scr_mover.cpp), and `laseron`/`laseroff`,
// `enableaimassist`/`disableaimassist` and the turret arcs from KisakCOD (game_mp/g_scr_main_mp.cpp); GPL-3.0,
// copyright the KisakCOD contributors and Activision.
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

pub const FUNCS: &[(&str, Impl<FuncFn>)] = &[
    ("kick", f(kick)),
    ("ban", f(kick)),
    ("map", f(load_map)),
    ("soundexists", f(sound_exists)),
    (
        "getassignedteam",
        f(|_, _, a| a.entity(0).map(|_| Value::Int(0))),
    ),
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
    (
        "enableaimassist",
        m(|g, _, e, _| set_aim_assist(g, e, true)),
    ),
    (
        "disableaimassist",
        m(|g, _, e, _| set_aim_assist(g, e, false)),
    ),
    ("laseron", m(|g, _, e, _| set_laser(g, e, true))),
    ("laseroff", m(|g, _, e, _| set_laser(g, e, false))),
    ("devaddpitch", m(|g, _, e, a| dev_add_rotate(g, e, a, 1))),
    ("devaddyaw", m(|g, _, e, a| dev_add_rotate(g, e, a, 2))),
    ("devaddroll", m(|g, _, e, a| dev_add_rotate(g, e, a, 0))),
    ("buttonpressed", m(|_, _, _, _| Ok(Value::Int(0)))),
    // `settargetent`/`cleartargetent` exist only as HUD element methods in the original tables.
    ("setleftarc", m(|g, _, e, a| set_arc(g, e, a, Arc::Left))),
    ("setrightarc", m(|g, _, e, a| set_arc(g, e, a, Arc::Right))),
    ("settoparc", m(|g, _, e, a| set_arc(g, e, a, Arc::Top))),
    (
        "setbottomarc",
        m(|g, _, e, a| set_arc(g, e, a, Arc::Bottom)),
    ),
];

/// `laseron` / `laseroff`: `ScrCmd_LaserOn`/`ScrCmd_LaserOff` (KisakCOD `game_mp/g_scr_main_mp.cpp:1904-1918`) set or
/// clear the entity's laser flag; clients looking through night vision draw a script model's laser from its
/// `tag_laser` (or `tag_flash`).
fn set_laser(g: &mut Game, e: EntRef, on: bool) -> R {
    live(g, e)?;
    if let Some(ent) = g.ent_mut(e.num) {
        ent.x.laser = on;
    }
    Ok(Value::Undefined)
}

/// `enableaimassist` / `disableaimassist`: `GScr_EnableAimAssist`/`GScr_DisableAimAssist` (KisakCOD
/// `game_mp/g_scr_main_mp.cpp:2151-2170`): a brush model the clients' aim assist may target.
fn set_aim_assist(g: &mut Game, e: EntRef, on: bool) -> R {
    live(g, e)?;
    let Some(ent) = g.ent_mut(e.num).filter(|ent| ent.brush_model.is_some()) else {
        return Err("Currently only supported on entities with brush models".into());
    };
    ent.x.aim_assist = on;
    Ok(Value::Undefined)
}

#[derive(Clone, Copy)]
enum Arc {
    Left,
    Right,
    Top,
    Bottom,
}

/// `setleftarc(deg)` and the other three: `GScr_SetLeftArc` and friends (KisakCOD
/// `game_mp/g_scr_main_mp.cpp:5149-5198`). The right and top arcs are the negated minimum yaw and pitch, the left
/// and bottom the maximum; none may cross the turret's forward direction.
fn set_arc(g: &mut Game, e: EntRef, a: Args, arc: Arc) -> R {
    live(g, e)?;
    if g.ent(e.num).is_none_or(|ent| ent.turret.is_none()) {
        return Err("entity is not a turret".into());
    }
    let deg = a.float(0)?;
    let t = g
        .ent_mut(e.num)
        .and_then(|ent| ent.turret.as_deref_mut())
        .expect("checked above");
    match arc {
        Arc::Right => t.arc_min[1] = (-deg).min(0.0),
        Arc::Left => t.arc_max[1] = deg.max(0.0),
        Arc::Top => t.arc_min[0] = (-deg).min(0.0),
        Arc::Bottom => t.arc_max[0] = deg.max(0.0),
    }
    Ok(Value::Undefined)
}

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
