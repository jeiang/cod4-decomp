// SPDX-License-Identifier: GPL-3.0-only
//! The state ring killcams replay: every server frame, the entities clients can see and, for
//! each player in the match, the player state, weapon inventory and the archived hud elements
//! and objectives that player's screen had. `archivetime` of a spectating client picks the frame
//! to serve ([`Archive::at`]); the stock `_killcam.gsc` asks for up to a few seconds back, the
//! ring keeps [`KEEP_MS`].

use crate::netsv::Sent;
use net::ui::{HudElem, MAX_OBJECTIVES, Objective};
use sim::pm::PlayerState;
use sim::weapon::PlayerWeapons;
use std::collections::VecDeque;

/// How far back the ring reaches: the longest killcam (a few seconds) plus the death delay,
/// with room for the scripts' predelay and the attacker's latency offset.
pub const KEEP_MS: i32 = 15_000;

/// What one player's screen showed at one frame.
#[derive(Debug, Clone)]
pub struct ArchPlayer {
    pub ps: PlayerState,
    pub inv: Box<[i32; PlayerWeapons::WORDS]>,
    /// The archived hud elements the player saw (those flagged to survive into killcams).
    pub hud: Vec<HudElem>,
    pub objectives: [Objective; MAX_OBJECTIVES],
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub time: i32,
    /// By entity number, wire-rounded.
    pub entities: Vec<Sent>,
    /// By client slot; `None` for a slot nobody was playing in.
    pub players: Vec<Option<ArchPlayer>>,
}

#[derive(Debug, Default)]
pub struct Archive {
    frames: VecDeque<Frame>,
}

impl Archive {
    /// Appends the frame of server time `f.time` and drops what is older than [`KEEP_MS`].
    pub fn record(&mut self, f: Frame) {
        if self.frames.back().is_some_and(|b| f.time <= b.time) {
            // The clock went back (a restart): the old history is another match.
            self.frames.clear();
        }
        let oldest = f.time - KEEP_MS;
        self.frames.push_back(f);
        while self.frames.front().is_some_and(|f| f.time < oldest) {
            self.frames.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.frames.clear();
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// How many milliseconds before `now` the history reaches.
    pub fn span(&self, now: i32) -> i32 {
        self.frames.front().map_or(0, |f| (now - f.time).max(0))
    }

    /// The newest frame at or before `time`, else the oldest one.
    pub fn at(&self, time: i32) -> Option<&Frame> {
        let i = self.frames.partition_point(|f| f.time <= time);
        self.frames.get(i.saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(time: i32) -> Frame {
        Frame {
            time,
            entities: Vec::new(),
            players: Vec::new(),
        }
    }

    #[test]
    fn the_ring_keeps_fifteen_seconds_and_serves_the_frame_at_or_before_a_time() {
        let mut a = Archive::default();
        for i in 0..1000 {
            a.record(frame(i * 33));
        }
        let now = 999 * 33;
        assert!(a.span(now) <= KEEP_MS && a.span(now) > KEEP_MS - 40);
        assert!(a.len() <= (KEEP_MS / 33 + 2) as usize);
        assert_eq!(a.at(now - 5000).unwrap().time, (now - 5000) / 33 * 33);
        // Older than the history: the oldest frame; newer than all: the newest.
        assert_eq!(a.at(0).unwrap().time, now - a.span(now));
        assert_eq!(a.at(now + 100).unwrap().time, now);
        assert!(Archive::default().at(5).is_none());
    }

    #[test]
    fn a_clock_that_restarts_drops_the_old_match() {
        let mut a = Archive::default();
        a.record(frame(5000));
        a.record(frame(5033));
        a.record(frame(33));
        assert_eq!(a.len(), 1);
        assert_eq!(a.at(0).unwrap().time, 33);
    }
}
