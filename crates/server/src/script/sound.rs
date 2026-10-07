// SPDX-License-Identifier: GPL-3.0-or-later
//! Sound builtins. The server plays nothing: each call becomes a console command to the clients that should
//! hear it, which the client's sound system turns into voices (`crates/client/src/sound.rs`).
//!
//! | command | meaning |
//! |---|---|
//! | `snd <ent> <x> <y> <z> <alias>` | one-shot at a position, attached to an entity |
//! | `lsnd <alias>` | a 2D sound for one client |
//! | `loop <ent> <x> <y> <z> <alias>` / `stoploop <ent> <alias>` | a looping sound on an entity |
//! | `ambient <fade ms> <alias>` / `ambientstop <fade ms>` | the map's ambience |
//! | `music <alias>` / `musicstop <fade ms>` | the music, its own stream beside the ambience |
//! | `reverb <priority> <room> <wet> <fade ms>` / `reverboff <priority> <fade ms>` | `setReverb`: the room effect |

use gsc::{EntRef, Value, Vm};

use super::Args;
use crate::client::Team;
use crate::game::{Game, SoundTo};

type R = Result<Value, String>;

fn origin(g: &Game, e: EntRef) -> [f32; 3] {
    g.ent(e.num).map_or([0.0; 3], |e| e.origin)
}

fn at(g: &Game, e: EntRef) -> String {
    let o = origin(g, e);
    format!("{} {:.1} {:.1} {:.1}", e.num, o[0], o[1], o[2])
}

fn fade_ms(a: &Args, i: usize) -> i32 {
    a.float(i).map_or(0, |s| (s * 1000.0).round() as i32).max(0)
}

pub fn ambient_play(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let alias = a.string(0)?;
    let line = format!("ambient {} {alias}", fade_ms(&a, 1));
    g.ambient = Some(line.clone());
    g.sound_out.push((SoundTo::All, line));
    Ok(Value::Undefined)
}

pub fn ambient_stop(g: &mut Game, _: &mut Vm, a: Args) -> R {
    g.ambient = None;
    g.sound_out
        .push((SoundTo::All, format!("ambientstop {}", fade_ms(&a, 0))));
    Ok(Value::Undefined)
}

pub fn play_sound(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let line = format!("snd {} {}", at(g, e), a.string(0)?);
    g.sound_out.push((SoundTo::All, line));
    Ok(Value::Undefined)
}

pub fn play_sound_to_player(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let to = a.entity_or_undefined(1)?.unwrap_or(e);
    if g.is_client(to.num) {
        g.sound_out
            .push((SoundTo::Client(to.num), format!("lsnd {}", a.string(0)?)));
    }
    Ok(Value::Undefined)
}

pub fn play_sound_to_team(g: &mut Game, _: &mut Vm, _: EntRef, a: Args) -> R {
    let team = match a.string(1)?.to_ascii_lowercase().as_str() {
        "axis" => Team::Axis,
        "allies" => Team::Allies,
        _ => return Ok(Value::Undefined),
    };
    g.sound_out
        .push((SoundTo::Team(team), format!("lsnd {}", a.string(0)?)));
    Ok(Value::Undefined)
}

pub fn play_local_sound(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    if g.is_client(e.num) {
        g.sound_out
            .push((SoundTo::Client(e.num), format!("lsnd {}", a.string(0)?)));
    }
    Ok(Value::Undefined)
}

pub fn play_loop_sound(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let line = format!("loop {} {}", at(g, e), a.string(0)?);
    g.sound_out.push((SoundTo::All, line));
    Ok(Value::Undefined)
}

/// `stoploopsound([alias])`: without a name the entity's loop sounds all stop.
pub fn stop_loop_sound(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let alias = a.string(0).unwrap_or("*");
    g.sound_out
        .push((SoundTo::All, format!("stoploop {} {alias}", e.num)));
    Ok(Value::Undefined)
}

pub fn music_play(g: &mut Game, _: &mut Vm, a: Args) -> R {
    g.sound_out
        .push((SoundTo::All, format!("music {}", a.string(0)?)));
    Ok(Value::Undefined)
}

pub fn music_stop(g: &mut Game, _: &mut Vm, a: Args) -> R {
    g.sound_out
        .push((SoundTo::All, format!("musicstop {}", fade_ms(&a, 0))));
    Ok(Value::Undefined)
}

/// `snd_enveffectsprio_level` is 1 and `snd_enveffectsprio_shellshock` 2.
fn env_priority(a: &Args) -> Result<u8, String> {
    match a.string(0)?.to_ascii_lowercase().as_str() {
        "snd_enveffectsprio_level" => Ok(1),
        "snd_enveffectsprio_shellshock" => Ok(2),
        _ => Err(
            "priority must be 'snd_enveffectsprio_level' or 'snd_enveffectsprio_shellshock'".into(),
        ),
    }
}

/// `player setReverb(priority, roomtype, drylevel = 1, wetlevel = 0.5, fadetime = 0)`. The dry level is
/// accepted but not sent: the original's mixer ignores it (`MSS_GetDryLevel` returns 1).
pub fn set_reverb(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    if !g.is_client(e.num) {
        return Err(format!("entity {} is not a player", e.num));
    }
    let prio = env_priority(&a)?;
    let room = a.string(1)?;
    let wet = a.float(3).unwrap_or(0.5).clamp(0.0, 1.0);
    let line = format!("reverb {prio} {room} {wet} {}", fade_ms(&a, 4));
    g.sound_out.push((SoundTo::Client(e.num), line));
    Ok(Value::Undefined)
}

/// `player deactivateReverb(priority, fadetime = 0)`.
pub fn deactivate_reverb(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    if !g.is_client(e.num) {
        return Err(format!("entity {} is not a player", e.num));
    }
    let prio = env_priority(&a)?;
    let line = format!("reverboff {prio} {}", fade_ms(&a, 1));
    g.sound_out.push((SoundTo::Client(e.num), line));
    Ok(Value::Undefined)
}
