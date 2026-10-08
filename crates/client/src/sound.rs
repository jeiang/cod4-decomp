// SPDX-License-Identifier: GPL-3.0-or-later
//! What the player hears: the `audio` crate fed from the match. Sound tables load on a thread while the
//! match connects; sounds asked for before they are ready are dropped.
//!
//! Sources of sound, all by alias name:
//! - player events in the snapshots (`ev::*` of the own player state and of every other player's entity):
//!   weapon fire, reload, raise, footsteps, landings;
//! - the server's sound commands (`script::sound` in the server crate): script `playsound`, local sounds,
//!   loops, the map's ambience and the music.

use crate::events::ClientEvent;
use assets::vfs::{LANGUAGES, Vfs};
use assets::zone::weapon::WeaponDef;
use audio::bank::Bank;
use audio::{Cue, NO_ENTITY, Sound};
use serde_json::{Value, json};
use server::content::Install;
use sim::pm::ev;
use sim::weapon::damage::SURFACE_TYPE_NAMES;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

enum State {
    Loading(Receiver<Result<Bank, String>>),
    Ready(Box<Sound>),
    Failed(String),
}

pub struct ClientSound {
    state: State,
    device: bool,
    /// Last event sequence seen per entity, so an event plays once.
    seen: HashMap<u16, u8>,
    own_seq: Option<u8>,
    /// Sounds asked for before the tables were ready.
    dropped: u64,
    /// Server commands that arrived while the tables loaded (the map's ambience is sent on connect).
    pending: Vec<String>,
    /// Where the listener is, for sounds that depend on it (bullet whiz-bys).
    eye: [f32; 3],
}

/// Who an event belongs to.
pub struct Who<'a> {
    /// The player's own state: first-person sounds, no position.
    pub own: bool,
    pub entity: u16,
    pub origin: [f32; 3],
    pub weapon: Option<&'a WeaponDef>,
}

impl ClientSound {
    /// Starts loading the sound tables of `map`. `device = false` mixes without a sound card.
    pub fn start(install: &Path, map: &str, device: bool) -> Self {
        let (install, map) = (install.to_owned(), map.to_owned());
        let (tx, rx) = channel();
        let spawned = std::thread::Builder::new()
            .name("sound-load".into())
            .spawn(move || {
                let _ = tx.send(load(&install, &map));
            });
        let state = match spawned {
            Ok(_) => State::Loading(rx),
            Err(e) => State::Failed(e.to_string()),
        };
        Self {
            state,
            device,
            seen: HashMap::new(),
            own_seq: None,
            dropped: 0,
            pending: Vec::new(),
            eye: [0.0; 3],
        }
    }

    fn ready(&mut self) -> Option<&mut Sound> {
        if let State::Loading(rx) = &self.state {
            match rx.try_recv() {
                Ok(Ok(bank)) => self.state = State::Ready(Box::new(Sound::new(bank, self.device))),
                Ok(Err(e)) => self.state = State::Failed(e),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(e) => self.state = State::Failed(e.to_string()),
            }
        }
        match &mut self.state {
            State::Ready(s) => Some(s),
            _ => None,
        }
    }

    /// Once a frame: the listener (the eye, `yaw` in radians counter-clockwise from +x) and the mixer's upkeep.
    pub fn frame(&mut self, eye: [f32; 3], yaw: f32, dt: f32) {
        self.eye = eye;
        let Some(s) = self.ready() else { return };
        s.set_listener(eye, yaw);
        s.tick(Duration::from_secs_f32(dt.clamp(0.0, 0.25)));
        for l in std::mem::take(&mut self.pending) {
            self.run(&l);
        }
    }

    fn play(&mut self, alias: &str, cue: Cue) {
        match self.ready() {
            Some(s) => {
                s.play(alias, cue);
            }
            None => self.dropped += 1,
        }
    }

    /// Plays the first of `names` the tables have.
    fn play_first(&mut self, names: &[String], cue: Cue) {
        let Some(s) = self.ready() else {
            self.dropped += 1;
            return;
        };
        if let Some(n) = names.iter().find(|n| s.bank.has(n)) {
            s.play(n, cue);
        }
    }

    /// Plays `alias` as a 2D sound (menus).
    pub fn play_ui(&mut self, alias: &str) {
        self.play(alias, Cue::default());
    }

    /// Plays `alias` at a world position (effect sounds, impacts).
    pub fn play_world(&mut self, alias: &str, origin: [f32; 3]) {
        self.play(
            alias,
            Cue {
                origin: Some(origin),
                ..Cue::default()
            },
        );
    }

    /// The sounds of the world events (see [`crate::events`]): bullet impacts and whiz-bys, explosions and
    /// bounces. `weapon` finds a weapon by index; `muzzle` where a shooter's bullets start.
    pub fn world_events(
        &mut self,
        events: &[ClientEvent],
        weapon: &dyn Fn(u16) -> Option<Arc<WeaponDef>>,
        muzzle: &dyn Fn(u16) -> Option<[f32; 3]>,
    ) {
        for e in events {
            match e {
                ClientEvent::BulletImpact {
                    origin,
                    surface,
                    weapon: w,
                    shooter,
                    ..
                } => {
                    let def = weapon(*w);
                    let prefix = match def.as_ref().map(|d| d.impact_type) {
                        Some(IMPACT_BULLET_SMALL) => "bullet_small",
                        Some(IMPACT_BULLET_LARGE) => "bullet_large",
                        Some(IMPACT_BULLET_AP) => "bullet_ap",
                        Some(IMPACT_SHOTGUN) => "bulletspray_small",
                        _ => continue,
                    };
                    self.surface_sound(prefix, *surface, *origin);
                    if let Some(start) = muzzle(*shooter) {
                        self.whizby(start, *origin);
                    }
                }
                ClientEvent::Explosion {
                    origin, weapon: w, ..
                } => {
                    let prefix = match weapon(*w).map(|d| d.impact_type) {
                        Some(IMPACT_GRENADE_EXPLODE) => "grenade_explode",
                        Some(IMPACT_ROCKET_EXPLODE) => "rocket_explode",
                        _ => continue,
                    };
                    // The server does not say what the blast sat on.
                    self.surface_sound(prefix, 0, *origin);
                }
                ClientEvent::MissileBounce {
                    origin,
                    surface,
                    weapon: w,
                    ..
                } => {
                    let name = weapon(*w)
                        .and_then(|d| d.bounce_sound.as_ref()?.get(usize::from(*surface))?.clone());
                    if let Some(name) = name.filter(|n| !n.is_empty()) {
                        self.play_world(&name, *origin);
                    }
                }
                _ => {}
            }
        }
    }

    /// `<prefix>_<surface>`, else `<prefix>_default`.
    fn surface_sound(&mut self, prefix: &str, surface: u8, at: [f32; 3]) {
        let mut names: Vec<String> = surface_names(surface)
            .into_iter()
            .map(|s| format!("{prefix}_{s}"))
            .collect();
        names.dedup();
        self.play_first(
            &names,
            Cue {
                origin: Some(at),
                ..Cue::default()
            },
        );
    }

    fn whizby(&mut self, start: [f32; 3], end: [f32; 3]) {
        if let Some(at) = whizby_at(self.eye, start, end) {
            self.play_world("whizby", at);
        }
    }

    /// The player events of an entity or player state, each once. `seq` is its event sequence number and
    /// `events` its newest events, oldest first.
    pub fn events(&mut self, who: &Who, seq: u8, newest: &[(u8, u8)]) {
        let last = if who.own {
            self.own_seq.replace(seq)
        } else {
            self.seen.insert(who.entity, seq)
        };
        // The first look at an entity only learns its counter.
        let Some(last) = last else { return };
        let fresh = usize::from(seq.wrapping_sub(last)).min(newest.len());
        for &(event, parm) in &newest[newest.len() - fresh..] {
            self.event(who, event, parm);
        }
    }

    fn event(&mut self, who: &Who, event: u8, parm: u8) {
        let cue = if who.own {
            Cue::default()
        } else {
            Cue {
                origin: Some([who.origin[0], who.origin[1], who.origin[2] + 40.0]),
                entity: u32::from(who.entity),
                ..Cue::default()
            }
        };
        let plr = if who.own { "_plr" } else { "" };
        if let Some(name) = weapon_sound(who.weapon, event, who.own) {
            self.play(&name, cue);
            return;
        }
        let surf = surface_names;
        let step = |kind: &str, parm: u8| -> Vec<String> {
            surf(parm)
                .into_iter()
                .map(|s| format!("step_{kind}{plr}_{s}"))
                .collect()
        };
        match event {
            ev::FOOTSTEP_SPRINT => self.play_first(&step("sprint", parm), cue),
            ev::FOOTSTEP_RUN => self.play_first(&step("run", parm), cue),
            ev::FOOTSTEP_WALK => self.play_first(&step("walk", parm), cue),
            ev::FOOTSTEP_PRONE => self.play_first(&step("prone", parm), cue),
            e if (ev::LANDING_FIRST..ev::LANDING_FIRST + 28).contains(&e) => {
                let names: Vec<String> = surf(e - ev::LANDING_FIRST + 1)
                    .into_iter()
                    .map(|s| format!("land{}_{s}", plr))
                    .collect();
                self.play_first(&names, cue);
            }
            e if (ev::LANDING_PAIN_FIRST..ev::LANDING_PAIN_FIRST + 28).contains(&e) => {
                self.play(&format!("land{plr}_damage"), cue);
            }
            _ => {}
        }
    }

    /// Whether a server console line is one of the sound commands.
    pub fn is_command(line: &str) -> bool {
        matches!(
            line.split_whitespace().next(),
            Some(
                "snd"
                    | "lsnd"
                    | "loop"
                    | "stoploop"
                    | "ambient"
                    | "ambientstop"
                    | "music"
                    | "musicstop"
                    | "reverb"
                    | "reverboff"
            )
        )
    }

    /// A console command from the server (see `server::script::sound`). Ambience and music wait for the tables.
    pub fn command(&mut self, line: &str) {
        if self.ready().is_none() {
            if self.pending.len() < 64 {
                self.pending.push(line.to_owned());
            }
            return;
        }
        for l in std::mem::take(&mut self.pending) {
            self.run(&l);
        }
        self.run(line);
    }

    fn run(&mut self, line: &str) {
        let mut w = line.split_whitespace();
        let Some(cmd) = w.next() else { return };
        let rest: Vec<&str> = w.collect();
        let num = |i: usize| rest.get(i).and_then(|v| v.parse::<f32>().ok());
        let pos = || Some([num(1)?, num(2)?, num(3)?]);
        match (cmd, rest.as_slice()) {
            ("snd", [ent, _, _, _, alias]) => {
                let entity = ent.parse().unwrap_or(NO_ENTITY);
                self.play(
                    alias,
                    Cue {
                        origin: pos(),
                        entity,
                        ..Cue::default()
                    },
                );
            }
            ("loop", [ent, _, _, _, alias]) => {
                let entity = ent.parse().unwrap_or(NO_ENTITY);
                self.play(
                    alias,
                    Cue {
                        origin: pos(),
                        entity,
                        ..Cue::default()
                    },
                );
            }
            ("stoploop", [ent, alias]) => {
                if let (Some(s), Ok(e)) = (self.ready(), ent.parse::<u32>()) {
                    if *alias == "*" {
                        s.stop_entity(e);
                    } else {
                        s.stop_loop(e, alias);
                    }
                }
            }
            ("lsnd", [alias]) => self.play(alias, Cue::default()),
            ("ambient", [fade, alias]) => {
                let fade = fade.parse().unwrap_or(0);
                match self.ready() {
                    Some(s) => s.ambient_play(alias, fade),
                    None => self.dropped += 1,
                }
            }
            ("ambientstop", [fade]) => {
                if let Some(s) = self.ready() {
                    s.ambient_stop(fade.parse().unwrap_or(0));
                }
            }
            ("music", [alias]) => match self.ready() {
                Some(s) => s.music_play(alias),
                None => self.dropped += 1,
            },
            ("musicstop", [fade]) => {
                if let Some(s) = self.ready() {
                    s.music_stop(fade.parse().unwrap_or(0));
                }
            }
            ("reverb", [prio, room, wet, fade]) => {
                if let (Some(s), Ok(p)) = (self.ready(), prio.parse::<usize>()) {
                    s.set_reverb(
                        p,
                        room,
                        wet.parse().unwrap_or(0.0),
                        fade.parse().unwrap_or(0),
                    );
                }
            }
            ("reverboff", [prio, fade]) => {
                if let (Some(s), Ok(p)) = (self.ready(), prio.parse::<usize>()) {
                    s.deactivate_reverb(p, fade.parse().unwrap_or(0));
                }
            }
            _ => {}
        }
    }

    /// What was heard, for the client report and the harness.
    pub fn report(&mut self) -> Value {
        let dropped = self.dropped;
        match &mut self.state {
            State::Ready(s) => {
                let st = s.stats();
                use std::sync::atomic::Ordering::Relaxed;
                let mut top: Vec<(&String, &u64)> = s.played.aliases.iter().collect();
                top.sort_by(|a, b| b.1.cmp(a.1));
                json!({
                    "ready": true,
                    "device": s.has_device(),
                    "device_note": s.device_note,
                    "lists": s.bank.stats.lists,
                    "aliases": s.bank.stats.aliases,
                    "load_ms": s.bank.stats.zones.iter().map(|(n, d)| (n.clone(), d.as_secs_f64() * 1000.0)).collect::<HashMap<_, _>>(),
                    "started": st.started.load(Relaxed),
                    "refused": st.refused.load(Relaxed),
                    "replaced": st.replaced.load(Relaxed),
                    "finished": st.finished.load(Relaxed),
                    "underruns": st.underruns.load(Relaxed),
                    "lost_commands": st.lost_commands.load(Relaxed),
                    "active": st.active.load(Relaxed),
                    "peak": st.take_peak(),
                    "by_channel": s.played.by_channel,
                    "out_of_range": s.played.out_of_range,
                    "missing": s.played.missing,
                    "failed": s.played.failed,
                    "top_aliases": top.iter().take(12).map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
                    "dropped_before_ready": dropped,
                })
            }
            State::Loading(_) => json!({"ready": false, "dropped_before_ready": dropped}),
            State::Failed(e) => {
                json!({"ready": false, "error": e, "dropped_before_ready": dropped})
            }
        }
    }
}

const IMPACT_BULLET_SMALL: i32 = 1;
const IMPACT_BULLET_LARGE: i32 = 2;
const IMPACT_BULLET_AP: i32 = 3;
const IMPACT_SHOTGUN: i32 = 4;
const IMPACT_GRENADE_EXPLODE: i32 = 6;
const IMPACT_ROCKET_EXPLODE: i32 = 7;

/// The surface name an alias may use, then the stock alias's spelling of it, then `default`.
fn surface_names(s: u8) -> Vec<String> {
    let name = SURFACE_TYPE_NAMES
        .get(usize::from(s))
        .copied()
        .unwrap_or("default");
    let mut v = vec![name.to_owned()];
    if name == "asphalt" {
        v.push("asphault".into());
    }
    v.push("default".into());
    v
}

/// `WhizbySound`: a bullet passing within 140 units of the listener, at least 64 past its start, makes
/// the `whizby` sound at the nearest point of its path, 16 units back.
fn whizby_at(eye: [f32; 3], start: [f32; 3], end: [f32; 3]) -> Option<[f32; 3]> {
    let d = sub(end, start);
    let len = dot(d, d).sqrt();
    if len < 1.0 {
        return None;
    }
    let dir = [d[0] / len, d[1] / len, d[2] / len];
    let along = dot(sub(eye, start), dir);
    if along < 64.0 || len < along {
        return None;
    }
    let at = [
        start[0] + dir[0] * (along - 16.0),
        start[1] + dir[1] * (along - 16.0),
        start[2] + dir[2] * (along - 16.0),
    ];
    let off = sub(
        [
            at[0] + dir[0] * 16.0,
            at[1] + dir[1] * 16.0,
            at[2] + dir[2] * 16.0,
        ],
        eye,
    );
    (dot(off, off) <= 140.0 * 140.0).then_some(at)
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn load(install: &Path, map: &str) -> Result<Bank, String> {
    let install = Install::open(install).map_err(|e| format!("cannot open the install: {e}"))?;
    let lang = LANGUAGES
        .iter()
        .position(|l| *l == install.language)
        .unwrap_or(0);
    let vfs = Arc::new(Vfs::open_stock(&install.root, lang).map_err(|e| e.to_string())?);
    // No map: the menus' own tables (the front end's `mouse_click`, `mouse_over`, ... are in the SP code zone).
    let wanted: &[&str] = if map.is_empty() {
        &["code_post_gfx_mp", "code_post_gfx"]
    } else {
        &["code_post_gfx_mp", "localized_common_mp", map]
    };
    let zones: Vec<PathBuf> = wanted.iter().filter_map(|z| install.zone_path(z)).collect();
    Bank::load(vfs, &zones)
}

/// The weapon's sound alias for a player event, `_player` (first-person) variants for the own player.
fn weapon_sound(w: Option<&WeaponDef>, event: u8, own: bool) -> Option<String> {
    let s = &w?.sounds;
    let pick = |plr: &Option<Arc<str>>, other: &Option<Arc<str>>| {
        let n = if own {
            plr.as_ref().or(other.as_ref())
        } else {
            other.as_ref()
        };
        n.filter(|n| !n.is_empty()).map(|n| n.to_string())
    };
    match event {
        ev::FIRE_WEAPON => pick(&s.fire_sound_player, &s.fire_sound),
        ev::FIRE_WEAPON_LASTSHOT => pick(&s.fire_last_sound_player, &s.fire_last_sound)
            .or_else(|| pick(&s.fire_sound_player, &s.fire_sound)),
        ev::NOAMMO => pick(&s.empty_fire_sound_player, &s.empty_fire_sound),
        ev::RELOAD => pick(&s.reload_sound_player, &s.reload_sound),
        ev::RELOAD_FROM_EMPTY => pick(&s.reload_empty_sound_player, &s.reload_empty_sound),
        ev::RELOAD_START => pick(&s.reload_start_sound_player, &s.reload_start_sound),
        ev::RELOAD_END => pick(&s.reload_end_sound_player, &s.reload_end_sound),
        ev::RECHAMBER_WEAPON => pick(&s.rechamber_sound_player, &s.rechamber_sound),
        ev::RAISE_WEAPON => pick(&s.raise_sound_player, &s.raise_sound),
        ev::FIRST_RAISE_WEAPON => pick(&s.first_raise_sound_player, &s.first_raise_sound),
        ev::PUTAWAY_WEAPON => pick(&s.putaway_sound_player, &s.putaway_sound),
        ev::PULLBACK_WEAPON => pick(&s.pullback_sound_player, &s.pullback_sound),
        ev::MELEE_SWIPE => pick(&s.melee_swipe_sound_player, &s.melee_swipe_sound),
        ev::DETONATE => pick(&s.detonate_sound_player, &s.detonate_sound),
        ev::WEAPON_ALT => pick(&s.alt_switch_sound_player, &s.alt_switch_sound),
        ev::NIGHTVISION_WEAR => pick(
            &s.night_vision_wear_sound_player,
            &s.night_vision_wear_sound,
        ),
        ev::NIGHTVISION_REMOVE => pick(
            &s.night_vision_remove_sound_player,
            &s.night_vision_remove_sound,
        ),
        _ => None,
    }
}

/// `--audio-selftest`: loads the real tables and checks, with no window and no sound card, that a match's
/// sounds come out right: direction, falloff, range, voice caps, footsteps, and streamed ambience and music.
/// Returns what was measured, or what failed.
#[cfg(not(target_arch = "wasm32"))]
pub fn selftest(install: &Path, map: &str) -> Result<Value, Vec<String>> {
    let bank = load(install, map).map_err(|e| vec![e])?;
    let menus = load(install, "").map_err(|e| vec![format!("menu sounds: {e}")])?;
    let mut s = Sound::new(bank, false);
    s.set_listener([0.0; 3], 0.0);
    let mut bad = Vec::new();
    let mut m = serde_json::Map::new();
    let mut check = |ok: bool, what: String| {
        if !ok {
            bad.push(what);
        }
    };
    let energy = |s: &mut Sound, frames| {
        let out = s.render(frames);
        out.as_chunks::<2>()
            .0
            .iter()
            .fold([0.0f32; 2], |e, f| [e[0] + f[0] * f[0], e[1] + f[1] * f[1]])
    };
    check(
        s.bank.channels.len() == 33,
        format!("{} channels, expected 33", s.bank.channels.len()),
    );
    check(
        s.bank.stats.aliases > 5000,
        format!("{} aliases", s.bank.stats.aliases),
    );
    m.insert("aliases".into(), s.bank.stats.aliases.into());
    // The front end's click and hover sounds sit in the SP code zone, which MP-only installs lack.
    let has_sp = Install::open(install)
        .map(|i| i.zone_path("code_post_gfx").is_some())
        .unwrap_or(false);
    m.insert("menu_sounds_available".into(), u8::from(has_sp).into());
    if has_sp {
        for name in ["mouse_click", "mouse_over"] {
            let on_menu = menus
                .aliases_of(name)
                .first()
                .is_some_and(|a| menus.channels[usize::from(a.channel)].name == "menu");
            check(on_menu, format!("no menu sound {name}"));
        }
    }

    // Direction and falloff of a weapon shot: the listener faces +x, its left is +y.
    let shot = "weap_ak47_fire_npc";
    let hear = |s: &mut Sound, at: [f32; 3]| {
        s.play(
            shot,
            Cue {
                origin: Some(at),
                ..Cue::default()
            },
        )
        .map(|_| energy(s, 48_000))
    };
    let left = hear(&mut s, [300.0, 300.0, 0.0]);
    let right = hear(&mut s, [300.0, -300.0, 0.0]);
    let near = hear(&mut s, [150.0, 0.0, 0.0]);
    let far = hear(&mut s, [1500.0, 0.0, 0.0]);
    let beyond = hear(&mut s, [1.0e6, 0.0, 0.0]);
    match (left, right, near, far) {
        (Some(l), Some(r), Some(n), Some(f)) => {
            check(
                l[0] > 3.0 * l[1],
                format!("a shot on the left is not left: {l:?}"),
            );
            check(
                r[1] > 3.0 * r[0],
                format!("a shot on the right is not right: {r:?}"),
            );
            check(
                (n[0] + n[1]) > 4.0 * (f[0] + f[1]),
                format!("near {n:?} far {f:?}: no falloff"),
            );
            m.insert("left_ratio".into(), (l[0] / l[1].max(1e-9)).into());
            m.insert("right_ratio".into(), (r[1] / r[0].max(1e-9)).into());
            m.insert(
                "near_far_ratio".into(),
                ((n[0] + n[1]) / (f[0] + f[1]).max(1e-9)).into(),
            );
        }
        _ => check(false, format!("{shot} did not play")),
    }
    check(beyond.is_none(), "a shot beyond its range started".into());

    // Footsteps (positioned, per surface).
    let step = s.play(
        "step_run_concrete",
        Cue {
            origin: Some([200.0, 200.0, 0.0]),
            ..Cue::default()
        },
    );
    check(step.is_some(), "step_run_concrete did not play".into());
    let e = energy(&mut s, 24_000);
    check(
        e[0] > e[1],
        format!("a footstep on the left is not left: {e:?}"),
    );

    // A channel never exceeds its cap: 40 bullet impacts on a channel capped at 10.
    let cap = s
        .bank
        .channels
        .iter()
        .find(|c| c.name == "bulletimpact")
        .map(|c| usize::from(c.max_voices));
    for i in 0..40 {
        s.play(
            "bullet_large_concrete",
            Cue {
                origin: Some([100.0 + i as f32, 0.0, 0.0]),
                ..Cue::default()
            },
        );
    }
    let _ = energy(&mut s, 480);
    let active = s.stats().active.load(std::sync::atomic::Ordering::Relaxed);
    m.insert("impact_voices".into(), active.into());
    if let Some(cap) = cap {
        check(
            active as usize <= cap,
            format!("{active} impact voices, cap {cap}"),
        );
    }

    // A bullet passing the listener whizzes by on the side it passes; one far away or starting too close does not.
    let near_miss = whizby_at([0.0; 3], [-500.0, 60.0, 0.0], [500.0, 60.0, 0.0]);
    check(
        near_miss.is_some_and(|p| p[1] > 0.0),
        format!("no whizby for a near miss: {near_miss:?}"),
    );
    check(
        whizby_at([0.0; 3], [-500.0, 400.0, 0.0], [500.0, 400.0, 0.0]).is_none(),
        "a far miss whizzed".into(),
    );
    check(
        whizby_at([0.0; 3], [-20.0, 60.0, 0.0], [500.0, 60.0, 0.0]).is_none(),
        "a bullet starting beside the listener whizzed".into(),
    );
    if let Some(p) = near_miss {
        let w = s.play(
            "whizby",
            Cue {
                origin: Some(p),
                ..Cue::default()
            },
        );
        check(w.is_some(), "whizby did not play".into());
        let e = energy(&mut s, 24_000);
        check(
            e[0] > e[1],
            format!("a whizby on the left is not left: {e:?}"),
        );
    }
    // Impact, explosion and bounce sounds exist for the surfaces.
    for prefix in [
        "bullet_small",
        "bullet_large",
        "bullet_ap",
        "bulletspray_small",
        "grenade_explode",
        "rocket_explode",
    ] {
        check(
            surface_names(5)
                .iter()
                .any(|n| s.bank.has(&format!("{prefix}_{n}"))),
            format!("no {prefix} sound for concrete"),
        );
    }

    // Room reverb: a shot leaves a tail in a hangar that a dry room does not, and the effect falls back when
    // deactivated. Then an EQ: a 300 Hz low-pass takes the crack out of the shot's channel.
    let shot_energy = |s: &mut Sound| {
        let at = Cue {
            origin: Some([300.0, 0.0, 0.0]),
            ..Cue::default()
        };
        s.play(shot, at);
        let body = energy(s, 24_000);
        let tail = energy(s, 96_000);
        (body[0] + body[1], tail[0] + tail[1])
    };
    let (_, dry_tail) = shot_energy(&mut s);
    check(
        s.set_reverb(1, "hangar", 1.0, 0),
        "hangar is not a room".into(),
    );
    let (_, wet_tail) = shot_energy(&mut s);
    s.deactivate_reverb(1, 0);
    let (_, off_tail) = shot_energy(&mut s);
    check(
        wet_tail > 4.0 * dry_tail.max(1e-9),
        format!("no reverb tail: dry {dry_tail} wet {wet_tail}"),
    );
    check(
        off_tail < 2.0 * dry_tail.max(1e-9) + 1e-6,
        format!("the reverb stayed on: {off_tail} vs {dry_tail}"),
    );
    check(
        !s.set_reverb(1, "nowhere", 1.0, 0),
        "an unknown room was accepted".into(),
    );
    m.insert(
        "reverb_tail_ratio".into(),
        (wet_tail / dry_tail.max(1e-9)).into(),
    );
    let channel = s
        .bank
        .aliases_of(shot)
        .first()
        .map(|a| s.bank.channels[usize::from(a.channel)].name.clone());
    if let Some(channel) = channel {
        let (open, _) = shot_energy(&mut s);
        let band = audio::eq::Band {
            kind: audio::eq::EqType::LowPass,
            gain_db: 0.0,
            freq: 300.0,
            q: 0.707,
        };
        check(
            s.set_eq(&channel, 0, 0, Some(band)),
            format!("no channel {channel}"),
        );
        let (filtered, _) = shot_energy(&mut s);
        s.set_eq(&channel, 0, 0, None);
        check(
            filtered < 0.5 * open,
            format!("the EQ did not take the shot down: {open} -> {filtered}"),
        );
        m.insert("eq_ratio".into(), (filtered / open.max(1e-9)).into());
    }

    // Streamed ambience and music play out of the IWDs.
    let streamed = |channel: &str, s: &mut Sound| -> Option<String> {
        s.bank
            .names()
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .into_iter()
            .find(|n| {
                s.bank.aliases_of(n).iter().any(|a| {
                    matches!(a.audio, audio::bank::Clip::Streamed(_))
                        && s.bank.channels[usize::from(a.channel)].name == channel
                })
            })
    };
    for (channel, key) in [("ambient", "ambient_heard"), ("music", "music_heard")] {
        let Some(name) = streamed(channel, &mut s) else {
            check(false, format!("no streamed {channel} alias"));
            continue;
        };
        if channel == "music" {
            s.music_play(&name);
        } else {
            s.ambient_play(&name, 0);
        }
        // The decoder runs on its own thread; give it time to fill the ring.
        let mut heard = 0.0;
        for _ in 0..200 {
            std::thread::sleep(Duration::from_millis(25));
            let e = energy(&mut s, 2400);
            heard += e[0] + e[1];
            if heard > 1.0 {
                break;
            }
        }
        check(heard > 0.1, format!("{name} ({channel}) is not audible"));
        m.insert(key.into(), heard.into());
        m.insert(format!("{channel}_alias"), name.into());
    }
    check(
        s.played.failed.is_empty(),
        format!("sound files failed: {:?}", s.played.failed),
    );
    if bad.is_empty() {
        Ok(Value::Object(m))
    } else {
        Err(bad)
    }
}
