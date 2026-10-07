// SPDX-License-Identifier: GPL-3.0-or-later
//! Room reverb: the send bus behind `setReverb`. The original hands this to Miles' EAX reverb (`msseax.flt`,
//! closed source) with one of 26 room types; its wet level is the effect's and its dry level is fixed at 1
//! (`MSS_GetDryLevel`). Here a Freeverb-style bus (eight damped combs, four all-passes) is tuned per room by
//! decay time, damping and size, taken from the EAX preset of the same name [INFERENCE: the Miles tuning is
//! not documented, so these are the public EAX presets' decay times, not Miles' numbers].

/// `snd_roomStrings`, with the tuning: reverberation time in seconds, high-frequency damping 0..1 and a size
/// scale for the delay lines.
pub const ROOMS: [(&str, f32, f32, f32); 26] = [
    ("generic", 1.49, 0.5, 1.0),
    ("paddedcell", 0.17, 0.8, 0.3),
    ("room", 0.4, 0.6, 0.6),
    ("bathroom", 1.49, 0.3, 0.5),
    ("livingroom", 0.5, 0.8, 0.7),
    ("stoneroom", 2.31, 0.3, 0.9),
    ("auditorium", 4.32, 0.5, 1.6),
    ("concerthall", 3.92, 0.5, 1.8),
    ("cave", 2.91, 0.4, 1.6),
    ("arena", 7.24, 0.4, 2.0),
    ("hangar", 10.05, 0.4, 2.0),
    ("carpetedhallway", 0.3, 0.85, 0.7),
    ("hallway", 1.49, 0.5, 0.8),
    ("stonecorridor", 2.7, 0.35, 0.9),
    ("alley", 1.49, 0.5, 1.0),
    ("forest", 1.49, 0.7, 2.0),
    ("city", 1.49, 0.5, 1.8),
    ("mountains", 1.49, 0.6, 2.0),
    ("quarry", 1.49, 0.5, 1.4),
    ("plain", 1.49, 0.6, 2.0),
    ("parkinglot", 1.65, 0.4, 1.8),
    ("sewerpipe", 2.81, 0.3, 0.6),
    ("underwater", 1.49, 0.95, 0.8),
    ("drugged", 8.39, 0.5, 1.0),
    ("dizzy", 17.23, 0.5, 1.0),
    ("psychotic", 7.56, 0.5, 1.0),
];

/// The index of a room by name (case-insensitive).
pub fn room_index(name: &str) -> Option<u8> {
    ROOMS
        .iter()
        .position(|r| r.0.eq_ignore_ascii_case(name))
        .map(|i| i as u8)
}

const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASS: [usize; 4] = [556, 441, 341, 225];
/// Right channel offset, in samples at 44.1 kHz.
const SPREAD: usize = 23;
/// Largest room scale; sizes the delay buffers once.
const MAX_SCALE: f32 = 2.0;
const MAX_RATE_FACTOR: f32 = 2.2;

struct Comb {
    buf: Vec<f32>,
    len: usize,
    pos: usize,
    feedback: f32,
    damp: f32,
    store: f32,
}

impl Comb {
    fn new(cap: usize) -> Self {
        Self {
            buf: vec![0.0; cap.max(2)],
            len: cap.max(2),
            pos: 0,
            feedback: 0.0,
            damp: 0.0,
            store: 0.0,
        }
    }

    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let y = self.buf[self.pos];
        self.store = y * (1.0 - self.damp) + self.store * self.damp;
        self.buf[self.pos] = x + self.store * self.feedback;
        self.pos += 1;
        if self.pos >= self.len {
            self.pos = 0;
        }
        y
    }
}

struct AllPass {
    buf: Vec<f32>,
    len: usize,
    pos: usize,
}

impl AllPass {
    fn new(cap: usize) -> Self {
        Self {
            buf: vec![0.0; cap.max(2)],
            len: cap.max(2),
            pos: 0,
        }
    }

    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let b = self.buf[self.pos];
        let y = b - x;
        self.buf[self.pos] = x + b * 0.5;
        self.pos += 1;
        if self.pos >= self.len {
            self.pos = 0;
        }
        y
    }
}

/// A stereo reverb bus. Buffers are sized at creation for any room at `max_rate`; changing rooms and
/// processing allocate nothing.
pub struct Reverb {
    rate: f32,
    combs: [[Comb; 8]; 2],
    allpass: [[AllPass; 4]; 2],
    room: u8,
}

impl Reverb {
    pub fn new(rate: u32) -> Self {
        let r = rate as f32 / 44_100.0;
        let cap = |n: usize, extra: usize| {
            (((n + extra) as f32) * MAX_SCALE * r.max(1.0).min(MAX_RATE_FACTOR)).ceil() as usize + 2
        };
        let side = |extra: usize| {
            (
                std::array::from_fn(|i| Comb::new(cap(COMBS[i], extra))),
                std::array::from_fn(|i| AllPass::new(cap(ALLPASS[i], extra))),
            )
        };
        let (cl, al) = side(0);
        let (cr, ar) = side(SPREAD);
        let mut v = Self {
            rate: rate as f32,
            combs: [cl, cr],
            allpass: [al, ar],
            room: 0,
        };
        v.set_room(0);
        v
    }

    pub fn room(&self) -> u8 {
        self.room
    }

    /// Retunes to `room` (an index of [`ROOMS`]) and clears the tails.
    pub fn set_room(&mut self, room: u8) {
        let (_, rt60, damping, scale) = ROOMS[usize::from(room).min(ROOMS.len() - 1)];
        self.room = room.min(ROOMS.len() as u8 - 1);
        let r = self.rate / 44_100.0;
        for (side, extra) in [(0usize, 0usize), (1, SPREAD)] {
            for (i, c) in self.combs[side].iter_mut().enumerate() {
                let want = (((COMBS[i] + extra) as f32) * scale * r) as usize;
                c.len = want.clamp(2, c.buf.len());
                // Gain per trip so the tail falls 60 dB in `rt60` seconds.
                let secs = c.len as f32 / self.rate;
                c.feedback = 10f32.powf(-3.0 * secs / rt60.max(0.05)).min(0.985);
                c.damp = damping * 0.6;
                c.buf.fill(0.0);
                c.pos = 0;
                c.store = 0.0;
            }
            for (i, a) in self.allpass[side].iter_mut().enumerate() {
                let want = (((ALLPASS[i] + extra) as f32) * r) as usize;
                a.len = want.clamp(2, a.buf.len());
                a.buf.fill(0.0);
                a.pos = 0;
            }
        }
    }

    /// Feeds one mono frame of send and returns the stereo tail.
    #[inline]
    pub fn run(&mut self, x: f32) -> [f32; 2] {
        let x = x * 0.015;
        let mut out = [0.0; 2];
        for (side, o) in out.iter_mut().enumerate() {
            let mut sum = 0.0;
            for c in &mut self.combs[side] {
                sum += c.run(x);
            }
            for a in &mut self.allpass[side] {
                sum = a.run(sum);
            }
            *o = sum;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(room: &str, secs: f32) -> f32 {
        let mut r = Reverb::new(48_000);
        r.set_room(room_index(room).unwrap());
        let mut e = 0.0f32;
        let n = (secs * 48_000.0) as usize;
        for i in 0..n + 9600 {
            let t = r.run(if i == 0 { 1.0 } else { 0.0 });
            if i >= n {
                e += t[0] * t[0] + t[1] * t[1];
            }
        }
        e
    }

    #[test]
    fn every_room_name_resolves() {
        for (i, r) in ROOMS.iter().enumerate() {
            assert_eq!(room_index(r.0), Some(i as u8));
        }
        assert_eq!(room_index("PaddedCell"), Some(1));
        assert_eq!(room_index("nowhere"), None);
    }

    #[test]
    fn a_long_room_rings_on_where_a_short_one_has_died() {
        let (short, long) = (tail("paddedcell", 0.5), tail("hangar", 0.5));
        assert!(long > 100.0 * short, "short {short} long {long}");
    }
}
