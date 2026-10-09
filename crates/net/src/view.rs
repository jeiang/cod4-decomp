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

    /// Entities (not the viewer's own player) as they were at `render_time` of the server clock. Past the newest
    /// snapshot they hold where it left them, however long the stall: a frozen body, not a vanished one.
    pub fn interpolate(&self, render_time: i32, own_client: Option<u16>) -> Vec<EntityState> {
        self.interp(render_time, own_client, false)
    }

    /// [`interpolate`](Self::interpolate) over the entities and the actors the viewer has no line of sight to: what
    /// the compass and the names over heads read.
    pub fn interpolate_with_actors(
        &self,
        render_time: i32,
        own_client: Option<u16>,
    ) -> Vec<EntityState> {
        self.interp(render_time, own_client, true)
    }

    fn interp(&self, render_time: i32, own_client: Option<u16>, actors: bool) -> Vec<EntityState> {
        let Some((_, newest)) = self.snaps.back() else {
            return Vec::new();
        };
        let own = |e: &EntityState| {
            own_client.is_some_and(|c| e.etype == crate::entity::etype::PLAYER && e.client == c)
        };
        let all = |s: &Snapshot| -> Vec<EntityState> {
            let extra: &[EntityState] = if actors { &s.actors } else { &[] };
            s.entities
                .iter()
                .chain(extra)
                .filter(|e| !own(e))
                .cloned()
                .collect()
        };
        if render_time >= newest.server_time {
            return all(newest);
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
            return all(b);
        };
        let span = (b.server_time - a.server_time).max(1) as f32;
        let f = ((render_time - a.server_time) as f32 / span).clamp(0.0, 1.0);
        let find = |n: u16| {
            a.entity(n).or_else(|| {
                actors
                    .then(|| {
                        a.actors
                            .binary_search_by_key(&n, |e| e.number)
                            .ok()
                            .map(|k| &a.actors[k])
                    })
                    .flatten()
            })
        };
        all(b)
            .into_iter()
            .map(|eb| match find(eb.number) {
                // The same thing as before: an entity number the server reused for another, or one it moved by
                // fiat (a respawn), does not slide.
                Some(ea)
                    if ea.etype == eb.etype
                        && ea.client == eb.client
                        && (ea.eflags ^ eb.eflags) & crate::entity::TELEPORT_BIT == 0 =>
                {
                    lerp(ea, &eb, f)
                }
                _ => eb,
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
    fn old_or_duplicate_snapshots_are_ignored() {
        let mut b = SnapshotBuffer::default();
        b.push(0, snap(100, 0.0, 0.0));
        b.push(1, snap(100, 5.0, 0.0));
        b.push(2, snap(50, 5.0, 0.0));
        assert_eq!(b.latest().unwrap().entities[0].origin[0], 0.0);
        assert_eq!(b.server_time(40), Some(140));
    }

    #[test]
    fn a_stall_freezes_everything_where_it_was_instead_of_deleting_it() {
        let mut b = SnapshotBuffer::default();
        b.push(0, snap(100, 0.0, 0.0));
        b.push(33, snap(200, 40.0, 0.0));
        for late in [50, 250, 5_000, 600_000] {
            let e = b.interpolate(200 + late, None);
            assert_eq!(e.len(), 1, "{late} ms past the newest snapshot");
            assert_eq!(e[0].origin[0], 40.0);
        }
    }

    #[test]
    fn a_respawn_flagged_by_the_server_does_not_slide_even_a_short_way() {
        let mut b = SnapshotBuffer::default();
        let mut after = snap(200, 30.0, 0.0);
        after.entities[0].eflags ^= crate::entity::TELEPORT_BIT;
        b.push(0, snap(100, 0.0, 0.0));
        b.push(33, after);
        // Well inside the distance a jump is guessed from: only the flag says it was one.
        assert_eq!(b.interpolate(150, None)[0].origin[0], 30.0);
    }

    #[test]
    fn a_reused_entity_number_of_another_player_does_not_slide() {
        let mut b = SnapshotBuffer::default();
        let mut other = snap(200, 30.0, 0.0);
        other.entities[0].client = 7;
        b.push(0, snap(100, 0.0, 0.0));
        b.push(33, other);
        assert_eq!(b.interpolate(150, None)[0].origin[0], 30.0);
    }

    #[test]
    fn the_players_beyond_sight_are_interpolated_only_for_those_who_ask() {
        let mut b = SnapshotBuffer::default();
        let behind_wall = |t: i32, x: f32| {
            let mut s = snap(t, 0.0, 0.0);
            s.actors = std::mem::take(&mut s.entities);
            s.actors[0].origin[0] = x;
            s
        };
        b.push(0, behind_wall(100, 0.0));
        b.push(33, behind_wall(200, 100.0));
        assert!(b.interpolate(150, None).is_empty());
        let seen = b.interpolate_with_actors(150, None);
        assert_eq!(seen.len(), 1);
        assert!((seen[0].origin[0] - 50.0).abs() < 1e-3);
        assert!(b.interpolate_with_actors(150, Some(3)).is_empty());
    }
}
