// SPDX-License-Identifier: GPL-3.0-only
//! Sampling of [`XAnimParts`]: one animation at one normalised time, accumulated per bone.
//!
//! An animation names the bones it drives (`names`, script strings) and stores them grouped by
//! how each bone is encoded, in the order `bone_counts` gives:
//!
//! | group | contents                                                              |
//! |-------|-----------------------------------------------------------------------|
//! | 0     | rotation is identity                                                   |
//! | 1     | yaw-only rotation (`z`, `w`) as a keyframe track                       |
//! | 2     | full rotation as a keyframe track                                      |
//! | 3     | one constant yaw-only rotation                                         |
//! | 4     | one constant full rotation                                             |
//! | 5     | translation track, byte-quantised inside a per-bone box (mins, extent) |
//! | 6     | translation track, short-quantised inside a per-bone box               |
//! | 7     | one constant translation                                               |
//! | 8     | translation present but zero                                           |
//!
//! Groups 0-4 index `names` in order; groups 5-8 name their bone with one byte of `data_byte`
//! (an index into `names`), since a bone has both a rotation and a translation encoding. All
//! per-bone payloads sit in `data_byte`/`data_short`/`data_int` and the `random_data_*` frame
//! arrays, consumed strictly in group order, so a sample is a single forward walk.
//!
//! A keyframe track stores `size + 1` strictly increasing frame numbers and, for each, one
//! sample. The sample time `time * num_frames` selects the two surrounding keys and a lerp
//! fraction. Tracks of 64+ keys in animations of 256+ frames carry an extra coarse table in
//! `data_short` that only speeds up the original's search; the walk here binary-searches the
//! fine table and skips the coarse one.
//!
//! Rotations accumulate un-normalised as `weight * quat` and translations as
//! `weight * trans` with a separate `trans_weight`, exactly the original's accumulators, so
//! several animations can be summed and then normalised with [`Accum::finish`]. Rotations are
//! stored as `i16 / 32767`; the groups 1 and 3 encodings are quaternions with `x = y = 0`.
//! Non-looping animations sampled at exactly `1.0` take the last keyframe, flipping each
//! rotation into the hemisphere of what has accumulated so far (`XAnimCalcNonLoopEnd`).

use assets::zone::xanim::{Indices, XAnimParts};

/// "Not a bone of this rig" in an anim-to-model table.
pub const NO_BONE: u8 = u8::MAX;

const INV_I16: f32 = 1.0 / 32767.0;

/// Weighted sums for one bone before normalisation (the original's `DObjAnimMat` while
/// accumulating).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Accum {
    pub quat: [f32; 4],
    pub trans: [f32; 3],
    pub trans_weight: f32,
}

impl Accum {
    pub const ZERO: Accum = Accum {
        quat: [0.0; 4],
        trans: [0.0; 3],
        trans_weight: 0.0,
    };

    /// This bone's accumulation mixed with `over`'s by `weight` (0..=1): where `over` animated the bone's rotation (or
    /// translation) the result is the unit blend `(1 - weight) * self + weight * over`, normalised by
    /// [`Accum::finish`]; where it did not, the bone is unchanged.
    pub fn overlaid(&self, over: &Accum, weight: f32) -> Accum {
        if over.quat == [0.0; 4] && over.trans_weight == 0.0 {
            return *self;
        }
        let (oq, ot) = over.finish();
        let (bq, bt) = self.finish();
        let mut out = *self;
        if let Some(oq) = oq {
            let q = match bq {
                Some(bq) => {
                    let flip = if bq.iter().zip(&oq).map(|(a, b)| a * b).sum::<f32>() < 0.0 {
                        -1.0
                    } else {
                        1.0
                    };
                    std::array::from_fn(|i| (1.0 - weight) * bq[i] + weight * flip * oq[i])
                }
                None => oq,
            };
            out.quat = q;
        }
        if let Some(ot) = ot {
            out.trans = match bt {
                Some(bt) => std::array::from_fn(|i| (1.0 - weight) * bt[i] + weight * ot[i]),
                None => ot,
            };
            out.trans_weight = 1.0;
        }
        out
    }

    /// Unit rotation and weighted-mean translation of what was accumulated; `None` for the
    /// parts nothing animated (no rotation weight, no translation weight).
    pub fn finish(&self) -> (Option<[f32; 4]>, Option<[f32; 3]>) {
        let l2 = self.quat.iter().map(|c| c * c).sum::<f32>();
        let q = (l2 > 0.0).then(|| {
            let inv = 1.0 / l2.sqrt();
            self.quat.map(|c| c * inv)
        });
        let t = (self.trans_weight != 0.0).then(|| {
            let inv = 1.0 / self.trans_weight;
            self.trans.map(|c| c * inv)
        });
        (q, t)
    }
}

/// A track's index table and the keyframe it selects.
struct Track {
    /// First sample (frame) to read.
    key: usize,
    frac: f32,
}

struct Walk<'a> {
    a: &'a XAnimParts,
    wide: bool,
    frame_frac: f32,
    frame_index: u32,
    end: bool,
    db: usize,
    ds: usize,
    di: usize,
    rb: usize,
    rs: usize,
    ix: usize,
}

impl Walk<'_> {
    fn short(&self, i: usize) -> i16 {
        self.a.data_short.get(i).copied().unwrap_or(0)
    }

    fn rs(&self, i: usize) -> i16 {
        self.a.random_data_short.get(i).copied().unwrap_or(0)
    }

    fn float(&self, i: usize) -> f32 {
        f32::from_bits(self.a.data_int.get(i).copied().unwrap_or(0) as u32)
    }

    fn fine(&self, at: usize, i: usize) -> u32 {
        match &self.a.indices {
            Indices::Short(v) => v.get(at + i).copied().map_or(0, u32::from),
            Indices::Byte(v) => v.get(at + i).copied().map_or(0, u32::from),
            Indices::None => 0,
        }
    }

    /// Consumes one track header (`tableSize` word and index table) and picks the key pair.
    fn track(&mut self) -> (usize, Track) {
        let size = self.short(self.ds) as u16 as usize;
        self.ds += 1;
        let key_at = |w: &Self, i: usize| -> u32 {
            if !w.wide {
                w.a.data_byte.get(w.db + i).copied().map_or(0, u32::from)
            } else if size >= 64 {
                w.fine(w.ix, i)
            } else {
                w.short(w.ds + i) as u16 as u32
            }
        };
        let track = if self.end || size == 0 {
            // The last key (`size`) read as-is.
            Track {
                key: if self.end { size } else { 0 },
                frac: 0.0,
            }
        } else {
            // Largest i in 0..size with key[i] <= frame_index.
            let (mut lo, mut hi) = (0usize, size);
            while lo + 1 < hi {
                let mid = (lo + hi) / 2;
                if key_at(self, mid) <= self.frame_index {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let k0 = key_at(self, lo);
            let k1 = key_at(self, lo + 1);
            let span = k1.saturating_sub(k0);
            let frac = if span == 0 {
                0.0
            } else {
                ((self.frame_frac - k0 as f32) / span as f32).clamp(0.0, 1.0)
            };
            Track { key: lo, frac }
        };
        if !self.wide {
            self.db += size + 1;
        } else if size >= 64 {
            self.ds += ((size - 1) >> 8) + 2;
            self.ix += size + 1;
        } else {
            self.ds += size + 1;
        }
        (size, track)
    }
}

fn add_rot(acc: &mut Accum, w: f32, q: [f32; 4], end: bool) {
    let mut w = w;
    if end && acc.quat.iter().zip(&q).map(|(a, b)| a * b).sum::<f32>() < 0.0 {
        w = -w;
    }
    for (a, b) in acc.quat.iter_mut().zip(q) {
        *a += w * b;
    }
}

/// Adds `weight` times the animation at normalised `time` into `out`.
///
/// `to_model[i]` is the `out` index of the animation's `i`-th named part ([`NO_BONE`] skips
/// it; so does an index past `out`). Looping animations wrap `time` into `[0, 1)`; others clamp
/// it to `[0, 1]`. A one-pose animation (`num_frames == 0`) always takes its last keys. Allocation-free; a truncated
/// or malformed animation reads zeros rather than panicking.
pub fn accumulate(a: &XAnimParts, to_model: &[u8], time: f32, weight: f32, out: &mut [Accum]) {
    let time = if a.looping {
        let t = time - time.floor();
        if t >= 1.0 { 0.0 } else { t }
    } else {
        time.clamp(0.0, 1.0)
    };
    // A one-pose animation (`num_frames == 0`) holds only last keys, read the way the original reads a finished
    // non-looping animation (`XAnimCalcNonLoopEnd`).
    let end = !a.looping && (time >= 1.0 || a.num_frames == 0);
    let frame_frac = f32::from(a.num_frames) * time;
    let mut w = Walk {
        a,
        wide: a.num_frames >= 256,
        frame_frac,
        frame_index: frame_frac as u32,
        end,
        db: 0,
        ds: 0,
        di: 0,
        rb: 0,
        rs: 0,
        ix: 0,
    };
    let slot = |part: usize, out: &mut [Accum]| -> Option<usize> {
        let b = *to_model.get(part)?;
        (b != NO_BONE && usize::from(b) < out.len()).then_some(usize::from(b))
    };
    let counts = a.bone_counts.map(usize::from);
    let mut part = 0usize;

    for _ in 0..counts[0] {
        if let Some(b) = slot(part, out) {
            out[b].quat[3] += weight;
        }
        part += 1;
    }

    for _ in 0..counts[1] {
        let (size, t) = w.track();
        if let Some(b) = slot(part, out) {
            let f = 2 * t.key;
            let (z0, w0) = (w.rs(w.rs + f), w.rs(w.rs + f + 1));
            let (z1, w1) = if end {
                (z0, w0)
            } else {
                (w.rs(w.rs + f + 2), w.rs(w.rs + f + 3))
            };
            let l =
                |p: i16, q: i16| (f32::from(p) + (f32::from(q) - f32::from(p)) * t.frac) * INV_I16;
            add_rot(&mut out[b], weight, [0.0, 0.0, l(z0, z1), l(w0, w1)], end);
        }
        w.rs += 2 * size + 2;
        part += 1;
    }

    for _ in 0..counts[2] {
        let (size, t) = w.track();
        if let Some(b) = slot(part, out) {
            let f = 4 * t.key;
            let mut q = [0.0; 4];
            for (i, c) in q.iter_mut().enumerate() {
                let p = f32::from(w.rs(w.rs + f + i));
                let n = f32::from(w.rs(w.rs + f + 4 + i));
                *c = (p + (n - p) * t.frac) * INV_I16;
            }
            add_rot(&mut out[b], weight, q, end);
        }
        w.rs += 4 * size + 4;
        part += 1;
    }

    for _ in 0..counts[3] {
        if let Some(b) = slot(part, out) {
            let q = [
                0.0,
                0.0,
                f32::from(w.short(w.ds)) * INV_I16,
                f32::from(w.short(w.ds + 1)) * INV_I16,
            ];
            add_rot(&mut out[b], weight, q, end);
        }
        w.ds += 2;
        part += 1;
    }

    for _ in 0..counts[4] {
        if let Some(b) = slot(part, out) {
            let q = [0, 1, 2, 3].map(|i| f32::from(w.short(w.ds + i)) * INV_I16);
            add_rot(&mut out[b], weight, q, end);
        }
        w.ds += 4;
        part += 1;
    }

    for group in [5usize, 6] {
        for _ in 0..counts[group] {
            let named = usize::from(w.a.data_byte.get(w.db).copied().unwrap_or(0));
            w.db += 1;
            let (size, t) = w.track();
            if let Some(b) = slot(named, out) {
                let sample = |w: &Walk, k: usize, i: usize| -> f32 {
                    if group == 5 {
                        f32::from(
                            w.a.random_data_byte
                                .get(w.rb + 3 * k + i)
                                .copied()
                                .unwrap_or(0),
                        )
                    } else {
                        f32::from(w.rs(w.rs + 3 * k + i) as u16)
                    }
                };
                let mut tr = [0.0f32; 3];
                for (i, c) in tr.iter_mut().enumerate() {
                    let from = sample(&w, t.key, i);
                    let v = if end {
                        from
                    } else {
                        from + (sample(&w, t.key + 1, i) - from) * t.frac
                    };
                    *c = w.float(w.di + 3 + i) * v + w.float(w.di + i);
                }
                for (a, v) in out[b].trans.iter_mut().zip(tr) {
                    *a += weight * v;
                }
                out[b].trans_weight += weight;
            }
            w.di += 6;
            if group == 5 {
                w.rb += 3 * size + 3;
            } else {
                w.rs += 3 * size + 3;
            }
        }
    }

    for _ in 0..counts[7] {
        let named = usize::from(w.a.data_byte.get(w.db).copied().unwrap_or(0));
        w.db += 1;
        if let Some(b) = slot(named, out) {
            for i in 0..3 {
                out[b].trans[i] += weight * w.float(w.di + i);
            }
            out[b].trans_weight += weight;
        }
        w.di += 3;
    }

    for _ in 0..counts[8] {
        let named = usize::from(w.a.data_byte.get(w.db).copied().unwrap_or(0));
        w.db += 1;
        if let Some(b) = slot(named, out) {
            out[b].trans_weight += weight;
        }
    }
}
