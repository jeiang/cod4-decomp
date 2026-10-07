// SPDX-License-Identifier: GPL-3.0-or-later
//! Server-side animation clocks and notetrack delivery (`G_XAnimUpdateEnt`).
//!
//! An entity that plays a flagged animation gets a notify (`ent notify(flag, note)`) each time
//! the animation's clock crosses a notetrack. The frame advances the clock one notetrack at a
//! time and lets the scripts run between notetracks, so a script that reacts to a notetrack
//! sees the entity at exactly that point of the animation.

use std::rc::Rc;
use std::sync::Arc;

use crate::content::AnimInfo;

#[derive(Debug, Clone)]
pub struct Playing {
    pub info: Arc<AnimInfo>,
    /// The notify name scripts wait on.
    pub flag: Rc<str>,
    pub rate: f32,
    /// Normalized animation time in `[0, 1]`.
    pub time: f32,
}

#[derive(Debug, Default, Clone)]
pub struct AnimTree {
    pub playing: Vec<Playing>,
}

/// A notetrack reached while advancing, and the seconds of the step it took.
#[derive(Debug, PartialEq)]
pub struct Reached {
    pub flag: Rc<str>,
    pub note: Arc<str>,
    pub elapsed: f32,
}

impl AnimTree {
    /// Seconds an animation needs to go from `time` to `to` at its rate.
    fn seconds(p: &Playing, to: f32) -> f32 {
        (to - p.time) * p.info.length / p.rate
    }

    /// Advances by up to `dt` seconds, stopping at the first notetrack of any playing
    /// animation. `None` means the whole `dt` was consumed with nothing reached.
    pub fn step(&mut self, dt: f32) -> Option<Reached> {
        // The earliest event: a notetrack, or a looping animation reaching its end.
        // (seconds, animation, note); `note` is `None` for a loop edge.
        let mut first: Option<(f32, usize, Option<Arc<str>>)> = None;
        for (i, p) in self.playing.iter().enumerate() {
            if p.rate <= 0.0 || p.info.length <= 0.0 {
                continue;
            }
            let reach = (p.time + dt * p.rate / p.info.length).min(1.0);
            let mut consider = |at: f32, note: Option<Arc<str>>| {
                if first.as_ref().is_none_or(|f| at < f.0) {
                    first = Some((at, i, note));
                }
            };
            for (name, t) in &p.info.notes {
                if *t > p.time && *t <= reach {
                    consider(Self::seconds(p, *t), Some(name.clone()));
                }
            }
            if p.info.looping && reach >= 1.0 {
                consider(Self::seconds(p, 1.0), None);
            }
        }
        let at = first.as_ref().map_or(dt, |f| f.0.min(dt));
        for p in &mut self.playing {
            if p.rate > 0.0 && p.info.length > 0.0 {
                p.time = (p.time + at * p.rate / p.info.length).min(1.0);
            }
        }
        let (_, i, note) = first?;
        match note {
            Some(note) => {
                let p = &mut self.playing[i];
                if let Some(t) = p.info.notes.iter().find(|(n, _)| *n == note).map(|n| n.1) {
                    p.time = t;
                }
                Some(Reached {
                    flag: p.flag.clone(),
                    note,
                    elapsed: at,
                })
            }
            None => {
                self.playing[i].time = 0.0;
                let r = self.step(dt - at);
                // The loop edge took `at` seconds that the caller must also account for.
                r.map(|r| Reached {
                    elapsed: r.elapsed + at,
                    ..r
                })
            }
        }
    }
}

impl crate::game::Game {
    /// One step of entity `num`'s animation clock; see [`AnimTree::step`].
    pub fn step_anim(&mut self, num: u16, dt: f32) -> Option<Reached> {
        self.ent_mut(num)?.anim.as_mut()?.step(dt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(looping: bool, notes: &[(&str, f32)]) -> Arc<AnimInfo> {
        Arc::new(AnimInfo {
            looping,
            length: 2.0,
            notes: notes.iter().map(|(n, t)| ((*n).into(), *t)).collect(),
        })
    }

    fn play(info: Arc<AnimInfo>) -> AnimTree {
        AnimTree {
            playing: vec![Playing {
                info,
                flag: "a".into(),
                rate: 1.0,
                time: 0.0,
            }],
        }
    }

    #[test]
    fn notetracks_arrive_in_order_at_their_time() {
        let mut t = play(clip(false, &[("fire", 0.25), ("end", 1.0)]));
        let mut left = 3.0;
        let mut seen = Vec::new();
        let mut clock = 0.0;
        while let Some(r) = t.step(left) {
            left -= r.elapsed;
            clock += r.elapsed;
            seen.push((r.note.to_string(), clock));
        }
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].0, "fire");
        assert!((seen[0].1 - 0.5).abs() < 1e-4, "{}", seen[0].1);
        assert_eq!(seen[1].0, "end");
        assert!((seen[1].1 - 2.0).abs() < 1e-4, "{}", seen[1].1);
        assert_eq!(t.playing[0].time, 1.0);
    }

    #[test]
    fn a_note_is_delivered_once_per_pass_and_again_after_a_loop() {
        let mut t = play(clip(true, &[("step", 0.5)]));
        let mut hits = 0;
        let mut run = |steps: usize| {
            for _ in 0..steps {
                let mut left = 0.1;
                while let Some(r) = t.step(left) {
                    left -= r.elapsed;
                    hits += 1;
                }
            }
            hits
        };
        // The note sits at 1 s of a 2 s loop: crossed at 1 s and again at 3 s.
        assert_eq!(run(25), 1);
        assert_eq!(run(20), 2);
    }
}
