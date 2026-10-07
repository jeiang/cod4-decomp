// SPDX-License-Identifier: GPL-3.0-or-later
//! A single-producer single-consumer ring of `f32` samples between a decoder (producer) and the mixer
//! (consumer). Fixed size, no locks and no allocation after [`ring`], so the consumer may run on a realtime
//! audio thread (or an AudioWorklet).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

struct Shared {
    buf: Box<[AtomicU32]>,
    mask: usize,
    /// Total samples ever popped / pushed (wrapping); `tail - head` is the fill level.
    head: AtomicUsize,
    tail: AtomicUsize,
    /// The producer has pushed everything it ever will.
    done: AtomicBool,
    /// The consumer is gone; the producer should stop.
    closed: AtomicBool,
}

pub struct Producer(Arc<Shared>);
pub struct Consumer(Arc<Shared>);

/// A ring of at least `capacity` samples (rounded up to a power of two).
pub fn ring(capacity: usize) -> (Producer, Consumer) {
    let cap = capacity.next_power_of_two();
    let s = Arc::new(Shared {
        buf: (0..cap).map(|_| AtomicU32::new(0)).collect(),
        mask: cap - 1,
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
        done: AtomicBool::new(false),
        closed: AtomicBool::new(false),
    });
    (Producer(s.clone()), Consumer(s))
}

impl Producer {
    /// Free space in samples.
    pub fn space(&self) -> usize {
        let s = &self.0;
        s.buf.len() - s.tail.load(Ordering::Relaxed).wrapping_sub(s.head.load(Ordering::Acquire))
    }

    /// Pushes as many of `samples` as fit; returns how many.
    pub fn push(&mut self, samples: &[f32]) -> usize {
        let s = &self.0;
        let n = samples.len().min(self.space());
        let tail = s.tail.load(Ordering::Relaxed);
        for (i, v) in samples[..n].iter().enumerate() {
            s.buf[tail.wrapping_add(i) & s.mask].store(v.to_bits(), Ordering::Relaxed);
        }
        s.tail.store(tail.wrapping_add(n), Ordering::Release);
        n
    }

    /// No more samples will follow.
    pub fn finish(&self) {
        self.0.done.store(true, Ordering::Release);
    }

    /// The consumer was dropped.
    pub fn is_closed(&self) -> bool {
        self.0.closed.load(Ordering::Acquire)
    }
}

impl Consumer {
    pub fn len(&self) -> usize {
        let s = &self.0;
        s.tail.load(Ordering::Acquire).wrapping_sub(s.head.load(Ordering::Relaxed))
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Pops `out.len()` samples at once, or none when fewer are queued.
    pub fn pop_exact(&mut self, out: &mut [f32]) -> bool {
        let s = &self.0;
        if self.len() < out.len() {
            return false;
        }
        let head = s.head.load(Ordering::Relaxed);
        for (i, v) in out.iter_mut().enumerate() {
            *v = f32::from_bits(s.buf[head.wrapping_add(i) & s.mask].load(Ordering::Relaxed));
        }
        s.head.store(head.wrapping_add(out.len()), Ordering::Release);
        true
    }

    /// Everything has been popped and the producer is finished.
    pub fn is_finished(&self) -> bool {
        self.0.done.load(Ordering::Acquire) && self.is_empty()
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        self.0.closed.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_come_out_in_order_across_wraparound() {
        let (mut p, mut c) = ring(4);
        let mut next = 0.0;
        let mut want = 0.0;
        let mut out = [0.0; 3];
        for _ in 0..50 {
            let chunk: Vec<f32> = (0..3).map(|i| next + i as f32).collect();
            let n = p.push(&chunk);
            next += n as f32;
            while c.pop_exact(&mut out) {
                for v in out {
                    assert_eq!(v, want);
                    want += 1.0;
                }
            }
        }
        assert!(want > 60.0);
    }

    #[test]
    fn a_full_ring_takes_no_more_and_a_short_one_pops_nothing() {
        let (mut p, mut c) = ring(4);
        assert_eq!(p.push(&[1.0; 6]), 4);
        assert_eq!(p.push(&[1.0]), 0);
        let mut two = [0.0; 2];
        assert!(c.pop_exact(&mut two));
        assert_eq!(p.space(), 2);
        let mut five = [0.0; 5];
        assert!(!c.pop_exact(&mut five));
        assert_eq!(c.len(), 2);
        p.finish();
        assert!(!c.is_finished());
        assert!(c.pop_exact(&mut two));
        assert!(c.is_finished());
    }
}
