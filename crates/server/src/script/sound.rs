// SPDX-License-Identifier: GPL-3.0-only
//! Sound builtins. The server plays nothing: each call becomes a console command to the clients that should
//! hear it, which the client's sound system turns into voices (`crates/client/src/sound.rs`).
//!
//! | command | meaning |
//! |---|---|
//! | `snd <ent> <x> <y> <z> <alias>` | one-shot at a position, attached to an entity |
//! | `lsnd <alias>` | a 2D sound for one client |
//! | `msnd <ent> <x> <y> <z> <alias>` | as `snd`, played as a master: the slave aliases duck |
//! | `stoplsnd <alias>` | cuts a local sound short |
//! | `soundfade <volume> <ms>` | scales all sound |
//! | `stopsounds` | a restarted level: every sound of the old one ends |
//! | `sndname <i> <alias>` | names the alias index of an entity's `loop_sound` (see `net::entity`) |
//! | `ambient <fade ms> <alias>` / `ambientstop <fade ms>` | the map's ambience |
//! | `music <alias>` / `musicstop <fade ms>` | the music, its own stream beside the ambience |
//! | `chanvol <priority> <shock> <fade ms>` / `chanvoloff <priority> <fade ms>` | `setChannelVolumes`: duck the channels |
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

/// `snd <ent> <x> <y> <z> <alias>` (`msnd` when played as a master) from the entity.
fn positioned(g: &Game, e: EntRef, cmd: &str, alias: &str) -> String {
    format!("{cmd} {} {alias}", at(g, e))
}

pub fn play_sound(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let line = positioned(g, e, "snd", a.string(0)?);
    g.sound_out.push((SoundTo::All, line));
    Ok(Value::Undefined)
}

/// `playSoundAsMaster(alias)`: as `playsound`, and the slave aliases duck while it plays.
pub fn play_sound_as_master(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let line = positioned(g, e, "msnd", a.string(0)?);
    g.sound_out.push((SoundTo::All, line));
    Ok(Value::Undefined)
}

/// `playSoundToPlayer(alias, player)`: the entity's own sound, heard by that player only.
pub fn play_sound_to_player(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let to = a.entity_or_undefined(1)?.unwrap_or(e);
    if !g.is_client(to.num) {
        return Err(format!("entity {} is not a player", to.num));
    }
    let line = positioned(g, e, "snd", a.string(0)?);
    g.sound_out.push((SoundTo::Client(to.num), line));
    Ok(Value::Undefined)
}

/// `playSoundToTeam(alias, team, [ignore player])`: the entity's own sound, heard by a team less one player.
pub fn play_sound_to_team(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let team = match a.string(1)?.to_ascii_lowercase().as_str() {
        "axis" => Team::Axis,
        "allies" => Team::Allies,
        other => {
            return Err(format!(
                "Illegal team string '{other}'. Must be allies, or axis."
            ));
        }
    };
    let ignored = if a.len() > 2 {
        a.entity_or_undefined(2)?
    } else {
        None
    };
    let skip = match ignored {
        Some(p) if g.is_client(p.num) => Some(p.num),
        Some(p) => return Err(format!("entity {} is not a player", p.num)),
        None => None,
    };
    let to = skip.map_or(SoundTo::Team(team), |s| SoundTo::TeamExcept(team, s));
    let line = positioned(g, e, "snd", a.string(0)?);
    g.sound_out.push((to, line));
    Ok(Value::Undefined)
}

pub fn play_local_sound(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    if g.is_client(e.num) {
        g.sound_out
            .push((SoundTo::Client(e.num), format!("lsnd {}", a.string(0)?)));
    }
    Ok(Value::Undefined)
}

/// `stopLocalSound(alias)`: cuts a `playLocalSound` of that alias short.
pub fn stop_local_sound(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    if !g.is_client(e.num) {
        return Err(format!("entity {} is not a player", e.num));
    }
    g.sound_out
        .push((SoundTo::Client(e.num), format!("stoplsnd {}", a.string(0)?)));
    Ok(Value::Undefined)
}

/// `playLoopSound(alias)`: the entity loops the alias for as long as it lives or until `stopLoopSound`. The alias
/// is part of the entity's state, which is what lets a late joiner hear it.
pub fn play_loop_sound(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let index = g.sounds.index(a.string(0)?);
    // The wire carries ten bits of it.
    if index > 1023 {
        return Err("too many looped sound aliases".into());
    }
    if let Some(ent) = g.ent_mut(e.num) {
        ent.loop_sound = u16::try_from(index).unwrap_or(0);
    }
    Ok(Value::Undefined)
}

/// `stopLoopSound()`: the entity's loop ends.
pub fn stop_loop_sound(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    if let Some(ent) = g.ent_mut(e.num) {
        ent.loop_sound = 0;
    }
    Ok(Value::Undefined)
}

/// `soundFade(volume, [seconds])`: every client's sound goes to `volume` times itself over the time.
pub fn sound_fade(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let volume = a.float(0)?.max(0.0);
    g.sound_out.push((
        SoundTo::All,
        format!("soundfade {volume} {}", fade_ms(&a, 1)),
    ));
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

/// `snd_channelvolprio_holdbreath` is 1, `_pain` 2 and `_shellshock` 3.
fn channel_priority(a: &Args) -> Result<u8, String> {
    match a.string(0)?.to_ascii_lowercase().as_str() {
        "snd_channelvolprio_holdbreath" => Ok(1),
        "snd_channelvolprio_pain" => Ok(2),
        "snd_channelvolprio_shellshock" => Ok(3),
        _ => Err("priority must be 'snd_channelvolprio_holdbreath', 'snd_channelvolprio_pain', or 'snd_channelvolprio_shellshock'".into()),
    }
}

/// `player setChannelVolumes(priority, shockname, fadetime = 0)`.
pub fn set_channel_volumes(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    if !g.is_client(e.num) {
        return Err(format!("entity {} is not a player", e.num));
    }
    let prio = channel_priority(&a)?;
    let line = format!("chanvol {prio} {} {}", a.string(1)?, fade_ms(&a, 2));
    g.sound_out.push((SoundTo::Client(e.num), line));
    Ok(Value::Undefined)
}

/// `player deactivateChannelVolumes(priority, fadetime = 0)`.
pub fn deactivate_channel_volumes(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    if !g.is_client(e.num) {
        return Err(format!("entity {} is not a player", e.num));
    }
    let prio = channel_priority(&a)?;
    let line = format!("chanvoloff {prio} {}", fade_ms(&a, 1));
    g.sound_out.push((SoundTo::Client(e.num), line));
    Ok(Value::Undefined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gsc::{Builtins, EntClass, Options, compile};

    fn setup() -> (Game, Vm, u16, u16, u16) {
        let prog = compile(
            &[("t.gsc", "main() {}")],
            &Builtins::stock_mp(),
            Options::default(),
        )
        .unwrap();
        let mut vm = Vm::new(prog).unwrap();
        let mut g = Game::new(crate::cvar::Cvars::new(), Default::default());
        g.reset_level(4);
        let n: Vec<u16> = ["Ann", "Bob", "Cy"]
            .iter()
            .map(|name| g.connect_client(&mut vm, true, name).unwrap())
            .collect();
        (g, vm, n[0], n[1], n[2])
    }

    fn me(n: u16) -> EntRef {
        EntRef {
            num: n,
            class: EntClass::Entity,
        }
    }

    #[test]
    fn playsoundtoteam_skips_the_named_player_and_rejects_bad_arguments() {
        let (mut g, mut vm, ann, bob, _) = setup();
        let player = |vm: &mut Vm, n: u16| Value::Object(vm.entity(n, EntClass::Entity));
        let skip = player(&mut vm, bob);
        let args = [Value::str("snd_a"), Value::str("axis"), skip];
        play_sound_to_team(
            &mut g,
            &mut vm,
            me(ann),
            Args::new("playsoundtoteam", &args),
        )
        .unwrap();
        let (to, line) = g.sound_out.pop().unwrap();
        assert_eq!(to, SoundTo::TeamExcept(Team::Axis, bob));
        assert!(line.starts_with(&format!("snd {ann} ")), "{line}");
        let args = [Value::str("snd_a"), Value::str("allies")];
        play_sound_to_team(
            &mut g,
            &mut vm,
            me(ann),
            Args::new("playsoundtoteam", &args),
        )
        .unwrap();
        assert_eq!(g.sound_out.pop().unwrap().0, SoundTo::Team(Team::Allies));
        let args = [Value::str("snd_a"), Value::str("spectator")];
        assert!(
            play_sound_to_team(&mut g, &mut vm, me(ann), Args::new("x", &args)).is_err(),
            "an unknown team"
        );
        let args = [
            Value::str("snd_a"),
            Value::str("axis"),
            Value::Object(vm.entity(900, EntClass::Entity)),
        ];
        assert!(
            play_sound_to_team(&mut g, &mut vm, me(ann), Args::new("x", &args)).is_err(),
            "a skipped entity that is no player"
        );
        assert!(g.sound_out.is_empty());
    }

    #[test]
    fn soundexists_follows_the_loaded_alias_lists() {
        let (mut g, mut vm, ..) = setup();
        g.content.add_test_sound("Weap_Fire");
        let mut ask = |name: &str| {
            let v = [Value::str(name)];
            super::super::misc::sound_exists(&mut g, &mut vm, Args::new("soundexists", &v)).unwrap()
        };
        assert!(matches!(ask("weap_fire"), Value::Int(1)));
        assert!(matches!(ask("no_such_alias"), Value::Int(0)));
    }

    #[test]
    fn a_players_script_loop_is_not_published() {
        let (mut g, mut vm, ann, ..) = setup();
        let v = [Value::str("hum")];
        play_loop_sound(&mut g, &mut vm, me(ann), Args::new("playloopsound", &v)).unwrap();
        assert!(
            crate::netsv::world_entities(&g)
                .iter()
                .all(|s| s.loop_sound == 0)
        );
    }
}
