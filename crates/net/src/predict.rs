// SPDX-License-Identifier: GPL-3.0-only
// The carry by a ground mover follows `CG_AdjustPositionForMover` of KisakCOD (cgame_mp/cg_ents_mp.cpp; GPL-3.0, copyright the KisakCOD contributors and Activision).
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
use sim::pm::{PLAYER_MAXS, PLAYER_MINS, ef, ev, pmf};
use sim::pm::{Params, PlayerState, PmType, UserCmd, run_usercmd};
use sim::weapon::{PlayerWeapons, WeaponTable};
use sim::world::{ClipEnt, World};
use std::collections::VecDeque;
use std::sync::Arc;

/// Commands kept: about two seconds at 60 per second, far more than any round trip.
const KEEP: usize = 128;
/// A disagreement no larger than this is rounding (`CG_PredictPlayerState`).
const NOISE: f32 = 0.1;
/// How long a disagreement takes to fade, in milliseconds.
pub const SMOOTH_MS: i32 = 100;
/// `cg_viewZSmoothingMin`: a step smaller than this (units) is not smoothed.
pub const STEP_MIN: f32 = 1.0;
/// `cg_viewZSmoothingMax`: the most of a step (units) the view lags behind.
pub const STEP_MAX: f32 = 16.0;
/// `cg_viewZSmoothingTime` in milliseconds: how long the view takes to catch up with a step.
pub const STEP_MS: i32 = 100;

/// The view's lag behind the player's steps up stairs and ledges (`stepViewChange` / `stepViewStart`): the body is
/// where prediction puts it at once, the eye eases to the new height over [`STEP_MS`].
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct StepView {
    change: f32,
    start: i32,
}

impl StepView {
    /// How far to lower the eye below its true height at command time `now` (`CG_SmoothCameraZ`): the part of the
    /// last step the eye has not covered yet.
    pub fn offset(&self, now: i32) -> f32 {
        let since = now - self.start;
        if self.change == 0.0 || since < 0 {
            return 0.0;
        }
        let lerp = if since < STEP_MS {
            since as f32 / STEP_MS as f32
        } else {
            1.0
        };
        (1.0 - lerp) * self.change
    }

    /// Takes the steps a prediction pass found (`view_change` in total, the newest at `view_change_time`; unchanged
    /// `start` when it found none) at time `now`. `smooth` is false for a teleport or a state that does not walk.
    fn update(&mut self, now: i32, view_change: f32, view_change_time: i32, smooth: bool) {
        let settle = |s: &mut Self| {
            if STEP_MS < now - s.start {
                s.change = 0.0;
            }
        };
        if view_change == 0.0
            || view_change_time == self.start
            || !smooth
            || view_change.abs() < STEP_MIN
        {
            settle(self);
            return;
        }
        // The earlier step's share the eye had not covered when this one happened carries over.
        let mut left = 0.0;
        if STEP_MS > now - self.start {
            let since = view_change_time - self.start;
            if (0..STEP_MS).contains(&since) {
                left = (1.0 - since as f32 / STEP_MS as f32) * self.change;
            }
        }
        self.change = (view_change + left).clamp(-STEP_MAX, STEP_MAX);
        self.start = view_change_time;
    }
}

/// The map's collision plus the other players and the movers as the newest snapshot has them: people block
/// each other, so a replay that ignored them would walk through a player the server stopped at, and a lift not
/// there would drop the player standing on it.
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
            if e.etype == crate::entity::etype::BRUSH {
                // A mover stands where the snapshot has it; `Predictor::predict` carries the player on it from there.
                self.world.link(
                    e.number,
                    &ClipEnt {
                        contents: e.eflags as i32,
                        origin: e.origin,
                        angles: e.angles,
                        brush_model: Some(e.model),
                        ..ClipEnt::EMPTY
                    },
                );
                self.linked.push(e.number);
                continue;
            }
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
    /// The client's time (what its commands are stamped with), for easing the view over steps.
    pub time: i32,
}

pub struct Predicted {
    pub ps: PlayerState,
    pub inv: PlayerWeapons,
    /// Commands replayed on top of the snapshot.
    pub replayed: usize,
    /// The own player's events (event, parameter) no earlier pass delivered, oldest first: footsteps, jumps,
    /// landings, shots, reloads. Each comes out once however often its command is replayed.
    pub events: Vec<(u8, u8)>,
}

#[derive(Default)]
pub struct Predictor {
    cmds: VecDeque<UserCmd>,
    /// Where the last replay ended: its final command's time and the origin it reached.
    last: Option<(i32, [f32; 3])>,
    /// The disagreement being faded: offset to add to the predicted origin, and the client time it began.
    error: [f32; 3],
    error_from: i32,
    /// The server's teleport bit as the previous pass's snapshot had it.
    tele_bit: Option<u32>,
    /// The server's own event counter and spawn count as the last snapshot had them, `None` until the first pass.
    server_events: Option<(u8, u16)>,
    /// The predicted events (by event number) delivered and not yet confirmed by a snapshot's own events.
    pending: Vec<u8>,
    /// The eye's lag behind the steps taken.
    step: StepView,
    /// Prediction passes whose result differed from the previous pass, beyond rounding.
    pub corrections: u64,
    /// Steps up or down the eye began easing over.
    pub steps: u64,
    /// The vertical distance of the step the last pass began easing over, `0.0` when it began none.
    pub step_taken: f32,
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// How far the mover the player stands on has gone from where `snap` has it by server time `at`.
pub fn ground_carry(snap: &Snapshot, ps: &PlayerState, at: i32) -> [f32; 3] {
    let Some(m) = snap
        .entities
        .iter()
        .find(|e| e.etype == crate::entity::etype::BRUSH && e.number == ps.ground_entity_num)
    else {
        return [0.0; 3];
    };
    mover_offset(m.velocity, at - snap.server_time)
}

/// The way a mover moving at `velocity` (units per second) has gone in `ms`: what a client draws it ahead of its
/// snapshot by, and what it carries the player on it by. Capped, so a snapshot that stopped arriving does not send it
/// off.
pub fn mover_offset(velocity: [f32; 3], ms: i32) -> [f32; 3] {
    let s = ms.clamp(0, 250) as f32 * 0.001;
    velocity.map(|v| v * s)
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
        let mut carry = [0.0; 3];
        // Steps the replay finds, newer than the last one the eye already follows.
        let (mut view_change, mut view_change_time) = (0.0, self.step.start);
        // The previous pass's end, as this pass saw that moment, to measure the disagreement.
        let mut at_last = None;
        for cmd in self.cmds.iter().filter(|c| c.server_time > base) {
            if cmd.server_time - ps.command_time < 1 {
                continue;
            }
            let out = run_usercmd(
                &mut ps,
                &mut inv,
                *cmd,
                old,
                snap.ps.speed,
                env.weapons,
                env.params,
                env.world,
            );
            if out.view_change != 0.0 && view_change_time < out.view_change_time {
                view_change += out.view_change;
                view_change_time = out.view_change_time;
            }
            old = *cmd;
            replayed += 1;
            carry = ground_carry(snap, &ps, cmd.server_time);
            if self.last.is_some_and(|(t, _)| t == cmd.server_time) {
                at_last = Some(add(ps.origin, carry));
            }
        }
        // The commands ran against the movers where the snapshot has them; the one under the player has gone on since
        // (`CG_AdjustPositionForMover`).
        ps.origin = add(ps.origin, carry);
        // The server flips the teleport bit when it moves the player (a respawn, `setorigin`): that, not how far,
        // makes a disagreement a teleport, shown as one instead of slid.
        let tele_bit = snap.ps.e_flags & ef::TELEPORT_BIT;
        let teleported = self
            .tele_bit
            .replace(tele_bit)
            .is_some_and(|b| b != tele_bit);
        if teleported {
            self.error = [0.0; 3];
        } else if let (Some((_, was)), Some(now)) = (
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
            if len > NOISE {
                // Whatever of the old error is left still counts.
                let cur = self.error_at(env.time);
                self.error = [cur[0] + d[0], cur[1] + d[1], cur[2] + d[2]];
                self.error_from = env.time;
                self.corrections += 1;
            }
        }
        self.last = Some((ps.command_time, ps.origin));
        if teleported {
            self.step = StepView::default();
        }
        let walks = matches!(ps.pm_type, PmType::Normal | PmType::Noclip | PmType::Ufo);
        let before = self.step.start;
        self.step.update(
            env.time,
            view_change,
            view_change_time,
            walks && !teleported,
        );
        let began = self.step.start != before;
        self.steps += u64::from(began);
        self.step_taken = if began { view_change } else { 0.0 };
        let events = self.fresh_events(&snap.ps, &ps);
        Predicted {
            ps,
            inv,
            replayed,
            events,
        }
    }

    /// How far to lower the eye at time `now` for the stair steps taken ([`StepView::offset`]).
    pub fn step_offset(&self, now: i32) -> f32 {
        self.step.offset(now)
    }

    /// The fading disagreement to add to the predicted origin at client time `now`.
    pub fn error_at(&self, now: i32) -> [f32; 3] {
        let k = 1.0 - (now - self.error_from) as f32 / SMOOTH_MS as f32;
        if k <= 0.0 {
            return [0.0; 3];
        }
        self.error.map(|e| e * k)
    }

    /// Forgets which own events were delivered: the next pass only learns the counters. For after the player state
    /// was someone else's (a followed player), whose event ring says nothing about the player's own.
    pub fn resync_events(&mut self) {
        self.server_events = None;
        self.pending.clear();
    }

    /// The own events no earlier pass delivered, oldest first (`CG_CheckPlayerstateEvents`). `server` is the
    /// snapshot's state, `ps` the replayed one: the events the server raised since the last snapshot, and the ones
    /// the replay raised beyond them.
    ///
    /// An event is identified by its number alone (its parameter, a fall depth, may differ between replays). A
    /// server event that a delivered predicted one stands for is its confirmation and plays nothing; any other
    /// (a pickup the client could not predict) plays, whatever its place among the predicted ones. A predicted
    /// event a replay raises again plays nothing; one the replay no longer raises is forgotten, so it cannot
    /// swallow a later event of its kind.
    fn fresh_events(&mut self, server: &PlayerState, ps: &PlayerState) -> Vec<(u8, u8)> {
        let ring = |ps: &PlayerState, from: u8, to: u8| -> Vec<(u8, u8)> {
            let n = to.wrapping_sub(from).min(4);
            (1..=n)
                .rev()
                .map(|back| usize::from(to.wrapping_sub(back) & 3))
                .map(|slot| (ps.events[slot], ps.event_parms[slot]))
                .filter(|(e, _)| *e != ev::NONE)
                .collect()
        };
        let (seq, spawn) = (server.event_sequence, server.spawn_count);
        let predicted = ring(ps, seq, ps.event_sequence);
        let known = match self.server_events.replace((seq, spawn)) {
            Some((_, old_spawn)) if old_spawn != spawn => {
                // A new life counts from zero.
                self.pending.clear();
                Some(0)
            }
            Some((old, _)) => Some(old),
            None => None,
        };
        let Some(old) = known else {
            // The first pass only learns.
            self.pending = predicted.iter().map(|(e, _)| *e).collect();
            return Vec::new();
        };
        let mut out = Vec::new();
        // A counter that went backwards (a snapshot out of order) only resyncs.
        let behind = seq.wrapping_sub(old) > 128;
        if !behind {
            for (event, parm) in ring(server, old, seq) {
                match self.pending.iter().position(|p| *p == event) {
                    Some(i) => {
                        self.pending.remove(i);
                    }
                    None => out.push((event, parm)),
                }
            }
        }
        // What is left of the old prediction is a prefix of the new one, less the part that scrolled out of the
        // four slots: the smallest cut that leaves the rest in order among the new events.
        let ids: Vec<u8> = predicted.iter().map(|(e, _)| *e).collect();
        let covered = |cut: usize| {
            let mut next = ids.iter();
            self.pending[cut..].iter().all(|p| next.any(|e| e == p))
        };
        let cut = (0..=self.pending.len())
            .find(|&c| covered(c))
            .unwrap_or(self.pending.len());
        let mut old_events = self.pending[cut..].iter().peekable();
        for &(event, parm) in &predicted {
            if old_events.peek() == Some(&&event) {
                old_events.next();
            } else {
                out.push((event, parm));
            }
        }
        self.pending = ids;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityState;
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
                    190,
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
            time: 0,
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
            time: 0,
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
        let shown = |t: i32| after.origin[0] + pr.error_at(t)[0];
        assert!(
            (shown(0) - before[0]).abs() < 1.0,
            "no visible jump when the correction arrives"
        );
        assert!(
            (shown(SMOOTH_MS) - after.origin[0]).abs() < 1e-3,
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
            time: 0,
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
        moved.e_flags ^= ef::TELEPORT_BIT;
        pr.predict(&snapshot(&moved), &env);
        assert_eq!(pr.error_at(0), [0.0; 3]);
        assert_eq!(pr.corrections, 0);
    }

    #[test]
    fn a_large_shove_without_the_teleport_bit_is_slid() {
        let world = TestWorld::floor();
        let (w, p) = (table(), Params::default());
        let env = Env {
            world: &world,
            weapons: &w,
            params: &p,
            time: 0,
        };
        let cmds: Vec<UserCmd> = (1..=20).map(|i| cmd(i * 16, 127, 0)).collect();
        let truth = serve(&cmds, &env);
        let mut pr = Predictor::default();
        for c in &cmds {
            pr.push(*c);
        }
        pr.predict(&snapshot(&truth[9]), &env);
        let mut shoved = truth[9].clone();
        shoved.origin[1] += 100.0;
        pr.predict(&snapshot(&shoved), &env);
        assert!(pr.error_at(0)[1].abs() > 90.0);
        assert_eq!(pr.corrections, 1);
    }

    #[test]
    fn a_miss_of_a_tenth_of_a_unit_is_not_a_correction() {
        let world = TestWorld::floor();
        let (w, p) = (table(), Params::default());
        let env = Env {
            world: &world,
            weapons: &w,
            params: &p,
            time: 0,
        };
        let cmds: Vec<UserCmd> = (1..=20).map(|i| cmd(i * 16, 127, 0)).collect();
        let truth = serve(&cmds, &env);
        let mut pr = Predictor::default();
        for c in &cmds {
            pr.push(*c);
        }
        pr.predict(&snapshot(&truth[9]), &env);
        let mut off = truth[9].clone();
        off.origin[0] += 0.05;
        pr.predict(&snapshot(&off), &env);
        assert_eq!(pr.corrections, 0);
    }

    #[test]
    fn the_server_speed_in_the_snapshot_is_what_is_predicted() {
        let world = TestWorld::floor();
        let (w, p) = (table(), Params::default());
        let env = Env {
            world: &world,
            weapons: &w,
            params: &p,
            time: 0,
        };
        let cmds: Vec<UserCmd> = (1..=30).map(|i| cmd(i * 16, 127, 0)).collect();
        let mut fast = start();
        fast.speed = 250;
        let (mut ps, mut inv, mut old) =
            (fast.clone(), PlayerWeapons::default(), UserCmd::default());
        for c in &cmds {
            run_usercmd(&mut ps, &mut inv, *c, old, 250, &w, &p, &world);
            old = *c;
        }
        let mut pr = Predictor::default();
        for c in &cmds {
            pr.push(*c);
        }
        let out = pr.predict(&snapshot(&fast), &env);
        assert_eq!(out.ps.origin, ps.origin);
        assert_eq!(pr.corrections, 0);
    }

    /// The server's state with `events` in its ring, and the replayed state with `more` on top.
    fn states(events: &[(u8, u8)], more: &[(u8, u8)]) -> (PlayerState, PlayerState) {
        let mut server = start();
        for &(e, p) in events {
            server.add_event(e, u32::from(p));
        }
        let mut ps = server.clone();
        for &(e, p) in more {
            ps.add_event(e, u32::from(p));
        }
        (server, ps)
    }

    fn pass(pr: &mut Predictor, events: &[(u8, u8)], more: &[(u8, u8)]) -> Vec<(u8, u8)> {
        let (server, ps) = states(events, more);
        pr.fresh_events(&server, &ps)
    }

    #[test]
    fn an_event_replayed_every_pass_is_delivered_once() {
        let mut pr = Predictor::default();
        assert!(pass(&mut pr, &[], &[]).is_empty(), "the first pass learns");
        // Passes replay the same jump from the same snapshot, as the server has not caught up.
        assert_eq!(pass(&mut pr, &[], &[(ev::JUMP, 5)]), [(ev::JUMP, 5)]);
        for _ in 0..3 {
            assert!(pass(&mut pr, &[], &[(ev::JUMP, 5)]).is_empty());
        }
        // The server catches up and raises the same jump: a confirmation, not a second jump.
        assert!(pass(&mut pr, &[(ev::JUMP, 6)], &[]).is_empty());
        // A new event is delivered.
        assert_eq!(
            pass(&mut pr, &[(ev::JUMP, 6)], &[(ev::FIRE_WEAPON, 0)]),
            [(ev::FIRE_WEAPON, 0)]
        );
    }

    /// A pickup only the server knows of lands before a predicted jump in the same window.
    #[test]
    fn an_unpredicted_server_event_before_a_predicted_one_does_not_replay_it() {
        let mut pr = Predictor::default();
        pass(&mut pr, &[], &[]);
        assert_eq!(pass(&mut pr, &[], &[(ev::JUMP, 0)]), [(ev::JUMP, 0)]);
        // The next snapshot: the server raised the pickup, then the jump the client already played.
        let got = pass(&mut pr, &[(ev::ITEM_PICKUP, 2), (ev::JUMP, 0)], &[]);
        assert_eq!(got, [(ev::ITEM_PICKUP, 2)], "the jump is not played twice");
        assert!(pass(&mut pr, &[(ev::ITEM_PICKUP, 2), (ev::JUMP, 0)], &[]).is_empty());
    }

    /// The prediction raised an event the server never did (a mispredict); the same kind later is not swallowed.
    #[test]
    fn a_mispredicted_event_does_not_hide_the_real_one() {
        let mut pr = Predictor::default();
        pass(&mut pr, &[], &[]);
        assert_eq!(pass(&mut pr, &[], &[(ev::JUMP, 0)]), [(ev::JUMP, 0)]);
        // The replay no longer raises it (the server disagreed and ran on without a jump).
        assert!(pass(&mut pr, &[], &[]).is_empty());
        // A real jump later plays, from the prediction and then once only.
        assert_eq!(pass(&mut pr, &[], &[(ev::JUMP, 0)]), [(ev::JUMP, 0)]);
        assert!(pass(&mut pr, &[(ev::JUMP, 0)], &[]).is_empty());
        // A server counter that went backwards resyncs without delivering the old events again.
        assert!(pass(&mut pr, &[], &[]).is_empty());
        assert_eq!(
            pass(&mut pr, &[(ev::FOLIAGE_SOUND, 0)], &[]),
            [(ev::FOLIAGE_SOUND, 0)]
        );
    }

    #[test]
    fn a_new_life_delivers_its_first_events() {
        let mut pr = Predictor::default();
        pass(&mut pr, &[(ev::JUMP, 0)], &[]);
        let mut life = start();
        life.spawn_count = 1;
        life.add_event(ev::JUMP, 0);
        let ps = life.clone();
        assert_eq!(pr.fresh_events(&life, &ps), [(ev::JUMP, 0)]);
    }

    #[test]
    fn commands_the_server_already_ran_are_not_replayed_twice() {
        let world = TestWorld::floor();
        let (w, p) = (table(), Params::default());
        let env = Env {
            world: &world,
            weapons: &w,
            params: &p,
            time: 0,
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

    #[test]
    fn the_eye_eases_over_a_step_instead_of_snapping() {
        let mut world = TestWorld::floor();
        world.add(
            [60.0, -100.0, 0.0],
            [400.0, 100.0, 10.0],
            sim::contents::SOLID,
            0,
        );
        let (w, p) = (table(), Params::default());
        let cmds: Vec<UserCmd> = (1..=80).map(|i| cmd(i * 16 + 1, 127, 0)).collect();
        let env = |t| Env {
            world: &world,
            weapons: &w,
            params: &p,
            time: t,
        };
        let truth = serve(&cmds, &env(0));
        let mut pr = Predictor::default();
        let (mut worst_snap, mut stepped, mut last_eye) = (0.0f32, 0.0f32, None::<f32>);
        for (i, c) in cmds.iter().enumerate() {
            pr.push(*c);
            // The server is five commands behind.
            let acked = if i >= 5 {
                truth[i - 5].clone()
            } else {
                start()
            };
            let out = pr.predict(&snapshot(&acked), &env(c.server_time));
            let drop = pr.step_offset(c.server_time);
            stepped = stepped.max(drop.abs());
            let eye = out.ps.origin[2] + out.ps.view_height_current - drop;
            if let Some(last) = last_eye {
                worst_snap = worst_snap.max((eye - last).abs());
            }
            last_eye = Some(eye);
        }
        assert!(
            stepped > 5.0,
            "the 10-unit step was never smoothed: {stepped}"
        );
        assert!(worst_snap < 4.0, "the eye jumped {worst_snap} in one frame");
        assert_eq!(pr.step_offset(100_000), 0.0);
    }

    /// The curve of `CG_SmoothCameraZ`: the whole step at first, linear to nothing over `STEP_MS`, clamped.
    #[test]
    fn the_step_curve_is_linear_clamped_and_chained() {
        let mut s = StepView::default();
        s.update(1000, 10.0, 1000, true);
        assert_eq!(s.offset(1000), 10.0);
        assert!((s.offset(1050) - 5.0).abs() < 1e-4);
        assert_eq!(s.offset(1000 + STEP_MS), 0.0);
        // A second step half way carries the rest of the first.
        s.update(1050, 6.0, 1050, true);
        assert!((s.offset(1050) - 11.0).abs() < 1e-4);
        // Never more than the cap, either way.
        s.update(2000, 40.0, 2000, true);
        assert_eq!(s.offset(2000), STEP_MAX);
        s.update(3000, -40.0, 3000, true);
        assert_eq!(s.offset(3000), -STEP_MAX);
        // Too small, a state that does not walk, or no new step: nothing smoothed; an old one is dropped.
        let mut s = StepView::default();
        s.update(1000, 0.5, 1000, true);
        s.update(1000, 8.0, 1000, false);
        assert_eq!(s.offset(1000), 0.0);
        s.update(1000, 8.0, 1000, true);
        s.update(1000 + STEP_MS + 1, 0.0, 1000, true);
        assert_eq!(s.offset(1000), 0.0);
    }

    /// Standing on a lift that rises 100 units a second, the player is carried with it between the snapshot and the
    /// command the replay ends on, and by nothing when the thing under the feet stands still.
    #[test]
    fn the_player_is_carried_by_the_mover_it_stands_on() {
        let mut world = TestWorld::floor();
        world.blocks[0].entity = 5;
        let (w, p) = (table(), Params::default());
        let env = Env {
            world: &world,
            weapons: &w,
            params: &p,
            time: 0,
        };
        let lift = |velocity: [f32; 3]| EntityState {
            number: 5,
            etype: crate::entity::etype::BRUSH,
            velocity,
            ..EntityState::default()
        };
        let carried = |velocity: [f32; 3]| {
            let mut ps = start();
            ps.ground_entity_num = 5;
            ps.command_time = 1000;
            let mut snap = snapshot(&ps);
            snap.server_time = 1000;
            snap.entities.push(lift(velocity));
            let mut pr = Predictor::default();
            pr.push(UserCmd {
                server_time: 1100,
                ..UserCmd::default()
            });
            pr.predict(&snap, &env).ps.origin[2]
        };
        assert!((carried([0.0, 0.0, 100.0]) - 10.0).abs() < 1e-3);
        assert_eq!(carried([0.0; 3]), 0.0);
    }
}
