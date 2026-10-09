// SPDX-License-Identifier: GPL-3.0-only
//! Voice chat on the client: [`Talker`] turns the microphone into frames while the talk key is held, [`Hearing`]
//! turns the frames the server relays into sound. The codec and the wire are [`net::voice`]; the server decides who
//! hears whom (`sv_voice`, teams, `mute`).

use audio::capture::Capture;
use net::voice::{self, Encoder, FRAME_SAMPLES, Frame, Voice};
use std::collections::HashMap;
use std::time::Duration;
use web_time::Instant;

/// How long after the talk key goes up the microphone stays open (so a second press needs no reopening).
const MIC_LINGER: Duration = Duration::from_secs(2);
/// Wait before trying a microphone that failed to open again.
const RETRY: Duration = Duration::from_secs(5);

/// The player's side of voice chat.
#[derive(Default)]
pub struct Talker {
    mic: Option<Capture>,
    enc: Encoder,
    last_down: Option<Instant>,
    failed_at: Option<Instant>,
    /// Why the microphone could not be used, for the console (set once per failure).
    pub note: Option<String>,
    /// Frames sent so far.
    pub sent: u64,
}

impl Talker {
    /// Takes what the microphone heard since the last call. While `down` (and `enabled`), that is returned as coded
    /// frames to send; otherwise it is thrown away so nothing stale is sent when the key goes down.
    pub fn frames(&mut self, down: bool, enabled: bool, now: Instant) -> Vec<Frame> {
        let talking = down && enabled;
        if talking {
            self.last_down = Some(now);
            if self.mic.is_none() && self.failed_at.is_none_or(|t| now - t > RETRY) {
                match Capture::open() {
                    Ok(m) => {
                        self.mic = Some(m);
                        self.failed_at = None;
                    }
                    Err(e) => {
                        self.failed_at = Some(now);
                        self.note = Some(format!("voice chat: {e}"));
                    }
                }
            }
        }
        let Some(mic) = self.mic.as_mut() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut pcm = [0f32; FRAME_SAMPLES];
        while mic.waiting() >= FRAME_SAMPLES && mic.read(&mut pcm) {
            if talking {
                let mut ints = [0i16; FRAME_SAMPLES];
                for (i, s) in ints.iter_mut().zip(&pcm) {
                    *i = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                }
                out.push(self.enc.encode(&ints));
            }
        }
        self.sent += out.len() as u64;
        if self.last_down.is_none_or(|t| now - t > MIC_LINGER) {
            self.mic = None;
        }
        out
    }
}

/// Frames of audio gathered before a speaker's first sound plays, so the network's jitter does not break it up.
const PREBUFFER_FRAMES: usize = 4;
/// A speaker silent this long has finished an utterance: the next one prebuffers again.
const UTTERANCE_GAP: Duration = Duration::from_millis(400);
/// A speaker silent this long gives up their voice in the mixer.
const SPEAKER_IDLE: Duration = Duration::from_secs(4);
/// Most speakers heard at once: a server naming more is not given more mixer voices.
const MAX_SPEAKERS: usize = 16;
/// Most frames of lost audio replaced by silence (a longer gap is a new burst).
const MAX_FILL: u16 = 5;

struct Speaker {
    pipe: audio::engine::Pipe,
    /// Audio waiting for the prebuffer to fill.
    held: Vec<f32>,
    playing: bool,
    last: Instant,
    next_seq: u16,
}

/// The voices of other players, one mixer voice each.
#[derive(Default)]
pub struct Hearing {
    speakers: HashMap<u8, Speaker>,
}

impl Hearing {
    /// Plays a frame from `v.speaker` through `open`, which makes a pipe when the speaker has none.
    /// `gain` scales the audio (0 mutes).
    pub fn feed(
        &mut self,
        v: &Voice,
        gain: f32,
        now: Instant,
        open: impl FnOnce() -> Option<audio::engine::Pipe>,
    ) {
        let Some(pcm) = voice::decode(&v.frame) else {
            return;
        };
        let full = self.speakers.len() >= MAX_SPEAKERS;
        let sp = match self.speakers.entry(v.speaker) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(_) if full => {
                return;
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                let Some(pipe) = open() else { return };
                e.insert(Speaker {
                    pipe,
                    held: Vec::new(),
                    playing: false,
                    last: now,
                    next_seq: v.seq,
                })
            }
        };
        if now - sp.last > UTTERANCE_GAP {
            sp.playing = false;
            sp.held.clear();
        }
        sp.last = now;
        let gap = v.seq.wrapping_sub(sp.next_seq);
        sp.next_seq = v.seq.wrapping_add(1);
        let mut samples: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 2);
        if (1..=MAX_FILL).contains(&gap) && sp.playing {
            samples.resize(usize::from(gap) * FRAME_SAMPLES, 0.0);
        }
        samples.extend(pcm.iter().map(|s| f32::from(*s) / 32768.0 * gain));
        if sp.playing {
            sp.pipe.push(&samples);
        } else {
            sp.held.extend(samples);
            if sp.held.len() >= PREBUFFER_FRAMES * FRAME_SAMPLES {
                sp.pipe.push(&sp.held);
                sp.held.clear();
                sp.playing = true;
            }
        }
    }

    /// Closes the voices of speakers who have gone quiet; `close` takes each pipe.
    pub fn tick(&mut self, now: Instant, mut close: impl FnMut(audio::engine::Pipe)) {
        let gone: Vec<u8> = self
            .speakers
            .iter()
            .filter(|(_, s)| now - s.last > SPEAKER_IDLE)
            .map(|(k, _)| *k)
            .collect();
        for k in gone {
            if let Some(s) = self.speakers.remove(&k) {
                close(s.pipe);
            }
        }
    }

    /// Ends every voice; `close` takes each pipe.
    pub fn close_all(&mut self, mut close: impl FnMut(audio::engine::Pipe)) {
        for (_, s) in self.speakers.drain() {
            close(s.pipe);
        }
    }
}
