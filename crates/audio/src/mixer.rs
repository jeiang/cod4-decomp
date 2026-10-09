// SPDX-License-Identifier: GPL-3.0-only
// Parts of the audio channel volume groups and shell shock timeline are translated from KisakCOD (sound/snd.cpp, cgame/cg_shellshock.cpp, client_mp/cl_input.cpp; GPL-3.0).
//! The mixer: voices in, interleaved stereo `f32` out.
//!
//! [`Mixer::fill`] is the whole audio callback. It never locks, allocates or blocks: voices live in a fixed
//! table, commands arrive over a bounded lock-free queue ([`Handle`]), streamed audio over per-voice rings
//! ([`crate::ring`]), and the counters leave through atomics. Nothing in it touches a device or a thread, so the
//! same code runs under cpal, in an offline render, or inside a browser AudioWorklet.
//!
//! The mixer owns the original's voice rules: pools of 8 2D, 32 3D and 13 streamed voices, a per-channel voice
//! cap, a priority per channel and replacement of the lowest-priority (then farthest or quietest) voice, one
//! sound per entity on restricted channels, master/slave ducking, and distance gain from the alias's own falloff
//! curve. Positioned sounds are panned on the left/right of the listener; 2D sounds use the alias's speaker map.
//!
//! Two effects ride on it: a room reverb send bus (`setReverb`; sounds whose alias is not flagged no-wet feed
//! it) and a parametric EQ per entity channel.

#![allow(clippy::needless_range_loop)] // 2x2 gain matrices read better indexed

use crate::channels::{ChannelDef, MAX_CHANNELS};
use crate::curve::Curve;
use crate::eq::{BANDS, Band, ChannelEq, Coeffs, EQS, EqState};
use crate::reverb::Reverb;
use crate::ring::Consumer;
use crossbeam_queue::ArrayQueue;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub type VoiceId = u32;

/// No entity: a restricted channel does not replace anything.
pub const NO_ENTITY: u32 = u32::MAX;

/// Voices the original mixes at once, by kind (`MSS_Init`).
const POOL_SIZE: [usize; 3] = [8, 32, 13];
const MAX_VOICES: usize = 64;
const QUEUE: usize = 1024;
/// Frames mixed at a time, so the reverb send needs one fixed buffer.
const CHUNK: usize = 512;
/// What `snd_volume` is multiplied by to give the gain of everything (the original's `g_snd.volume`).
const MASTER: f32 = 0.75;
/// `snd_volume`'s default, so an untouched mixer is as loud as the original's.
pub const DEFAULT_VOLUME: f32 = 0.8;
/// How long a slave takes to duck or recover (`snd_slaveFadeTime`).
const SLAVE_FADE_MS: f32 = 500.0;
/// Stopping a voice fades it over this long instead of clicking.
const STOP_FADE_MS: f32 = 5.0;

/// Interleaved 16-bit samples held in memory: a loaded sound.
#[derive(Debug)]
pub struct Pcm {
    pub rate: u32,
    pub channels: u8,
    pub samples: Box<[i16]>,
}

impl Pcm {
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }
}

/// The consumer end of a decoder's ring.
pub struct Stream {
    pub ring: Consumer,
    pub rate: u32,
    pub channels: u8,
}

pub enum Source {
    Loaded(Arc<Pcm>),
    Stream(Stream),
}

impl Source {
    fn channels(&self) -> usize {
        match self {
            Source::Loaded(p) => usize::from(p.channels),
            Source::Stream(s) => usize::from(s.channels),
        }
    }

    fn rate(&self) -> u32 {
        match self {
            Source::Loaded(p) => p.rate,
            Source::Stream(s) => s.rate,
        }
    }
}

/// Where a sound is and how far it carries.
#[derive(Clone, Copy, Debug)]
pub struct Emitter {
    pub pos: [f32; 3],
    pub min: f32,
    pub max: f32,
    pub curve: Curve,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Duck {
    None,
    /// Ducks the slaves while it plays.
    Master,
    /// Ducked by masters down to this fraction.
    Slave(f32),
}

pub struct Play {
    pub id: VoiceId,
    pub source: Source,
    /// Row of `channels.def`.
    pub channel: u8,
    pub entity: u32,
    pub volume: f32,
    pub pitch: f32,
    pub looping: bool,
    /// Where a looping or randomly started sound begins, as a fraction of its length (loaded sounds only).
    pub start: f32,
    pub delay_ms: u32,
    pub fade_in_ms: u32,
    /// `None`: a 2D sound.
    pub emitter: Option<Emitter>,
    pub duck: Duck,
    /// 2D sounds: the gain of source channel `c` into the left (`[c][0]`) and right (`[c][1]`) speaker.
    pub speaker: [[f32; 2]; 2],
    /// Feeds the reverb bus (the alias does not have its no-wet flag).
    pub wet: bool,
}

impl Play {
    /// A plain 2D sound at full volume.
    pub fn new(id: VoiceId, source: Source, channel: u8) -> Self {
        Self {
            id,
            source,
            channel,
            entity: NO_ENTITY,
            volume: 1.0,
            pitch: 1.0,
            looping: false,
            start: 0.0,
            delay_ms: 0,
            fade_in_ms: 0,
            emitter: None,
            duck: Duck::None,
            speaker: [[0.5, 0.5], [1.0, 0.0]],
            wet: true,
        }
    }
}

/// Where the player's ears are.
#[derive(Clone, Copy, Debug)]
pub struct Listener {
    pub pos: [f32; 3],
    /// Unit vector to the listener's left.
    pub left: [f32; 3],
}

impl Listener {
    /// A listener at `pos` facing `yaw` radians counter-clockwise from +x (the game's x forward, y left, z up).
    pub fn from_yaw(pos: [f32; 3], yaw: f32) -> Self {
        Self {
            pos,
            left: [-yaw.sin(), yaw.cos(), 0.0],
        }
    }
}

enum Command {
    Play(Play),
    Stop(VoiceId),
    FadeOut(VoiceId, u32),
    StopEntity(u32),
    SetPosition(VoiceId, [f32; 3]),
    SetVolume(VoiceId, f32),
    /// `SND_StopSounds(SND_STOP_ALL)`: every voice, the room effect, the channel volume groups and the EQs.
    StopAll,
    /// `SND_FadeAllSounds`: the fade volume moves to `goal` over `ms` (at once for 0).
    Fade {
        goal: f32,
        ms: u32,
    },
    /// `snd_volume`.
    Master(f32),
    Listener(Listener),
    /// The room and wet level to fade to over `fade_ms`.
    Reverb {
        room: u8,
        wet: f32,
        fade_ms: u32,
    },
    /// Band `band` of EQ `eq` of entity channel `channel`.
    Eq {
        channel: u8,
        eq: u8,
        band: u8,
        set: Option<Band>,
    },
    ChannelVolumes {
        priority: u8,
        goals: [f32; MAX_CHANNELS],
        fade_ms: u32,
    },
    DeactivateChannelVolumes {
        priority: u8,
        fade_ms: u32,
    },
}

/// What the mixer has done, readable from any thread.
#[derive(Default)]
pub struct Stats {
    pub started: AtomicU64,
    /// Plays refused: the channel's cap or its pool was full of sounds that cannot be replaced.
    pub refused: AtomicU64,
    pub replaced: AtomicU64,
    pub finished: AtomicU64,
    /// Command queue was full.
    pub lost_commands: AtomicU64,
    /// A stream had nothing to play when its frame came due.
    pub underruns: AtomicU64,
    pub active: AtomicU32,
    /// The priority of the channel volume group in force (0 when none is).
    pub channel_priority: AtomicU32,
    /// Largest output sample seen, as `f32` bits.
    peak: AtomicU32,
}

impl Stats {
    pub fn peak(&self) -> f32 {
        f32::from_bits(self.peak.load(Ordering::Relaxed))
    }

    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }
}

/// A voice the mixer is done with: whether it played to its end (rather than being stopped or replaced), and
/// its source, handed back so the mixer never frees memory.
type Retired = (VoiceId, Source, bool);

/// The game-thread end: sends commands to the mixer.
pub struct Handle {
    queue: Arc<ArrayQueue<Command>>,
    retired: Arc<ArrayQueue<Retired>>,
    next_id: u32,
    pub stats: Arc<Stats>,
}

impl Handle {
    /// Frees the sources of finished voices and tells `ended` which they were, and whether each played to its end
    /// (a stopped or replaced voice did not). The mixer hands sources back instead of dropping them, so it never
    /// frees memory itself.
    pub fn reap(&self, mut ended: impl FnMut(VoiceId, bool)) {
        while let Some((id, _source, to_end)) = self.retired.pop() {
            ended(id, to_end);
        }
    }

    pub fn next_id(&mut self) -> VoiceId {
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.next_id
    }

    /// False when the queue was full and the command lost.
    fn send(&self, c: Command) -> bool {
        let sent = self.queue.push(c).is_ok();
        if !sent {
            self.stats.lost_commands.fetch_add(1, Ordering::Relaxed);
        }
        sent
    }

    pub fn play(&self, p: Play) {
        self.send(Command::Play(p));
    }

    pub fn stop(&self, id: VoiceId) {
        self.send(Command::Stop(id));
    }

    pub fn fade_out(&self, id: VoiceId, ms: u32) {
        self.send(Command::FadeOut(id, ms));
    }

    /// `SND_StopSounds(SND_STOP_ALL)`: ends every voice at once and puts the room effect, the channel volumes and
    /// the EQs back to their defaults.
    pub fn stop_all(&self) {
        self.send(Command::StopAll);
    }

    /// `SND_FadeAllSounds`: scales everything by `volume` from now, reached over `fade_ms`.
    pub fn fade_all(&self, volume: f32, fade_ms: u32) {
        self.send(Command::Fade {
            goal: volume.max(0.0),
            ms: fade_ms,
        });
    }

    /// Stops every voice attached to `entity`.
    pub fn stop_entity(&self, entity: u32) {
        self.send(Command::StopEntity(entity));
    }

    pub fn set_position(&self, id: VoiceId, pos: [f32; 3]) {
        self.send(Command::SetPosition(id, pos));
    }

    pub fn set_volume(&self, id: VoiceId, volume: f32) {
        self.send(Command::SetVolume(id, volume));
    }

    /// Fades the reverb bus to `wet` in `room` (an index of [`crate::reverb::ROOMS`]) over `fade_ms`.
    pub fn set_reverb(&self, room: u8, wet: f32, fade_ms: u32) {
        self.send(Command::Reverb {
            room,
            wet: wet.clamp(0.0, 1.0),
            fade_ms,
        });
    }

    /// Sets (`Some`) or clears (`None`) one band of an entity channel's EQ.
    pub fn set_eq(&self, channel: u8, eq: u8, band: u8, set: Option<Band>) {
        self.send(Command::Eq {
            channel,
            eq,
            band,
            set,
        });
    }

    /// The master volume, `snd_volume`: 0 to 1, scaling everything that plays.
    pub fn set_master_volume(&self, volume: f32) -> bool {
        self.send(Command::Master(volume))
    }

    /// `SND_SetChannelVolumes`: group `priority` (1 hold breath, 2 pain, 3 shell shock) takes `goals` (a volume per
    /// entity channel) over `fade_ms`.
    pub fn set_channel_volumes(&self, priority: u8, goals: [f32; MAX_CHANNELS], fade_ms: u32) {
        self.send(Command::ChannelVolumes {
            priority,
            goals,
            fade_ms,
        });
    }

    /// `SND_DeactivateChannelVolumes`.
    pub fn deactivate_channel_volumes(&self, priority: u8, fade_ms: u32) {
        self.send(Command::DeactivateChannelVolumes { priority, fade_ms });
    }

    pub fn set_listener(&self, l: Listener) {
        self.send(Command::Listener(l));
    }
}

/// What the mixer needs to know of a channel.
#[derive(Clone, Copy)]
struct ChannelInfo {
    priority: u8,
    is_3d: bool,
    restricted: bool,
    max_voices: u8,
}

/// Which of the three pools a voice takes a slot in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pool {
    Flat = 0,
    Positioned = 1,
    Streamed = 2,
}

struct Voice {
    id: VoiceId,
    source: Source,
    pool: Pool,
    channel: u8,
    entity: u32,
    volume: f32,
    /// Source frames per output frame.
    step: f64,
    /// Fraction of the way from `a` to `b`.
    frac: f64,
    a: [f32; 2],
    b: [f32; 2],
    /// Loaded: index of the next frame to read.
    next: usize,
    looping: bool,
    delay: u32,
    fade: f32,
    fade_to: f32,
    fade_step: f32,
    /// Fading out: the voice is freed when the fade reaches zero.
    dying: bool,
    /// The source ran out (as opposed to the voice being stopped).
    ended: bool,
    emitter: Option<Emitter>,
    duck: Duck,
    speaker: [[f32; 2]; 2],
    wet: bool,
    eq: EqState,
    gain: [[f32; 2]; 2],
    /// Source frames still to read before the next output frame (2 at the start, then 1 per advance).
    need: u8,
    fresh: bool,
    /// Voices started earlier lose ties.
    age: u64,
}

impl Voice {
    fn new(p: Play, out_rate: u32, pool: Pool, age: u64) -> Self {
        let src_rate = p.source.rate().max(1);
        let mut v = Voice {
            id: p.id,
            source: p.source,
            pool,
            channel: p.channel,
            entity: p.entity,
            volume: p.volume.max(0.0),
            step: f64::from(src_rate) * f64::from(p.pitch.max(0.01)) / f64::from(out_rate),
            frac: 0.0,
            a: [0.0; 2],
            b: [0.0; 2],
            next: 0,
            looping: p.looping,
            delay: (u64::from(p.delay_ms) * u64::from(out_rate) / 1000) as u32,
            fade: 1.0,
            fade_to: 1.0,
            fade_step: 0.0,
            dying: false,
            ended: false,
            emitter: p.emitter,
            duck: p.duck,
            speaker: p.speaker,
            wet: p.wet,
            eq: [[[[0.0; 2]; 2]; BANDS]; EQS],
            gain: [[0.0; 2]; 2],
            need: 2,
            fresh: true,
            age,
        };
        if let Source::Loaded(pcm) = &v.source {
            v.next = ((pcm.frames() as f32) * p.start.clamp(0.0, 1.0)) as usize;
        }
        if p.fade_in_ms > 0 {
            v.fade = 0.0;
            v.fade_step = 1000.0 / (p.fade_in_ms as f32 * out_rate as f32);
        }
        v
    }

    /// The next source frame; `None` at the end of a sound that does not loop, `Some(None)` when a stream is
    /// not ready.
    fn read(&mut self) -> Read {
        match &mut self.source {
            Source::Loaded(p) => {
                let ch = usize::from(p.channels.max(1));
                let frames = p.frames();
                if self.next >= frames {
                    if !self.looping || frames == 0 {
                        return Read::End;
                    }
                    self.next = 0;
                }
                let s = &p.samples[self.next * ch..];
                let f = |i: usize| f32::from(s[i.min(ch - 1)]) / 32768.0;
                self.next += 1;
                Read::Frame([f(0), if ch > 1 { f(1) } else { 0.0 }])
            }
            Source::Stream(st) => {
                let ch = usize::from(st.channels.max(1)).min(2);
                let mut buf = [0.0f32; 2];
                if st.ring.pop_exact(&mut buf[..ch]) {
                    Read::Frame(buf)
                } else if st.ring.is_finished() {
                    Read::End
                } else {
                    Read::Starved
                }
            }
        }
    }
}

enum Read {
    Frame([f32; 2]),
    End,
    Starved,
}

pub struct Mixer {
    queue: Arc<ArrayQueue<Command>>,
    retired: Arc<ArrayQueue<Retired>>,
    stats: Arc<Stats>,
    rate: u32,
    channels: [ChannelInfo; MAX_CHANNELS],
    voices: [Option<Voice>; MAX_VOICES],
    listener: Listener,
    /// The gain of everything: `snd_volume` times [`MASTER`].
    gain: f32,
    /// `soundfade`: a second master volume, 1 unless a script fades the sound.
    fade: Fader,
    /// 0 when no master plays, 1 when one does; slaves follow it over [`SLAVE_FADE_MS`].
    slave_lerp: f32,
    age: u64,
    reverb: Reverb,
    wet: f32,
    wet_goal: f32,
    wet_step: f32,
    eq: [ChannelEq; MAX_CHANNELS],
    chan_vol: ChannelVolumes,
    /// The mono sum of the sounds that feed the reverb, one chunk.
    send: Vec<f32>,
}

/// The gain of everything for a `snd_volume`; the dvar's range is 0 to 1.
fn master_gain(volume: f32) -> f32 {
    if volume.is_nan() {
        0.0
    } else {
        volume.clamp(0.0, 1.0) * MASTER
    }
}

/// A volume that moves to a goal at a constant rate.
struct Fader {
    volume: f32,
    goal: f32,
    /// Change per millisecond.
    rate: f32,
}

impl Fader {
    fn new() -> Self {
        Self {
            volume: 1.0,
            goal: 1.0,
            rate: 0.0,
        }
    }

    fn to(&mut self, goal: f32, ms: u32) {
        self.goal = goal;
        if ms == 0 {
            self.volume = goal;
            self.rate = 0.0;
        } else {
            self.rate = (goal - self.volume) / ms as f32;
        }
    }

    fn advance(&mut self, dt_ms: f32) {
        if self.rate == 0.0 {
            return;
        }
        self.volume += self.rate * dt_ms;
        if (self.rate > 0.0 && self.volume >= self.goal)
            || (self.rate < 0.0 && self.volume <= self.goal)
        {
            self.volume = self.goal;
            self.rate = 0.0;
        }
    }
}

/// A mixer and the handle that drives it.
pub fn mixer(rate: u32, channels: &[ChannelDef]) -> (Handle, Mixer) {
    let queue = Arc::new(ArrayQueue::new(QUEUE));
    let retired = Arc::new(ArrayQueue::new(MAX_VOICES * 2));
    let stats = Arc::new(Stats::default());
    let mut table = [ChannelInfo {
        priority: 0,
        is_3d: false,
        restricted: false,
        max_voices: MAX_CHANNELS as u8,
    }; MAX_CHANNELS];
    for (t, c) in table.iter_mut().zip(channels) {
        *t = ChannelInfo {
            priority: c.priority,
            is_3d: c.is_3d,
            restricted: c.restricted,
            max_voices: c.max_voices,
        };
    }
    (
        Handle {
            queue: queue.clone(),
            retired: retired.clone(),
            next_id: 0,
            stats: stats.clone(),
        },
        Mixer {
            queue,
            retired,
            stats,
            rate,
            channels: table,
            voices: [const { None }; MAX_VOICES],
            listener: Listener::from_yaw([0.0; 3], 0.0),
            gain: master_gain(DEFAULT_VOLUME),
            fade: Fader::new(),
            slave_lerp: 0.0,
            age: 0,
            reverb: Reverb::new(rate),
            wet: 0.0,
            wet_goal: 0.0,
            wet_step: 0.0,
            eq: [[[None; BANDS]; EQS]; MAX_CHANNELS],
            chan_vol: ChannelVolumes::new(),
            send: vec![0.0; CHUNK],
        },
    )
}

impl Mixer {
    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    /// Mixes `out.len() / 2` stereo frames into `out` (overwriting it).
    pub fn fill(&mut self, out: &mut [f32]) {
        for chunk in out.chunks_mut(CHUNK * 2) {
            self.fill_chunk(chunk);
        }
    }

    fn fill_chunk(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        let frames = out.len() / 2;
        if frames == 0 {
            return;
        }
        self.send[..frames].fill(0.0);
        while let Some(c) = self.queue.pop() {
            self.apply(c);
        }
        let dt_ms = frames as f32 * 1000.0 / self.rate as f32;
        let master_playing = self
            .voices
            .iter()
            .flatten()
            .any(|v| v.duck == Duck::Master && !v.dying && v.delay == 0);
        let target = if master_playing { 1.0 } else { 0.0 };
        let max_move = dt_ms / SLAVE_FADE_MS;
        self.slave_lerp += (target - self.slave_lerp).clamp(-max_move, max_move);

        let (listener, lerp) = (self.listener, self.slave_lerp);
        let mut finished = 0;
        let mut underruns = 0;
        self.chan_vol.advance(dt_ms);
        self.fade.advance(dt_ms);
        for slot in &mut self.voices {
            let Some(v) = slot else { continue };
            let ch = usize::from(v.channel).min(MAX_CHANNELS - 1);
            let eq = &self.eq[ch];
            let cv = self.chan_vol.groups[self.chan_vol.current][ch].volume;
            let (done, starved) =
                mix_voice(v, out, &mut self.send[..frames], eq, &listener, lerp, cv);
            underruns += starved;
            if done {
                if let Some(v) = slot.take() {
                    let _ = self.retired.push((v.id, v.source, v.ended));
                }
                finished += 1;
            }
        }
        if self.wet > 0.0 || self.wet_goal > 0.0 {
            for f in 0..frames {
                if self.wet != self.wet_goal {
                    self.wet = if self.wet_goal > self.wet {
                        (self.wet + self.wet_step).min(self.wet_goal)
                    } else {
                        (self.wet - self.wet_step).max(self.wet_goal)
                    };
                }
                let t = self.reverb.run(self.send[f]);
                out[2 * f] += t[0] * self.wet;
                out[2 * f + 1] += t[1] * self.wet;
            }
        }
        let mut peak = 0.0f32;
        for s in out.iter_mut() {
            *s = (*s * self.gain * self.fade.volume).clamp(-1.0, 1.0);
            peak = peak.max(s.abs());
        }
        let st = &self.stats;
        st.finished.fetch_add(finished, Ordering::Relaxed);
        st.underruns.fetch_add(underruns, Ordering::Relaxed);
        st.active.store(
            self.voices.iter().flatten().filter(|v| !v.dying).count() as u32,
            Ordering::Relaxed,
        );
        if peak > st.peak() {
            st.peak.store(peak.to_bits(), Ordering::Relaxed);
        }
    }

    fn apply(&mut self, c: Command) {
        match c {
            Command::Play(p) => self.start(p),
            Command::Stop(id) => self.fade_voice(id, STOP_FADE_MS),
            Command::FadeOut(id, ms) => self.fade_voice(id, ms as f32),
            Command::StopEntity(e) => {
                for v in self.voices.iter_mut().flatten().filter(|v| v.entity == e) {
                    fade_out(v, STOP_FADE_MS, self.rate);
                }
            }
            Command::SetPosition(id, pos) => {
                if let Some(e) = self.find(id).and_then(|v| v.emitter.as_mut()) {
                    e.pos = pos;
                }
            }
            Command::SetVolume(id, vol) => {
                if let Some(v) = self.find(id) {
                    v.volume = vol.max(0.0);
                }
            }
            Command::Master(v) => self.gain = master_gain(v),
            Command::Fade { goal, ms } => self.fade.to(goal, ms),
            Command::StopAll => self.stop_all(),
            Command::ChannelVolumes {
                priority,
                goals,
                fade_ms,
            } => {
                self.chan_vol.set(usize::from(priority), &goals, fade_ms);
                self.stats
                    .channel_priority
                    .store(self.chan_vol.current as u32, Ordering::Relaxed);
            }
            Command::DeactivateChannelVolumes { priority, fade_ms } => {
                self.chan_vol.deactivate(usize::from(priority), fade_ms);
                self.stats
                    .channel_priority
                    .store(self.chan_vol.current as u32, Ordering::Relaxed);
            }
            Command::Listener(l) => self.listener = l,
            Command::Reverb { room, wet, fade_ms } => {
                if room != self.reverb.room() {
                    self.reverb.set_room(room);
                }
                let frames = (u64::from(fade_ms.max(1)) * u64::from(self.rate) / 1000).max(1);
                self.wet_goal = wet;
                self.wet_step = (wet - self.wet).abs() / frames as f32;
            }
            Command::Eq {
                channel,
                eq,
                band,
                set,
            } => {
                if let Some(slot) = self
                    .eq
                    .get_mut(usize::from(channel))
                    .and_then(|c| c.get_mut(usize::from(eq)))
                    .and_then(|e| e.get_mut(usize::from(band)))
                {
                    *slot = set.map(|b| Coeffs::design(&b, self.rate));
                }
            }
        }
    }

    /// `SND_StopSounds(SND_STOP_ALL)`.
    fn stop_all(&mut self) {
        for slot in &mut self.voices {
            if let Some(v) = slot.take() {
                let _ = self.retired.push((v.id, v.source, false));
            }
        }
        self.slave_lerp = 0.0;
        self.wet = 0.0;
        self.wet_goal = 0.0;
        self.chan_vol = ChannelVolumes::new();
        self.stats.channel_priority.store(0, Ordering::Relaxed);
        self.eq = [[[None; BANDS]; EQS]; MAX_CHANNELS];
        self.stats.active.store(0, Ordering::Relaxed);
    }

    fn find(&mut self, id: VoiceId) -> Option<&mut Voice> {
        self.voices.iter_mut().flatten().find(|v| v.id == id)
    }

    fn fade_voice(&mut self, id: VoiceId, ms: f32) {
        let rate = self.rate;
        if let Some(v) = self.find(id) {
            fade_out(v, ms, rate);
        }
    }

    fn start(&mut self, p: Play) {
        let info = self.channels[usize::from(p.channel).min(MAX_CHANNELS - 1)];
        let pool = match (&p.source, info.is_3d) {
            (Source::Stream(_), _) => Pool::Streamed,
            (_, true) => Pool::Positioned,
            _ => Pool::Flat,
        };
        // One sound per entity on a restricted channel: the new one replaces the old.
        if info.restricted && p.entity != NO_ENTITY {
            let rate = self.rate;
            for v in self.voices.iter_mut().flatten() {
                if v.entity == p.entity && v.channel == p.channel && !v.dying {
                    fade_out(v, STOP_FADE_MS, rate);
                }
            }
        }
        let st = &self.stats;
        let on_channel = self
            .voices
            .iter()
            .flatten()
            .filter(|v| v.channel == p.channel && !v.dying)
            .count();
        if on_channel >= usize::from(info.max_voices) {
            st.refused.fetch_add(1, Ordering::Relaxed);
            // Handed back so the engine forgets the voice it was told of.
            let _ = self.retired.push((p.id, p.source, false));
            return;
        }
        let in_pool = |m: &Mixer| {
            m.voices
                .iter()
                .flatten()
                .filter(|v| v.pool == pool && !v.dying)
                .count()
        };
        let free = self.voices.iter().position(Option::is_none);
        let slot = if in_pool(self) < POOL_SIZE[pool as usize]
            && let Some(f) = free
        {
            f
        } else if let Some(victim) =
            self.victim(pool, info.priority, p.emitter.map(|e| e.pos), p.volume)
        {
            if let Some(v) = self.voices[victim].take() {
                let _ = self.retired.push((v.id, v.source, false));
            }
            self.stats.replaced.fetch_add(1, Ordering::Relaxed);
            victim
        } else {
            self.stats.refused.fetch_add(1, Ordering::Relaxed);
            let _ = self.retired.push((p.id, p.source, false));
            return;
        };
        self.age += 1;
        self.voices[slot] = Some(Voice::new(p, self.rate, pool, self.age));
        self.stats.started.fetch_add(1, Ordering::Relaxed);
    }

    /// The voice of `pool` a sound of `priority` may replace: the lowest priority not above it; among equals
    /// the farthest from the listener (positioned) or the quietest (flat), then the oldest.
    fn victim(&self, pool: Pool, priority: u8, at: Option<[f32; 3]>, volume: f32) -> Option<usize> {
        let l = self.listener.pos;
        let metric = |v: &Voice| match &v.emitter {
            Some(e) => dist2(e.pos, l),
            None => -v.volume,
        };
        let mut best: Option<(usize, u8, f32, u64)> = None;
        for (i, v) in self.voices.iter().enumerate() {
            let Some(v) = v else { continue };
            if v.pool != pool {
                continue;
            }
            let p = self.channels[usize::from(v.channel).min(MAX_CHANNELS - 1)].priority;
            if p > priority {
                continue;
            }
            let m = metric(v);
            let better = best.is_none_or(|(_, bp, bm, ba)| {
                p < bp || (p == bp && (m > bm || (m == bm && v.age < ba)))
            });
            if better {
                best = Some((i, p, m, v.age));
            }
        }
        let (i, p, m, _) = best?;
        // Equal priority: only replace a sound that matters less than the new one.
        if p == priority {
            let new = at.map_or(-volume, |at| dist2(at, l));
            if new >= m {
                return None;
            }
        }
        Some(i)
    }
}

fn fade_out(v: &mut Voice, ms: f32, rate: u32) {
    v.dying = true;
    v.fade_to = 0.0;
    let frames = (ms.max(0.0) * rate as f32 / 1000.0).max(1.0);
    v.fade_step = v.fade / frames;
}

fn dist2(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum()
}

/// Reads source frames until the interpolation pair is complete.
fn fetch(v: &mut Voice) -> Read {
    while v.need > 0 {
        match v.read() {
            Read::Frame(x) => {
                if v.need == 2 {
                    v.a = x;
                } else {
                    v.b = x;
                }
                v.need -= 1;
            }
            other => return other,
        }
    }
    Read::Frame(v.b)
}

/// Mixes one voice into `out`; returns whether it is finished and how many frames starved.
fn mix_voice(
    v: &mut Voice,
    out: &mut [f32],
    send: &mut [f32],
    eq: &ChannelEq,
    l: &Listener,
    slave_lerp: f32,
    chan_volume: f32,
) -> (bool, u64) {
    let filtered = eq.iter().flatten().any(Option::is_some);
    let frames = out.len() / 2;
    let src_ch = v.source.channels().clamp(1, 2);
    let mut level = v.volume * chan_volume;
    if let Duck::Slave(pct) = v.duck {
        level *= 1.0 - (1.0 - pct) * slave_lerp;
    }
    // gain[o][c]: source channel c into output o (left, right).
    let mut target = [[0.0f32; 2]; 2];
    match &v.emitter {
        Some(e) => {
            let rel = [
                e.pos[0] - l.pos[0],
                e.pos[1] - l.pos[1],
                e.pos[2] - l.pos[2],
            ];
            let d = dist2(e.pos, l.pos).sqrt();
            level *= e.curve.attenuate(d, e.min, e.max);
            // Pan by the sine of the angle to the right of the listener; a sound on top of the listener is central.
            let pan = if d > 1.0 {
                -(rel[0] * l.left[0] + rel[1] * l.left[1] + rel[2] * l.left[2]) / d
            } else {
                0.0
            };
            let angle = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
            let per = level / src_ch as f32;
            for c in 0..src_ch {
                target[0][c] = per * angle.cos();
                target[1][c] = per * angle.sin();
            }
        }
        None => {
            for (o, t) in target.iter_mut().enumerate() {
                for c in 0..src_ch {
                    t[c] = level * v.speaker[c][o];
                }
            }
        }
    }
    if std::mem::take(&mut v.fresh) {
        v.gain = target;
    }
    let n = frames as f32;
    let mut ramp = [[0.0f32; 2]; 2];
    for o in 0..2 {
        for c in 0..2 {
            ramp[o][c] = (target[o][c] - v.gain[o][c]) / n;
        }
    }
    let (mut starved, mut finished) = (0, false);
    for f in 0..frames {
        let g = v.gain;
        for o in 0..2 {
            for c in 0..2 {
                v.gain[o][c] += ramp[o][c];
            }
        }
        if v.delay > 0 {
            v.delay -= 1;
            continue;
        }
        if v.fade != v.fade_to {
            v.fade = if v.fade_to > v.fade {
                (v.fade + v.fade_step).min(v.fade_to)
            } else {
                (v.fade - v.fade_step).max(v.fade_to)
            };
            if v.dying && v.fade <= 0.0 {
                finished = true;
                break;
            }
        }
        match fetch(v) {
            Read::Frame(_) => {}
            Read::End => {
                v.ended = true;
                finished = true;
                break;
            }
            Read::Starved => {
                starved += 1;
                continue;
            }
        }
        let t = v.frac as f32;
        let mut s = [
            v.a[0] + (v.b[0] - v.a[0]) * t,
            v.a[1] + (v.b[1] - v.a[1]) * t,
        ];
        if filtered {
            for (e, bands) in eq.iter().enumerate() {
                for (b, co) in bands.iter().enumerate() {
                    if let Some(co) = co {
                        for (c, x) in s.iter_mut().enumerate().take(src_ch) {
                            *x = co.run(&mut v.eq[e][b][c], *x);
                        }
                    }
                }
            }
        }
        let mut sum = 0.0;
        for o in 0..2 {
            let y = v.fade * (s[0] * g[o][0] + s[1] * g[o][1]);
            out[2 * f + o] += y;
            sum += y;
        }
        if v.wet {
            send[f] += sum * 0.5;
        }
        v.frac += v.step;
        while v.frac >= 1.0 {
            v.frac -= 1.0;
            v.a = v.b;
            v.need = 1;
            if !matches!(fetch(v), Read::Frame(_)) {
                break;
            }
        }
    }
    v.gain = target;
    (finished, starved)
}

/// How many channel volume groups there are: the base (always on) and priorities 1 to 3 (hold breath, pain,
/// shell shock).
pub const CHANNEL_GROUPS: usize = 4;

/// One channel's volume of a group, easing to its goal.
#[derive(Clone, Copy)]
struct ChanVol {
    volume: f32,
    goal: f32,
    /// Change per millisecond.
    rate: f32,
}

/// `snd_channelvolgroup`s: the highest-priority active group sets the volume of every entity channel.
struct ChannelVolumes {
    groups: [[ChanVol; MAX_CHANNELS]; CHANNEL_GROUPS],
    active: [bool; CHANNEL_GROUPS],
    current: usize,
}

impl ChannelVolumes {
    fn new() -> Self {
        let one = ChanVol {
            volume: 1.0,
            goal: 1.0,
            rate: 0.0,
        };
        let mut active = [false; CHANNEL_GROUPS];
        active[0] = true;
        Self {
            groups: [[one; MAX_CHANNELS]; CHANNEL_GROUPS],
            active,
            current: 0,
        }
    }

    /// `SND_SetChannelVolumes`.
    fn set(&mut self, priority: usize, goals: &[f32; MAX_CHANNELS], fade_ms: u32) {
        if !(1..CHANNEL_GROUPS).contains(&priority) {
            return;
        }
        self.active[priority] = true;
        let fade = fade_ms.max(1) as f32;
        let from = self.groups[self.current];
        for ((c, goal), from) in self.groups[priority].iter_mut().zip(goals).zip(from) {
            let goal = goal.clamp(0.0, 1.0);
            c.goal = goal;
            c.volume = from.volume;
            c.rate = (goal - from.volume) / fade;
        }
        if priority != self.current && !self.active[priority + 1..].iter().any(|a| *a) {
            self.current = priority;
        }
    }

    /// `SND_DeactivateChannelVolumes`: falls back to the next lower active group, fading from where this one was.
    fn deactivate(&mut self, priority: usize, fade_ms: u32) {
        if !(1..CHANNEL_GROUPS).contains(&priority) {
            return;
        }
        self.active[priority] = false;
        if priority != self.current {
            return;
        }
        let fade = fade_ms.max(1) as f32;
        let from = self.groups[priority];
        self.current = (0..priority).rev().find(|i| self.active[*i]).unwrap_or(0);
        for (c, from) in self.groups[self.current].iter_mut().zip(from) {
            c.volume = from.volume;
            c.rate = (c.goal - from.volume) / fade;
        }
    }

    /// `SND_UpdateVolume` for every channel of the current group.
    fn advance(&mut self, dt_ms: f32) {
        for c in &mut self.groups[self.current] {
            if c.rate == 0.0 {
                continue;
            }
            c.volume += dt_ms * c.rate;
            if (c.rate > 0.0 && c.volume > c.goal) || (c.rate < 0.0 && c.volume < c.goal) {
                c.volume = c.goal;
                c.rate = 0.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring::ring;

    const RATE: u32 = 48_000;

    fn chan(name: &str, priority: u8, is_3d: bool, restricted: bool, max: u8) -> ChannelDef {
        ChannelDef {
            name: name.into(),
            priority,
            is_3d,
            restricted,
            pausable: true,
            max_voices: max,
        }
    }

    fn table() -> Vec<ChannelDef> {
        vec![
            chan("flat", 1, false, false, 64),
            chan("world", 1, true, false, 64),
            chan("hi", 5, true, false, 64),
            chan("capped", 1, true, false, 2),
            chan("one", 1, true, true, 64),
        ]
    }

    /// Half-scale DC lasting `ms`.
    fn dc(ms: u32) -> Arc<Pcm> {
        Arc::new(Pcm {
            rate: RATE,
            channels: 1,
            samples: vec![16384; (RATE * ms / 1000) as usize].into(),
        })
    }

    fn at(pos: [f32; 3], min: f32, max: f32) -> Option<Emitter> {
        Some(Emitter {
            pos,
            min,
            max,
            curve: Curve::LINEAR,
        })
    }

    /// Fills enough blocks to settle gain ramps and returns the last block's mean level per side.
    fn level(m: &mut Mixer) -> [f32; 2] {
        let mut out = vec![0.0; 960];
        for _ in 0..4 {
            m.fill(&mut out);
        }
        let n = (out.len() / 2) as f32;
        [
            out.iter().step_by(2).sum::<f32>() / n,
            out.iter().skip(1).step_by(2).sum::<f32>() / n,
        ]
    }

    fn world_play(h: &mut Handle, channel: u8, pos: [f32; 3]) -> VoiceId {
        let id = h.next_id();
        let mut p = Play::new(id, Source::Loaded(dc(2000)), channel);
        p.emitter = at(pos, 50.0, 1050.0);
        h.play(p);
        id
    }

    #[test]
    fn fading_all_sounds_scales_everything_and_a_timed_fade_takes_its_time() {
        let (mut h, mut m) = mixer(RATE, &table());
        world_play(&mut h, 1, [400.0, 0.0, 0.0]);
        let open = level(&mut m)[0];
        h.fade_all(0.25, 0);
        let quiet = level(&mut m)[0];
        assert!(
            (quiet - 0.25 * open).abs() < 0.01 * open,
            "{open} -> {quiet}"
        );
        // Back to full over 100 ms: a quarter of the way in, it is still short of full.
        h.fade_all(1.0, 400);
        let mut out = vec![0.0; 960];
        m.fill(&mut out);
        let early = out.iter().step_by(2).sum::<f32>() / (out.len() / 2) as f32;
        assert!(
            early > quiet && early < 0.8 * open,
            "{quiet} {early} {open}"
        );
        for _ in 0..40 {
            m.fill(&mut out);
        }
        let full = level(&mut m)[0];
        assert!((full - open).abs() < 0.01 * open, "{open} -> {full}");
    }

    #[test]
    fn a_channel_volume_group_ducks_its_channels_and_clearing_it_restores_them() {
        let (mut h, mut m) = mixer(RATE, &table());
        world_play(&mut h, 1, [400.0, 0.0, 0.0]);
        let open = level(&mut m)[0];
        // Priority 3 quiets channel 1 only; the group in force is 3 until it is cleared.
        let mut goals = [1.0; MAX_CHANNELS];
        goals[1] = 0.1;
        h.set_channel_volumes(3, goals, 0);
        let ducked = level(&mut m)[0];
        assert_eq!(h.stats.channel_priority.load(Ordering::Relaxed), 3);
        assert!(
            (ducked - 0.1 * open).abs() < 0.01 * open,
            "{open} -> {ducked}"
        );
        // A lower priority does not take over from the active higher one.
        h.set_channel_volumes(1, [1.0; MAX_CHANNELS], 0);
        assert_eq!(level(&mut m)[0] / ducked, 1.0);
        assert_eq!(h.stats.channel_priority.load(Ordering::Relaxed), 3);
        h.deactivate_channel_volumes(3, 0);
        let back = level(&mut m)[0];
        assert_eq!(h.stats.channel_priority.load(Ordering::Relaxed), 1);
        assert!((back - open).abs() < 0.01 * open, "{open} -> {back}");
        h.deactivate_channel_volumes(1, 0);
        level(&mut m);
        assert_eq!(h.stats.channel_priority.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_channel_volume_fade_takes_its_time() {
        let (mut h, mut m) = mixer(RATE, &table());
        world_play(&mut h, 1, [400.0, 0.0, 0.0]);
        let open = level(&mut m)[0];
        let mut goals = [1.0; MAX_CHANNELS];
        goals[1] = 0.0;
        h.set_channel_volumes(3, goals, 2000);
        // 4 chunks of 480 frames at 48 kHz is 40 ms of a 2 s fade.
        let part = level(&mut m)[0];
        assert!(part > 0.9 * open && part < open, "{open} -> {part}");
    }

    #[test]
    fn a_sound_on_the_left_is_louder_in_the_left_ear() {
        let (mut h, mut m) = mixer(RATE, &table());
        // Facing +x, the listener's left is +y.
        world_play(&mut h, 1, [0.0, 400.0, 0.0]);
        let [l, r] = level(&mut m);
        assert!(l > 4.0 * r, "left {l} right {r}");
        let (mut h, mut m) = mixer(RATE, &table());
        world_play(&mut h, 1, [0.0, -400.0, 0.0]);
        let [l, r] = level(&mut m);
        assert!(r > 4.0 * l, "left {l} right {r}");
        let (mut h, mut m) = mixer(RATE, &table());
        world_play(&mut h, 1, [400.0, 0.0, 0.0]);
        let [l, r] = level(&mut m);
        assert!((l - r).abs() < 1e-4, "ahead is central: {l} {r}");
    }

    #[test]
    fn turning_the_listener_moves_the_sound_across() {
        let (mut h, mut m) = mixer(RATE, &table());
        world_play(&mut h, 1, [0.0, 400.0, 0.0]);
        // Facing +y puts +y ahead; facing -x puts +y on the right.
        h.set_listener(Listener::from_yaw([0.0; 3], std::f32::consts::PI));
        let [l, r] = level(&mut m);
        assert!(r > 4.0 * l, "left {l} right {r}");
    }

    #[test]
    fn volume_follows_the_falloff_curve() {
        let gain = |d: f32| {
            let (mut h, mut m) = mixer(RATE, &table());
            world_play(&mut h, 1, [d, 0.0, 0.0]);
            let [l, r] = level(&mut m);
            (l * l + r * r).sqrt()
        };
        let near = gain(10.0);
        let mid = gain(550.0);
        let far = gain(1050.0);
        assert!((mid / near - 0.5).abs() < 0.01, "mid {mid} near {near}");
        assert_eq!(far, 0.0);
    }

    #[test]
    fn snd_volume_scales_the_output_and_defaults_to_stock_gain() {
        assert!((master_gain(DEFAULT_VOLUME) - 0.6).abs() < 1e-6);
        let level_at = |volume: Option<f32>| {
            let (mut h, mut m) = mixer(RATE, &table());
            if let Some(v) = volume {
                h.set_master_volume(v);
            }
            let id = h.next_id();
            let mut p = Play::new(id, Source::Loaded(dc(500)), 0);
            p.speaker = [[1.0, 1.0], [0.0; 2]];
            h.play(p);
            level(&mut m)[0]
        };
        let default = level_at(None);
        assert!((default - 0.5 * 0.6).abs() < 1e-3, "{default}");
        assert!((level_at(Some(0.4)) * 2.0 - level_at(Some(0.8))).abs() < 1e-3);
        assert_eq!(level_at(Some(0.0)), 0.0);
        // Out of range values are the dvar's limits.
        assert_eq!(level_at(Some(7.0)), level_at(Some(1.0)));
        assert_eq!(level_at(Some(-1.0)), 0.0);
    }

    #[test]
    fn a_flat_mono_sound_goes_through_its_speaker_map() {
        let (mut h, mut m) = mixer(RATE, &table());
        let id = h.next_id();
        let mut p = Play::new(id, Source::Loaded(dc(500)), 0);
        p.speaker = [[0.5, 0.25], [0.0; 2]];
        h.play(p);
        let [l, r] = level(&mut m);
        assert!(
            (l - 0.5 * 0.5 * master_gain(DEFAULT_VOLUME)).abs() < 1e-3,
            "{l}"
        );
        assert!(
            (r - 0.5 * 0.25 * master_gain(DEFAULT_VOLUME)).abs() < 1e-3,
            "{r}"
        );
    }

    #[test]
    fn a_sound_ends_and_a_loop_does_not() {
        let (mut h, mut m) = mixer(RATE, &table());
        let (a, b) = (h.next_id(), h.next_id());
        h.play(Play::new(a, Source::Loaded(dc(20)), 0));
        let mut looped = Play::new(b, Source::Loaded(dc(20)), 0);
        looped.looping = true;
        h.play(looped);
        let mut out = vec![0.0; 960];
        for _ in 0..20 {
            m.fill(&mut out);
        }
        assert_eq!(h.stats.finished.load(Ordering::Relaxed), 1);
        assert_eq!(h.stats.active.load(Ordering::Relaxed), 1);
        assert!(out.iter().any(|s| *s != 0.0));
    }

    #[test]
    fn a_channel_never_exceeds_its_voice_cap() {
        let (mut h, mut m) = mixer(RATE, &table());
        for _ in 0..5 {
            world_play(&mut h, 3, [10.0, 0.0, 0.0]);
        }
        let mut out = vec![0.0; 96];
        m.fill(&mut out);
        assert_eq!(h.stats.active.load(Ordering::Relaxed), 2);
        assert_eq!(h.stats.refused.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn a_full_pool_replaces_the_farthest_voice_only_for_a_higher_or_nearer_sound() {
        let (mut h, mut m) = mixer(RATE, &table());
        for i in 0..32 {
            world_play(&mut h, 1, [100.0 + 10.0 * i as f32, 0.0, 0.0]);
        }
        let mut out = vec![0.0; 96];
        m.fill(&mut out);
        assert_eq!(h.stats.active.load(Ordering::Relaxed), 32);
        // Farther than all and of equal priority: refused.
        world_play(&mut h, 1, [5000.0, 0.0, 0.0]);
        m.fill(&mut out);
        assert_eq!(h.stats.refused.load(Ordering::Relaxed), 1);
        // Nearer: replaces the farthest.
        world_play(&mut h, 1, [20.0, 0.0, 0.0]);
        m.fill(&mut out);
        assert_eq!(h.stats.replaced.load(Ordering::Relaxed), 1);
        // Higher priority replaces regardless of distance.
        world_play(&mut h, 2, [5000.0, 0.0, 0.0]);
        m.fill(&mut out);
        assert_eq!(h.stats.replaced.load(Ordering::Relaxed), 2);
        assert_eq!(h.stats.active.load(Ordering::Relaxed), 32);
    }

    #[test]
    fn a_restricted_channel_keeps_one_sound_per_entity() {
        let (mut h, mut m) = mixer(RATE, &table());
        for _ in 0..3 {
            let id = h.next_id();
            let mut p = Play::new(id, Source::Loaded(dc(2000)), 4);
            p.entity = 7;
            p.emitter = at([10.0, 0.0, 0.0], 50.0, 500.0);
            h.play(p);
        }
        let mut out = vec![0.0; 960];
        m.fill(&mut out);
        m.fill(&mut out);
        assert_eq!(h.stats.active.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_master_ducks_its_slaves() {
        let (mut h, mut m) = mixer(RATE, &table());
        let id = h.next_id();
        let mut slave = Play::new(id, Source::Loaded(dc(5000)), 0);
        slave.duck = Duck::Slave(0.25);
        slave.speaker = [[1.0, 1.0], [0.0; 2]];
        h.play(slave);
        let before = level(&mut m)[0];
        let id = h.next_id();
        let mut master = Play::new(id, Source::Loaded(dc(5000)), 0);
        master.duck = Duck::Master;
        master.volume = 0.0;
        h.play(master);
        let mut out = vec![0.0; 4800];
        for _ in 0..40 {
            m.fill(&mut out);
        }
        let after = out[0];
        assert!((after / before - 0.25).abs() < 0.02, "{after} / {before}");
    }

    #[test]
    fn a_stream_plays_what_the_ring_holds_and_ends_with_it() {
        let (mut h, mut m) = mixer(RATE, &table());
        let (mut tx, rx) = ring(8192);
        assert_eq!(tx.push(&vec![0.5; 2000]), 2000);
        tx.finish();
        let id = h.next_id();
        let mut p = Play::new(
            id,
            Source::Stream(Stream {
                ring: rx,
                rate: RATE,
                channels: 2,
            }),
            0,
        );
        p.speaker = [[1.0, 0.0], [0.0, 1.0]];
        h.play(p);
        let mut out = vec![0.0; 960];
        m.fill(&mut out);
        assert!((out[10] - 0.5 * master_gain(DEFAULT_VOLUME)).abs() < 1e-3);
        m.fill(&mut out);
        m.fill(&mut out);
        assert_eq!(h.stats.finished.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_stream_with_no_data_yet_is_silent_and_counted_not_ended() {
        let (mut h, mut m) = mixer(RATE, &table());
        let (mut tx, rx) = ring(8192);
        let id = h.next_id();
        h.play(Play::new(
            id,
            Source::Stream(Stream {
                ring: rx,
                rate: RATE,
                channels: 1,
            }),
            0,
        ));
        let mut out = vec![0.0; 960];
        m.fill(&mut out);
        assert!(out.iter().all(|s| *s == 0.0));
        assert_eq!(h.stats.finished.load(Ordering::Relaxed), 0);
        assert!(h.stats.underruns.load(Ordering::Relaxed) > 0);
        tx.push(&vec![0.5; 4000]);
        m.fill(&mut out);
        assert!(out.iter().any(|s| *s != 0.0));
    }

    #[test]
    fn a_start_delay_holds_the_sound_back() {
        let (mut h, mut m) = mixer(RATE, &table());
        let id = h.next_id();
        let mut p = Play::new(id, Source::Loaded(dc(1000)), 0);
        p.delay_ms = 10;
        h.play(p);
        let mut out = vec![0.0; 2000];
        m.fill(&mut out);
        // 10 ms is 480 frames: silent up to there, sounding after.
        assert!(out[..2 * 470].iter().all(|s| *s == 0.0));
        assert!(out[2 * 500] != 0.0);
    }

    #[test]
    fn higher_pitch_plays_faster() {
        let (mut h, mut m) = mixer(RATE, &table());
        let id = h.next_id();
        let mut p = Play::new(id, Source::Loaded(dc(100)), 0);
        p.pitch = 2.0;
        h.play(p);
        let mut out = vec![0.0; 960];
        for _ in 0..6 {
            m.fill(&mut out);
        }
        assert_eq!(
            h.stats.finished.load(Ordering::Relaxed),
            1,
            "100 ms ends within 60 ms at double speed"
        );
    }

    fn energy(m: &mut Mixer, frames: usize) -> f32 {
        let mut out = vec![0.0; frames * 2];
        m.fill(&mut out);
        out.iter().map(|s| s * s).sum()
    }

    fn click(h: &mut Handle, wet: bool) {
        let id = h.next_id();
        let mut p = Play::new(id, Source::Loaded(dc(5)), 0);
        p.wet = wet;
        h.play(p);
    }

    #[test]
    fn reverb_rings_on_after_the_sound_and_no_wet_sounds_stay_dry() {
        let tail = |wet: bool, reverb: bool| {
            let (mut h, mut m) = mixer(RATE, &table());
            if reverb {
                h.set_reverb(crate::reverb::room_index("hangar").unwrap(), 1.0, 1);
            }
            click(&mut h, wet);
            let _ = energy(&mut m, 4800);
            energy(&mut m, 24_000)
        };
        let dry = tail(true, false);
        assert!(dry < 1e-6, "a dry mix is silent after the sound: {dry}");
        assert!(tail(true, true) > 1e-3, "no reverb tail");
        assert!(
            tail(false, true) < 1e-6,
            "a no-wet sound reached the reverb"
        );
    }

    #[test]
    fn the_reverb_fades_in_over_its_time() {
        let (mut h, mut m) = mixer(RATE, &table());
        h.set_reverb(1, 1.0, 1000);
        let id = h.next_id();
        let mut p = Play::new(id, Source::Loaded(dc(2000)), 0);
        p.looping = true;
        h.play(p);
        let early = energy(&mut m, 2400);
        let _ = energy(&mut m, 60_000);
        let late = energy(&mut m, 2400);
        assert!(late > early, "early {early} late {late}");
    }

    #[test]
    fn an_eq_filters_only_its_channel_and_clearing_it_restores_the_sound() {
        let band = Band {
            kind: crate::eq::EqType::LowPass,
            gain_db: 0.0,
            freq: 200.0,
            q: 0.707,
        };
        // A 6 kHz tone on channels 0 and 1.
        let tone = || {
            Arc::new(Pcm {
                rate: RATE,
                channels: 1,
                samples: (0..RATE as usize)
                    .map(|i| {
                        ((i as f32 * 6000.0 * std::f32::consts::TAU / RATE as f32).sin() * 16384.0)
                            as i16
                    })
                    .collect::<Vec<_>>()
                    .into(),
            })
        };
        let (mut h, mut m) = mixer(RATE, &table());
        h.set_eq(0, 0, 0, Some(band));
        for ch in [0u8, 1] {
            let id = h.next_id();
            let mut p = Play::new(id, Source::Loaded(tone()), ch);
            p.volume = 0.5;
            h.play(p);
        }
        let _ = energy(&mut m, 2400);
        // Both voices are summed; compare against channel 1 alone.
        let both = energy(&mut m, 4800);
        let (mut h2, mut m2) = mixer(RATE, &table());
        let id = h2.next_id();
        let mut p = Play::new(id, Source::Loaded(tone()), 1);
        p.volume = 0.5;
        h2.play(p);
        let _ = energy(&mut m2, 2400);
        let only_unfiltered = energy(&mut m2, 4800);
        assert!(
            both < only_unfiltered * 1.1,
            "the filtered voice still sounds: {both} vs {only_unfiltered}"
        );
        assert!(
            both > only_unfiltered * 0.9,
            "the other channel was filtered too"
        );
    }
}
