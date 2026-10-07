// SPDX-License-Identifier: GPL-3.0-or-later
//! Lag compensation: a short history of every player's body, so a shot is judged against the
//! world the shooter saw. A client draws other players a fixed interval behind the server
//! clock ([`net::view::INTERP_DELAY_MS`]) and stamps every command with its clock; when the
//! command fires, the server puts the other players back where they were at that moment of
//! the stamp minus the interval, traces, and leaves them where they are. Rewinding is capped
//! so a laggy client cannot shoot far into the past.

use crate::playeranim::PlayerPoseState;
use sim::Vec3;
use std::collections::VecDeque;

/// Samples per player: about a second at 30 frames per second.
const SAMPLES: usize = 32;
/// The furthest back in time a shot may reach, in milliseconds.
pub const MAX_REWIND_MS: i32 = 250;
/// A jump bigger than this between two samples is a teleport: no sliding between them.
const TELEPORT: f32 = 256.0;

/// One player's body at one server frame.
#[derive(Debug, Clone)]
pub struct Sample {
    pub time: i32,
    pub origin: Vec3,
    pub mins: Vec3,
    pub maxs: Vec3,
    pub pose: PlayerPoseState,
}

#[derive(Debug, Default)]
pub struct LagRing {
    players: Vec<VecDeque<Sample>>,
}

impl LagRing {
    pub fn new(clients: usize) -> Self {
        Self {
            players: (0..clients).map(|_| VecDeque::new()).collect(),
        }
    }

    pub fn record(&mut self, client: u16, s: Sample) {
        let Some(q) = self.players.get_mut(usize::from(client)) else {
            return;
        };
        if q.back().is_some_and(|b| s.time <= b.time) {
            return;
        }
        if q.len() == SAMPLES {
            q.pop_front();
        }
        q.push_back(s);
    }

    /// Forgets a player (left, or was respawned elsewhere).
    pub fn clear(&mut self, client: u16) {
        if let Some(q) = self.players.get_mut(usize::from(client)) {
            q.clear();
        }
    }

    /// Where `client` was at `time`: between the two samples around it, the pose of the earlier.
    /// `now` caps how far back: a request older than [`MAX_REWIND_MS`] gets that limit's moment.
    /// `None` when there is no history that reaches back that far, so the caller uses the live
    /// body.
    pub fn at(&self, client: u16, time: i32, now: i32) -> Option<Sample> {
        let q = self.players.get(usize::from(client))?;
        let time = time.max(now - MAX_REWIND_MS);
        let (first, last) = (q.front()?, q.back()?);
        if time >= last.time {
            return None;
        }
        if time <= first.time {
            return Some(first.clone());
        }
        let i = q.iter().position(|s| s.time > time)?;
        let (a, b) = (&q[i - 1], &q[i]);
        let f = (time - a.time) as f32 / (b.time - a.time).max(1) as f32;
        let d: f32 = (0..3).map(|k| (b.origin[k] - a.origin[k]).powi(2)).sum();
        let mut s = a.clone();
        if d <= TELEPORT * TELEPORT {
            for k in 0..3 {
                s.origin[k] = a.origin[k] + (b.origin[k] - a.origin[k]) * f;
            }
        }
        s.time = time;
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(time: i32, x: f32) -> Sample {
        Sample {
            time,
            origin: [x, 0.0, 0.0],
            mins: [-15.0; 3],
            maxs: [15.0; 3],
            pose: PlayerPoseState::default(),
        }
    }

    fn ring() -> LagRing {
        let mut r = LagRing::new(2);
        for i in 0..10 {
            r.record(1, sample(i * 33, i as f32 * 10.0));
        }
        r
    }

    #[test]
    fn a_time_between_samples_interpolates() {
        let r = ring();
        let s = r.at(1, 33 * 4 + 16, 300).unwrap();
        assert!((s.origin[0] - 44.8).abs() < 0.5, "{}", s.origin[0]);
    }

    #[test]
    fn now_or_the_future_is_the_live_body_and_the_past_is_capped() {
        let r = ring();
        assert!(r.at(1, 33 * 9, 300).is_none());
        assert!(r.at(1, 10_000, 300).is_none());
        // Asking for a time long before `now - MAX_REWIND_MS` gets the cap's moment.
        let now = 33 * 9 + 5;
        let s = r.at(1, -5000, now).unwrap();
        assert!(
            s.origin[0] >= (now - MAX_REWIND_MS) as f32 / 33.0 * 10.0 - 11.0,
            "{}",
            s.origin[0]
        );
    }

    #[test]
    fn a_teleport_does_not_slide_and_unknown_players_have_no_history() {
        let mut r = LagRing::new(1);
        r.record(0, sample(0, 0.0));
        r.record(0, sample(33, 5000.0));
        r.record(0, sample(66, 5001.0));
        assert_eq!(r.at(0, 16, 100).unwrap().origin[0], 0.0);
        assert!(r.at(0, 40, 100).unwrap().origin[0] > 4000.0);
        assert!(LagRing::new(1).at(0, 10, 100).is_none());
        assert!(r.at(7, 10, 100).is_none());
    }
}
