// SPDX-License-Identifier: GPL-3.0-or-later
//! Client-side prediction: the player's own movement is run locally, with the same
//! [`sim::pm::run_usercmd`] the server uses, so it responds at once instead of one round trip
//! later. Every snapshot carries the server's state for the last command it ran; the client
//! restarts from that state and replays the commands the server has not run yet. When the
//! replay lands somewhere other than where the previous one did (the server disagreed: a hit,
//! a push, a collision with something the client does not know of) the difference is shown
//! fading out instead of as a jump.

use crate::snapshot::Snapshot;
use sim::cm::{Collide, ENTITYNUM_NONE};
use sim::contents;
use sim::pm::{PLAYER_MAXS, PLAYER_MINS, pmf};
use sim::pm::{Params, PlayerState, UserCmd, run_usercmd};
use sim::weapon::{PlayerWeapons, WeaponTable};
use sim::world::{ClipEnt, World};
use std::collections::VecDeque;
use std::sync::Arc;

/// Commands kept: about two seconds at 60 per second, far more than any round trip.
const KEEP: usize = 128;
/// A disagreement larger than this is a teleport (a respawn): shown as one, not slid.
const SNAP_DISTANCE: f32 = 64.0;
/// A disagreement smaller than this is rounding.
const NOISE: f32 = 0.01;
/// How long a disagreement takes to fade, in milliseconds.
pub const SMOOTH_MS: i32 = 100;

/// The map's collision plus the other players as the newest snapshot has them: people block
/// each other, so a replay that ignored them would walk through a player the server stopped at.
pub struct PlayerBoxes {
    world: World,
    linked: Vec<u16>,
}

impl PlayerBoxes {
    pub fn new(clipmap: Arc<assets::zone::clipmap::Clipmap>) -> Self {
        Self {
            world: World::new(clipmap),
            linked: Vec::new(),
        }
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    /// Moves every other living player to where `snap` has it.
    pub fn sync(&mut self, snap: &Snapshot) {
        for n in self.linked.drain(..) {
            self.world.unlink(n);
        }
        for e in &snap.entities {
            let alive = !matches!(e.pm_type, 2..=5 | 7 | 8);
            if e.etype != crate::entity::etype::PLAYER || e.client == snap.ps.client_num || !alive {
                continue;
            }
            let mut maxs = PLAYER_MAXS;
            if e.pm_flags & pmf::PRONE != 0 {
                maxs[2] = 30.0;
            } else if e.pm_flags & pmf::DUCKED != 0 {
                maxs[2] = 50.0;
            }
            self.world.link(
                e.number,
                &ClipEnt {
                    contents: contents::PLAYER,
                    origin: e.origin,
                    mins: PLAYER_MINS,
                    maxs,
                    owner: ENTITYNUM_NONE,
                    ..ClipEnt::EMPTY
                },
            );
            self.linked.push(e.number);
        }
    }
}

/// Everything the replay needs besides the commands.
pub struct Env<'a, W: Collide> {
    pub world: &'a W,
    pub weapons: &'a WeaponTable,
    pub params: &'a Params,
    /// `g_speed`, from the server.
    pub speed: i32,
}

pub struct Predicted {
    pub ps: PlayerState,
    pub inv: PlayerWeapons,
    /// Commands replayed on top of the snapshot.
    pub replayed: usize,
}

#[derive(Default)]
pub struct Predictor {
    cmds: VecDeque<UserCmd>,
    /// Where the last replay ended: its final command's time and the origin it reached.
    last: Option<(i32, [f32; 3])>,
    /// The disagreement being faded: offset to add to the predicted origin, and when it began.
    error: [f32; 3],
    error_from: i32,
    /// Prediction passes whose result differed from the previous pass, beyond rounding.
    pub corrections: u64,
}

impl Predictor {
    /// Records a command about to be sent. Times must increase.
    pub fn push(&mut self, cmd: UserCmd) {
        if self
            .cmds
            .back()
            .is_some_and(|c| cmd.server_time <= c.server_time)
        {
            return;
        }
        if self.cmds.len() == KEEP {
            self.cmds.pop_front();
        }
        self.cmds.push_back(cmd);
    }

    pub fn latest(&self) -> Option<&UserCmd> {
        self.cmds.back()
    }

    /// The state after every command the server has not acknowledged in `snap`.
    pub fn predict<W: Collide>(&mut self, snap: &Snapshot, env: &Env<'_, W>) -> Predicted {
        let mut ps = snap.ps.clone();
        let base = ps.command_time;
        let mut inv = PlayerWeapons::from_words(&snap.inv);
        // Commands the server has run are done with; the newest of them is the "old" command
        // the next one is compared with.
        while self.cmds.len() > 1 && self.cmds[1].server_time <= ps.command_time {
            self.cmds.pop_front();
        }
        let mut old = self
            .cmds
            .front()
            .filter(|c| c.server_time <= ps.command_time)
            .copied()
            .unwrap_or_default();
        let mut replayed = 0;
        // The previous pass's end, as this pass saw that moment, to measure the disagreement.
        let mut at_last = None;
        for cmd in self.cmds.iter().filter(|c| c.server_time > base) {
            if cmd.server_time - ps.command_time < 1 {
                continue;
            }
            run_usercmd(
                &mut ps,
                &mut inv,
                *cmd,
                old,
                env.speed,
                env.weapons,
                env.params,
                env.world,
            );
            old = *cmd;
            replayed += 1;
            if self.last.is_some_and(|(t, _)| t == cmd.server_time) {
                at_last = Some(ps.origin);
            }
        }
        if let (Some((t, was)), Some(now)) = (
            self.last,
            at_last.or_else(|| {
                // The previous end was acknowledged meanwhile: the snapshot is that moment.
                self.last
                    .filter(|(t, _)| *t == snap.ps.command_time)
                    .map(|_| snap.ps.origin)
            }),
        ) {
            let d = [was[0] - now[0], was[1] - now[1], was[2] - now[2]];
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            if len > SNAP_DISTANCE {
                self.error = [0.0; 3];
            } else if len > NOISE {
                // Whatever of the old error is left still counts.
                let cur = self.error_at(t);
                self.error = [cur[0] + d[0], cur[1] + d[1], cur[2] + d[2]];
                self.error_from = t;
                self.corrections += 1;
            }
        }
        self.last = Some((ps.command_time, ps.origin));
        Predicted { ps, inv, replayed }
    }

    /// The fading disagreement to add to the predicted origin at command time `t`.
    pub fn error_at(&self, t: i32) -> [f32; 3] {
        let k = 1.0 - (t - self.error_from) as f32 / SMOOTH_MS as f32;
        if k <= 0.0 {
            return [0.0; 3];
        }
        self.error.map(|e| e * k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim::pm::test_world::TestWorld;
    use sim::pm::{PmType, button};

    fn table() -> WeaponTable {
        WeaponTable::from_infos(Vec::new()).expect("empty table")
    }

    fn cmd(t: i32, forward: i8, yaw: i32) -> UserCmd {
        UserCmd {
            server_time: t,
            forwardmove: forward,
            angles: [0, yaw, 0],
            buttons: if t % 400 == 0 { button::JUMP } else { 0 },
            ..UserCmd::default()
        }
    }

    fn start() -> PlayerState {
        PlayerState {
            origin: [0.0, 0.0, 0.0],
            pm_type: PmType::Normal,
            view_height_target: sim::pm::VIEW_STAND,
            view_height_current: sim::pm::VIEW_STAND as f32,
            speed: 190,
            gravity: 800,
            ..PlayerState::default()
        }
    }

    /// The server's reference run of `cmds`, returning the state after each.
    fn serve(cmds: &[UserCmd], env: &Env<'_, TestWorld>) -> Vec<PlayerState> {
        let (mut ps, mut inv, mut old) = (start(), PlayerWeapons::default(), UserCmd::default());
        cmds.iter()
            .map(|c| {
                run_usercmd(
                    &mut ps,
                    &mut inv,
                    *c,
                    old,
                    env.speed,
                    env.weapons,
                    env.params,
                    env.world,
                );
                old = *c;
                ps.clone()
            })
            .collect()
    }

    fn snapshot(ps: &PlayerState) -> Snapshot {
        let mut s = Snapshot::empty();
        s.ps = ps.clone();
        s
    }

    #[test]
    fn replaying_unacknowledged_commands_reaches_the_servers_state() {
        let world = TestWorld::floor();
        let (w, p) = (table(), Params::default());
        let env = Env {
            world: &world,
            weapons: &w,
            params: &p,
            speed: 190,
        };
        let cmds: Vec<UserCmd> = (1..=60)
            .map(|i| cmd(i * 16, 127, (i * 300) % 65536))
            .collect();
        let truth = serve(&cmds, &env);
        let mut pr = Predictor::default();
        for c in &cmds {
            pr.push(*c);
        }
        // The server has run the first 40; the client has sent 60.
        let out = pr.predict(&snapshot(&truth[39]), &env);
        assert_eq!(out.replayed, 20);
        assert_eq!(out.ps, truth[59]);
        // A later snapshot (the server caught up to 50) predicts the same end.
        let out = pr.predict(&snapshot(&truth[49]), &env);
        assert_eq!(out.replayed, 10);
        assert_eq!(out.ps, truth[59]);
        assert_eq!(pr.corrections, 0);
    }

    #[test]
    fn a_server_correction_fades_instead_of_jumping() {
        let world = TestWorld::floor();
        let (w, p) = (table(), Params::default());
        let env = Env {
            world: &world,
            weapons: &w,
            params: &p,
            speed: 190,
        };
        let cmds: Vec<UserCmd> = (1..=40).map(|i| cmd(i * 16, 127, 0)).collect();
        let truth = serve(&cmds, &env);
        let mut pr = Predictor::default();
        for c in &cmds {
            pr.push(*c);
        }
        let before = pr.predict(&snapshot(&truth[19]), &env).ps.origin;
        // The server pushed the player back 20 units at command 20.
        let mut pushed = truth[19].clone();
        pushed.origin[0] -= 20.0;
        let after = pr.predict(&snapshot(&pushed), &env).ps;
        assert!(
            (before[0] - after.origin[0] - 20.0).abs() < 1.0,
            "prediction follows the server"
        );
        let end = after.command_time;
        let shown = |t: i32| after.origin[0] + pr.error_at(t)[0];
        assert!(
            (shown(end) - before[0]).abs() < 1.0,
            "no visible jump when the correction arrives"
        );
        assert!(
            (shown(end + SMOOTH_MS) - after.origin[0]).abs() < 1e-3,
            "fully corrected after the fade"
        );
        assert_eq!(pr.corrections, 1);
    }

    #[test]
    fn a_teleport_is_not_slid() {
        let world = TestWorld::floor();
        let (w, p) = (table(), Params::default());
        let env = Env {
            world: &world,
            weapons: &w,
            params: &p,
            speed: 190,
        };
        let cmds: Vec<UserCmd> = (1..=20).map(|i| cmd(i * 16, 127, 0)).collect();
        let truth = serve(&cmds, &env);
        let mut pr = Predictor::default();
        for c in &cmds {
            pr.push(*c);
        }
        pr.predict(&snapshot(&truth[9]), &env);
        let mut moved = truth[9].clone();
        moved.origin[1] += 1000.0;
        let out = pr.predict(&snapshot(&moved), &env);
        assert_eq!(pr.error_at(out.ps.command_time), [0.0; 3]);
    }

    #[test]
    fn commands_the_server_already_ran_are_not_replayed_twice() {
        let world = TestWorld::floor();
        let (w, p) = (table(), Params::default());
        let env = Env {
            world: &world,
            weapons: &w,
            params: &p,
            speed: 190,
        };
        let cmds: Vec<UserCmd> = (1..=10).map(|i| cmd(i * 16, 127, 0)).collect();
        let truth = serve(&cmds, &env);
        let mut pr = Predictor::default();
        for c in &cmds {
            pr.push(*c);
        }
        let out = pr.predict(&snapshot(&truth[9]), &env);
        assert_eq!(out.replayed, 0);
        assert_eq!(out.ps, truth[9]);
    }
}
