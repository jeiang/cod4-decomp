// SPDX-License-Identifier: GPL-3.0-only
// `asin`, `acos`, `getsubstr`, `strtok`, `addtestclient`, `spawnturret`, `precacheturret` and `Scr_PlayFXOnTag` translated in part from KisakCOD (game_mp/g_scr_main_mp.cpp; GPL-3.0, copyright the KisakCOD contributors and Activision).
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
        r(|_, _, a| Ok(Value::Float(unit_range(a.float(0)?)?.asin().to_degrees()))),
    ),
    (
        "acos",
        r(|_, _, a| Ok(Value::Float(unit_range(a.float(0)?)?.acos().to_degrees()))),
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
    ("precacheshader", r(precache_shader)),
    ("precachestring", r(precache_string)),
    ("precacheitem", r(precache_item)),
    ("precacheshellshock", r(precache_shell_shock)),
    // A null sub in the stock game: the 1.7 handler (0x4d74b0, shared with other unused builtins) is a bare `ret`
    // (KisakCOD `game_mp/g_scr_main_mp.cpp:144`, `KISAK_NULLSUB`).
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
    ("precacheturret", r(precache_turret)),
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
    ("addtestclient", r(add_test_client)),
    ("objective_add", r(uicmd::objective_add)),
    ("objective_delete", r(uicmd::objective_delete)),
    ("objective_state", r(uicmd::objective_state)),
    ("objective_icon", r(uicmd::objective_icon)),
    ("objective_position", r(uicmd::objective_position)),
    ("objective_onentity", r(uicmd::objective_on_entity)),
    ("objective_team", r(uicmd::objective_team)),
    ("objective_current", r(uicmd::objective_current)),
    // world rendering, FX, audio: not a server concern until clients exist
    ("setexpfog", r(uicmd::set_exp_fog)),
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
    ("soundfade", Real(super::sound::sound_fade)),
    ("playrumbleonposition", r(|_, _, _| Ok(Value::Undefined))),
    (
        "playrumblelooponposition",
        r(|_, _, _| Ok(Value::Undefined)),
    ),
    ("stopallrumbles", r(|_, _, _| Ok(Value::Undefined))),
    // collision, weapons, bots
    ("bullettrace", r(bullet_trace)),
    ("bullettracepassed", r(bullet_trace_passed)),
    ("sighttracepassed", r(sight_trace_passed)),
    ("physicstrace", r(physics_trace)),
    ("playerphysicstrace", r(player_physics_trace)),
    ("spawnhelicopter", r(spawn_helicopter)),
    ("spawnplane", r(spawn_plane)),
    ("spawnturret", r(spawn_turret)),
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

/// `asin` and `acos` take `[-1, 1]` and end the thread on anything else (`GScr_asin`).
fn unit_range(x: f32) -> Result<f32, String> {
    if (-1.0..=1.0).contains(&x) {
        Ok(x)
    } else {
        Err(format!("{} out of range", super::args::format_float(x)))
    }
}

/// A missing dvar reads as "" (and 0); the stock builtins ignore any second argument.
fn get_dvar(g: &mut Game, _: &mut Vm, a: Args) -> R {
    Ok(Value::str(g.cvars.string(a.string(0)?)))
}

fn dvar_typed(g: &mut Game, a: Args, f: fn(&str) -> Value) -> R {
    Ok(f(g.cvars.string(a.string(0)?)))
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
    if !n.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
        return Err(format!("Dvar {n} has an invalid dvar name"));
    }
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
    // `GScr_MakeDvarServerInfo`: `Dvar_AddFlags(dvar, DVAR_CODINFO)`, so the clients get the pair in the CODINFO
    // configstrings (`CG_ParseCodInfo` sets it on their side).
    g.cvars.add_flags(n, cvar::CODINFO);
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

/// The original's temporary string buffer for `getsubstr` and `strtok`: 1024 bytes.
const MAX_SUBSTR: usize = 1024;

fn get_substr(_: &mut Game, _: &mut Vm, a: Args) -> R {
    let s = a.string(0)?;
    let start = a.int(1)?.max(0) as usize;
    let end = match a.opt(2) {
        Some(_) => a.int(2)?.max(0) as usize,
        None => usize::MAX,
    };
    // The original copies into a 1024-byte buffer and refuses to go on once it is full.
    if end.saturating_sub(start) > MAX_SUBSTR && s.len() >= start + MAX_SUBSTR {
        return Err("string too long".into());
    }
    let end = end.min(s.len());
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
        if t.len() >= MAX_SUBSTR {
            return Err("string too long".into());
        }
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

/// The menu and icon precaches (`GScr_PrecacheMenu`, `GScr_PrecacheStatusIcon`, `GScr_PrecacheHeadIcon`,
/// `GScr_PrecacheLocationSelector`: KisakCOD `game_mp/g_scr_main_mp.cpp:2539`, `:2586`, `:2635`, `:2881`) and
/// [`precache_shader`]: the name is registered whether or not the zones have it; one they lack is warned about.
fn precache(g: &mut Game, a: Args, table: Table) -> R {
    let n = a.string(0)?;
    warn_missing_asset(g, a.name, table, n);
    g.precache(table, n)?;
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

/// `Scr_PlayFXOnTag`: register the effect/tag pair and raise an event on the visible target, not a temp entity.
fn play_fx_on_tag(g: &mut Game, _: &mut Vm, a: Args) -> R {
    if a.len() != 3 {
        return Err("Incorrect number of parameters".into());
    }
    let index = a.int(0)?;
    if !(1..100).contains(&index) {
        return Err(format!("effect id {index} is invalid\n"));
    }
    let n = a.entity(1)?.num;
    let model = &g.ent(n).ok_or("entity does not exist")?.model;
    if model.is_empty() {
        return Err("cannot play fx on entity with no model".into());
    }
    let tag = a.string(2)?.to_ascii_lowercase();
    if tag.contains('"') {
        return Err("cannot use \" characters in tag names\n".into());
    }
    if g.local_tag(n, &tag).is_none() {
        return Err(format!(
            "tag '{tag}' does not exist on entity with model '{model}'"
        ));
    }
    let text = format!("{index:02}{tag}");
    let cs = (1..net::ui::cs::EFFECT_TAGS_COUNT)
        .find(|&i| {
            g.configstrings
                .get(&u32::from(net::ui::cs::EFFECT_TAGS + i))
                .is_none_or(|s| s == &text)
        })
        .ok_or("too many effect tags (max 255)")?;
    g.set_configstring(net::ui::cs::EFFECT_TAGS + cs, &text);
    let event = crate::tempev::ev::PLAY_FX_ON_TAG;
    if let Some(c) = g.client_mut(n) {
        c.ps.add_event(event, u32::from(cs));
    } else {
        let e = g.ent_mut(n).ok_or("entity does not exist")?;
        e.events.rotate_left(1);
        e.events[3] = (event, cs as u8);
        e.event_sequence = e.event_sequence.wrapping_add(1);
    }
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
    g.vision_set(&name, ms, night);
    Ok(Value::Undefined)
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

/// `loadfx(name)`: `Scr_LoadFX` (KisakCOD `game_mp/g_scr_main_mp.cpp:4094`). The name is under `fx/`, given
/// without the folder. A new one registers only while the level initializes: after that `G_EffectIndex`
/// (`game_mp/g_utils_mp.cpp:338`) finds only a loaded one, and its `G_FindConfigstringIndex` (`:120-127`) raises
/// `effect "<name>" not precached` before `Scr_LoadFX`'s own error, which only an empty name reaches. A name the
/// zones lack registers (the client draws nothing for it) and is warned about.
fn load_fx(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let n = a.string(0)?;
    if n.starts_with("fx/") {
        return Err("effect name should start after the 'fx' folder.".into());
    }
    let mut id = g.fx.find(n);
    if id == 0 && !n.is_empty() {
        if !g.level.initializing {
            return Err(format!("effect \"{n}\" not precached"));
        }
        if !g.content.effect_defined(n) {
            logging::dedup!(
                WARN,
                n,
                builtin = a.name,
                name = n,
                "effect is not in the loaded zones"
            );
        }
        id = g.fx.index(n);
    }
    if id == 0 && !g.level.initializing {
        return Err("loadFx must be called before any wait statements in the level script, or on an already loaded effect\n".into());
    }
    Ok(Value::Int(id as i32))
}

/// `precacheshader(name)`: `Scr_PrecacheShader` (KisakCOD `game_mp/g_scr_main_mp.cpp:3783`): only while the level
/// initializes, and never with an empty name.
fn precache_shader(g: &mut Game, _: &mut Vm, a: Args) -> R {
    if !g.level.initializing {
        return Err("precacheShader must be called before any wait statements in the gametype or level script\n".into());
    }
    if a.string(0)?.is_empty() {
        return Err("Shader name string is empty".into());
    }
    precache(g, a, Table::Material)
}

/// Warns once per name when a precached material or menu is in none of the loaded zones: the client draws the
/// default material for it, or opens no menu.
fn warn_missing_asset(g: &mut Game, builtin: &str, table: Table, name: &str) {
    let missing = match table {
        Table::Material => !g.content.material_defined(name),
        Table::Menu => !g.content.menu_exists(name),
        Table::Model | Table::Text => false,
    };
    if missing && !name.is_empty() {
        logging::dedup!(
            WARN,
            name,
            builtin,
            name,
            "precached asset is not in the loaded zones"
        );
    }
}

/// `precacheshellshock(name)`: `Scr_PrecacheShellShock` (KisakCOD `game_mp/g_scr_main_mp.cpp:3751`), only while
/// the level initializes. It reads `shock/<name>.shock` as `BG_LoadShellShockDvars` does
/// (`bgame/bg_misc.cpp:2179`): a missing file is a console error and `shock/default.shock` stands in; without that
/// too the stock server drops the level (`Com_Error`), here the script errors. The players' shocks read the files
/// themselves (the client's `shellshock` handling), so nothing more is kept.
fn precache_shell_shock(g: &mut Game, _: &mut Vm, a: Args) -> R {
    if !g.level.initializing {
        return Err("precacheShellShock must be called before any wait statements in the gametype or level script\n".into());
    }
    let n = a.string(0)?;
    let file = format!("shock/{n}.shock");
    if g.content.rawfile(&file).is_some() {
        return Ok(Value::Undefined);
    }
    logging::dedup!(
        ERROR,
        file.as_str(),
        file = file.as_str(),
        "couldn't open the shell shock file; the default shock stands in"
    );
    if g.content.rawfile("shock/default.shock").is_none() {
        logging::dedup!(
            ERROR,
            "shock/default.shock",
            "couldn't open 'shock/default.shock'. This is a default shock file that you should have."
        );
        return Err(format!("couldn't find shell shock '{n}' -- see console"));
    }
    Ok(Value::Undefined)
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

/// `addtestclient()`: a test client joins and the new player entity comes back, or nothing when the server is full.
/// As in the original it has no brain: it stands still while the `TestClient` script picks its team and class (only
/// `bots N` makes roaming bots). Unlike the original, which runs the connect callback and `begin` before this
/// returns, both wait until the calling thread yields (`ScriptHost::run_calls`).
fn add_test_client(g: &mut Game, vm: &mut Vm, _: Args) -> R {
    let name = format!("bot{}", g.bot_serial);
    let Some(n) = g.connect_client(vm, true, &name) else {
        return Ok(Value::Undefined);
    };
    g.bot_serial += 1;
    g.pending_begin.push(n);
    Ok(g.entity_value(vm, n))
}

/// The most turrets the level holds (`turretInfo[32]`).
const MAX_TURRETS: usize = 32;

/// `precacheturret(weaponinfo)`: registers a turret's weapon; only the level's first run may.
fn precache_turret(g: &mut Game, _: &mut Vm, a: Args) -> R {
    if !g.level.initializing {
        return Err(
            "precacheTurret must be called before any wait statements in the level script\n".into(),
        );
    }
    let n = a.string(0)?;
    if g.content.weapon(n).is_none() {
        return Err(format!("bad weaponinfo '{n}' specified for turret"));
    }
    g.items.index(n);
    Ok(Value::Undefined)
}

/// `spawnturret(classname, origin, weaponinfo)`: a turret players can mount; a script sets its model and angles.
fn spawn_turret(g: &mut Game, vm: &mut Vm, a: Args) -> R {
    let (class, origin, weapon) = (a.string(0)?, a.vector(1)?, a.string(2)?);
    if g.turrets().count() >= MAX_TURRETS {
        return Err(format!(
            "G_SpawnTurret: max number of turrets ({MAX_TURRETS}) exceeded"
        ));
    }
    let mut e = Ent::new(EntKind::Plain, class);
    e.origin = origin;
    let n = g.spawn(e)?;
    g.init_clip(n, None);
    let vars = vec![("weaponinfo".to_owned(), weapon.to_owned())];
    let made = g.init_turret(n, &vars).and_then(|()| {
        if g.level.initializing || g.items.find(weapon) != 0 {
            Ok(())
        } else {
            Err(format!("turret '{weapon}' not precached"))
        }
    });
    if let Err(e) = made {
        g.free_entity(vm, n);
        return Err(e);
    }
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
    fn a_missing_playfxontag_bone_terminates_the_script_thread_with_the_stock_error() {
        let prog = compile(
            &[(
                "t.gsc",
                "main(ent) { playfxontag(1, ent, \"unknown_tag\"); level.survived = true; }",
            )],
            &Builtins::stock_mp(),
            Options::default(),
        )
        .unwrap();
        let entry = prog.find("t", "main").unwrap();
        let dispatch = crate::script::Dispatch::new(&prog);
        let mut vm = Vm::new(prog).unwrap();
        let (mut g, _) = setup();
        g.content.test_model_bone("test_model", "tag_origin");
        let mut e = Ent::new(EntKind::Plain, "script_model");
        e.model = "test_model".into();
        let n = g.spawn(e).unwrap();
        let ent = g.entity_value(&mut vm, n);
        let mut host = crate::script::ScriptHost {
            game: &mut g,
            dispatch: &dispatch,
        };
        let err = vm.call(&mut host, entry, None, &[ent]).unwrap_err();
        assert!(
            err.message
                .contains("tag 'unknown_tag' does not exist on entity with model 'test_model'"),
            "{}",
            err.message
        );
        assert!(vm.level().get("survived").is_none());
        assert!(host.game.tempev.live(host.game.level.time).next().is_none());
    }

    #[test]
    fn playfxontag_raises_the_target_event_with_a_deduplicated_effect_tag_configstring() {
        let (mut g, mut vm) = setup();
        g.content.test_model_bone("test_model", "tag_origin");
        g.note_model("test_model");
        let mut e = Ent::new(EntKind::Plain, "script_model");
        e.model = "test_model".into();
        e.origin = [10.0, 20.0, 30.0];
        let n = g.spawn(e).unwrap();
        let ent = g.entity_value(&mut vm, n);
        let args = [Value::Int(1), ent, Value::str("TAG_ORIGIN")];
        for sequence in 1..=2 {
            play_fx_on_tag(&mut g, &mut vm, Args::new("playfxontag", &args)).unwrap();
            let sent = crate::netsv::world_snapshot(&g);
            let target = sent.iter().find(|s| s.state.number == n).unwrap();
            assert_eq!(target.state.event, ev::PLAY_FX_ON_TAG);
            assert_eq!(target.state.event_parm, 1);
            assert_eq!(target.state.event_seq, sequence);
            assert_eq!(target.state.etype, etype::SCRIPT_MODEL);
            assert_eq!(
                g.configstrings[&u32::from(net::ui::cs::EFFECT_TAGS + 1)],
                "01tag_origin"
            );
            assert!(
                !g.configstrings
                    .contains_key(&u32::from(net::ui::cs::EFFECT_TAGS + 2))
            );
            assert!(g.tempev.live(g.level.time).next().is_none());
        }
        g.ent_mut(n).unwrap().hidden = true;
        assert!(world_entities(&g).iter().all(|s| s.number != n));
        let sent = crate::netsv::world_snapshot(&g);
        assert_eq!(
            sent.iter().find(|s| s.state.number == n).unwrap().shown_to,
            Some(0)
        );
    }

    #[test]
    fn playfxontag_refuses_missing_bones_models_and_invalid_parameters_without_events() {
        let (mut g, mut vm) = setup();
        let n = g.spawn(Ent::new(EntKind::Plain, "script_model")).unwrap();
        let ent = g.entity_value(&mut vm, n);
        let mut args = [Value::Int(1), ent, Value::str("unknown_tag")];
        assert_eq!(
            play_fx_on_tag(&mut g, &mut vm, Args::new("playfxontag", &args)).unwrap_err(),
            "cannot play fx on entity with no model"
        );
        g.content.test_model_bone("test_model", "tag_origin");
        g.ent_mut(n).unwrap().model = "test_model".into();
        assert_eq!(
            play_fx_on_tag(&mut g, &mut vm, Args::new("playfxontag", &args)).unwrap_err(),
            "tag 'unknown_tag' does not exist on entity with model 'test_model'"
        );
        args[2] = Value::str("tag_origin");
        for bad in [0, -1, 100] {
            args[0] = Value::Int(bad);
            assert_eq!(
                play_fx_on_tag(&mut g, &mut vm, Args::new("playfxontag", &args)).unwrap_err(),
                format!("effect id {bad} is invalid\n")
            );
        }
        args[0] = Value::Int(1);
        args[2] = Value::str("tag_\"bad");
        assert!(
            play_fx_on_tag(&mut g, &mut vm, Args::new("playfxontag", &args))
                .unwrap_err()
                .contains("cannot use")
        );
        assert_eq!(
            play_fx_on_tag(&mut g, &mut vm, Args::new("playfxontag", &args[..2])).unwrap_err(),
            "Incorrect number of parameters"
        );
        assert_eq!(g.ent(n).unwrap().event_sequence, 0);
        assert!(g.tempev.live(g.level.time).next().is_none());
        assert!(
            !g.configstrings
                .keys()
                .any(|&i| i >= u32::from(net::ui::cs::EFFECT_TAGS))
        );
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

#[cfg(test)]
mod edge_tests {
    use super::*;
    use gsc::{Builtins, Options, compile};

    fn setup() -> (Game, Vm) {
        let prog = compile(
            &[("t.gsc", "main() {}")],
            &Builtins::stock_mp(),
            Options::default(),
        )
        .unwrap();
        (
            Game::new(crate::cvar::Cvars::new(), Default::default()),
            Vm::new(prog).unwrap(),
        )
    }

    fn builtin(name: &str) -> FuncFn {
        match TABLE.iter().find(|(n, _)| *n == name) {
            Some((_, Real(f))) => *f,
            _ => panic!("{name} is not a real builtin"),
        }
    }

    fn call(f: fn(&mut Game, &mut Vm, Args) -> R, args: &[Value]) -> R {
        let (mut g, mut vm) = setup();
        f(&mut g, &mut vm, Args::new("t", args))
    }

    fn text(r: R) -> String {
        match r.unwrap() {
            Value::Str(s) => s.to_string(),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn asin_and_acos_end_the_thread_outside_the_unit_range() {
        for x in [1.5, -2.0] {
            let a = [Value::Float(x)];
            for name in ["asin", "acos"] {
                let f = builtin(name);
                let (mut g, mut vm) = setup();
                let e = f(&mut g, &mut vm, Args::new(name, &a)).unwrap_err();
                assert!(e.contains("out of range"), "{e}");
            }
        }
    }

    #[test]
    fn getdvar_ignores_a_default_and_reads_a_missing_dvar_as_empty() {
        let (mut g, mut vm) = setup();
        let a = [Value::str("nope"), Value::str("fallback")];
        assert_eq!(
            text(get_dvar(&mut g, &mut vm, Args::new("getdvar", &a))),
            ""
        );
        let r = dvar_typed(&mut g, Args::new("getdvarint", &a), |s| {
            Value::Int(cvar::parse_int(s))
        });
        assert!(matches!(r, Ok(Value::Int(0))));
    }

    #[test]
    fn setdvar_refuses_a_name_the_original_refuses() {
        let (mut g, mut vm) = setup();
        let bad = [Value::str("bad name"), Value::Int(1)];
        let e = set_dvar(&mut g, &mut vm, Args::new("setdvar", &bad)).unwrap_err();
        assert!(e.contains("invalid dvar name"), "{e}");
        let ok = [Value::str("good_name1"), Value::Int(1)];
        set_dvar(&mut g, &mut vm, Args::new("setdvar", &ok)).unwrap();
        assert_eq!(g.cvars.string("good_name1"), "1");
    }

    #[test]
    fn a_script_added_test_client_is_a_numbered_bot_that_begins_later() {
        let (mut g, mut vm) = setup();
        g.reset_level(4);
        for want in ["bot0", "bot1"] {
            let v = add_test_client(&mut g, &mut vm, Args::new("addtestclient", &[])).unwrap();
            let Value::Object(o) = v else {
                panic!("an entity, not {v:?}")
            };
            let n = o.entity().expect("an entity").num;
            assert!(g.clients[usize::from(n)].bot);
            assert_eq!(g.clients[usize::from(n)].name, want);
            assert!(g.pending_begin.contains(&n));
        }
    }

    #[test]
    fn getsubstr_and_strtok_stop_at_the_original_buffer() {
        let long = "a".repeat(2000);
        let args = |s: &str, from: i32, to: Option<i32>| {
            let mut v = vec![Value::str(s), Value::Int(from)];
            v.extend(to.map(Value::Int));
            v
        };
        assert_eq!(text(call(get_substr, &args("hello", 1, Some(3)))), "el");
        assert_eq!(
            text(call(get_substr, &args(&long, 0, Some(1024)))).len(),
            1024
        );
        assert!(call(get_substr, &args(&long, 0, Some(1025))).is_err());
        assert!(call(get_substr, &args(&long, 0, None)).is_err());
        // A short string under a huge end is fine.
        assert_eq!(text(call(get_substr, &args("hello", 2, Some(5000)))), "llo");
        let tok = [Value::str(&long), Value::str(",")];
        assert!(call(str_tok, &tok).is_err());
        let ok = [
            Value::str(&format!("{},b", "a".repeat(1023))),
            Value::str(","),
        ];
        assert!(call(str_tok, &ok).is_ok());
    }
}

#[cfg(test)]
mod precache_tests {
    use super::*;
    use gsc::{Builtins, Options, compile};
    use tracing::Level;

    fn setup(initializing: bool) -> (Game, Vm) {
        let prog = compile(
            &[("t.gsc", "main() {}")],
            &Builtins::stock_mp(),
            Options::default(),
        )
        .unwrap();
        let mut g = Game::new(crate::cvar::Cvars::new(), Default::default());
        g.level.initializing = initializing;
        (g, Vm::new(prog).unwrap())
    }

    fn call(
        f: fn(&mut Game, &mut Vm, Args) -> R,
        g: &mut Game,
        vm: &mut Vm,
        name: &str,
        arg: &str,
    ) -> R {
        let v = [Value::str(arg)];
        f(g, vm, Args::new(name, &v))
    }

    fn warned<'a>(events: &'a [logging::Record], level: Level) -> Option<&'a logging::Record> {
        events.iter().find(|e| e.level == level)
    }

    fn precache_menu(g: &mut Game, _: &mut Vm, a: Args) -> R {
        precache(g, a, Table::Menu)
    }

    fn precache_status_icon(g: &mut Game, _: &mut Vm, a: Args) -> R {
        precache(g, a, Table::Material)
    }

    #[test]
    fn loadfx_rejects_the_fx_folder_and_new_effects_after_init() {
        let (mut g, mut vm) = setup(true);
        g.content.add_test_effect("fx/smoke/logtest_known");
        let err = call(load_fx, &mut g, &mut vm, "loadfx", "fx/smoke/logtest_known").unwrap_err();
        assert!(err.contains("after the 'fx' folder"), "{err}");
        let id = call(load_fx, &mut g, &mut vm, "loadfx", "smoke/logtest_known").unwrap();
        assert!(matches!(id, Value::Int(1)));
        g.level.initializing = false;
        assert!(matches!(
            call(load_fx, &mut g, &mut vm, "loadfx", "SMOKE/logtest_known"),
            Ok(Value::Int(1))
        ));
        let err = call(load_fx, &mut g, &mut vm, "loadfx", "smoke/logtest_late").unwrap_err();
        assert!(err.contains("not precached"), "{err}");
        let err = call(load_fx, &mut g, &mut vm, "loadfx", "").unwrap_err();
        assert!(err.contains("loadFx must be called"), "{err}");
        assert_eq!(g.fx.find("smoke/logtest_late"), 0);
    }

    #[test]
    fn loadfx_warns_once_about_an_effect_the_zones_lack() {
        let (mut g, mut vm) = setup(true);
        g.content.add_test_effect("fx/smoke/logtest_present");
        let (_, events) = logging::capture(|| {
            call(load_fx, &mut g, &mut vm, "loadfx", "smoke/logtest_present").unwrap();
        });
        assert!(warned(&events, Level::WARN).is_none());
        let (id, events) = logging::capture(|| {
            call(load_fx, &mut g, &mut vm, "loadfx", "smoke/logtest_absent").unwrap()
        });
        assert!(matches!(id, Value::Int(2)), "it registers all the same");
        let w = warned(&events, Level::WARN).expect("a warning");
        assert_eq!(w.field("builtin"), Some("loadfx"));
        assert_eq!(w.field("name"), Some("smoke/logtest_absent"));
    }

    #[test]
    fn material_and_menu_precaches_warn_about_names_the_zones_lack() {
        let (mut g, mut vm) = setup(true);
        g.content.add_test_material("logtest_icon");
        g.content.add_test_menu("logtest_menu");
        let (_, events) = logging::capture(|| {
            call(
                precache_shader,
                &mut g,
                &mut vm,
                "precacheshader",
                "LOGTEST_ICON",
            )
            .unwrap();
            call(
                precache_menu,
                &mut g,
                &mut vm,
                "precachemenu",
                "logtest_menu",
            )
            .unwrap();
        });
        assert!(warned(&events, Level::WARN).is_none());
        let (_, events) = logging::capture(|| {
            call(
                precache_status_icon,
                &mut g,
                &mut vm,
                "precachestatusicon",
                "logtest_no_icon",
            )
            .unwrap();
        });
        let w = warned(&events, Level::WARN).expect("a warning");
        assert_eq!(w.field("builtin"), Some("precachestatusicon"));
        assert_eq!(w.field("name"), Some("logtest_no_icon"));
        let (_, events) = logging::capture(|| {
            call(
                precache_menu,
                &mut g,
                &mut vm,
                "precachemenu",
                "logtest_no_menu",
            )
            .unwrap();
        });
        assert_eq!(
            warned(&events, Level::WARN).and_then(|w| w.field("name")),
            Some("logtest_no_menu")
        );
        assert!(
            g.configstrings.values().any(|v| v == "logtest_no_menu"),
            "registered all the same"
        );
    }

    #[test]
    fn precacheshader_only_while_initializing_and_never_empty() {
        let (mut g, mut vm) = setup(true);
        g.content.add_test_material("logtest_shader");
        let err = call(precache_shader, &mut g, &mut vm, "precacheshader", "").unwrap_err();
        assert!(err.contains("Shader name string is empty"), "{err}");
        g.level.initializing = false;
        let err = call(
            precache_shader,
            &mut g,
            &mut vm,
            "precacheshader",
            "logtest_shader",
        )
        .unwrap_err();
        assert!(err.contains("precacheShader must be called"), "{err}");
    }

    #[test]
    fn precacheshellshock_reads_the_shock_file_with_the_default_standing_in() {
        let (mut g, mut vm) = setup(true);
        g.content.add_test_rawfile("shock/logtest_shock.shock", "x");
        let (r, events) = logging::capture(|| {
            call(
                precache_shell_shock,
                &mut g,
                &mut vm,
                "precacheshellshock",
                "logtest_shock",
            )
        });
        assert!(r.is_ok() && events.iter().all(|e| e.level != Level::ERROR));
        let (r, events) = logging::capture(|| {
            call(
                precache_shell_shock,
                &mut g,
                &mut vm,
                "precacheshellshock",
                "logtest_none",
            )
        });
        let err = r.unwrap_err();
        assert!(
            err.contains("couldn't find shell shock 'logtest_none'"),
            "{err}"
        );
        assert_eq!(
            warned(&events, Level::ERROR).and_then(|e| e.field("file")),
            Some("shock/logtest_none.shock")
        );
        g.content.add_test_rawfile("shock/default.shock", "x");
        let (r, events) = logging::capture(|| {
            call(
                precache_shell_shock,
                &mut g,
                &mut vm,
                "precacheshellshock",
                "logtest_fallback",
            )
        });
        assert!(r.is_ok());
        assert_eq!(
            warned(&events, Level::ERROR).and_then(|e| e.field("file")),
            Some("shock/logtest_fallback.shock")
        );
        g.level.initializing = false;
        let err = call(
            precache_shell_shock,
            &mut g,
            &mut vm,
            "precacheshellshock",
            "logtest_shock",
        )
        .unwrap_err();
        assert!(err.contains("precacheShellShock must be called"), "{err}");
    }
}
