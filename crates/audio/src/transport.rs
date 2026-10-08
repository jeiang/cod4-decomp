// SPDX-License-Identifier: GPL-3.0-or-later
//! The protocol between the mixer and the browser's AudioWorklet, as plain Rust so it is tested natively.
//!
//! The worklet (`device/worklet.js`) owns a ring of interleaved stereo `f32` frames, `CAPACITY` frames long
//! (a power of two), and four 32-bit control words: `[write, read, underruns, 0]`. `write` and `read` count
//! frames since the start and wrap at 2^32, which the capacity divides, so `write - read` is always the fill
//! and `index & mask` the slot. The mixer is the only writer of `write`; the worklet's `process()` is the only
//! writer of `read` and `underruns`. With cross-origin isolation both sides share one `SharedArrayBuffer` and
//! the words are `Atomics`; without it the main thread posts `Float32Array` chunks that the worklet copies into
//! its own ring through the same `write` word, and posts `read`/`underruns` back. `process()` consumes up to
//! one 128-frame quantum per call; when fewer frames are queued it plays what there is, zeroes the rest and
//! counts one underrun. Nothing counts before the first frame arrives.

use std::ops::Range;

/// Frames the worklet pulls per `process()` call.
pub const QUANTUM: u32 = 128;

/// Worklet ring size in frames: about 340 ms at 48 kHz, far above [`TARGET_MS`].
pub const CAPACITY: u32 = 1 << 14;

/// How much audio the producer keeps queued ahead of the worklet. 80 ms rides out a main-thread stall of a
/// few frames (the producer is a 10 ms timer on the game's thread) for 80 ms added latency.
pub const TARGET_MS: u32 = 80;

/// What the debug overlay shows about the output.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutputStats {
    /// Frames the worklet has consumed since it started.
    pub frames_played: u64,
    /// `process()` calls that found too few frames queued.
    pub underruns: u32,
    /// Frames mixed ahead of the worklet right now.
    pub buffered_frames: u32,
    /// Frames the mixer has produced.
    pub frames_rendered: u64,
    /// Largest sample the worklet has output.
    pub peak: f32,
    /// Sign changes of the left channel the worklet has output (a tone's frequency is `crossings / 2 / s`).
    pub crossings: u32,
    /// The ring is a `SharedArrayBuffer` (`false`: chunks are posted).
    pub shared: bool,
    /// The worklet node is attached and pulling.
    pub running: bool,
    /// Why there is no sound, when there is none.
    pub error: Option<String>,
}

/// The producer's view of the ring: how much to render now and where it goes.
#[derive(Clone, Debug)]
pub struct Pacer {
    mask: u32,
    target: u32,
    written: u32,
    last_read: u32,
    played: u64,
    rendered: u64,
}

impl Pacer {
    /// `capacity` frames (a power of two), keeping `target` frames queued.
    pub fn new(capacity: u32, target: u32) -> Self {
        assert!(capacity.is_power_of_two() && target <= capacity);
        Self {
            mask: capacity - 1,
            target,
            written: 0,
            last_read: 0,
            played: 0,
            rendered: 0,
        }
    }

    /// Takes the worklet's `read` word. Call at least once per 2^31 frames of playback.
    pub fn observe(&mut self, read: u32) {
        self.played += u64::from(read.wrapping_sub(self.last_read));
        self.last_read = read;
    }

    /// Frames the worklet has not consumed yet.
    pub fn buffered(&self) -> u32 {
        self.written.wrapping_sub(self.last_read)
    }

    /// Frames to render now to bring the queue back to its target.
    pub fn wanted(&self) -> u32 {
        self.target.saturating_sub(self.buffered())
    }

    /// Ring frame ranges the next `n` frames occupy: the second is the wrapped remainder, usually empty.
    pub fn spans(&self, n: u32) -> (Range<usize>, Range<usize>) {
        debug_assert!(n <= self.mask + 1 - self.buffered());
        let at = (self.written & self.mask) as usize;
        let n = n as usize;
        let cap = self.mask as usize + 1;
        let first = n.min(cap - at);
        (at..at + first, 0..n - first)
    }

    /// Records that `n` frames were written; returns the new `write` word to publish.
    pub fn commit(&mut self, n: u32) -> u32 {
        self.written = self.written.wrapping_add(n);
        self.rendered += u64::from(n);
        self.written
    }

    pub fn played(&self) -> u64 {
        self.played
    }

    pub fn rendered(&self) -> u64 {
        self.rendered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `device/worklet.js` `process()` in Rust: the consumer half of the protocol.
    struct Consumer {
        ring: Vec<f32>,
        write: u32,
        read: u32,
        underruns: u32,
        started: bool,
    }

    impl Consumer {
        fn new(capacity: u32) -> Self {
            Self {
                ring: vec![0.0; capacity as usize * 2],
                write: 0,
                read: 0,
                underruns: 0,
                started: false,
            }
        }

        fn process(&mut self, out: &mut [f32]) {
            let n = out.len() / 2;
            if !self.started && self.write == 0 {
                out.fill(0.0);
                return;
            }
            self.started = true;
            let mask = self.ring.len() as u32 / 2 - 1;
            let take = (self.write.wrapping_sub(self.read) as usize).min(n);
            for f in 0..take {
                let slot = (self.read.wrapping_add(f as u32) & mask) as usize;
                out[2 * f..2 * f + 2].copy_from_slice(&self.ring[2 * slot..2 * slot + 2]);
            }
            out[2 * take..].fill(0.0);
            if take < n {
                self.underruns += 1;
            }
            self.read = self.read.wrapping_add(take as u32);
        }
    }

    /// The producer half: renders `wanted` frames of a ramp into the ring along the pacer's spans.
    fn produce(p: &mut Pacer, c: &mut Consumer, next: &mut f32) {
        let n = p.wanted();
        let (a, b) = p.spans(n);
        for slot in a.chain(b) {
            c.ring[2 * slot] = *next;
            c.ring[2 * slot + 1] = -*next;
            *next += 1.0;
        }
        c.write = p.commit(n);
    }

    #[test]
    fn frames_arrive_in_order_across_wraparound_and_u32_overflow() {
        let mut p = Pacer::new(1024, 300);
        let mut c = Consumer::new(1024);
        // Start near the u32 limit so both the ring and the counters wrap.
        let start = u32::MAX - 1000;
        p.written = start;
        p.last_read = start;
        c.write = start;
        c.read = start;
        let mut next = 0.0;
        let mut expect = 0.0;
        let mut out = [0.0f32; 2 * QUANTUM as usize];
        for _ in 0..200 {
            produce(&mut p, &mut c, &mut next);
            c.process(&mut out);
            p.observe(c.read);
            for f in out.as_chunks::<2>().0 {
                assert_eq!(*f, [expect, -expect]);
                expect += 1.0;
            }
        }
        assert_eq!(c.underruns, 0);
        assert_eq!(p.played(), 200 * u64::from(QUANTUM));
        assert!(p.written < start, "the counter wrapped");
    }

    #[test]
    fn queue_settles_at_the_target() {
        let mut p = Pacer::new(CAPACITY, 3840);
        let mut c = Consumer::new(CAPACITY);
        let mut next = 0.0;
        let mut out = [0.0f32; 2 * QUANTUM as usize];
        produce(&mut p, &mut c, &mut next);
        assert_eq!(p.buffered(), 3840);
        for _ in 0..10 {
            c.process(&mut out);
            p.observe(c.read);
            assert_eq!(p.wanted(), QUANTUM);
            produce(&mut p, &mut c, &mut next);
            assert_eq!(p.buffered(), 3840);
        }
        assert_eq!(p.rendered(), 3840 + 10 * u64::from(QUANTUM));
    }

    #[test]
    fn a_starved_ring_counts_one_underrun_per_quantum_and_plays_the_rest() {
        let mut c = Consumer::new(1024);
        let mut out = [1.0f32; 2 * QUANTUM as usize];
        // Before the first frame arrives nothing is an underrun.
        c.process(&mut out);
        assert_eq!(c.underruns, 0);
        assert!(out.iter().all(|&s| s == 0.0));
        c.ring[..6].copy_from_slice(&[1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
        c.write = 3;
        out.fill(9.0);
        c.process(&mut out);
        assert_eq!(c.underruns, 1);
        assert_eq!(&out[..8], [1.0, 1.0, 2.0, 2.0, 3.0, 3.0, 0.0, 0.0]);
        c.process(&mut out);
        c.process(&mut out);
        assert_eq!(c.underruns, 3);
        assert_eq!(c.read, 3);
    }

    #[test]
    fn spans_split_at_the_end_of_the_ring() {
        let mut p = Pacer::new(16, 16);
        p.commit(12);
        p.observe(12);
        assert_eq!(p.spans(3), (12..15, 0..0));
        assert_eq!(p.spans(4), (12..16, 0..0));
        assert_eq!(p.spans(10), (12..16, 0..6));
    }

    #[test]
    fn observe_extends_the_read_word_past_u32() {
        let mut p = Pacer::new(16, 8);
        p.last_read = u32::MAX - 2;
        p.written = u32::MAX - 2;
        p.observe(5);
        assert_eq!(p.played(), 8);
        p.observe(6);
        assert_eq!(p.played(), 9);
    }
}
