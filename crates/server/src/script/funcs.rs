// SPDX-License-Identifier: GPL-3.0-only
//! Builtin functions (no entity receiver).

use std::rc::Rc;

use gsc::{Array, EntClass, Key, Obj, Value, Vm};

use super::Impl::{self, Later, Real};
use super::args::{Args, display};
use super::{FuncFn, hud, uicmd};
use crate::cvar;
use crate::game::{Ent, EntKind, Game, WorldFx};
use crate::tempev::Physics;
use crate::ui::Table;
use sim::Vec3;
use sim::cm::{Collide, ENTITYNUM_NONE, ENTITYNUM_WORLD};
use sim::contents;

type R = Result<Value, String>;

const fn r(f: FuncFn) -> Impl<FuncFn> {
    Real(f)
}

const M3: &str = "M3 bots, pmove, traces, weapons";
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
    ("setteamradar", r(set_team_radar)),
    ("getteamradar", r(get_team_radar)),
    (
        "setvotestring",
        r(|g, _, a| {
            if !a.is_empty() {
                g.script_vote_string(a.string(0)?);
            }
            Ok(Value::Undefined)
        }),
    ),
    (
        "setvotetime",
        r(|g, _, a| {
            if !a.is_empty() {
                g.script_vote_time(a.int(0)?);
            }
            Ok(Value::Undefined)
        }),
    ),
    (
        "setvoteyescount",
        r(|g, _, a| {
            if !a.is_empty() {
                g.script_vote_yes(a.int(0)?);
            }
            Ok(Value::Undefined)
        }),
    ),
    (
        "setvotenocount",
        r(|g, _, a| {
            if !a.is_empty() {
                g.script_vote_no(a.int(0)?);
            }
            Ok(Value::Undefined)
        }),
    ),
    ("setwinningplayer", r(uicmd::set_winning_player)),
    ("setwinningteam", r(uicmd::set_winning_team)),
    (
        "exitlevel",
        r(|g, _, a| {
            g.level.exit_requested = true;
            g.level.save_persist = !a.is_empty() && a.int(0)? != 0;
            Ok(Value::Undefined)
        }),
    ),
    (
        "map_restart",
        r(|g, _, a| {
            g.level.map_restart_requested = true;
            g.level.save_persist = !a.is_empty() && a.int(0)? != 0;
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
    ("playloopedfx", r(play_looped_fx)),
    ("spawnfx", r(spawn_fx)),
    ("triggerfx", r(trigger_fx)),
    ("earthquake", r(earthquake)),
    (
        "physicsexplosionsphere",
        r(|g, _, a| physics_explosion(g, a, false)),
    ),
    (
        "physicsexplosioncylinder",
        r(|g, _, a| physics_explosion(g, a, true)),
    ),
    ("physicsjolt", r(physics_jolt)),
    ("physicsjitter", r(physics_jitter)),
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
    ("spawnplane", r(spawn_plane)),
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

/// `logPrint(text, ...)`: the parameters joined (up to 1023 bytes) as one line of the game log.
fn log_print(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let mut line = String::new();
    for i in 0..a.len() {
        let part = a.display(i)?;
        if line.len() + part.len() >= 1024 {
            break;
        }
        line.push_str(&part);
    }
    g.log_print(&line);
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

/// `playfx(fx, origin, [forward, [up]])`: an effect where the script says, turned to face `forward` and rolled so its
/// up is `up`.
fn play_fx(g: &mut Game, _: &mut Vm, a: Args) -> R {
    if !(2..=4).contains(&a.len()) {
        return Err("Incorrect number of parameters".into());
    }
    let index = a.int(0)?;
    let origin = a.vector(1)?;
    let angles = fx_angles(g, &a, 2, "playFx", index)?;
    let now = g.level.time;
    g.tempev.add(now, crate::tempev::ev::PLAY_FX, |s| {
        s.origin = origin;
        s.angles = angles;
        s.model = index.clamp(0, 1023) as u16;
        s.client = 1023;
    });
    Ok(Value::Undefined)
}

/// `Scr_FxParamError`: a parameter error that names the effect.
fn fx_param_error(g: &Game, what: &str, index: i32) -> String {
    let name = usize::try_from(index)
        .ok()
        .filter(|i| *i > 0)
        .and_then(|i| g.fx.name(i))
        .unwrap_or("not successfully loaded");
    format!("{what} (effect = {name})")
}

/// The angles of the optional `forward` (at parameter `at`) and `up` (at `at + 1`) the effect builtins take
/// (`Scr_SetFxAngles`): straight up with neither, facing `forward` with one, and rolled to put `up` above with both.
fn fx_angles(g: &Game, a: &Args, at: usize, who: &str, index: i32) -> Result<V3, String> {
    use sim::pm::math::{cross, mad, normalize};
    let unit = |i: usize, what: &str| -> Result<V3, String> {
        let mut v = a.vector(i)?;
        if normalize(&mut v) == 0.0 {
            return Err(fx_param_error(
                g,
                &format!("{who} called with (0 0 0) {what} direction"),
                index,
            ));
        }
        Ok(v)
    };
    let upper = at + 1;
    // `playloopedfx` takes the up vector before the forward vector is checked; the order of the errors is the original's.
    let up = (a.len() > upper).then(|| unit(upper, "up")).transpose()?;
    let forward = (a.len() > at).then(|| unit(at, "forward")).transpose()?;
    Ok(match (forward, up) {
        (None, _) => [270.0, 0.0, 0.0],
        (Some(f), None) => vec_to_angles(f),
        (Some(f), Some(u)) => {
            let mut u = mad(&u, -sim::pm::math::dot(&f, &u), &f);
            if normalize(&mut u) == 0.0 {
                return Err(
                    "forward and up vectors are the same direction or exact opposite directions"
                        .into(),
                );
            }
            crate::tags::axis_to_angles(&[f, cross(&u, &f), u])
        }
    })
}

/// `spawnfx(fx, origin, [forward, [up]])`: an effect entity that plays when `triggerfx` is called on it.
fn spawn_fx(g: &mut Game, vm: &mut Vm, a: Args) -> R {
    if !(2..=4).contains(&a.len()) {
        return Err("Incorrect number of parameters".into());
    }
    let index = a.int(0)?;
    let origin = a.vector(1)?;
    let angles = fx_angles(g, &a, 2, "spawnFx", index)?;
    let mut e = Ent::new(EntKind::Plain, "fx");
    e.origin = origin;
    e.angles = angles;
    e.world_fx = Some(WorldFx::Once {
        effect: index.clamp(0, 1023) as u16,
        triggers: 0,
        start_ms: 0,
    });
    let n = g.spawn(e)?;
    Ok(g.entity_value(vm, n))
}

/// `playloopedfx(fx, repeat, origin, [cull, [forward, [up]]])`: an effect entity that replays every `repeat` seconds
/// until it is deleted, for clients within `cull` units of it.
fn play_looped_fx(g: &mut Game, vm: &mut Vm, a: Args) -> R {
    if !(3..=6).contains(&a.len()) {
        return Err("Incorrect number of parameters".into());
    }
    let index = a.int(0)?;
    let cull = if a.len() > 3 { a.float(3)? } else { 0.0 };
    let origin = a.vector(2)?;
    let angles = fx_angles(g, &a, 4, "playLoopedFx", index)?;
    let period = sim::pm::math::snap_to_int(a.float(1)? * 1000.0);
    if period <= 0 {
        return Err(fx_param_error(
            g,
            "playLoopedFx called with repeat < 0.001 seconds",
            index,
        ));
    }
    let mut e = Ent::new(EntKind::Plain, "fx");
    e.origin = origin;
    e.angles = angles;
    e.world_fx = Some(WorldFx::Looped {
        effect: index.clamp(0, 1023) as u16,
        period_ms: period.min((1 << 21) - 1) as u32,
        cull,
    });
    let n = g.spawn(e)?;
    Ok(g.entity_value(vm, n))
}

/// `triggerfx(fx, [delay])`: plays an entity `spawnfx` made, after `delay` seconds.
fn trigger_fx(g: &mut Game, _: &mut Vm, a: Args) -> R {
    if a.is_empty() || a.len() > 2 {
        return Err("Incorrect number of parameters".into());
    }
    let n = a.entity(0)?.num;
    let delay = if a.len() == 2 {
        sim::pm::math::snap_to_int(a.float(1)? * 1000.0).clamp(0, 1 << 22)
    } else {
        0
    };
    let now = g.level.time;
    match g.ent_mut(n).map(|e| &mut e.world_fx) {
        Some(Some(WorldFx::Once {
            triggers, start_ms, ..
        })) => {
            *triggers = triggers.wrapping_add(1).max(1);
            *start_ms = now + delay;
            Ok(Value::Undefined)
        }
        _ => Err("entity wasn't created with 'newFx'".into()),
    }
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

/// The outer and inner radius of a physics event (`origin, outer, inner, ...`).
fn physics_radii(a: &Args) -> Result<(f32, f32), String> {
    let outer = a.float(1)?.round();
    let inner = a.float(2)?;
    if inner < 0.0 {
        return Err("Radius is negative".into());
    }
    if inner > outer {
        return Err("Inner radius is outside the outer radius".into());
    }
    Ok((outer, inner))
}

/// `physicsexplosionsphere(origin, outer, inner, magnitude)` and `physicsexplosioncylinder`: loose bodies are thrown
/// from the origin.
fn physics_explosion(g: &mut Game, a: Args, cylinder: bool) -> R {
    if a.len() != 4 {
        return Err("Incorrect number of parameters".into());
    }
    let origin = a.vector(0)?;
    let (outer, inner) = physics_radii(&a)?;
    let magnitude = a.float(3)?;
    g.physics_event(
        origin,
        Physics::Explosion {
            cylinder,
            outer,
            inner,
            magnitude,
        },
    );
    Ok(Value::Undefined)
}

/// `physicsjolt(origin, outer, inner, impulse)`: loose bodies are thrown along `impulse`.
fn physics_jolt(g: &mut Game, _: &mut Vm, a: Args) -> R {
    if a.len() != 4 {
        return Err("Incorrect number of parameters".into());
    }
    let origin = a.vector(0)?;
    let (outer, inner) = physics_radii(&a)?;
    let impulse = a.vector(3)?;
    g.physics_event(
        origin,
        Physics::Jolt {
            outer,
            inner,
            impulse,
        },
    );
    Ok(Value::Undefined)
}

/// `physicsjitter(origin, outer, inner, min, max)`: loose bodies hop by a distance between `min` and `max`.
fn physics_jitter(g: &mut Game, _: &mut Vm, a: Args) -> R {
    if a.len() != 5 {
        return Err("Incorrect number of parameters".into());
    }
    let origin = a.vector(0)?;
    let (outer, inner) = physics_radii(&a)?;
    let (min, max) = (a.float(3)?, a.float(4)?);
    if max < min {
        return Err("Maximum jitter is less than minimum jitter".into());
    }
    g.physics_event(
        origin,
        Physics::Jitter {
            outer,
            inner,
            min,
            max,
        },
    );
    Ok(Value::Undefined)
}

/// `earthquake(scale, duration, origin, radius)`: shakes the camera of every client near the origin.
fn earthquake(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let scale = a.float(0)?;
    let duration = sim::pm::math::snap_to_int(a.float(1)? * 1000.0);
    let origin = a.vector(2)?;
    let radius = a.float(3)?;
    if scale <= 0.0 {
        return Err("Scale must be greater than 0".into());
    }
    if duration <= 0 {
        return Err("duration must be greater than 0".into());
    }
    if radius <= 0.0 {
        return Err("Radius must be greater than 0".into());
    }
    g.earthquake(origin, scale, duration, radius);
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
    let n = g.spawn_helicopter(
        owner.num,
        a.vector(1)?,
        a.vector(2)?,
        a.string(3)?,
        a.string(4)?,
    )?;
    Ok(g.entity_value(vm, n))
}

/// `spawnPlane(owner, "script_model", origin)`: a model the airstrike flies past; clients draw it as a script model and mark it on the compass.
fn spawn_plane(g: &mut Game, vm: &mut Vm, a: Args) -> R {
    let owner = a.entity(0)?;
    if !g.is_client(owner.num) {
        return Err("Owner entity is not a player".into());
    }
    if a.string(1)? != "script_model" {
        return Err("spawnPlane only spawns a script_model".into());
    }
    let mut e = Ent::new(EntKind::Plain, "script_model");
    e.origin = a.vector(2)?;
    e.x.plane_owner = Some(owner.num);
    let n = g.spawn(e)?;
    g.init_clip(n, None);
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
        "axis" => Ok(1),
        "allies" => Ok(2),
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

/// The index into `Game::team_radar`: `none` (free-for-all players), `axis` or `allies`.
fn radar_team(a: Args) -> Result<usize, String> {
    match a.string(0)? {
        "none" => Ok(0),
        "axis" => Ok(1),
        "allies" => Ok(2),
        t => Err(format!("team '{t}' is not none, allies or axis")),
    }
}

fn set_team_radar(g: &mut Game, _: &mut Vm, a: Args) -> R {
    g.team_radar[radar_team(a)?] = a.int(1)? != 0;
    Ok(Value::Undefined)
}

fn get_team_radar(g: &mut Game, _: &mut Vm, a: Args) -> R {
    Ok(Value::Int(i32::from(g.team_radar[radar_team(a)?])))
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

#[cfg(test)]
mod team_score_tests {
    use super::*;
    use gsc::{Builtins, Options, compile};
    use net::ui::cs;

    fn vm() -> Vm {
        let prog = compile(
            &[("t.gsc", "main() {}")],
            &Builtins::stock_mp(),
            Options::default(),
        )
        .unwrap();
        Vm::new(prog).unwrap()
    }

    #[test]
    fn a_team_score_reaches_that_teams_banner_only() {
        let (mut g, mut vm) = (
            Game::new(crate::cvar::Cvars::new(), Default::default()),
            vm(),
        );
        let set = |g: &mut Game, vm: &mut Vm, team: &str, n: i32| {
            let v = [Value::str(team), Value::Int(n)];
            set_team_score(g, vm, Args::new("setteamscore", &v)).unwrap();
        };
        set(&mut g, &mut vm, "allies", 5);
        g.refresh_client_info();
        assert_eq!(g.configstrings[&u32::from(cs::SCORES_ALLIES)], "5");
        assert_eq!(g.configstrings[&u32::from(cs::SCORES_AXIS)], "0");
        set(&mut g, &mut vm, "axis", 7);
        g.refresh_client_info();
        assert_eq!(g.configstrings[&u32::from(cs::SCORES_ALLIES)], "5");
        assert_eq!(g.configstrings[&u32::from(cs::SCORES_AXIS)], "7");
        let v = [Value::str("allies")];
        assert!(matches!(
            get_team_score(&mut g, &mut vm, Args::new("getteamscore", &v)),
            Ok(Value::Int(5))
        ));
    }
}

#[cfg(test)]
mod world_fx_tests {
    use super::*;
    use crate::netsv::world_entities;
    use crate::tempev::{Earthquake, ev};
    use gsc::{Builtins, Options, compile};
    use net::entity::etype;

    fn setup() -> (Game, Vm) {
        let prog = compile(
            &[("t.gsc", "main() {}")],
            &Builtins::stock_mp(),
            Options::default(),
        )
        .unwrap();
        let mut g = Game::new(crate::cvar::Cvars::new(), Default::default());
        g.fx.index("fx/test/smoke");
        (g, Vm::new(prog).unwrap())
    }

    fn v(x: f32, y: f32, z: f32) -> Value {
        Value::Vector([x, y, z])
    }

    fn num(r: R) -> u16 {
        match r.unwrap() {
            Value::Object(o) => o.entity().expect("an entity").num,
            o => panic!("{o:?}"),
        }
    }

    fn fx_states(g: &Game) -> Vec<net::entity::EntityState> {
        world_entities(g)
            .into_iter()
            .filter(|e| matches!(e.etype, etype::FX | etype::LOOP_FX))
            .collect()
    }

    #[test]
    fn a_spawned_effect_plays_once_per_trigger_until_deleted() {
        let (mut g, mut vm) = setup();
        let args = [Value::Int(1), v(1.0, 2.0, 3.0)];
        let n = num(spawn_fx(&mut g, &mut vm, Args::new("spawnfx", &args)));
        let triggers = |g: &Game| fx_states(g).iter().map(|e| e.event_seq).collect::<Vec<_>>();
        assert_eq!(triggers(&g), [0]);
        let at = fx_states(&g)[0].clone();
        assert_eq!(
            (at.etype, at.model, at.origin),
            (etype::FX, 1, [1.0, 2.0, 3.0])
        );
        // Straight up unless told otherwise.
        assert_eq!(at.angles[0].rem_euclid(360.0), 270.0);

        let ent = g.entity_value(&mut vm, n);
        for (i, delay) in [None, Some(1.5)].into_iter().enumerate() {
            let mut a = vec![ent.clone()];
            a.extend(delay.map(Value::Float));
            trigger_fx(&mut g, &mut vm, Args::new("triggerfx", &a)).unwrap();
            assert_eq!(triggers(&g), [i as u8 + 1]);
        }
        assert_eq!(fx_states(&g)[0].eflags, 1500);

        g.free_entity(&mut vm, n);
        assert!(fx_states(&g).is_empty());
    }

    #[test]
    fn only_an_effect_entity_can_be_triggered() {
        let (mut g, mut vm) = setup();
        let args = [Value::Int(1), Value::Float(0.5), v(0.0, 0.0, 0.0)];
        let looped = num(play_looped_fx(
            &mut g,
            &mut vm,
            Args::new("playloopedfx", &args),
        ));
        let ent = g.entity_value(&mut vm, looped);
        assert!(trigger_fx(&mut g, &mut vm, Args::new("triggerfx", &[ent])).is_err());
    }

    #[test]
    fn a_looped_effect_carries_its_period_and_cull_distance() {
        let (mut g, mut vm) = setup();
        let args = [
            Value::Int(1),
            Value::Float(0.25),
            v(0.0, 0.0, 0.0),
            Value::Float(900.0),
        ];
        play_looped_fx(&mut g, &mut vm, Args::new("playloopedfx", &args)).unwrap();
        let e = &fx_states(&g)[0];
        assert_eq!(
            (e.etype, e.pm_flags, e.velocity[0]),
            (etype::LOOP_FX, 250, 900.0)
        );

        let bad = [Value::Int(1), Value::Float(0.0), v(0.0, 0.0, 0.0)];
        let err = play_looped_fx(&mut g, &mut vm, Args::new("playloopedfx", &bad)).unwrap_err();
        assert!(err.contains("fx/test/smoke"), "{err}");
        assert_eq!(fx_states(&g).len(), 1);
    }

    #[test]
    fn the_up_vector_rolls_an_effect_and_bad_directions_are_refused() {
        let (mut g, mut vm) = setup();
        let play = |g: &mut Game, vm: &mut Vm, extra: &[Value]| {
            let mut a = vec![Value::Int(1), v(0.0, 0.0, 0.0)];
            a.extend_from_slice(extra);
            play_fx(g, vm, Args::new("playfx", &a))
        };
        let last = |g: &Game| {
            let e = g.tempev.live(g.level.time).last().unwrap().clone();
            assert_eq!(e.event, ev::PLAY_FX);
            e.angles
        };
        // Facing +X with +Y above puts the effect's up on +Y.
        play(&mut g, &mut vm, &[v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0)]).unwrap();
        let (f, _, up) = sim::pm::math::angle_vectors(&last(&g));
        assert!(
            (f[0] - 1.0).abs() < 1e-2 && (up[1] - 1.0).abs() < 1e-2,
            "{f:?} {up:?}"
        );
        // The same forward without an up has no roll.
        play(&mut g, &mut vm, &[v(1.0, 0.0, 0.0)]).unwrap();
        let (_, _, up) = sim::pm::math::angle_vectors(&last(&g));
        assert!((up[2] - 1.0).abs() < 1e-2, "{up:?}");

        assert!(play(&mut g, &mut vm, &[v(0.0, 0.0, 0.0)]).is_err());
        assert!(play(&mut g, &mut vm, &[v(1.0, 0.0, 0.0), v(0.0, 0.0, 0.0)]).is_err());
        assert!(play(&mut g, &mut vm, &[v(1.0, 0.0, 0.0), v(-2.0, 0.0, 0.0)]).is_err());
        assert!(
            play(
                &mut g,
                &mut vm,
                &[v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0), v(0.0, 0.0, 1.0)]
            )
            .is_err()
        );
    }

    #[test]
    fn earthquake_and_physics_world_events_reach_the_clients() {
        let (mut g, mut vm) = setup();
        let a = [
            Value::Float(0.4),
            Value::Int(2),
            v(10.0, 20.0, 30.0),
            Value::Int(1000),
        ];
        earthquake(&mut g, &mut vm, Args::new("earthquake", &a)).unwrap();
        let s = g.tempev.live(g.level.time).last().unwrap().clone();
        let q = Earthquake::decode(&s).unwrap();
        assert_eq!(
            (q.duration_ms, q.radius, s.origin),
            (2000, 1000.0, [10.0, 20.0, 30.0])
        );
        assert!((q.scale - 0.4).abs() < 0.02);
        for bad in [
            [
                Value::Int(0),
                Value::Int(2),
                v(0.0, 0.0, 0.0),
                Value::Int(1),
            ],
            [
                Value::Int(1),
                Value::Int(0),
                v(0.0, 0.0, 0.0),
                Value::Int(1),
            ],
            [
                Value::Int(1),
                Value::Int(2),
                v(0.0, 0.0, 0.0),
                Value::Int(0),
            ],
        ] {
            assert!(earthquake(&mut g, &mut vm, Args::new("earthquake", &bad)).is_err());
        }

        let cyl = [
            v(0.0, 0.0, 0.0),
            Value::Int(300),
            Value::Int(100),
            Value::Int(2),
        ];
        physics_explosion(&mut g, Args::new("physicsexplosioncylinder", &cyl), true).unwrap();
        assert_eq!(
            g.tempev.live(g.level.time).last().unwrap().event,
            ev::PHYSICS_EXPLOSION_CYLINDER
        );
        let inside_out = [
            v(0.0, 0.0, 0.0),
            Value::Int(100),
            Value::Int(300),
            Value::Int(2),
        ];
        assert!(
            physics_explosion(
                &mut g,
                Args::new("physicsexplosionsphere", &inside_out),
                false
            )
            .is_err()
        );
    }

    #[test]
    fn entities_never_reach_the_event_slots_even_when_the_table_is_full() {
        let (mut g, mut vm) = setup();
        let args = [Value::Int(1), Value::Float(1.0), v(0.0, 0.0, 0.0)];
        let mut made = 0;
        while play_looped_fx(&mut g, &mut vm, Args::new("playloopedfx", &args)).is_ok() {
            made += 1;
            assert!(made < 2000);
        }
        assert!(made > 800);
        // Events of every kind still fit beside them: strictly ascending numbers, none dropped.
        for _ in 0..70 {
            let a = [
                Value::Float(0.4),
                Value::Int(2),
                v(0.0, 0.0, 0.0),
                Value::Int(100),
            ];
            earthquake(&mut g, &mut vm, Args::new("earthquake", &a)).unwrap();
        }
        let all = world_entities(&g);
        assert!(all.windows(2).all(|w| w[0].number < w[1].number));
        assert_eq!(
            all.iter().filter(|e| e.etype == etype::LOOP_FX).count(),
            made
        );
        assert_eq!(all.iter().filter(|e| e.etype == etype::EVENT).count(), 62);
        assert!(all.iter().all(|e| e.number < 1022));
    }

    #[test]
    fn an_earthquake_longer_or_wider_than_the_wire_holds_is_clamped_not_wrapped() {
        let (mut g, mut vm) = setup();
        let a = [
            Value::Float(0.4),
            Value::Int(60),
            v(0.0, 0.0, 0.0),
            Value::Int(50000),
        ];
        earthquake(&mut g, &mut vm, Args::new("earthquake", &a)).unwrap();
        let q = Earthquake::decode(g.tempev.live(g.level.time).last().unwrap()).unwrap();
        assert_eq!((q.duration_ms, q.radius), (16383, 16383.0));
    }
}
