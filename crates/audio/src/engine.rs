// SPDX-License-Identifier: GPL-3.0-or-later
//! The game-facing sound system: alias names in, voices out.
//!
//! [`Sound::play`] does what the original's `SND_PlaySoundAlias` does on the game thread: picks a variant,
//! rolls volume and pitch, skips positioned sounds out of range, continues loops already playing, opens
//! streamed files, layers the secondary alias, and hands the mixer one [`Play`].

use crate::bank::{Alias, Bank, Clip};
use crate::decode::{self, StreamJob};
use crate::device::{Config, Output};
use crate::eq::Band;
use crate::mixer::{
    Duck, Emitter, Handle, Listener, Mixer, NO_ENTITY, Play, Source, VoiceId, mixer,
};
use crate::reverb::room_index;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

/// Environment effect priorities: none, level, shellshock.
pub const ENV_PRIORITIES: usize = 3;

/// Secondary aliases chain at most this deep (the original stops at 10).
const MAX_CHAIN: u32 = 10;

/// Optional parts of a play request.
#[derive(Clone, Copy, Debug)]
pub struct Cue {
    /// World position; `None` plays a positioned channel at the listener.
    pub origin: Option<[f32; 3]>,
    pub entity: u32,
    /// Scales the alias's rolled volume.
    pub volume: f32,
    pub fade_in_ms: u32,
}

impl Default for Cue {
    fn default() -> Self {
        Self {
            origin: None,
            entity: NO_ENTITY,
            volume: 1.0,
            fade_in_ms: 0,
        }
    }
}

/// What has played, by channel name, for reports and tests.
#[derive(Clone, Debug, Default)]
pub struct Played {
    pub by_channel: BTreeMap<String, u64>,
    pub aliases: BTreeMap<String, u64>,
    pub out_of_range: u64,
    pub missing: BTreeMap<String, u64>,
    pub failed: Vec<String>,
}

enum Out {
    /// Held to keep the stream running.
    Device(#[expect(dead_code)] Output),
    /// No device: the mixer runs against the clock and the samples are dropped (or rendered by a test).
    Silent(Box<Mixer>),
}

pub struct Sound {
    pub bank: Bank,
    handle: Handle,
    out: Out,
    rate: u32,
    streams: Sender<StreamJob>,
    listener: Listener,
    loops: HashMap<(u32, String), VoiceId>,
    ambient: Option<VoiceId>,
    music: Option<VoiceId>,
    pub played: Played,
    /// Why there is no device, when there is none.
    pub device_note: Option<String>,
    scratch: Vec<f32>,
    owed: f64,
    /// `setReverb` by priority (`snd_enveffectsprio_level`, `_shellshock`): room and wet level.
    effects: [Option<(u8, f32)>; ENV_PRIORITIES],
}

fn stream_thread(rx: Receiver<StreamJob>) {
    let mut jobs: Vec<StreamJob> = Vec::new();
    loop {
        loop {
            match rx.try_recv() {
                Ok(j) => jobs.push(j),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if jobs.is_empty() {
                        return;
                    }
                    break;
                }
            }
        }
        jobs.retain_mut(StreamJob::pump);
        std::thread::sleep(Duration::from_millis(5));
    }
}

impl Sound {
    /// `use_device = false` never opens a sound card (the mixer still runs, silently).
    pub fn new(bank: Bank, use_device: bool) -> Self {
        let mut note = None;
        let device = if use_device {
            Config::probe().map_err(|e| note = Some(e)).ok()
        } else {
            note = Some("sound card disabled".into());
            None
        };
        let rate = device.as_ref().map_or(48_000, Config::sample_rate);
        let (handle, mx) = mixer(rate, &bank.channels);
        let out = match device {
            Some(cfg) => match cfg.start(mx) {
                Ok(o) => Out::Device(o),
                Err(e) => {
                    // The mixer moved into the failed stream; rebuild a silent one.
                    note = Some(e);
                    let (h2, m2) = mixer(rate, &bank.channels);
                    return Self::assemble(bank, h2, Out::Silent(Box::new(m2)), rate, note);
                }
            },
            None => Out::Silent(Box::new(mx)),
        };
        Self::assemble(bank, handle, out, rate, note)
    }

    fn assemble(bank: Bank, handle: Handle, out: Out, rate: u32, note: Option<String>) -> Self {
        let (streams, rx) = channel();
        std::thread::Builder::new()
            .name("audio-streams".into())
            .spawn(move || stream_thread(rx))
            .expect("spawn the stream decoder");
        Self {
            bank,
            handle,
            out,
            rate,
            streams,
            listener: Listener::from_yaw([0.0; 3], 0.0),
            loops: HashMap::new(),
            ambient: None,
            music: None,
            played: Played::default(),
            device_note: note,
            scratch: vec![0.0; 4096],
            owed: 0.0,
            effects: [None; ENV_PRIORITIES],
        }
    }

    pub fn has_device(&self) -> bool {
        matches!(self.out, Out::Device(_))
    }

    pub fn stats(&self) -> &Arc<crate::mixer::Stats> {
        &self.handle.stats
    }

    /// Moves the listener (the player's eye; `yaw` radians counter-clockwise from +x).
    pub fn set_listener(&mut self, pos: [f32; 3], yaw: f32) {
        self.listener = Listener::from_yaw(pos, yaw);
        self.handle.set_listener(self.listener);
    }

    /// Housekeeping once a frame; without a device also runs the mixer for `dt`.
    pub fn tick(&mut self, dt: Duration) {
        let loops = &mut self.loops;
        let ambient = &mut self.ambient;
        let music = &mut self.music;
        self.handle.reap(|id| {
            loops.retain(|_, v| *v != id);
            if *ambient == Some(id) {
                *ambient = None;
            }
            if *music == Some(id) {
                *music = None;
            }
        });
        if let Out::Silent(m) = &mut self.out {
            self.owed += dt.as_secs_f64() * f64::from(self.rate);
            while self.owed >= 1.0 {
                let frames = (self.owed as usize).min(self.scratch.len() / 2);
                m.fill(&mut self.scratch[..frames * 2]);
                self.owed -= frames as f64;
            }
        }
    }

    /// Renders `frames` stereo frames from a device-less mixer (tests, offline checks).
    pub fn render(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        if let Out::Silent(m) = &mut self.out {
            for chunk in out.chunks_mut(self.scratch.len()) {
                m.fill(chunk);
            }
        }
        out
    }

    pub fn play(&mut self, alias: &str, cue: Cue) -> Option<VoiceId> {
        self.play_chain(alias, cue, 0)
    }

    fn play_chain(&mut self, name: &str, cue: Cue, depth: u32) -> Option<VoiceId> {
        let Some(alias) = self.bank.pick(name) else {
            *self
                .played
                .missing
                .entry(name.to_ascii_lowercase())
                .or_default() += 1;
            return None;
        };
        let id = self.start(&alias, name, cue);
        if let Some(sec) = alias.secondary.as_deref()
            && depth < MAX_CHAIN
        {
            self.play_chain(sec, cue, depth + 1);
        }
        id
    }

    fn start(&mut self, alias: &Arc<Alias>, name: &str, cue: Cue) -> Option<VoiceId> {
        let def = self.bank.channels.get(usize::from(alias.channel))?.clone();
        let emitter = if def.is_3d {
            let pos = cue.origin.unwrap_or(self.listener.pos);
            let d2: f32 = (0..3)
                .map(|i| (pos[i] - self.listener.pos[i]).powi(2))
                .sum();
            if d2 > alias.dist.1 * alias.dist.1 {
                self.played.out_of_range += 1;
                return None;
            }
            Some(Emitter {
                pos,
                min: alias.dist.0,
                max: alias.dist.1,
                curve: alias.curve,
            })
        } else {
            None
        };
        let key = (cue.entity, alias.name.to_ascii_lowercase());
        if alias.looping
            && cue.entity != NO_ENTITY
            && let Some(&id) = self.loops.get(&key)
        {
            if let Some(e) = emitter {
                self.handle.set_position(id, e.pos);
            }
            return Some(id);
        }
        let (source, stereo) = match &alias.audio {
            Clip::Silent => return None,
            Clip::Loaded(p) => (Source::Loaded(p.clone()), p.channels == 2),
            Clip::Streamed(path) => {
                let opened = self
                    .bank
                    .read_stream(path)
                    .and_then(|b| StreamJob::open(b, decode::extension(path), alias.looping));
                match opened {
                    Ok((job, stream)) => {
                        let stereo = stream.channels == 2;
                        let _ = self.streams.send(job);
                        (Source::Stream(stream), stereo)
                    }
                    Err(e) => {
                        if self.played.failed.len() < 16 {
                            self.played.failed.push(e);
                        }
                        return None;
                    }
                }
            }
        };
        let id = self.handle.next_id();
        let mut p = Play::new(id, source, alias.channel);
        p.entity = cue.entity;
        p.volume = self.bank.between(alias.volume) * cue.volume;
        p.pitch = self.bank.between(alias.pitch);
        p.looping = alias.looping;
        if alias.random_looping {
            p.start = self.bank.between((0.0, 1.0));
        }
        p.delay_ms = alias.delay_ms;
        p.fade_in_ms = cue.fade_in_ms;
        p.emitter = emitter;
        p.duck = if alias.master {
            Duck::Master
        } else if alias.slave {
            Duck::Slave(alias.slave_percentage)
        } else {
            Duck::None
        };
        p.speaker = alias.speaker[usize::from(stereo)];
        p.wet = !alias.no_wet;
        self.handle.play(p);
        if alias.looping && cue.entity != NO_ENTITY {
            self.loops.insert(key, id);
        }
        *self.played.by_channel.entry(def.name).or_default() += 1;
        *self
            .played
            .aliases
            .entry(name.to_ascii_lowercase())
            .or_default() += 1;
        Some(id)
    }

    pub fn stop(&mut self, id: VoiceId) {
        self.handle.stop(id);
    }

    /// Stops a looping sound started with this entity and alias.
    pub fn stop_loop(&mut self, entity: u32, alias: &str) {
        if let Some(id) = self.loops.remove(&(entity, alias.to_ascii_lowercase())) {
            self.handle.stop(id);
        }
    }

    pub fn stop_entity(&mut self, entity: u32) {
        self.loops.retain(|k, _| k.0 != entity);
        self.handle.stop_entity(entity);
    }

    pub fn move_voice(&self, id: VoiceId, pos: [f32; 3]) {
        self.handle.set_position(id, pos);
    }

    /// `ambientPlay`: crossfades the map's ambience or music to `alias`.
    pub fn ambient_play(&mut self, alias: &str, fade_ms: u32) {
        if let Some(old) = self.ambient.take() {
            self.handle.fade_out(old, fade_ms);
        }
        self.ambient = self.play(
            alias,
            Cue {
                fade_in_ms: fade_ms,
                ..Cue::default()
            },
        );
    }

    pub fn ambient_stop(&mut self, fade_ms: u32) {
        if let Some(old) = self.ambient.take() {
            self.handle.fade_out(old, fade_ms);
        }
    }

    /// `musicPlay`: the music replaces what played before (a quick fade) and runs beside the ambience.
    pub fn music_play(&mut self, alias: &str) {
        self.music_stop(200);
        self.music = self.play(alias, Cue::default());
    }

    pub fn music_stop(&mut self, fade_ms: u32) {
        if let Some(old) = self.music.take() {
            self.handle.fade_out(old, fade_ms);
        }
    }

    /// `snd_setEnvironmentEffects`: the room and wet level of one priority, faded in over `fade_ms`. The
    /// highest active priority is the one heard. The original's dry level is fixed at 1 and is not used.
    /// Returns false for an unknown room or priority.
    pub fn set_reverb(&mut self, priority: usize, room: &str, wet: f32, fade_ms: u32) -> bool {
        let (Some(room), true) = (room_index(room), (1..ENV_PRIORITIES).contains(&priority)) else {
            return false;
        };
        self.effects[priority] = Some((room, wet.clamp(0.0, 1.0)));
        self.send_reverb(fade_ms);
        true
    }

    /// `snd_deactivateEnvironmentEffects`: falls back to the next lower active priority, or to no reverb.
    pub fn deactivate_reverb(&mut self, priority: usize, fade_ms: u32) {
        if let Some(e) = self.effects.get_mut(priority) {
            *e = None;
        }
        self.send_reverb(fade_ms);
    }

    fn send_reverb(&mut self, fade_ms: u32) {
        let (room, wet) = self
            .effects
            .iter()
            .rev()
            .flatten()
            .next()
            .copied()
            .unwrap_or((0, 0.0));
        self.handle.set_reverb(room, wet, fade_ms);
    }

    /// `snd_setEq`: sets one band of an entity channel's EQ (`eq` 0 or 1, `band` 0 to 2). Returns false for an
    /// unknown channel or an index out of range.
    pub fn set_eq(&mut self, channel: &str, eq: u8, band: u8, set: Option<Band>) -> bool {
        let Some(row) = self
            .bank
            .channels
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(channel))
        else {
            return false;
        };
        if usize::from(eq) >= crate::eq::EQS || usize::from(band) >= crate::eq::BANDS {
            return false;
        }
        self.handle.set_eq(row as u8, eq, band, set);
        true
    }
}
