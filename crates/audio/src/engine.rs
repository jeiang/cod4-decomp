// SPDX-License-Identifier: GPL-3.0-only
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
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

/// Environment effect priorities: none, level, shellshock.
pub const ENV_PRIORITIES: usize = 3;

/// Secondary aliases chain at most this deep (the original stops at 10).
const MAX_CHAIN: u32 = 10;
/// How many voices of one entity and alias [`Sound::stop_alias`] remembers.
const MAX_STOPPABLE: usize = 4;

/// Optional parts of a play request.
#[derive(Clone, Copy, Debug)]
pub struct Cue {
    /// World position; `None` plays a positioned channel at the listener.
    pub origin: Option<[f32; 3]>,
    pub entity: u32,
    /// Scales the alias's rolled volume.
    pub volume: f32,
    pub fade_in_ms: u32,
    /// A later [`Sound::stop_alias`] may cut this sound short (a reload, `stoplocalsound`).
    pub stoppable: bool,
    /// Plays as a master whatever the alias says (`playSoundAsMaster`): the slaves duck while it does.
    pub master: bool,
}

impl Default for Cue {
    fn default() -> Self {
        Self {
            origin: None,
            entity: NO_ENTITY,
            volume: 1.0,
            fade_in_ms: 0,
            stoppable: false,
            master: false,
        }
    }
}

/// The voices [`Sound::stop_alias`] can cut, by entity and lower-case alias. Only sounds asked for as
/// [`Cue::stoppable`] are kept, so busy footsteps never push a reload out.
#[derive(Default)]
struct Stoppable(HashMap<(u32, String), VecDeque<VoiceId>>);

impl Stoppable {
    fn record(&mut self, entity: u32, alias: &str, id: VoiceId) {
        let q = self
            .0
            .entry((entity, alias.to_ascii_lowercase()))
            .or_default();
        if q.len() >= MAX_STOPPABLE {
            q.pop_front();
        }
        q.push_back(id);
    }

    fn take(&mut self, entity: u32, alias: &str) -> Vec<VoiceId> {
        self.0
            .remove(&(entity, alias.to_ascii_lowercase()))
            .map(Vec::from)
            .unwrap_or_default()
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
    /// Music aliases asked for while the music was still playing.
    pub music_ignored: u64,
}

enum Out {
    /// Held to keep the stream running.
    Device(#[cfg_attr(not(target_arch = "wasm32"), expect(dead_code))] Output),
    /// No device: the mixer runs against the clock and the samples are dropped (or rendered by a test).
    Silent(Box<Mixer>),
}

pub struct Sound {
    pub bank: Bank,
    handle: Handle,
    out: Out,
    rate: u32,
    streams: Streams,
    listener: Listener,
    loops: HashMap<(u32, String), VoiceId>,
    /// The newest one-shot voices of entities, by lower-case alias name, for [`Sound::stop_alias`]. A voice that has
    /// ended is a harmless stop.
    stoppable: Stoppable,
    /// Entity loops that were out of range when asked for, started by [`Sound::follow_entity`] once their entity is
    /// near enough.
    waiting: HashMap<(u32, String), Arc<Alias>>,
    /// The map's ambience: the primary alias and its secondary, each with the voice playing it.
    ambient: [Option<(Arc<str>, VoiceId)>; 2],
    music: Option<VoiceId>,
    /// Where each voice with a chain alias goes on when it ends: the alias and where it plays.
    chains: HashMap<VoiceId, (Arc<str>, Cue)>,
    pub played: Played,
    /// Why there is no device, when there is none.
    pub device_note: Option<String>,
    scratch: Vec<f32>,
    owed: f64,
    /// `setReverb` by priority (`snd_enveffectsprio_level`, `_shellshock`): room and wet level.
    effects: [Option<(u8, f32)>; ENV_PRIORITIES],
    /// The two voices of a shell shock's tinnitus (loud, quiet).
    shock_loops: [Option<VoiceId>; 2],
    /// `snd_volume` as last sent to the mixer.
    volume: f32,
}

/// Where streamed files decode. Natively a thread feeds each job's ring; on `wasm32`, which has no threads
/// to spare, the owner decodes in small slices from [`Sound::pump`].
#[cfg(not(target_arch = "wasm32"))]
mod streams {
    use crate::decode::StreamJob;
    use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
    use std::time::Duration;

    pub struct Streams(Sender<StreamJob>);

    impl Streams {
        pub fn new() -> Self {
            let (tx, rx) = channel();
            std::thread::Builder::new()
                .name("audio-streams".into())
                .spawn(move || thread(rx))
                .expect("spawn the stream decoder");
            Self(tx)
        }

        pub fn add(&mut self, job: StreamJob) {
            let _ = self.0.send(job);
        }

        pub fn pump(&mut self) {}
    }

    fn thread(rx: Receiver<StreamJob>) {
        let mut jobs: Vec<StreamJob> = Vec::new();
        loop {
            loop {
                match rx.try_recv() {
                    Ok(j) => jobs.push(j),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
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
}

#[cfg(target_arch = "wasm32")]
mod streams {
    use crate::decode::StreamJob;

    /// Samples one job decodes per [`Streams::pump`] (about 90 ms of 44.1 kHz stereo, a few ms of work).
    const SLICE: usize = 8192;

    pub struct Streams(Vec<StreamJob>);

    impl Streams {
        pub fn new() -> Self {
            Self(Vec::new())
        }

        /// Decodes the first slice at once so the voice has samples when the mixer first looks.
        pub fn add(&mut self, mut job: StreamJob) {
            if job.pump_for(SLICE) {
                self.0.push(job);
            }
        }

        pub fn pump(&mut self) {
            self.0.retain_mut(|j| j.pump_for(SLICE));
        }
    }
}

use streams::Streams;

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
        Self {
            bank,
            handle,
            out,
            rate,
            streams: Streams::new(),
            listener: Listener::from_yaw([0.0; 3], 0.0),
            loops: HashMap::new(),
            stoppable: Stoppable::default(),
            waiting: HashMap::new(),
            ambient: [None, None],
            music: None,
            chains: HashMap::new(),
            played: Played::default(),
            device_note: note,
            scratch: vec![0.0; 4096],
            owed: 0.0,
            effects: [None; ENV_PRIORITIES],
            shock_loops: [None; 2],
            volume: crate::mixer::DEFAULT_VOLUME,
        }
    }

    pub fn has_device(&self) -> bool {
        matches!(self.out, Out::Device(_))
    }

    /// The browser output's counters (frames played, underruns, queue depth); `None` natively and without a
    /// device, where the mixer's own [`Sound::stats`] are all there is.
    pub fn output_stats(&self) -> Option<crate::transport::OutputStats> {
        #[cfg(target_arch = "wasm32")]
        if let Out::Device(o) = &self.out {
            return Some(o.stats());
        }
        None
    }

    pub fn stats(&self) -> &Arc<crate::mixer::Stats> {
        &self.handle.stats
    }

    /// `snd_volume`: the master volume, 0 to 1. Cheap to call every frame.
    pub fn set_volume(&mut self, volume: f32) {
        // Only remembered once queued, so a full queue is retried next frame.
        if volume != self.volume && self.handle.set_master_volume(volume) {
            self.volume = volume;
        }
    }

    /// Moves the listener (the player's eye; `yaw` radians counter-clockwise from +x).
    pub fn set_listener(&mut self, pos: [f32; 3], yaw: f32) {
        self.listener = Listener::from_yaw(pos, yaw);
        self.handle.set_listener(self.listener);
    }

    /// Decodes the next slice of every streamed file. **Call once per frame on `wasm32`** (before or after
    /// [`Sound::tick`]), where there is no decoder thread; a streamed voice starves if it is not called.
    /// Natively a thread decodes and this does nothing.
    pub fn pump(&mut self) {
        self.streams.pump();
    }

    /// Housekeeping once a frame; without a device also runs the mixer for `dt`.
    pub fn tick(&mut self, dt: Duration) {
        let loops = &mut self.loops;
        let ambient = &mut self.ambient;
        let music = &mut self.music;
        let chains = &mut self.chains;
        let mut chained = Vec::new();
        self.handle.reap(|id, to_end| {
            loops.retain(|_, v| *v != id);
            for track in ambient.iter_mut() {
                if track.as_ref().is_some_and(|(_, v)| *v == id) {
                    *track = None;
                }
            }
            if *music == Some(id) {
                *music = None;
            }
            // `SND_StopChannelAndPlayChainAlias`: a sound that ran out goes on with its chain alias.
            if let Some(c) = chains.remove(&id)
                && to_end
            {
                chained.push(c);
            }
        });
        for (name, cue) in chained {
            self.play(&name, cue);
        }
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

    /// `SND_GetKnownLength`: how long the first variant of `alias` plays, in milliseconds, when its samples are in
    /// the tables (a streamed file's length is not known before it plays).
    pub fn known_length_ms(&self, alias: &str) -> Option<u32> {
        match &self.bank.aliases_of(alias).first()?.audio {
            Clip::Loaded(p) => Some((p.frames() as u64 * 1000 / u64::from(p.rate.max(1))) as u32),
            _ => None,
        }
    }

    pub fn play(&mut self, alias: &str, cue: Cue) -> Option<VoiceId> {
        self.play_chain(alias, cue, 0)
    }

    fn play_chain(&mut self, name: &str, cue: Cue, depth: u32) -> Option<VoiceId> {
        let Some(alias) = self.bank.pick(name) else {
            self.missing(name);
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

    fn missing(&mut self, name: &str) {
        *self
            .played
            .missing
            .entry(name.to_ascii_lowercase())
            .or_default() += 1;
    }

    fn start(&mut self, alias: &Arc<Alias>, name: &str, cue: Cue) -> Option<VoiceId> {
        let def = self.bank.channels.get(usize::from(alias.channel))?.clone();
        let emitter = if def.is_3d {
            let pos = cue.origin.unwrap_or(self.listener.pos);
            let d2: f32 = (0..3)
                .map(|i| (pos[i] - self.listener.pos[i]).powi(2))
                .sum();
            if d2 > alias.dist.1 * alias.dist.1 {
                if alias.looping && cue.entity != NO_ENTITY {
                    self.waiting
                        .insert((cue.entity, alias.name.to_ascii_lowercase()), alias.clone());
                } else {
                    self.played.out_of_range += 1;
                }
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
        // A looping or randomly started sound begins at a random point of its file.
        let start = if alias.random_looping {
            self.bank.between((0.0, 1.0))
        } else {
            0.0
        };
        let (source, stereo) = match &alias.audio {
            Clip::Silent => return None,
            Clip::Loaded(p) => (Source::Loaded(p.clone()), p.channels == 2),
            Clip::Streamed(path) => {
                let opened = self.bank.read_stream(path).and_then(|b| {
                    StreamJob::open(b, decode::extension(path), alias.looping, start)
                });
                match opened {
                    Ok((job, stream)) => {
                        let stereo = stream.channels == 2;
                        self.streams.add(job);
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
        p.start = start;
        p.delay_ms = alias.delay_ms;
        p.fade_in_ms = cue.fade_in_ms;
        p.emitter = emitter;
        p.duck = if alias.master || cue.master {
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
        } else if cue.stoppable {
            self.stoppable.record(cue.entity, name, id);
        }
        if let Some(chain) = &alias.chain
            && !alias.looping
            && !chain.eq_ignore_ascii_case(&alias.name)
        {
            let at = Cue {
                origin: cue.origin,
                entity: cue.entity,
                ..Cue::default()
            };
            self.chains.insert(id, (chain.clone(), at));
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
        let key = (entity, alias.to_ascii_lowercase());
        self.waiting.remove(&key);
        if let Some(id) = self.loops.remove(&key) {
            self.handle.stop(id);
        }
    }

    /// `CG_StopSoundAlias`: stops the sounds of `alias` that `entity` started.
    pub fn stop_alias(&mut self, entity: u32, alias: &str) {
        for id in self.stoppable.take(entity, alias) {
            self.handle.stop(id);
        }
    }

    /// Ends the looping sounds of `entity`, and only those.
    pub fn stop_entity_loops(&mut self, entity: u32) {
        self.waiting.retain(|k, _| k.0 != entity);
        let keys: Vec<_> = self
            .loops
            .keys()
            .filter(|k| k.0 == entity)
            .cloned()
            .collect();
        for k in keys {
            if let Some(id) = self.loops.remove(&k) {
                self.handle.stop(id);
            }
        }
    }

    pub fn stop_entity(&mut self, entity: u32) {
        self.loops.retain(|k, _| k.0 != entity);
        self.waiting.retain(|k, _| k.0 != entity);
        self.handle.stop_entity(entity);
    }

    /// The entity moved to `pos`: its looping sounds move with it, and those that were out of range start if they
    /// are in range now.
    pub fn follow_entity(&mut self, entity: u32, pos: [f32; 3]) {
        for (k, id) in &self.loops {
            if k.0 == entity {
                self.handle.set_position(*id, pos);
            }
        }
        let ready: Vec<_> = self
            .waiting
            .iter()
            .filter(|(k, _)| k.0 == entity)
            .map(|(k, a)| (k.1.clone(), a.clone()))
            .collect();
        for (name, alias) in ready {
            let cue = Cue {
                origin: Some(pos),
                entity,
                ..Cue::default()
            };
            if self.start(&alias, &name, cue).is_some() {
                self.waiting.remove(&(entity, name));
            }
        }
    }

    pub fn move_voice(&self, id: VoiceId, pos: [f32; 3]) {
        self.handle.set_position(id, pos);
    }

    /// `SND_PlayAmbientAlias`: crossfades the map's ambience to `alias` and its secondary layer. A layer already
    /// playing the alias asked for carries on; one that is not wanted fades out.
    pub fn ambient_play(&mut self, alias: &str, fade_ms: u32) {
        let Some(primary) = self.bank.pick(alias) else {
            self.missing(alias);
            return;
        };
        let secondary = primary.secondary.as_deref().and_then(|n| self.bank.pick(n));
        let cue = Cue {
            fade_in_ms: fade_ms,
            ..Cue::default()
        };
        for (slot, wanted) in [Some(primary), secondary].into_iter().enumerate() {
            if let (Some(a), Some((playing, _))) = (&wanted, &self.ambient[slot])
                && a.name.eq_ignore_ascii_case(playing)
            {
                continue;
            }
            if let Some((_, old)) = self.ambient[slot].take() {
                self.handle.fade_out(old, fade_ms);
            }
            if let Some(a) = wanted {
                self.ambient[slot] = self.start(&a, &a.name, cue).map(|id| (a.name.clone(), id));
            }
        }
    }

    /// `SND_StopAmbient`: both layers fade out.
    pub fn ambient_stop(&mut self, fade_ms: u32) {
        for (_, old) in self.ambient.iter_mut().filter_map(Option::take) {
            self.handle.fade_out(old, fade_ms);
        }
    }

    /// The voices of the ambience, primary first (none that has ended).
    pub fn ambient_voices(&self) -> Vec<VoiceId> {
        self.ambient.iter().flatten().map(|(_, v)| *v).collect()
    }

    /// `SND_PlayMusicAlias`: starts the music alongside the ambience, unless music is still playing (the original
    /// refuses a new one then and says so).
    pub fn music_play(&mut self, alias: &str) {
        if self.music.is_some() {
            self.played.music_ignored += 1;
            return;
        }
        self.music = self.play(alias, Cue::default());
    }

    pub fn music_stop(&mut self, fade_ms: u32) {
        if let Some(old) = self.music.take() {
            self.handle.fade_out(old, fade_ms);
        }
    }

    /// `SND_StopSounds(SND_STOP_ALL)`, for a map restart: every voice, the room effects, the channel volumes and the
    /// EQs go back to how a new level starts.
    pub fn stop_all(&mut self) {
        self.handle.stop_all();
        self.loops.clear();
        self.waiting.clear();
        self.stoppable = Stoppable::default();
        self.chains.clear();
        self.ambient = [None, None];
        self.music = None;
        self.shock_loops = [None; 2];
        self.effects = [None; ENV_PRIORITIES];
    }

    /// `SND_FadeAllSounds` (`soundfade`): everything goes to `volume` times its own over `fade_ms`; with no time to
    /// fade, silence stops the sounds outright.
    pub fn fade_all(&mut self, volume: f32, fade_ms: u32) {
        self.handle.fade_all(volume, fade_ms);
        if fade_ms == 0 && volume <= 0.0 {
            self.stop_all();
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

    /// `SND_PlayBlendedSoundAliases` of a shell shock's tinnitus: the loud and the quiet loop play together and
    /// `fade` (0 to 1) moves the ear from one to the other. Call every frame while it rings.
    pub fn shock_loop(&mut self, loud: &str, quiet: &str, fade: f32) {
        let fade = fade.clamp(0.0, 1.0);
        for (i, (name, share)) in [(loud, 1.0 - fade), (quiet, fade)].into_iter().enumerate() {
            let Some(base) = self.bank.pick(name).map(|a| a.volume.1) else {
                continue;
            };
            if self.shock_loops[i].is_none() {
                self.shock_loops[i] = self.play(name, Cue::default());
            }
            if let Some(id) = self.shock_loops[i] {
                self.handle.set_volume(id, base * share);
            }
        }
    }

    /// The tinnitus ends.
    pub fn shock_loop_stop(&mut self) {
        for id in self.shock_loops.iter_mut().filter_map(Option::take) {
            self.handle.fade_out(id, 100);
        }
    }

    /// `SND_SetChannelVolumes`: group `priority` (1 hold breath, 2 pain, 3 shell shock) sets the volume of each entity
    /// channel (by row of `channels.def`; missing ones stay full) over `fade_ms`.
    pub fn set_channel_volumes(&mut self, priority: u8, volumes: &[f32], fade_ms: u32) {
        let mut goals = [1.0; crate::channels::MAX_CHANNELS];
        for (g, v) in goals.iter_mut().zip(volumes) {
            *g = *v;
        }
        self.handle.set_channel_volumes(priority, goals, fade_ms);
    }

    /// `SND_DeactivateChannelVolumes`.
    pub fn deactivate_channel_volumes(&mut self, priority: u8, fade_ms: u32) {
        self.handle.deactivate_channel_volumes(priority, fade_ms);
    }

    /// The priority of the channel volume group in force, 0 when none.
    pub fn channel_volume_priority(&self) -> u32 {
        self.handle
            .stats
            .channel_priority
            .load(std::sync::atomic::Ordering::Relaxed)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_voice_is_taken_once_for_its_entity_and_alias() {
        let mut s = Stoppable::default();
        s.record(3, "Reload_M4", 7);
        assert!(s.take(4, "reload_m4").is_empty());
        assert!(s.take(3, "reload_other").is_empty());
        assert_eq!(s.take(3, "reload_m4"), [7]);
        assert!(s.take(3, "reload_m4").is_empty());
    }

    #[test]
    fn other_sounds_do_not_push_a_recorded_voice_out() {
        let mut s = Stoppable::default();
        s.record(3, "reload", 1);
        for i in 0..500 {
            s.record(i % 20, "step", 100 + i);
        }
        assert_eq!(s.take(3, "reload"), [1]);
    }

    /// Half-scale DC lasting `ms` at 48 kHz.
    fn dc(ms: u32) -> Clip {
        Clip::Loaded(Arc::new(crate::mixer::Pcm {
            rate: 48_000,
            channels: 1,
            samples: vec![16384; (48 * ms) as usize].into(),
        }))
    }

    fn chan(name: &str, is_3d: bool) -> crate::channels::ChannelDef {
        crate::channels::ChannelDef {
            name: name.into(),
            priority: 1,
            is_3d,
            restricted: false,
            pausable: true,
            max_voices: 32,
        }
    }

    /// A silent-device engine over the given alias lists, channel 0 flat and channel 1 positioned.
    fn engine(lists: Vec<(&str, Vec<Alias>)>) -> Sound {
        let bank = Bank::synthetic(vec![chan("flat", false), chan("world", true)], lists);
        let mut s = Sound::new(bank, false);
        s.set_listener([0.0; 3], 0.0);
        s
    }

    fn active(s: &Sound) -> u32 {
        s.stats().active.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Runs the mixer for `ms` and the engine's housekeeping.
    fn run(s: &mut Sound, ms: u64) {
        s.render(48 * ms as usize);
        s.tick(Duration::from_millis(ms));
    }

    #[test]
    fn stop_all_ends_loops_ambience_music_and_the_room_effect() {
        let mut looping = Alias::test("hum", 1, dc(500));
        looping.looping = true;
        let mut s = engine(vec![
            ("hum", vec![looping]),
            ("amb", vec![Alias::test("amb", 0, dc(2000))]),
            ("mus", vec![Alias::test("mus", 0, dc(2000))]),
        ]);
        s.play(
            "hum",
            Cue {
                origin: Some([50.0, 0.0, 0.0]),
                entity: 7,
                ..Cue::default()
            },
        );
        s.ambient_play("amb", 0);
        s.music_play("mus");
        assert!(s.set_reverb(1, "hangar", 1.0, 0));
        s.set_channel_volumes(3, &[0.1, 0.1], 0);
        run(&mut s, 20);
        assert_eq!(active(&s), 3);
        assert_eq!(s.channel_volume_priority(), 3);
        s.stop_all();
        run(&mut s, 20);
        assert_eq!(active(&s), 0);
        assert_eq!(s.channel_volume_priority(), 0);
        assert!(s.ambient_voices().is_empty());
        // The loop is not remembered: asking for it again starts a voice.
        s.play(
            "hum",
            Cue {
                origin: Some([50.0, 0.0, 0.0]),
                entity: 7,
                ..Cue::default()
            },
        );
        run(&mut s, 20);
        assert_eq!(active(&s), 1);
        // And the music is free to play again.
        s.music_play("mus");
        assert_eq!(s.played.music_ignored, 0);
        // Leftover reverb is gone: a fresh one-shot has no tail after it ends.
        s.stop_all();
        s.play("amb", Cue::default());
        run(&mut s, 2100);
        let tail: f32 = s.render(4800).iter().map(|x| x * x).sum();
        assert!(tail < 1e-9, "a tail of {tail} after a restart");
    }

    #[test]
    fn an_ambient_alias_asked_for_again_carries_on_and_its_secondary_fades_with_it() {
        let mut main = Alias::test("amb", 0, dc(5000));
        main.secondary = Some("amb_sec".into());
        let mut s = engine(vec![
            ("amb", vec![main]),
            ("amb_sec", vec![Alias::test("amb_sec", 0, dc(5000))]),
            ("other", vec![Alias::test("other", 0, dc(5000))]),
        ]);
        s.ambient_play("amb", 0);
        s.ambient_play("amb", 0);
        run(&mut s, 20);
        assert_eq!(s.played.aliases.get("amb"), Some(&1), "restarted");
        assert_eq!(s.played.aliases.get("amb_sec"), Some(&1));
        assert_eq!(active(&s), 2);
        assert_eq!(s.ambient_voices().len(), 2);
        // Another ambience takes both layers away, the secondary too.
        s.ambient_play("other", 10);
        run(&mut s, 200);
        assert_eq!(active(&s), 1);
        s.ambient_stop(10);
        run(&mut s, 200);
        assert_eq!(active(&s), 0);
    }

    #[test]
    fn a_new_music_alias_waits_for_the_music_to_end() {
        let mut s = engine(vec![
            ("one", vec![Alias::test("one", 0, dc(100))]),
            ("two", vec![Alias::test("two", 0, dc(100))]),
        ]);
        s.music_play("one");
        s.music_play("two");
        assert_eq!(s.played.music_ignored, 1);
        assert_eq!(s.played.aliases.get("two"), None);
        run(&mut s, 300);
        s.music_play("two");
        assert_eq!(s.played.aliases.get("two"), Some(&1));
    }

    #[test]
    fn music_the_mixer_refused_does_not_block_the_next_one() {
        let mut capped = chan("capped", false);
        capped.max_voices = 1;
        let bank = Bank::synthetic(
            vec![chan("flat", false), capped],
            vec![
                ("fill", vec![Alias::test("fill", 1, dc(300))]),
                ("mus", vec![Alias::test("mus", 1, dc(2000))]),
            ],
        );
        let mut s = Sound::new(bank, false);
        s.play("fill", Cue::default());
        s.music_play("mus");
        run(&mut s, 20);
        assert_eq!(active(&s), 1, "the music was refused");
        run(&mut s, 400);
        s.music_play("mus");
        assert_eq!(s.played.music_ignored, 0);
        run(&mut s, 20);
        assert_eq!(active(&s), 1, "the music plays once the channel is free");
    }

    #[test]
    fn a_sound_that_runs_out_goes_on_with_its_chain_alias_but_a_stopped_one_does_not() {
        let mut first = Alias::test("first", 1, dc(50));
        first.chain = Some("second".into());
        let mut s = engine(vec![
            ("first", vec![first]),
            ("second", vec![Alias::test("second", 1, dc(50))]),
        ]);
        let at = Cue {
            origin: Some([100.0, 0.0, 0.0]),
            entity: 3,
            ..Cue::default()
        };
        let id = s.play("first", at).unwrap();
        s.stop(id);
        run(&mut s, 200);
        assert_eq!(
            s.played.aliases.get("second"),
            None,
            "a stopped sound chained"
        );
        s.play("first", at);
        run(&mut s, 200);
        assert_eq!(s.played.aliases.get("second"), Some(&1));
    }

    #[test]
    fn a_local_sound_can_be_stopped_by_alias() {
        let mut s = engine(vec![("beep", vec![Alias::test("beep", 0, dc(2000))])]);
        let local = Cue {
            stoppable: true,
            ..Cue::default()
        };
        s.play("beep", local);
        s.play("beep", local);
        run(&mut s, 20);
        assert_eq!(active(&s), 2);
        s.stop_alias(NO_ENTITY, "BEEP");
        run(&mut s, 50);
        assert_eq!(active(&s), 0);
    }

    #[test]
    fn playing_as_master_ducks_a_slave_that_is_not_a_master_itself() {
        let mut slave = Alias::test("slave", 0, dc(20_000));
        slave.slave = true;
        slave.slave_percentage = 0.25;
        let mut s = engine(vec![
            ("slave", vec![slave]),
            ("voice", vec![Alias::test("voice", 0, dc(20_000))]),
        ]);
        s.play("slave", Cue::default());
        let level = |s: &mut Sound| {
            run(s, 500);
            let out = s.render(4800);
            out.iter().map(|x| x.abs()).sum::<f32>() / out.len() as f32
        };
        let open = level(&mut s);
        s.play("voice", Cue::default());
        let plain = level(&mut s);
        s.stop_all();
        s.play("slave", Cue::default());
        s.play(
            "voice",
            Cue {
                master: true,
                ..Cue::default()
            },
        );
        let ducked = level(&mut s);
        // The same two sounds, but only the one played as a master pulls the slave down.
        assert!(
            ducked < plain - 0.2 * open,
            "open {open}, plain {plain}, as master {ducked}"
        );
    }
}
