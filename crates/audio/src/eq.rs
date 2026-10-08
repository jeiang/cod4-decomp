// SPDX-License-Identifier: GPL-3.0-only
//! The per-channel parametric EQ of the original (`snd_setEq`): two EQs of three bands for every entity
//! channel, each band a low-pass, high-pass, low shelf, high shelf or bell. The coefficients are the usual
//! biquads of the Audio EQ Cookbook; the original's Miles filter is closed source, so its exact response is
//! not reproduced. `gain` is in decibels [INFERENCE: Miles' own unit is not documented].

use std::f32::consts::PI;

/// `snd_eqTypeStrings`, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EqType {
    LowPass,
    HighPass,
    LowShelf,
    HighShelf,
    Bell,
}

impl EqType {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "lowpass" => Self::LowPass,
            "highpass" => Self::HighPass,
            "lowshelf" => Self::LowShelf,
            "highshelf" => Self::HighShelf,
            "bell" => Self::Bell,
            _ => return None,
        })
    }
}

/// One band's settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub kind: EqType,
    pub gain_db: f32,
    pub freq: f32,
    pub q: f32,
}

/// Normalised biquad coefficients (`a0 = 1`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coeffs {
    b: [f32; 3],
    a: [f32; 2],
}

impl Coeffs {
    pub fn design(b: &Band, rate: u32) -> Self {
        let rate = rate.max(1) as f32;
        let freq = b.freq.clamp(10.0, rate * 0.45);
        let q = b.q.max(0.05);
        let w = 2.0 * PI * freq / rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
        let amp = 10f32.powf(b.gain_db / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match b.kind {
            EqType::LowPass => (
                (1.0 - cos) / 2.0,
                1.0 - cos,
                (1.0 - cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            EqType::HighPass => (
                (1.0 + cos) / 2.0,
                -(1.0 + cos),
                (1.0 + cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            EqType::Bell => (
                1.0 + alpha * amp,
                -2.0 * cos,
                1.0 - alpha * amp,
                1.0 + alpha / amp,
                -2.0 * cos,
                1.0 - alpha / amp,
            ),
            EqType::LowShelf => {
                let s = 2.0 * amp.sqrt() * alpha;
                (
                    amp * ((amp + 1.0) - (amp - 1.0) * cos + s),
                    2.0 * amp * ((amp - 1.0) - (amp + 1.0) * cos),
                    amp * ((amp + 1.0) - (amp - 1.0) * cos - s),
                    (amp + 1.0) + (amp - 1.0) * cos + s,
                    -2.0 * ((amp - 1.0) + (amp + 1.0) * cos),
                    (amp + 1.0) + (amp - 1.0) * cos - s,
                )
            }
            EqType::HighShelf => {
                let s = 2.0 * amp.sqrt() * alpha;
                (
                    amp * ((amp + 1.0) + (amp - 1.0) * cos + s),
                    -2.0 * amp * ((amp - 1.0) + (amp + 1.0) * cos),
                    amp * ((amp + 1.0) + (amp - 1.0) * cos - s),
                    (amp + 1.0) - (amp - 1.0) * cos + s,
                    2.0 * ((amp - 1.0) - (amp + 1.0) * cos),
                    (amp + 1.0) - (amp - 1.0) * cos - s,
                )
            }
        };
        Self {
            b: [b0 / a0, b1 / a0, b2 / a0],
            a: [a1 / a0, a2 / a0],
        }
    }

    /// One sample through the filter (transposed direct form II).
    #[inline]
    pub fn run(&self, st: &mut [f32; 2], x: f32) -> f32 {
        let y = self.b[0] * x + st[0];
        st[0] = self.b[1] * x - self.a[0] * y + st[1];
        st[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// How many EQs and bands the original has per channel.
pub const EQS: usize = 2;
pub const BANDS: usize = 3;

/// The filters of one entity channel; `None` bands are off.
pub type ChannelEq = [[Option<Coeffs>; BANDS]; EQS];

/// A voice's filter memory: per EQ, band and source channel.
pub type EqState = [[[[f32; 2]; 2]; BANDS]; EQS];

#[cfg(test)]
mod tests {
    use super::*;

    fn gain_at(b: Band, hz: f32) -> f32 {
        let rate = 48_000;
        let c = Coeffs::design(&b, rate);
        let mut st = [0.0; 2];
        let n = 9600;
        let mut peak = 0.0f32;
        for i in 0..n {
            let x = (2.0 * PI * hz * i as f32 / rate as f32).sin();
            let y = c.run(&mut st, x);
            if i > n / 2 {
                peak = peak.max(y.abs());
            }
        }
        peak
    }

    #[test]
    fn a_low_pass_passes_lows_and_stops_highs() {
        let b = Band {
            kind: EqType::LowPass,
            gain_db: 0.0,
            freq: 1000.0,
            q: 0.707,
        };
        assert!(gain_at(b, 100.0) > 0.95);
        assert!(gain_at(b, 8000.0) < 0.05);
    }

    #[test]
    fn a_bell_boosts_its_centre_by_its_gain() {
        let b = Band {
            kind: EqType::Bell,
            gain_db: 12.0,
            freq: 2000.0,
            q: 1.0,
        };
        let boost = gain_at(b, 2000.0);
        assert!((boost - 3.98).abs() < 0.2, "{boost}");
        assert!((gain_at(b, 100.0) - 1.0).abs() < 0.1);
    }

    #[test]
    fn shelves_scale_their_side() {
        let lo = Band {
            kind: EqType::LowShelf,
            gain_db: -12.0,
            freq: 500.0,
            q: 0.707,
        };
        assert!(gain_at(lo, 50.0) < 0.3);
        assert!((gain_at(lo, 10_000.0) - 1.0).abs() < 0.1);
        let hi = Band {
            kind: EqType::HighShelf,
            gain_db: -12.0,
            freq: 4000.0,
            q: 0.707,
        };
        assert!(gain_at(hi, 15_000.0) < 0.3);
        assert!((gain_at(hi, 100.0) - 1.0).abs() < 0.1);
    }
}
