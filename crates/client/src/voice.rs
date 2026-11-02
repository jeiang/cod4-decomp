// SPDX-License-Identifier: GPL-3.0-only
//! Voice chat on the client: [`Talker`] turns the microphone into frames while the talk key is held, [`Hearing`]
//! turns the frames the server relays into sound. The codec and the wire are [`net::voice`]; the server decides who
//! hears whom (`sv_voice`, teams, `mute`).

use audio::capture::Capture;
use net::voice::{self, Encoder, FRAME_SAMPLES, Frame, Voice};
use std::collections::HashMap;
use std::time::Duration;
use web_time::Instant;

/// How long after the microphone was last wanted (talk key or record meter) it stays open, so a second press needs no
/// reopening.
const MIC_LINGER: Duration = Duration::from_secs(2);
/// Wait before trying a microphone that failed to open again.
const RETRY: Duration = Duration::from_secs(5);

/// The player's side of voice chat.
#[derive(Default)]
pub struct Talker {
    mic: Option<Capture>,
    enc: Encoder,
    last_wanted: Option<Instant>,
    failed_at: Option<Instant>,
    /// Why the microphone could not be used, for the console (set once per failure).
    pub note: Option<String>,
    /// Frames sent so far.
    pub sent: u64,
    /// `voice_current_voicelevel`: the mean sample size (16-bit units) of the last block the microphone gave, 0 while
    /// it is closed (`Voice_GetVoiceLevel` reads 0 before voice is up).
    pub current_level: f32,
    /// Frames read from the microphone so far, sent or thrown away.
    pub heard: u64,
}

impl Talker {
    /// Whether the microphone is open now.
    pub fn mic_open(&self) -> bool {
        self.mic.is_some()
    }

    /// Takes what the microphone heard since the last call. The microphone is open while `talking` (the talk key held
    /// with voice on) or while the record meter is shown (`metering`): stock records whenever its UI runs
    /// (`CL_VoiceFrame`, `Voice_GetLocalVoiceData`), which only the meter can see, so this opens it only then. Each
    /// block is scaled by `scaler` (`winvoice_mic_scaler`, clamped to 0.5..1.5 as `Voice_GetLocalVoiceData` does) and
    /// measured for the meter before the talk key is looked at (`Record_QueueAudioDataForEncoding`); while `talking`
    /// it is also returned as coded frames to send.
    pub fn frames(
        &mut self,
        talking: bool,
        metering: bool,
        scaler: f32,
        now: Instant,
    ) -> Vec<Frame> {
        if talking || metering {
            self.last_wanted = Some(now);
            if self.mic.is_none() && self.failed_at.is_none_or(|t| now - t > RETRY) {
                match Capture::open() {
                    Ok(m) => {
                        self.mic = Some(m);
                        self.failed_at = None;
                    }
                    Err(e) => {
                        self.failed_at = Some(now);
                        tracing::warn!(error = %e, "microphone cannot be opened (no capture on the test hosts: #488)");
                        self.note = Some(format!("voice chat: {e}"));
                    }
                }
            }
        }
        // The browser reports a refused permission after the open call returned.
        if let Some(e) = self.mic.as_mut().and_then(Capture::failure) {
            self.mic = None;
            self.failed_at = Some(now);
            tracing::warn!(error = %e, "microphone failed");
            self.note = Some(format!("voice chat: {e}"));
        }
        let Some(mic) = self.mic.as_mut() else {
            self.current_level = 0.0;
            return Vec::new();
        };
        let scale = 32767.0 * scaler.clamp(0.5, 1.5);
        let mut out = Vec::new();
        let mut pcm = [0f32; FRAME_SAMPLES];
        while mic.waiting() >= FRAME_SAMPLES && mic.read(&mut pcm) {
            let mut ints = [0i16; FRAME_SAMPLES];
            for (i, s) in ints.iter_mut().zip(&pcm) {
                *i = (s * scale).clamp(-32768.0, 32767.0) as i16;
            }
            self.current_level = mean_size(&ints);
            self.heard += 1;
            if talking {
                out.push(self.enc.encode(&ints));
            }
        }
        self.sent += out.len() as u64;
        if self.last_wanted.is_none_or(|t| now - t > MIC_LINGER) {
            self.mic = None;
        }
        out
    }
}

/// The mean sample size of a block, in 16-bit units (`Record_QueueAudioDataForEncoding`).
fn mean_size(block: &[i16]) -> f32 {
    if block.is_empty() {
        return 0.0;
    }
    block.iter().map(|s| f32::from(*s).abs()).sum::<f32>() / block.len() as f32
}

/// Updates the record meter at each draw (`Voice_GetVoiceLevel`): the current level over 32767 goes into the next of
/// six slots, each a sixth, and the meter is their sum.
pub fn meter_level(slots: &mut [f32; 6], count: &mut usize, current: f32) -> f32 {
    slots[*count % 6] = current / 32767.0 / 6.0;
    *count = count.wrapping_add(1);
    slots.iter().sum()
}

/// Frames of audio gathered before a speaker's first sound plays, so the network's jitter does not break it up.
const PREBUFFER_FRAMES: usize = 4;
/// A speaker silent this long has finished an utterance: the next one prebuffers again.
const UTTERANCE_GAP: Duration = Duration::from_millis(400);
/// A speaker counts as talking for this long after a packet arrives (`Voice_IsClientTalking`, strictly less).
const TALK_TIME: Duration = Duration::from_millis(300);
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
    /// When each speaker's last voice arrived (`s_clientTalkTime`), set before decoding as stock does: the talker
    /// icon does not depend on the audio playing.
    talk_time: HashMap<u8, Instant>,
}

impl Hearing {
    /// The speakers whose voice arrived within [`TALK_TIME`] (`Voice_IsClientTalking`).
    pub fn talking(&self, now: Instant) -> impl Iterator<Item = u16> + '_ {
        self.talk_time
            .iter()
            .filter_map(move |(&client, &t)| (now - t < TALK_TIME).then_some(u16::from(client)))
    }

    /// Plays a frame from `v.speaker` through `open`, which makes a pipe when the speaker has none.
    /// `gain` scales the audio (0 mutes).
    pub fn feed(
        &mut self,
        v: &Voice,
        gain: f32,
        now: Instant,
        open: impl FnOnce() -> Option<audio::engine::Pipe>,
    ) {
        self.talk_time.insert(v.speaker, now);
        let Some(pcm) = voice::decode(&v.frame) else {
            logging::dedup!(
                WARN,
                "decode",
                speaker = v.speaker,
                "undecodable voice frame dropped"
            );
            return;
        };
        let full = self.speakers.len() >= MAX_SPEAKERS;
        let sp = match self.speakers.entry(v.speaker) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(_) if full => {
                logging::dedup!(
                    WARN,
                    "speakers",
                    speaker = v.speaker,
                    "too many speakers; voice dropped"
                );
                return;
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                let Some(pipe) = open() else {
                    logging::dedup!(
                        WARN,
                        "pipe",
                        speaker = v.speaker,
                        "no mixer voice for a speaker"
                    );
                    return;
                };
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
        self.talk_time.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_undecodable_voice_frame_is_dropped_with_a_warning() {
        let v = Voice {
            speaker: 3,
            seq: 0,
            frame: [0xff; net::voice::FRAME_BYTES],
        };
        let mut hearing = Hearing::default();
        let (_, events) = logging::capture(|| {
            hearing.feed(&v, 1.0, Instant::now(), || {
                panic!("no pipe for a bad frame")
            });
        });
        let w = events
            .iter()
            .find(|e| e.level == tracing::Level::WARN)
            .expect("a warning");
        assert_eq!(w.field("speaker"), Some("3"));
    }

    #[test]
    fn a_speaker_talks_for_less_than_300_ms_after_a_packet_even_when_it_cannot_play() {
        let v = Voice {
            speaker: 5,
            seq: 0,
            frame: [0xff; net::voice::FRAME_BYTES],
        };
        let mut hearing = Hearing::default();
        let t0 = Instant::now();
        let _ = logging::capture(|| hearing.feed(&v, 1.0, t0, || None));
        let at = |ms| {
            hearing
                .talking(t0 + Duration::from_millis(ms))
                .collect::<Vec<_>>()
        };
        assert_eq!(at(299), vec![5]);
        assert!(at(300).is_empty());
    }

    #[test]
    fn the_record_meter_averages_six_draws_of_the_mean_sample_size() {
        assert_eq!(mean_size(&[100, -300, 0, 200]), 150.0);
        let (mut slots, mut count) = ([0.0; 6], 0);
        let full = 32767.0;
        assert!((meter_level(&mut slots, &mut count, full) - 1.0 / 6.0).abs() < 1e-6);
        for _ in 0..5 {
            meter_level(&mut slots, &mut count, full);
        }
        assert!((meter_level(&mut slots, &mut count, full) - 1.0).abs() < 1e-5);
        assert!((meter_level(&mut slots, &mut count, 0.0) - 5.0 / 6.0).abs() < 1e-5);
    }
}
