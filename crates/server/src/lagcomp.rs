// SPDX-License-Identifier: GPL-3.0-only
//! Lag compensation (antilag): a short history of every live player's origin, so a shot is judged where the
//! shooter saw the others. Follows KisakCOD (GPL-3.0; Infinity Ward, Activision, the KisakCOD contributors)
//! `server_mp/sv_snapshot_mp.cpp` `SV_GetClientPositionAtTime` and `game/g_weapon.cpp` `G_AntiLagRewindClientPos` /
//! `G_AntiLag_RestoreClientPos`.
//!
//! A client draws the others at the time it stamps its commands with (`cl.serverTime`, `CL_FinishMove`); when a
//! command fires, the server moves every other live player's origin back to that time, keeps their current stance
//! box and animation pose, judges the shot, and puts the origins back. Only the origin is archived: the stock
//! snapshot archive keeps `ps.origin` per frame and nothing of the box or the pose.

use sim::Vec3;
use std::collections::VecDeque;

/// Samples per player: about a second at 30 frames per second, more than the 400 ms window needs.
const SAMPLES: usize = 32;
/// `G_AntiLagRewindClientPos`: a command more than this old (ms) is judged against the live bodies.
pub const MAX_REWIND_MS: i32 = 400;

/// One player's origin at one server frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub time: i32,
    pub origin: Vec3,
}

#[derive(Debug, Default)]
pub struct LagRing {
    players: Vec<VecDeque<Sample>>,
}

/// Whether `G_AntiLagRewindClientPos` rewinds at all for a command stamped `game_time` judged at `now`: the command
/// is more than one server frame (`1000 / sv_fps`) and at most [`MAX_REWIND_MS`] old.
pub fn in_window(game_time: i32, now: i32, sv_fps: i32) -> bool {
    let age = now - game_time;
    age <= MAX_REWIND_MS && age > 1000 / sv_fps.max(1)
}

impl LagRing {
    pub fn new(clients: usize) -> Self {
        Self {
            players: (0..clients).map(|_| VecDeque::new()).collect(),
        }
    }

    /// Files a live player's origin for this frame.
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

    /// Forgets a player: gone, dead or not playing this frame. `SV_GetClientPositionAtTime` refuses a time it can
    /// reach only through such a frame, and a cleared history cannot reach past it.
    pub fn clear(&mut self, client: u16) {
        if let Some(q) = self.players.get_mut(usize::from(client)) {
            q.clear();
        }
    }

    /// `SV_GetClientPositionAtTime`: where `client` was at `time`, interpolated between the two frames around it.
    /// `None` when the history does not reach back that far (the player is then left where it is).
    pub fn at(&self, client: u16, time: i32) -> Option<Vec3> {
        let q = self.players.get(usize::from(client))?;
        let first = q.front()?;
        if time < first.time {
            return None;
        }
        let Some(i) = q.iter().position(|s| s.time >= time) else {
            // Newer than the newest frame: the newest origin.
            return q.back().map(|s| s.origin);
        };
        let b = &q[i];
        if b.time == time || i == 0 {
            return Some(b.origin);
        }
        let a = &q[i - 1];
        let f = (time - a.time) as f32 / (b.time - a.time) as f32;
        Some(std::array::from_fn(|k| {
            a.origin[k] + (b.origin[k] - a.origin[k]) * f
        }))
    }
}

/// `AntilagClientStore`: the live origins of the players a rewind moved, to put back.
#[derive(Debug, Default)]
#[must_use = "the rewound players stay rewound until restored"]
pub struct Rewound {
    moved: Vec<(u16, Vec3)>,
}

impl crate::game::Game {
    /// The archive `SV_GetClientPositionAtTime` reads: every live player's `ps.origin` this frame. A player who is
    /// not playing, is dead or is gone has no frame, so no time before now can be reached through it.
    pub fn lag_record(&mut self) {
        let time = self.level.time;
        for n in 0..self.clients.len() {
            let c = &self.clients[n];
            let alive = c.connected()
                && c.session == crate::client::Session::Playing
                && c.ps.pm_type < sim::pm::PmType::Dead
                && self
                    .ents
                    .get(n)
                    .and_then(Option::as_ref)
                    .is_some_and(|e| e.health > 0);
            if !alive {
                self.lag.clear(n as u16);
                continue;
            }
            let origin = c.ps.origin;
            self.lag.record(n as u16, Sample { time, origin });
        }
    }

    /// `G_SetLastServerTime`: keeps `client->lastServerTime` at the newest command's `serverTime`, no more than a
    /// second old.
    pub fn set_last_server_time(&mut self, n: u16, server_time: i32) {
        let now = self.level.time;
        let Some(c) = self.client_mut(n) else { return };
        let t = server_time.max(now - 1000);
        if t >= c.last_server_time || now <= t {
            c.last_server_time = t;
        }
    }

    /// `HandleClientEvent`: the time a player's shot or knife swing is judged at, `client->lastServerTime` with
    /// `g_antilag` on, `level.time` with it off.
    pub fn shot_game_time(&self, n: u16) -> i32 {
        match self.client(n) {
            Some(c) if self.cvars.bool("g_antilag") => c.last_server_time,
            _ => self.level.time,
        }
    }

    /// `G_AntiLagRewindClientPos`: with `g_antilag` on and `game_time` more than a server frame and at most
    /// [`MAX_REWIND_MS`] old, moves every playing client whose origin at `game_time` the history holds back there.
    /// Only the origin moves; the stance box and the animation pose stay as they are now.
    pub fn antilag_rewind(&mut self, game_time: i32) -> Rewound {
        let mut r = Rewound::default();
        if !self.cvars.bool("g_antilag")
            || !in_window(game_time, self.level.time, self.cvars.int("sv_fps"))
        {
            return r;
        }
        for n in 0..self.clients.len() as u16 {
            let c = &self.clients[usize::from(n)];
            if !c.connected() || c.session != crate::client::Session::Playing {
                continue;
            }
            let Some(past) = self.lag.at(n, game_time) else {
                continue;
            };
            let Some(e) = self.ent_mut(n) else { continue };
            r.moved.push((n, e.origin));
            e.origin = past;
            self.relink(n);
        }
        r
    }

    /// `G_AntiLag_RestoreClientPos`: puts the players a rewind moved back where they are.
    pub fn antilag_restore(&mut self, r: Rewound) {
        for (n, origin) in r.moved {
            if let Some(e) = self.ent_mut(n) {
                e.origin = origin;
            }
            self.relink(n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring() -> LagRing {
        // 20 server frames a second, the stock `sv_fps`; the body walks 10 units a frame.
        let mut r = LagRing::new(2);
        for i in 0..10 {
            r.record(
                1,
                Sample {
                    time: i * 50,
                    origin: [i as f32 * 10.0, 0.0, 0.0],
                },
            );
        }
        r
    }

    #[test]
    fn a_time_between_frames_interpolates_the_origin() {
        let r = ring();
        assert_eq!(r.at(1, 200).unwrap()[0], 40.0);
        assert_eq!(r.at(1, 225).unwrap()[0], 45.0);
        assert_eq!(r.at(1, 0).unwrap()[0], 0.0);
    }

    #[test]
    fn a_time_the_history_cannot_reach_leaves_the_body_live() {
        let r = ring();
        assert!(r.at(1, -1).is_none());
        assert!(r.at(0, 100).is_none());
        assert!(r.at(7, 100).is_none());
        let mut r = ring();
        // Died (or stopped playing): the frames before are out of reach.
        r.clear(1);
        r.record(
            1,
            Sample {
                time: 500,
                origin: [0.0; 3],
            },
        );
        assert!(r.at(1, 450).is_none());
        assert_eq!(r.at(1, 500), Some([0.0; 3]));
    }

    #[test]
    fn the_window_is_more_than_one_frame_and_at_most_400_ms() {
        // `level.time - gameTime <= 400 && level.time - gameTime > 1000 / sv_fps`.
        assert!(!in_window(1000, 1000, 20));
        assert!(!in_window(950, 1000, 20));
        assert!(in_window(949, 1000, 20));
        assert!(in_window(600, 1000, 20));
        assert!(!in_window(599, 1000, 20));
        assert!(in_window(966, 1000, 30));
        assert!(!in_window(967, 1000, 30));
    }
}
