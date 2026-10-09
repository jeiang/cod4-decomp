// SPDX-License-Identifier: GPL-3.0-only
//! Voice chat on the wire. Wire compatibility with the original (speex over its own voice packets) is not a goal, so
//! this is a native design: 8 kHz mono audio in 20 ms frames, each coded as IMA ADPCM (4 bits a sample) behind a small
//! header that carries the predictor and step index, so every frame decodes on its own and a lost packet costs only its
//! 20 ms. A frame is [`FRAME_BYTES`] bytes (about 33 kbit/s).
//!
//! Frames travel unreliably inside the netchan messages ([`crate::session`]): a client sends its own, the server stamps
//! each with the speaker's slot and forwards it to the players who may hear it.

/// Samples a second.
pub const RATE: u32 = 8000;
/// Samples in one frame (20 ms).
pub const FRAME_SAMPLES: usize = 160;
/// Bytes of one coded frame: predictor (`i16`), step index (`u8`), then two samples a byte.
pub const FRAME_BYTES: usize = 3 + FRAME_SAMPLES / 2;
/// Frames kept waiting to be sent in either direction; the oldest are dropped past this (a talker is better served by
/// fresh audio than by a backlog).
pub const MAX_QUEUED: usize = 12;
/// Frames one packet carries.
pub const MAX_PER_PACKET: usize = 3;

/// One coded frame.
pub type Frame = [u8; FRAME_BYTES];

const STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
const INDEX_ADJUST: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// Where the ADPCM state is: what the decoder will hold when it reaches the next sample.
#[derive(Clone, Copy, Default)]
struct State {
    pred: i32,
    index: i32,
}

impl State {
    fn step(&mut self, code: u8) -> i16 {
        let step = STEPS[self.index as usize];
        let mut diff = step >> 3;
        if code & 4 != 0 {
            diff += step;
        }
        if code & 2 != 0 {
            diff += step >> 1;
        }
        if code & 1 != 0 {
            diff += step >> 2;
        }
        self.pred = if code & 8 != 0 {
            self.pred - diff
        } else {
            self.pred + diff
        }
        .clamp(-32768, 32767);
        self.index = (self.index + INDEX_ADJUST[usize::from(code & 7)]).clamp(0, 88);
        self.pred as i16
    }

    fn code(&mut self, sample: i16) -> u8 {
        let step = STEPS[self.index as usize];
        let mut diff = i32::from(sample) - self.pred;
        let mut code = 0u8;
        if diff < 0 {
            code = 8;
            diff = -diff;
        }
        let mut s = step;
        if diff >= s {
            code |= 4;
            diff -= s;
        }
        s >>= 1;
        if diff >= s {
            code |= 2;
            diff -= s;
        }
        s >>= 1;
        if diff >= s {
            code |= 1;
        }
        // The decoder's own reconstruction keeps both ends in step.
        self.step(code);
        code
    }
}

/// Codes a talker's audio frame by frame.
#[derive(Default)]
pub struct Encoder(State);

impl Encoder {
    pub fn encode(&mut self, pcm: &[i16; FRAME_SAMPLES]) -> Frame {
        let mut out = [0u8; FRAME_BYTES];
        out[..2].copy_from_slice(&(self.0.pred as i16).to_le_bytes());
        out[2] = self.0.index as u8;
        for (i, pair) in pcm.as_chunks::<2>().0.iter().enumerate() {
            let lo = self.0.code(pair[0]);
            let hi = self.0.code(pair[1]);
            out[3 + i] = lo | hi << 4;
        }
        out
    }
}

/// Decodes one frame. `None` for bytes that are not a frame (wrong length, a step index out of range).
pub fn decode(bytes: &[u8]) -> Option<[i16; FRAME_SAMPLES]> {
    if bytes.len() != FRAME_BYTES || usize::from(bytes[2]) >= STEPS.len() {
        return None;
    }
    let mut st = State {
        pred: i32::from(i16::from_le_bytes([bytes[0], bytes[1]])),
        index: i32::from(bytes[2]),
    };
    let mut out = [0i16; FRAME_SAMPLES];
    for (i, b) in bytes[3..].iter().enumerate() {
        out[2 * i] = st.step(b & 15);
        out[2 * i + 1] = st.step(b >> 4);
    }
    Some(out)
}

/// A frame as the server forwards it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voice {
    /// The speaker's client slot (the server sets it; a client's own packets carry none).
    pub speaker: u8,
    /// Counts up for each frame the speaker sent, so a listener sees gaps.
    pub seq: u16,
    pub frame: Frame,
}

/// Mean absolute amplitude of a frame, 0 to 1: what a talk indicator shows.
pub fn level(pcm: &[i16]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    pcm.iter().map(|s| f32::from(s.unsigned_abs())).sum::<f32>() / (pcm.len() as f32 * 32768.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(phase: &mut f32) -> [i16; FRAME_SAMPLES] {
        let mut pcm = [0i16; FRAME_SAMPLES];
        for s in &mut pcm {
            *phase += 440.0 * std::f32::consts::TAU / RATE as f32;
            *s = (phase.sin() * 12_000.0) as i16;
        }
        pcm
    }

    #[test]
    fn a_tone_survives_the_codec_and_a_lost_frame_only_costs_itself() {
        let mut enc = Encoder::default();
        let mut phase = 0.0;
        let mut worst = Vec::new();
        for n in 0..6 {
            let pcm = tone(&mut phase);
            let frame = enc.encode(&pcm);
            if n == 2 {
                continue; // lost on the way
            }
            let back = decode(&frame).expect("a frame decodes");
            let err = pcm
                .iter()
                .zip(&back)
                .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
                .max()
                .unwrap();
            worst.push((n, err));
        }
        for (n, err) in worst {
            // Frame 0 starts from silence, so the step size still has to grow into the tone.
            if n > 0 {
                assert!(err < 3000, "frame {n} is off by {err}");
            }
        }
    }

    #[test]
    fn bytes_that_are_not_a_frame_do_not_decode() {
        assert!(decode(&[0; FRAME_BYTES - 1]).is_none());
        let mut bad = [0u8; FRAME_BYTES];
        bad[2] = 200;
        assert!(decode(&bad).is_none());
    }
}
