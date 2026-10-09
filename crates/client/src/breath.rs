// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (GPL-3.0, KisakCOD contributors): `cgame/cg_weapons.cpp` (`HoldBreathUpdate`,
// `HoldBreathSoundLerp`).
//! The sound of holding the breath on a scoped weapon: the breath taken in, a heartbeat while it is held, the breath
//! let out (or a gasp when it ran out), and the dip of the channel volumes the held breath brings.
//!
//! [`Breath::step`] follows the player state's `HOLD_BREATH` weapon flag and says which sounds start;
//! [`Breath::duck`] is how far the other channels are turned down.

use sim::pm::math::diff_track;

/// `player_breath_snd_lerp`: how fast the channel dip follows the held breath (per second).
const SND_LERP: f32 = 2.0;
/// `player_breath_snd_delay`: the pause after a breath sound before the next can start (ms).
const SND_DELAY: i32 = 1000;

/// A sound to start (`weap_sniper_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cue {
    BreathIn,
    Heartbeat,
    BreathOut,
    Gasp,
}

impl Cue {
    pub fn alias(self) -> &'static str {
        match self {
            Cue::BreathIn => "weap_sniper_breathin",
            Cue::Heartbeat => "weap_sniper_heartbeat",
            Cue::BreathOut => "weap_sniper_breathout",
            Cue::Gasp => "weap_sniper_breathgasp",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Breath {
    /// Time until another breath sound may start (ms).
    delay: i32,
    /// How far the held breath has dipped the channels, 0 to 1.
    frac: f32,
    /// How long the breath has been held (or, once let out, since it was taken); -1 when not holding.
    time: i32,
    /// How long the breath-in sound lasts; the heartbeat takes over after it.
    in_time: i32,
}

impl Default for Breath {
    fn default() -> Self {
        Breath {
            delay: 0,
            frac: 0.0,
            time: -1,
            in_time: 0,
        }
    }
}

impl Breath {
    /// `HoldBreathUpdate`: the frame's `dt_ms`, whether the breath is held, the longest it can be held (ms) and how
    /// long the breath-in sound lasts (ms).
    pub fn step(
        &mut self,
        dt_ms: i32,
        holding: bool,
        hold_ms: i32,
        in_length: i32,
    ) -> Option<Cue> {
        if self.delay > 0 {
            self.delay -= dt_ms;
        }
        let mut cue = None;
        if holding {
            self.frac = diff_track(1.0, self.frac, SND_LERP, dt_ms as f32 * 0.001);
            if self.time >= 0 {
                if self.time > self.in_time {
                    cue = Some(Cue::Heartbeat);
                }
            } else {
                self.time = 0;
                if self.delay > 0 {
                    self.in_time = 0;
                } else {
                    cue = Some(Cue::BreathIn);
                    self.in_time = in_length;
                    self.delay = SND_DELAY;
                }
            }
            self.time += dt_ms;
        } else {
            if self.time >= 0 {
                self.time += dt_ms;
                if self.time <= hold_ms {
                    if self.delay <= 0 {
                        cue = Some(Cue::BreathOut);
                        self.delay = SND_DELAY;
                    }
                } else {
                    cue = Some(Cue::Gasp);
                }
            }
            self.time = -1;
            self.in_time = 0;
            self.frac = 0.0;
        }
        cue
    }

    /// `HoldBreathSoundLerp`: how far the channels are dipped, 0 (not at all) to 1 (as far as the hold-breath
    /// volumes say).
    pub fn duck(&self) -> f32 {
        self.frac
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOLD: i32 = 4500;

    fn run(b: &mut Breath, holding: bool, ms: i32) -> Vec<Cue> {
        (0..ms / 10)
            .filter_map(|_| b.step(10, holding, HOLD, 300))
            .collect()
    }

    #[test]
    fn breath_in_then_heartbeat_then_out() {
        let mut b = Breath::default();
        let held = run(&mut b, true, 1000);
        assert_eq!(held[0], Cue::BreathIn);
        assert_eq!(held.iter().filter(|c| **c == Cue::BreathIn).count(), 1);
        assert!(held[1..].iter().all(|c| *c == Cue::Heartbeat) && held.len() > 10);
        assert!(b.duck() > 0.5);
        // Let go: after the delay it breathes out; never a gasp inside the hold time.
        let out = run(&mut b, false, 100);
        assert_eq!(out, [Cue::BreathOut]);
        assert_eq!(b.duck(), 0.0);
    }

    #[test]
    fn running_out_of_breath_gasps() {
        let mut b = Breath::default();
        run(&mut b, true, 5000);
        assert_eq!(run(&mut b, false, 100), [Cue::Gasp]);
    }

    #[test]
    fn a_quick_retake_waits_out_the_sound_delay() {
        let mut b = Breath::default();
        run(&mut b, true, 200);
        run(&mut b, false, 10);
        let again = run(&mut b, true, 100);
        assert!(!again.contains(&Cue::BreathIn));
        assert_eq!(run(&mut Breath::default(), false, 1000), []);
    }
}
