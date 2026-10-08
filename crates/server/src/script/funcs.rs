// SPDX-License-Identifier: GPL-3.0-or-later
//! Builtin functions (no entity receiver).

use std::rc::Rc;

use gsc::{Array, EntClass, Key, Obj, Value, Vm};

use super::Impl::{self, Later, Real};
use super::args::{Args, display};
use super::{FuncFn, hud, uicmd};
use crate::cvar;
use crate::game::{Ent, EntKind, Game};
use crate::ui::Table;
use sim::Vec3;
use sim::cm::{Collide, ENTITYNUM_NONE, ENTITYNUM_WORLD};
use sim::contents;

type R = Result<Value, String>;

const fn r(f: FuncFn) -> Impl<FuncFn> {
    Real(f)
}

const M3: &str = "M3 bots, pmove, traces, weapons";
const M5: &str = "M5 connect and play";
const M7: &str = "M7 audio";
const M8: &str = "M8 FX, decals, post";

pub const TABLE: &[(&str, Impl<FuncFn>)] = &[
    // core and types
    ("isdefined", r(is_defined)),
    (
        "isstring",
        r(|_, _, a| Ok(bool_v(matches!(a.get(0)?, Value::Str(_))))),
    ),
    ("isalive", r(is_alive)),
    ("getanimlength", r(get_anim_length)),
    ("animhasnotetrack", r(anim_has_notetrack)),
    ("getnotetracktimes", r(get_notetrack_times)),
    ("isplayer", r(is_player)),
    (
        "isplayernumber",
        r(|_, _, a| Ok(bool_v(matches!(a.get(0)?, Value::Int(_))))),
    ),
    ("int", r(cast_int)),
    (
        "spawnstruct",
        r(|_, _, _| Ok(Value::Object(Obj::new_struct()))),
    ),
    ("getarraykeys", r(get_array_keys)),
    ("issplitscreen", r(|_, _, _| Ok(Value::Int(0)))),
    (
        "resettimeout",
        r(|_, vm, _| {
            vm.reset_timeout();
            Ok(Value::Undefined)
        }),
    ),
    // developer-only in the original: no-ops unless `developer` is set
    ("assert", r(assert)),
    ("assertex", r(assert)),
    ("assertmsg", r(assert_msg)),
    ("print", r(|g, _, a| dev_print(g, a, false))),
    ("println", r(|g, _, a| dev_print(g, a, true))),
    ("print3d", r(|_, _, _| Ok(Value::Undefined))),
    ("line", r(|_, _, _| Ok(Value::Undefined))),
    ("createprintchannel", r(|_, _, _| Ok(Value::Undefined))),
    ("setprintchannel", r(|_, _, _| Ok(Value::Undefined))),
    ("logstring", r(log_string)),
    ("logprint", r(log_print)),
    ("iprintln", r(uicmd::level_print)),
    ("iprintlnbold", r(uicmd::level_print_bold)),
    ("allclientsprint", r(uicmd::level_print)),
    ("clientprint", r(uicmd::client_print_console)),
    ("announcement", r(uicmd::announcement)),
    ("clientannouncement", r(uicmd::client_announcement)),
    // dvars
    ("getdvar", r(get_dvar)),
    (
        "getdvarint",
        r(|g, _, a| dvar_typed(g, a, |s| Value::Int(cvar::parse_int(s)))),
    ),
    (
        "getdvarfloat",
        r(|g, _, a| dvar_typed(g, a, |s| Value::Float(cvar::parse_float(s)))),
    ),
    ("setdvar", r(set_dvar)),
    ("makedvarserverinfo", r(make_dvar_server_info)),
    ("gettime", r(|g, _, _| Ok(Value::Int(g.level.time)))),
    ("getstarttime", r(|_, _, _| Ok(Value::Int(0)))),
    // math
    (
        "sin",
        r(|_, _, a| Ok(Value::Float(a.float(0)?.to_radians().sin()))),
    ),
    (
        "cos",
        r(|_, _, a| Ok(Value::Float(a.float(0)?.to_radians().cos()))),
    ),
    (
        "tan",
        r(|_, _, a| Ok(Value::Float(a.float(0)?.to_radians().tan()))),
    ),
    (
        "asin",
        r(|_, _, a| Ok(Value::Float(a.float(0)?.asin().to_degrees()))),
    ),
    (
        "acos",
        r(|_, _, a| Ok(Value::Float(a.float(0)?.acos().to_degrees()))),
    ),
    (
        "atan",
        r(|_, _, a| Ok(Value::Float(a.float(0)?.atan().to_degrees()))),
    ),
    ("abs", r(abs)),
    (
        "min",
        r(|_, _, a| Ok(Value::Float(a.float(0)?.min(a.float(1)?)))),
    ),
    (
        "max",
        r(|_, _, a| Ok(Value::Float(a.float(0)?.max(a.float(1)?)))),
    ),
    ("floor", r(|_, _, a| Ok(Value::Float(a.float(0)?.floor())))),
    ("ceil", r(|_, _, a| Ok(Value::Float(a.float(0)?.ceil())))),
    ("sqrt", r(|_, _, a| Ok(Value::Float(a.float(0)?.sqrt())))),
    ("randomint", r(random_int)),
    ("randomintrange", r(random_int_range)),
    (
        "randomfloat",
        r(|g, _, a| Ok(Value::Float(rand_f(g) * a.float(0)?))),
    ),
    ("randomfloatrange", r(random_float_range)),
    (
        "distance",
        r(|_, _, a| Ok(Value::Float(len2(sub(a.vector(0)?, a.vector(1)?)).sqrt()))),
    ),
    ("distance2d", r(distance2d)),
    (
        "distancesquared",
        r(|_, _, a| Ok(Value::Float(len2(sub(a.vector(0)?, a.vector(1)?))))),
    ),
    (
        "length",
        r(|_, _, a| Ok(Value::Float(len2(a.vector(0)?).sqrt()))),
    ),
    (
        "lengthsquared",
        r(|_, _, a| Ok(Value::Float(len2(a.vector(0)?)))),
    ),
    ("closer", r(closer)),
    (
        "vectordot",
        r(|_, _, a| Ok(Value::Float(dot(a.vector(0)?, a.vector(1)?)))),
    ),
    ("vectornormalize", r(vector_normalize)),
    (
        "vectortoangles",
        r(|_, _, a| Ok(Value::Vector(vec_to_angles(a.vector(0)?)))),
    ),
    ("vectorlerp", r(vector_lerp)),
    (
        "anglestoforward",
        r(|_, _, a| Ok(Value::Vector(angle_vectors(a.vector(0)?).0))),
    ),
    (
        "anglestoright",
        r(|_, _, a| Ok(Value::Vector(angle_vectors(a.vector(0)?).1))),
    ),
    (
        "anglestoup",
        r(|_, _, a| Ok(Value::Vector(angle_vectors(a.vector(0)?).2))),
    ),
    ("vectorfromlinetopoint", r(vector_from_line_to_point)),
    ("pointonsegmentnearesttopoint", r(point_on_segment)),
    // strings and tables
    (
        "issubstr",
        r(|_, _, a| Ok(bool_v(a.string(0)?.contains(a.string(1)?)))),
    ),
    ("getsubstr", r(get_substr)),
    (
        "tolower",
        r(|_, _, a| Ok(Value::str(&a.string(0)?.to_ascii_lowercase()))),
    ),
    ("strtok", r(str_tok)),
    ("tablelookup", r(|g, _, a| table_lookup(g, a, false))),
    ("tablelookupistring", r(|g, _, a| table_lookup(g, a, true))),
    // precache: the server records names and indices
    ("precachemodel", r(precache_model)),
    (
        "precacheshader",
        r(|g, _, a| precache(g, a, Table::Material)),
    ),
    ("precachestring", r(precache_string)),
    ("precacheitem", r(precache_item)),
    ("precacheshellshock", r(|_, _, _| Ok(Value::Undefined))),
    ("precacherumble", r(|_, _, _| Ok(Value::Undefined))),
    ("precachemenu", r(|g, _, a| precache(g, a, Table::Menu))),
    (
        "precachestatusicon",
        r(|g, _, a| precache(g, a, Table::Material)),
    ),
    (
        "precacheheadicon",
        r(|g, _, a| precache(g, a, Table::Material)),
    ),
    (
        "precachelocationselector",
        r(|g, _, a| precache(g, a, Table::Material)),
    ),
    ("precacheturret", r(|_, _, _| Ok(Value::Undefined))),
    ("loadfx", r(load_fx)),
    // entities and world
    ("getent", r(get_ent)),
    ("getentarray", r(get_ent_array)),
    ("spawn", r(spawn)),
    (
        "worldentnumber",
        r(|_, _, _| Ok(Value::Int(i32::from(ENTITYNUM_WORLD)))),
    ),
    (
        "getnorthyaw",
        r(|g, _, _| Ok(Value::Float(g.level.north_yaw))),
    ),
    ("setmapcenter", r(|g, _, a| store_vec(g, a, "mapcenter"))),
    ("setminimap", r(set_minimap)),
    ("newhudelem", r(hud::new_hud_elem)),
    ("newclienthudelem", r(hud::new_client_hud_elem)),
    ("newteamhudelem", r(hud::new_team_hud_elem)),
    // game state
    ("getteamscore", r(get_team_score)),
    ("setteamscore", r(set_team_score)),
    (
        "setgameendtime",
        r(|g, _, a| {
            let t = a.int(0)?;
            g.set_configstring(net::ui::cs::GAMEENDTIME, &t.to_string());
            Ok(Value::Undefined)
        }),
    ),
    ("setclientnamemode", r(|_, _, _| Ok(Value::Undefined))),
    ("updateclientnames", r(|_, _, _| Ok(Value::Undefined))),
    ("setarchive", r(set_archive)),
    ("matchend", r(|_, _, _| Ok(Value::Undefined))),
    ("setplayerteamrank", r(|_, _, _| Ok(Value::Undefined))),
    ("sendranks", r(|_, _, _| Ok(Value::Undefined))),
    ("endparty", r(|_, _, _| Ok(Value::Undefined))),
    ("endlobby", r(|_, _, _| Ok(Value::Undefined))),
    ("setteamradar", r(|g, _, a| set_team_flag(g, a, "radar"))),
    ("getteamradar", r(|g, _, a| get_team_flag(g, a, "radar"))),
    ("setvotestring", Later(M5)),
    ("setvotetime", Later(M5)),
    ("setvoteyescount", Later(M5)),
    ("setvotenocount", Later(M5)),
    ("setwinningplayer", r(uicmd::set_winning_player)),
    ("setwinningteam", r(uicmd::set_winning_team)),
    (
        "exitlevel",
        r(|g, _, _| {
            g.level.exit_requested = true;
            Ok(Value::Undefined)
        }),
    ),
    (
        "map_restart",
        r(|g, _, _| {
            g.level.map_restart_requested = true;
            Ok(Value::Undefined)
        }),
    ),
    (
        "mapexists",
        r(|g, _, a| Ok(bool_v(g.map_exists(a.string(0)?)))),
    ),
    (
        "isvalidgametype",
        r(|g, _, a| Ok(bool_v(g.valid_gametype(a.string(0)?)))),
    ),
    ("addtestclient", Later("M3 bots")),
    ("objective_add", r(uicmd::objective_add)),
    ("objective_delete", r(uicmd::objective_delete)),
    ("objective_state", r(uicmd::objective_state)),
    ("objective_icon", r(uicmd::objective_icon)),
    ("objective_position", r(uicmd::objective_position)),
    ("objective_onentity", r(uicmd::objective_on_entity)),
    ("objective_team", r(uicmd::objective_team)),
    ("objective_current", r(uicmd::objective_current)),
    // world rendering, FX, audio: not a server concern until clients exist
    ("setexpfog", Later(M8)),
    ("visionsetnaked", r(|g, _, a| vision_set(g, a, false))),
    ("visionsetnight", r(|g, _, a| vision_set(g, a, true))),
    ("playfx", r(play_fx)),
    ("playfxontag", r(play_fx_on_tag)),
    ("playloopedfx", Later(M8)),
    ("spawnfx", Later(M8)),
    ("triggerfx", Later(M8)),
    ("earthquake", Later(M8)),
    ("physicsexplosionsphere", r(physics_explosion_sphere)),
    ("physicsexplosioncylinder", Later(M8)),
    ("physicsjolt", Later(M8)),
    ("physicsjitter", Later(M8)),
    ("grenadeexplosioneffect", Later(M8)),
    ("ambientplay", Real(super::sound::ambient_play)),
    ("ambientstop", Real(super::sound::ambient_stop)),
    ("musicplay", Real(super::sound::music_play)),
    ("musicstop", Real(super::sound::music_stop)),
    ("soundfade", Later(M7)),
    ("playrumbleonposition", Later(M7)),
    ("playrumblelooponposition", Later(M7)),
    ("stopallrumbles", Later(M7)),
    // collision, weapons, bots
    ("bullettrace", r(bullet_trace)),
    ("bullettracepassed", r(bullet_trace_passed)),
    ("sighttracepassed", r(sight_trace_passed)),
    ("physicstrace", r(physics_trace)),
    ("playerphysicstrace", r(player_physics_trace)),
    ("spawnhelicopter", r(spawn_helicopter)),
    ("spawnplane", Later("M8 vehicles")),
    ("spawnturret", Later(M3)),
];

/// `Com_SurfaceTypeToName`: the surface type (bits 20..24 of the surface flags) as the scripts
/// see it. Index 0 and anything past the table is `default`.
fn surface_type_name(flags: i32) -> &'static str {
    const NAMES: [&str; 28] = [
        "bark",
        "brick",
        "carpet",
        "cloth",
        "concrete",
        "dirt",
        "flesh",
        "foliage",
        "glass",
        "grass",
        "gravel",
        "ice",
        "metal",
        "mud",
        "paper",
        "plaster",
        "rock",
        "sand",
        "snow",
        "water",
        "wood",
        "asphalt",
        "ceramic",
        "plastic",
        "rubber",
        "cushion",
        "fruit",
        "paintedmetal",
    ];
    let i = ((flags & 0x01F0_0000) >> 20) as usize;
    i.checked_sub(1)
        .and_then(|i| NAMES.get(i))
        .copied()
        .unwrap_or("default")
}

/// The `(start, end, hitCharacters, ignoreEntity)` arguments of the shot traces.
fn shot_args(a: &Args, mask: i32) -> Result<(Vec3, Vec3, u16, i32), String> {
    let (start, end) = (a.vector(0)?, a.vector(1)?);
    let mask = if a.int(2)? == 0 {
        mask & !contents::PLAYER
    } else {
        mask
    };
    let ignore = match a.opt(3) {
        Some(Value::Object(o)) => o.entity().map_or(ENTITYNUM_NONE, |e| e.num),
        _ => ENTITYNUM_NONE,
    };
    Ok((start, end, ignore, mask))
}

fn world(g: &Game) -> Result<&sim::world::World, String> {
    g.world
        .as_ref()
        .ok_or_else(|| "no map is loaded".to_owned())
}

fn bullet_trace(g: &mut Game, vm: &mut Vm, a: Args) -> R {
    let (start, end, ignore, mask) = shot_args(&a, contents::MASK_SHOT)?;
    let t = world(g)?.bullet_trace(start, end, ignore, mask);
    let mut out = Array::new();
    let at = |k: &str, v: Value, out: &mut Array| out.set(Key::Str(k.into()), v);
    at("fraction", Value::Float(t.fraction), &mut out);
    let pos = std::array::from_fn(|i| start[i] + (end[i] - start[i]) * t.fraction);
    at("position", Value::Vector(pos), &mut out);
    let hit = match t.hit_id {
        ENTITYNUM_NONE | ENTITYNUM_WORLD => Value::Undefined,
        n => Value::Object(vm.entity(n, EntClass::Entity)),
    };
    at("entity", hit, &mut out);
    if t.fraction >= 1.0 {
        let d = sub(end, start);
        let l = len2(d).sqrt();
        let n = if l == 0.0 {
            [0.0; 3]
        } else {
            [d[0] / l, d[1] / l, d[2] / l]
        };
        at("normal", Value::Vector(n), &mut out);
        at("surfacetype", Value::str("none"), &mut out);
    } else {
        at("normal", Value::Vector(t.normal), &mut out);
        at(
            "surfacetype",
            Value::str(surface_type_name(t.surface_flags)),
            &mut out,
        );
    }
    Ok(Value::Array(Rc::new(out)))
}

fn bullet_trace_passed(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let (start, end, ignore, mask) = shot_args(&a, contents::MASK_SHOT)?;
    let ok = world(g)?.trace_passed(start, end, [0.0; 3], [0.0; 3], ignore, ENTITYNUM_NONE, mask);
    Ok(bool_v(ok))
}

/// Contents `sighttracepassed` collides with: solid, foliage, sky, no-sight, clip-shot,
/// vehicles, players (bit-exact from the original).
const SIGHT_MASK: i32 = 0x0280_1803;

fn sight_trace_passed(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let (start, end, ignore, mask) = shot_args(&a, SIGHT_MASK)?;
    let blocked = world(g)?.sight_trace(
        0,
        start,
        end,
        [0.0; 3],
        [0.0; 3],
        ignore,
        ENTITYNUM_NONE,
        mask,
    );
    Ok(bool_v(blocked == 0))
}

/// Mask of the physics traces: solid, clipshot-less world, player-clip, vehicle (0x820011).
const PHYSICS_MASK: i32 = 0x0082_0011;

fn physics_at(g: &Game, a: &Args, mins: Vec3, maxs: Vec3) -> R {
    let (start, end) = (a.vector(0)?, a.vector(1)?);
    let t = world(g)?.trace(start, end, mins, maxs, ENTITYNUM_NONE, PHYSICS_MASK);
    Ok(Value::Vector(std::array::from_fn(|i| {
        start[i] + (end[i] - start[i]) * t.fraction
    })))
}

fn physics_trace(g: &mut Game, _: &mut Vm, a: Args) -> R {
    physics_at(g, &a, [0.0; 3], [0.0; 3])
}

fn player_physics_trace(g: &mut Game, _: &mut Vm, a: Args) -> R {
    physics_at(g, &a, sim::pm::PLAYER_MINS, sim::pm::PLAYER_MAXS)
}

fn bool_v(b: bool) -> Value {
    Value::Int(i32::from(b))
}

/// The animation a `%name` value refers to.
fn anim_arg<'a>(
    g: &'a Game,
    a: &Args,
) -> Result<&'a std::sync::Arc<crate::content::AnimInfo>, String> {
    match a.get(0)? {
        Value::Anim(n) => g
            .content
            .anim(n)
            .ok_or_else(|| format!("animation '{n}' is not loaded")),
        o => Err(format!("type {} is not an anim", o.type_name())),
    }
}

fn get_anim_length(g: &mut Game, _: &mut Vm, a: Args) -> R {
    Ok(Value::Float(anim_arg(g, &a)?.length))
}

fn anim_has_notetrack(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let name = a.string(1)?;
    Ok(bool_v(
        anim_arg(g, &a)?.notes.iter().any(|(n, _)| &**n == name),
    ))
}

fn get_notetrack_times(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let name = a.string(1)?;
    let mut out = Array::new();
    for (i, (_, t)) in anim_arg(g, &a)?
        .notes
        .iter()
        .filter(|(n, _)| &**n == name)
        .enumerate()
    {
        out.set(Key::Int(i as i32), Value::Float(*t));
    }
    Ok(Value::Array(Rc::new(out)))
}

fn is_defined(_: &mut Game, _: &mut Vm, a: Args) -> R {
    Ok(bool_v(match a.get(0)? {
        Value::Undefined => false,
        Value::Object(o) => !o.is_dead(),
        _ => true,
    }))
}

fn is_alive(g: &mut Game, _: &mut Vm, a: Args) -> R {
    // `GScr_IsAlive`: anything that is not an entity (undefined, say) is not alive.
    Ok(bool_v(match a.get(0)? {
        Value::Object(o) => o
            .entity()
            .is_some_and(|e| g.ent(e.num).is_some_and(|e| e.health > 0)),
        _ => false,
    }))
}

fn is_player(g: &mut Game, _: &mut Vm, a: Args) -> R {
    Ok(bool_v(match a.get(0)? {
        Value::Object(o) => o
            .entity()
            .is_some_and(|e| g.ent(e.num).is_some_and(|e| e.kind == EntKind::Client)),
        _ => false,
    }))
}

fn cast_int(_: &mut Game, _: &mut Vm, a: Args) -> R {
    Ok(Value::Int(match a.get(0)? {
        Value::Int(n) => *n,
        Value::Float(x) => *x as i32,
        Value::Str(s) => cvar::parse_int(s),
        o => return Err(format!("cannot cast {} to int", o.type_name())),
    }))
}

fn get_array_keys(_: &mut Game, _: &mut Vm, a: Args) -> R {
    let arr = a.array(0)?;
    let mut out = Array::new();
    for (i, k) in arr.keys_original_order().enumerate() {
        let v = match k {
            Key::Int(n) => Value::Int(*n),
            Key::Str(s) => Value::Str(s.clone()),
        };
        out.set(Key::Int(i as i32), v);
    }
    Ok(Value::Array(Rc::new(out)))
}

fn developer(g: &Game) -> bool {
    g.cvars.int("developer") > 0
}

fn assert(g: &mut Game, _: &mut Vm, a: Args) -> R {
    if developer(g) && !truthy(a.get(0)?) {
        let msg = a.opt(1).map(display).unwrap_or_default();
        return Err(format!("assert fail: {msg}"));
    }
    Ok(Value::Undefined)
}

fn assert_msg(g: &mut Game, _: &mut Vm, a: Args) -> R {
    if developer(g) {
        return Err(format!("assert fail: {}", a.display(0)?));
    }
    Ok(Value::Undefined)
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Undefined => false,
        Value::Int(n) => *n != 0,
        Value::Float(x) => *x != 0.0,
        _ => true,
    }
}

fn dev_print(g: &mut Game, a: Args, newline: bool) -> R {
    if developer(g) {
        let mut s: String = a.v.iter().map(display).collect();
        if newline {
            s.push('\n');
        }
        g.print(s);
    }
    Ok(Value::Undefined)
}

/// `logString(text)`: the game log line; the stage counters read the bomb events from it.
pub(super) fn log_string(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let line = a.display(0)?;
    if line.starts_with("bomb planted") {
        g.stats.plants += 1;
    } else if line.starts_with("bomb defused") {
        g.stats.defuses += 1;
    } else if line.starts_with("hardpoint: ") {
        g.stats.hardpoints += 1;
        if line.contains("airstrike_mp") {
            g.stats.airstrikes += 1;
        } else if line.contains("helicopter_mp") {
            g.stats.helicopters += 1;
        }
    }
    Ok(Value::Undefined)
}

fn log_print(_: &mut Game, _: &mut Vm, _: Args) -> R {
    Ok(Value::Undefined)
}

/// Chat-area prints reach clients; the server console shows them too.
fn set_archive(g: &mut Game, _: &mut Vm, a: Args) -> R {
    g.archive_enabled = a.int(0)? != 0;
    Ok(Value::Undefined)
}

fn get_dvar(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let n = a.string(0)?;
    match g.cvars.get(n) {
        Some(v) => Ok(Value::str(&v.value)),
        None => Ok(a.opt(1).cloned().unwrap_or_else(|| Value::str(""))),
    }
}

fn dvar_typed(g: &mut Game, a: Args, f: fn(&str) -> Value) -> R {
    let n = a.string(0)?;
    match g.cvars.get(n) {
        Some(v) => Ok(f(&v.value)),
        None => Ok(match a.opt(1) {
            Some(d) => d.clone(),
            None => f(""),
        }),
    }
}

fn set_dvar(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let n = a.string(0)?;
    let v = match a.get(1)? {
        Value::Int(i) => i.to_string(),
        Value::Float(x) => super::args::format_float(*x),
        Value::Str(s) | Value::LocStr(s) => s.to_string(),
        Value::Vector(v) => display(&Value::Vector(*v)),
        o => return Err(format!("type {} is not a string", o.type_name())),
    };
    g.script_set_dvar(n, &v);
    Ok(Value::Undefined)
}

fn make_dvar_server_info(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let n = a.string(0)?;
    if let Some(d) = a.opt(1) {
        if !g.cvars.exists(n) {
            g.cvars.set(n, &display(d));
        }
    } else if !g.cvars.exists(n) {
        g.cvars.set(n, "");
    }
    g.cvars.add_flags(n, cvar::SERVERINFO);
    Ok(Value::Undefined)
}

fn abs(_: &mut Game, _: &mut Vm, a: Args) -> R {
    Ok(match a.get(0)? {
        Value::Int(n) => Value::Int(n.wrapping_abs()),
        _ => Value::Float(a.float(0)?.abs()),
    })
}

fn rand_f(g: &mut Game) -> f32 {
    (g.rand() & 0x7FFF) as f32 / 32768.0
}

fn random_int(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let n = a.int(0)?;
    if n <= 0 {
        return Err("RandomInt parm must be positive integer".into());
    }
    Ok(Value::Int((g.rand() % n as u32) as i32))
}

fn random_int_range(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let (lo, hi) = (a.int(0)?, a.int(1)?);
    let span = hi.wrapping_sub(lo);
    if span <= 0 {
        return Err("RandomIntRange parms must be ordered".into());
    }
    Ok(Value::Int(lo + (g.rand() % span as u32) as i32))
}

fn random_float_range(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let (lo, hi) = (a.float(0)?, a.float(1)?);
    Ok(Value::Float(lo + rand_f(g) * (hi - lo)))
}

type V3 = [f32; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn len2(a: V3) -> f32 {
    dot(a, a)
}

fn distance2d(_: &mut Game, _: &mut Vm, a: Args) -> R {
    let d = sub(a.vector(0)?, a.vector(1)?);
    Ok(Value::Float((d[0] * d[0] + d[1] * d[1]).sqrt()))
}

fn closer(_: &mut Game, _: &mut Vm, a: Args) -> R {
    let p = a.vector(0)?;
    Ok(bool_v(
        len2(sub(p, a.vector(1)?)) < len2(sub(p, a.vector(2)?)),
    ))
}

fn vector_normalize(_: &mut Game, _: &mut Vm, a: Args) -> R {
    let v = a.vector(0)?;
    let l = len2(v).sqrt();
    Ok(Value::Vector(if l == 0.0 {
        v
    } else {
        [v[0] / l, v[1] / l, v[2] / l]
    }))
}

fn vector_lerp(_: &mut Game, _: &mut Vm, a: Args) -> R {
    let (p, q, t) = (a.vector(0)?, a.vector(1)?, a.float(2)?);
    Ok(Value::Vector([
        p[0] + (q[0] - p[0]) * t,
        p[1] + (q[1] - p[1]) * t,
        p[2] + (q[2] - p[2]) * t,
    ]))
}

/// `vectoangles`: pitch is negative looking up, roll 0.
pub fn vec_to_angles(v: V3) -> V3 {
    let (yaw, pitch);
    if v[0] == 0.0 && v[1] == 0.0 {
        yaw = 0.0;
        pitch = if v[2] > 0.0 { 90.0 } else { 270.0 };
    } else {
        let mut y = v[1].atan2(v[0]).to_degrees();
        if y < 0.0 {
            y += 360.0;
        }
        yaw = y;
        let fwd = (v[0] * v[0] + v[1] * v[1]).sqrt();
        let mut p = v[2].atan2(fwd).to_degrees();
        if p < 0.0 {
            p += 360.0;
        }
        pitch = p;
    }
    [-pitch, yaw, 0.0]
}

/// `AngleVectors`: forward, right, up for (pitch, yaw, roll) in degrees.
pub fn angle_vectors(a: V3) -> (V3, V3, V3) {
    let (sy, cy) = a[1].to_radians().sin_cos();
    let (sp, cp) = a[0].to_radians().sin_cos();
    let (sr, cr) = a[2].to_radians().sin_cos();
    (
        [cp * cy, cp * sy, -sp],
        [-sr * sp * cy + cr * sy, -sr * sp * sy - cr * cy, -sr * cp],
        [cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp],
    )
}

fn vector_from_line_to_point(_: &mut Game, _: &mut Vm, a: Args) -> R {
    let (p0, p1, pt) = (a.vector(0)?, a.vector(1)?, a.vector(2)?);
    let d = sub(p1, p0);
    let l2 = len2(d);
    let t = if l2 == 0.0 {
        0.0
    } else {
        dot(sub(pt, p0), d) / l2
    };
    let on = [p0[0] + d[0] * t, p0[1] + d[1] * t, p0[2] + d[2] * t];
    Ok(Value::Vector(sub(pt, on)))
}

fn point_on_segment(_: &mut Game, _: &mut Vm, a: Args) -> R {
    let (p0, p1, pt) = (a.vector(0)?, a.vector(1)?, a.vector(2)?);
    let d = sub(p1, p0);
    let l2 = len2(d);
    let t = if l2 == 0.0 {
        0.0
    } else {
        (dot(sub(pt, p0), d) / l2).clamp(0.0, 1.0)
    };
    Ok(Value::Vector([
        p0[0] + d[0] * t,
        p0[1] + d[1] * t,
        p0[2] + d[2] * t,
    ]))
}

fn get_substr(_: &mut Game, _: &mut Vm, a: Args) -> R {
    let s = a.string(0)?;
    let start = a.int(1)?.max(0) as usize;
    let end = match a.opt(2) {
        Some(_) => (a.int(2)?.max(0) as usize).min(s.len()),
        None => s.len(),
    };
    if start >= end {
        return Ok(Value::str(""));
    }
    Ok(Value::str(&String::from_utf8_lossy(
        &s.as_bytes()[start..end],
    )))
}

fn str_tok(_: &mut Game, _: &mut Vm, a: Args) -> R {
    let (s, delims) = (a.string(0)?, a.string(1)?);
    let mut out = Array::new();
    for (i, t) in s
        .split(|c| delims.contains(c))
        .filter(|t| !t.is_empty())
        .enumerate()
    {
        out.set(Key::Int(i as i32), Value::str(t));
    }
    Ok(Value::Array(Rc::new(out)))
}

fn table_lookup(g: &mut Game, a: Args, localized: bool) -> R {
    let file = a.string(0)?;
    let (search, ret) = (a.int(1)?, a.int(3)?);
    let key = match a.get(2)? {
        Value::Int(n) => n.to_string(),
        _ => a.string(2)?.to_owned(),
    };
    let t = g
        .content
        .string_table(file)
        .ok_or_else(|| format!("Could not find stringtable '{file}'"))?;
    let cols = t.column_count as usize;
    let (search, ret) = (search as usize, ret as usize);
    if search >= cols || ret >= cols {
        return Err(format!(
            "column {} out of range for '{file}'",
            search.max(ret)
        ));
    }
    let cell = |r: usize, c: usize| t.values.get(r * cols + c).and_then(|n| n.as_deref());
    let hit = (0..t.row_count as usize)
        .find(|&r| cell(r, search).is_some_and(|s| s.eq_ignore_ascii_case(&key)))
        .and_then(|r| cell(r, ret))
        .unwrap_or("");
    Ok(if localized {
        Value::LocStr(hit.into())
    } else {
        Value::str(hit)
    })
}

fn precache(g: &mut Game, a: Args, table: Table) -> R {
    let n = a.string(0)?.to_owned();
    g.precache(table, &n)?;
    Ok(Value::Undefined)
}

fn precache_model(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let n = a.string(0)?;
    if g.content.model(n).is_none() {
        g.print(format!("precachemodel: model '{n}' not found\n"));
    }
    g.precache(Table::Model, n)?;
    Ok(Value::Undefined)
}

fn precache_string(g: &mut Game, _: &mut Vm, a: Args) -> R {
    match a.get(0)? {
        Value::LocStr(s) => {
            g.precache(Table::Text, &format!("&{s}"))?;
            Ok(Value::Undefined)
        }
        Value::Str(s) => {
            let s = s.to_string();
            g.precache(Table::Text, &s)?;
            Ok(Value::Undefined)
        }
        o => Err(format!("type {} is not a localized string", o.type_name())),
    }
}

fn precache_item(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let n = a.string(0)?;
    if g.content.weapon(n).is_none() {
        return Err(format!("unknown item '{n}'"));
    }
    g.items.index(n);
    Ok(Value::Undefined)
}

/// `playfx(fx, origin, forward, up)`: an effect where the script says.
fn play_fx(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let index = a.int(0)?;
    let origin = a.vector(1)?;
    let forward = if a.len() > 2 {
        a.vector(2)?
    } else {
        [0.0, 0.0, 1.0]
    };
    emit_fx(g, index, origin, forward, 1023);
    Ok(Value::Undefined)
}

/// `playfxontag(fx, entity, tag)`: an effect at a tag of an entity, played where the tag is now.
fn play_fx_on_tag(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let index = a.int(0)?;
    let ent = a.entity(1)?;
    let tag = a.string(2)?.to_ascii_lowercase();
    let n = ent.num;
    let (origin, forward) = match g.world_tag(n, &tag) {
        Some(m) => (m[3], m[0]),
        None => (g.ent(n).map_or([0.0; 3], |e| e.origin), [1.0, 0.0, 0.0]),
    };
    emit_fx(g, index, origin, forward, n);
    Ok(Value::Undefined)
}

/// `visionsetnaked(name, seconds)` and `visionsetnight(name, seconds)`: every client blends to the named vision file.
fn vision_set(g: &mut Game, a: Args, night: bool) -> R {
    let name = a.string(0)?.to_owned();
    let ms = if a.len() > 1 {
        (a.float(1)? * 1000.0).round() as i32
    } else {
        1000
    };
    g.vision[usize::from(night)] = Some(name.clone());
    g.send(
        crate::ui::Dest::All,
        net::ui::ServerCmd::Vision { night, name, ms },
    );
    Ok(Value::Undefined)
}

fn emit_fx(g: &mut Game, index: i32, origin: V3, forward: V3, ent: u16) {
    let now = g.level.time;
    g.tempev.add(now, crate::tempev::ev::PLAY_FX, |s| {
        s.origin = origin;
        s.angles = crate::tempev::dir_to_angles(forward);
        s.model = index.clamp(0, 1023) as u16;
        s.client = ent;
    });
}

/// `physicsexplosionsphere(origin, radius, inner, strength)`: tells the clients' bodies to be thrown.
fn physics_explosion_sphere(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let origin = a.vector(0)?;
    let radius = a.float(1)?;
    let strength = a.float(3)?;
    let now = g.level.time;
    g.tempev
        .add(now, crate::tempev::ev::PHYSICS_EXPLOSION, |s| {
            s.origin = origin;
            s.velocity = [radius, 0.0, 0.0];
            s.weapon = (strength * 10.0).clamp(0.0, 511.0) as u16;
        });
    Ok(Value::Undefined)
}

fn load_fx(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let n = a.string(0)?.to_owned();
    Ok(Value::Int(g.fx.index(&n) as i32))
}

/// The engine fields `getent` and `getentarray` can search by (string-typed fields).
fn ent_string_field<'a>(e: &'a Ent, key: &str) -> Option<&'a str> {
    match key.to_ascii_lowercase().as_str() {
        "classname" => Some(&e.classname),
        "target" => e.target.as_deref(),
        "targetname" => e.targetname.as_deref(),
        _ => None,
    }
}

fn find_ents(g: &Game, a: Args) -> Result<Vec<u16>, String> {
    let (name, key) = (a.string(0)?, a.string(1)?);
    if !matches!(
        key.to_ascii_lowercase().as_str(),
        "classname" | "target" | "targetname"
    ) {
        return Ok(Vec::new());
    }
    Ok(g.in_use()
        .filter(|(_, e)| e.kind != EntKind::World)
        .filter(|(_, e)| ent_string_field(e, key) == Some(name))
        .map(|(n, _)| n)
        .collect())
}

fn get_ent(g: &mut Game, vm: &mut Vm, a: Args) -> R {
    let found = find_ents(g, a)?;
    match found.as_slice() {
        [] => Ok(Value::Undefined),
        [n] => Ok(Value::Object(vm.entity(*n, EntClass::Entity))),
        _ => Err("getent used with more than one entity".into()),
    }
}

fn get_ent_array(g: &mut Game, vm: &mut Vm, a: Args) -> R {
    let nums: Vec<u16> = if a.is_empty() {
        g.in_use().map(|(n, _)| n).collect()
    } else {
        find_ents(g, a)?
    };
    let mut out = Array::new();
    for (i, n) in nums.into_iter().enumerate() {
        out.set(
            Key::Int(i as i32),
            Value::Object(vm.entity(n, EntClass::Entity)),
        );
    }
    Ok(Value::Array(Rc::new(out)))
}

/// `spawnHelicopter(owner, origin, angles, vehicleType, model)`.
fn spawn_helicopter(g: &mut Game, vm: &mut Vm, a: Args) -> R {
    let owner = a.entity(0)?;
    let n = g.spawn_helicopter(owner.num, a.vector(1)?, a.vector(2)?, a.string(3)?, a.string(4)?)?;
    Ok(g.entity_value(vm, n))
}

fn spawn(g: &mut Game, vm: &mut Vm, a: Args) -> R {
    let class = a.string(0)?;
    let origin = a.vector(1)?;
    let flags = if a.len() > 2 { a.int(2)? } else { 0 };
    let kind = match class {
        "script_model" | "script_origin" | "info_notnull" | "info_notnull_big" => EntKind::Plain,
        "trigger_radius" => EntKind::Trigger,
        c if c.starts_with("weapon_") => EntKind::Item,
        c => return Err(format!("{c} cannot be spawned dynamically")),
    };
    let mut e = Ent::new(kind, class);
    e.origin = origin;
    e.spawnflags = flags;
    let n = g.spawn(e)?;
    let extent = (class == "trigger_radius" && a.len() > 4)
        .then(|| Ok::<_, String>((a.float(3)?, a.float(4)?)))
        .transpose()?;
    g.init_clip(n, extent);
    let obj = vm.entity(n, EntClass::Entity);
    if class == "trigger_radius" {
        for (i, f) in ["radius", "height"].into_iter().enumerate() {
            if a.len() > 3 + i {
                obj.set(&f.into(), Value::Float(a.float(3 + i)?));
            }
        }
    }
    Ok(Value::Object(obj))
}

fn store_vec(g: &mut Game, a: Args, key: &str) -> R {
    let v = a.vector(0)?;
    g.configstrings
        .insert(config_key(key), format!("{} {} {}", v[0], v[1], v[2]));
    Ok(Value::Undefined)
}

/// `setMiniMap(material, upperLeftX, upperLeftY, lowerRightX, lowerRightY)`: the clients draw the compass from it.
fn set_minimap(g: &mut Game, _: &mut Vm, a: Args) -> R {
    if a.len() != 5 {
        return Err("Expecting 5 arguments".into());
    }
    let text = format!(
        "\"{}\" {} {} {} {}",
        a.string(0)?,
        a.float(1)?,
        a.float(2)?,
        a.float(3)?,
        a.float(4)?
    );
    let (ul, lr) = ([a.float(1)?, a.float(2)?], [a.float(3)?, a.float(4)?]);
    let yaw = g.level.north_yaw.to_radians();
    let north = [yaw.cos(), yaw.sin()];
    let d = [lr[0] - ul[0], lr[1] - ul[1]];
    let size = [
        d[0] * north[1] - d[1] * north[0],
        -d[0] * north[0] - d[1] * north[1],
    ];
    if size[0] < 0.0 || size[1] < 0.0 {
        return Err("lower-right X and Y coordinates must be both south and east of upper-left X and Y coordinates in terms of the northyaw".into());
    }
    g.compass = Some([size[0], size[1], north[0], north[1], ul[0], ul[1]]);
    g.set_configstring(net::ui::cs::MINIMAP, &text);
    Ok(Value::Undefined)
}

/// Stable pseudo-indices for configstrings the scripts set by name.
fn config_key(name: &str) -> u32 {
    name.bytes().fold(0x1000_0000u32, |h, b| {
        h.wrapping_mul(31).wrapping_add(u32::from(b))
    })
}

fn team_index(a: Args) -> Result<usize, String> {
    match a.string(0)? {
        "allies" => Ok(1),
        "axis" => Ok(2),
        t => Err(format!("team '{t}' is not allies or axis")),
    }
}

fn get_team_score(g: &mut Game, _: &mut Vm, a: Args) -> R {
    Ok(Value::Int(g.team_score[team_index(a)?]))
}

fn set_team_score(g: &mut Game, _: &mut Vm, a: Args) -> R {
    g.team_score[team_index(a)?] = a.int(1)?;
    Ok(Value::Undefined)
}

fn set_team_flag(g: &mut Game, a: Args, key: &str) -> R {
    let t = team_index(a)?;
    g.configstrings
        .insert(config_key(&format!("{key}{t}")), a.int(1)?.to_string());
    Ok(Value::Undefined)
}

fn get_team_flag(g: &mut Game, a: Args, key: &str) -> R {
    let t = team_index(a)?;
    let v = g.configstrings.get(&config_key(&format!("{key}{t}")));
    Ok(Value::Int(v.map_or(0, |s| cvar::parse_int(s))))
}

#[cfg(test)]
mod trace_tests {
    use super::surface_type_name;

    #[test]
    fn surface_types_map_to_script_names() {
        assert_eq!(surface_type_name(0), "default");
        assert_eq!(surface_type_name(0x0010_0000), "bark");
        assert_eq!(surface_type_name(0x0150_0000 | 0x2), "wood");
        assert_eq!(surface_type_name(0x01C0_0000), "paintedmetal");
        assert_eq!(surface_type_name(0x01D0_0000), "default");
    }
}
