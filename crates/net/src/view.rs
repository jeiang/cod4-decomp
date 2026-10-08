// SPDX-License-Identifier: GPL-3.0-only
//! What a client does with the snapshots it has accepted: keep a short history, keep a clock in
//! step with the server's, and interpolate other entities between the two snapshots that straddle
//! the render time, a little in the past, so motion stays smooth despite jitter and loss.

use crate::entity::EntityState;
use crate::snapshot::Snapshot;
use std::collections::VecDeque;

/// Snapshots kept; at 30 per second this is about a second of history.
const HISTORY: usize = 32;
/// How far behind the newest server time other entities are drawn.
pub const INTERP_DELAY_MS: i32 = 100;
/// How long past the newest snapshot an entity is held in place before it is considered stale.
const HOLD_MS: i32 = 250;

/// How much of the gap between a snapshot's clock reading and the running estimate the estimate takes up.
const CLOCK_EASE: f64 = 0.05;
/// A reading this far (ms) from the estimate is a different clock (a map change, a long stall), not jitter: the estimate
/// jumps to it instead of easing.
const CLOCK_JUMP_MS: f64 = 100.0;

#[derive(Debug, Default)]
pub struct SnapshotBuffer {
    /// Oldest first; `recv_ms` is the client clock at arrival.
    snaps: VecDeque<(u64, Snapshot)>,
    /// Estimated `server time - client clock` in ms, eased toward each snapshot's reading.
    offset: Option<f64>,
    /// The newest time handed out, so the clock never runs backwards.
    handed_out: std::cell::Cell<i32>,
}

impl SnapshotBuffer {
    pub fn push(&mut self, recv_ms: u64, snap: Snapshot) {
        if self
            .snaps
            .back()
            .is_some_and(|(_, s)| snap.server_time <= s.server_time)
        {
            return;
        }
        if self.snaps.len() == HISTORY {
            self.snaps.pop_front();
        }
        let reading = f64::from(snap.server_time) - recv_ms as f64;
        self.offset = Some(match self.offset {
            Some(o) if (reading - o).abs() < CLOCK_JUMP_MS => o + (reading - o) * CLOCK_EASE,
            _ => {
                self.handed_out.set(i32::MIN);
                reading
            }
        });
        self.snaps.push_back((recv_ms, snap));
    }

    /// Forgets the history (a new level restarts the server clock).
    pub fn clear(&mut self) {
        self.snaps.clear();
    }

    pub fn latest(&self) -> Option<&Snapshot> {
        self.snaps.back().map(|(_, s)| s)
    }

    /// The server's time now, as far as this client can tell: the local clock plus an offset eased toward what each
    /// snapshot reads. A snapshot's arrival time carries the network's and the sender's jitter; taking the newest
    /// snapshot's time plus the time since it arrived would move this clock by that jitter at every snapshot, and the
    /// player's own commands and every interpolated body would follow it. Never runs backwards.
    pub fn server_time(&self, now_ms: u64) -> Option<i32> {
        let offset = self.offset?;
        let t = (now_ms as f64 + offset).round() as i32;
        let t = t.max(self.handed_out.get());
        self.handed_out.set(t);
        Some(t)
    }

    /// Entities (not the viewer's own player) as they were at `render_time` of the server clock.
    pub fn interpolate(&self, render_time: i32, own_client: Option<u16>) -> Vec<EntityState> {
        let Some((_, newest)) = self.snaps.back() else {
            return Vec::new();
        };
        let own = |e: &EntityState| {
            own_client.is_some_and(|c| e.etype == crate::entity::etype::PLAYER && e.client == c)
        };
        if render_time >= newest.server_time {
            // Past the newest snapshot: hold it rather than invent motion.
            if render_time - newest.server_time > HOLD_MS {
                return Vec::new();
            }
            return newest
                .entities
                .iter()
                .filter(|e| !own(e))
                .cloned()
                .collect();
        }
        let Some(i) = self
            .snaps
            .iter()
            .position(|(_, s)| s.server_time > render_time)
        else {
            return Vec::new();
        };
        let b = &self.snaps[i].1;
        let Some(a) = i.checked_sub(1).map(|j| &self.snaps[j].1) else {
            // Older than anything kept: the oldest snapshot is the best there is.
            return b.entities.iter().filter(|e| !own(e)).cloned().collect();
        };
        let span = (b.server_time - a.server_time).max(1) as f32;
        let f = ((render_time - a.server_time) as f32 / span).clamp(0.0, 1.0);
        b.entities
            .iter()
            .filter(|e| !own(e))
            .map(|eb| match a.entity(eb.number) {
                Some(ea) if ea.etype == eb.etype && ea.client == eb.client => lerp(ea, eb, f),
                _ => eb.clone(),
            })
            .collect()
    }
}

fn lerp_angle(a: f32, b: f32, f: f32) -> f32 {
    let d = (b - a + 180.0).rem_euclid(360.0) - 180.0;
    a + d * f
}

/// `a` to `b` by `f`; discrete state comes from `b`. A jump of more than 256 units in one
/// snapshot is a teleport (respawn): no sliding across the map.
fn lerp(a: &EntityState, b: &EntityState, f: f32) -> EntityState {
    let mut e = b.clone();
    let d: f32 = (0..3).map(|i| (b.origin[i] - a.origin[i]).powi(2)).sum();
    if d > 256.0 * 256.0 {
        return e;
    }
    for i in 0..3 {
        e.origin[i] = a.origin[i] + (b.origin[i] - a.origin[i]) * f;
        e.angles[i] = lerp_angle(a.angles[i], b.angles[i], f);
        e.velocity[i] = a.velocity[i] + (b.velocity[i] - a.velocity[i]) * f;
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::etype;

    fn snap(t: i32, x: f32, yaw: f32) -> Snapshot {
        let mut s = Snapshot::empty();
        s.server_time = t;
        s.entities.push(EntityState {
            number: 3,
            etype: etype::PLAYER,
            client: 3,
            origin: [x, 0.0, 0.0],
            angles: [0.0, yaw, 0.0],
            ..EntityState::default()
        });
        s
    }

    /// Snapshots 33 ms apart that arrive up to 12 ms early or late: the clock a client reads every 4 ms moves by 4 ms
    /// a step, give or take a millisecond, and never backwards.
    #[test]
    fn the_clock_does_not_follow_arrival_jitter() {
        let mut b = SnapshotBuffer::default();
        let jitter = [0i64, 9, -7, 12, -11, 3, 8, -12, 5, -4, 10, -9];
        let mut sent = 0usize;
        let mut last: Option<i32> = None;
        let mut worst = 0;
        for now in (0u64..3000).step_by(4) {
            // Snapshot k leaves the server at 33 k and arrives 20 ms later plus its jitter.
            while 33 * sent as i64 + 20 + jitter[sent % jitter.len()] <= now as i64 {
                let t = 33 * sent as i32;
                b.push(
                    33 * sent as u64 + (20 + jitter[sent % jitter.len()]) as u64,
                    snap(t, 0.0, 0.0),
                );
                sent += 1;
            }
            let Some(t) = b.server_time(now) else {
                continue;
            };
            if let Some(l) = last {
                assert!(t >= l, "the clock ran back from {l} to {t} at {now}");
                // Past the first second the estimate has settled.
                if now > 1000 {
                    worst = worst.max((t - l - 4).abs());
                }
            }
            last = Some(t);
        }
        assert!(
            worst <= 1,
            "the clock stepped {worst} ms off the local rate"
        );
    }

    #[test]
    fn midpoint_interpolates_position_and_takes_the_short_way_round_an_angle() {
        let mut b = SnapshotBuffer::default();
        b.push(0, snap(100, 0.0, 350.0));
        b.push(33, snap(200, 100.0, 10.0));
        let e = b.interpolate(150, None);
        assert_eq!(e.len(), 1);
        assert!((e[0].origin[0] - 50.0).abs() < 1e-3);
        assert!((e[0].angles[1] - 360.0).abs() < 1e-3);
    }

    #[test]
    fn the_viewers_own_player_is_left_out_and_a_teleport_does_not_slide() {
        let mut b = SnapshotBuffer::default();
        b.push(0, snap(100, 0.0, 0.0));
        b.push(33, snap(200, 1000.0, 0.0));
        assert!(b.interpolate(150, Some(3)).is_empty());
        assert_eq!(b.interpolate(150, None)[0].origin[0], 1000.0);
    }

    #[test]
    fn a_stale_view_shows_nothing_and_old_or_duplicate_snapshots_are_ignored() {
        let mut b = SnapshotBuffer::default();
        b.push(0, snap(100, 0.0, 0.0));
        b.push(1, snap(100, 5.0, 0.0));
        b.push(2, snap(50, 5.0, 0.0));
        assert_eq!(b.latest().unwrap().entities[0].origin[0], 0.0);
        assert_eq!(b.interpolate(100 + HOLD_MS, None).len(), 1);
        assert!(b.interpolate(100 + HOLD_MS + 1, None).is_empty());
        assert_eq!(b.server_time(40), Some(140));
    }
}
