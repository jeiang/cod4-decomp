// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (cgame_mp/cg_players_mp.cpp CG_Corpse, cgame_mp/cg_ents_mp.cpp corpse and ragdoll creation, ragdoll/ragdoll_update.cpp Ragdoll_EnterRunning; GPL-3.0, copyright the KisakCOD contributors and Activision).
//! The bodies of dead players. The server's `clonePlayer` leaves a corpse entity that outlives its owner's life; each
//! client turns the entity into a ragdoll the first time it sees it (from the pose the player was last seen in, thrown
//! by the death's impulse) and draws that ragdoll as long as the entity is in the snapshots. A blast near a body wakes
//! it and throws it.

use super::{NetPlay, pose_input, team_of};
use crate::models::{Player, PlayerModelSet};
use crate::props::blast_of;
use crate::ragdoll::{MAX_SIMULATING, Ragdoll};
use net::entity::{EntityState, etype};
use render::ModelInstance;
use std::collections::HashMap;
use std::time::Duration;
use web_time::Instant;

/// How long a body out of the snapshots goes on simulating; after that it is settled at its resting pose, so one that
/// returns to view lies where it came to rest instead of falling again.
const KEEP: Duration = Duration::from_secs(5);
/// How long a body the snapshots no longer carry is remembered at all.
const FORGET: Duration = Duration::from_secs(600);
/// A body first seen with no pose to fall from plays its death clip this long (in steps) before it is made.
const SETTLE_STEPS: usize = 12;
const SETTLE_STEP: f32 = 0.25;

/// One corpse entity's body.
pub(super) struct Body {
    /// The spawn counter the server's entity carried: another in the same entity number is another body.
    seq: u8,
    player: Player,
    at: [f32; 3],
    /// `None` without a ragdoll definition: the body keeps the pose it was made in.
    ragdoll: Option<Ragdoll>,
    seen: Instant,
    settled: bool,
}

impl NetPlay {
    /// The body the scripts gave a player (`model`), failing that the stock body of the team in `flags`.
    pub(super) fn body_set(&self, model: u16, flags: u32) -> Option<PlayerModelSet> {
        self.net
            .ui_ref()
            .map(|u| u.model(model).to_owned())
            .and_then(|n| self.lib.body_models(&n))
            .or_else(|| team_of(flags).and_then(|t| self.lib.team_models(t)))
    }

    /// Makes the body of the corpse entity `e`: from the pose its owner was last seen in if the client has it, else
    /// from the end of its death clip.
    fn make_body(&mut self, e: &EntityState, now: Instant) -> Option<Body> {
        let set = self.body_set(e.model, e.eflags)?;
        let mut player = match self.lib.player(&set) {
            Ok(p) => p,
            Err(f) => {
                self.c.player_faults.insert(f);
                return None;
            }
        };
        let push = self.pushes.remove(&e.client).unwrap_or_default();
        let live = self
            .remotes
            .get(&e.client)
            .filter(|r| r.body == set.body)
            .and_then(|r| r.player.ragdoll(&self.lib.ragdoll, e.origin, push));
        let mut ragdoll = live.or_else(|| {
            let input = pose_input(e, None, true);
            for _ in 0..SETTLE_STEPS {
                player.update(SETTLE_STEP, &input);
            }
            player.ragdoll(&self.lib.ragdoll, e.origin, push)
        });
        if let Some(r) = &mut ragdoll {
            self.c.ragdolls += 1;
            let running = self
                .corpses
                .values()
                .filter(|b| b.ragdoll.as_ref().is_some_and(|r| !r.at_rest()))
                .count();
            if running >= MAX_SIMULATING {
                r.freeze();
            }
        }
        Some(Body {
            seq: e.event_seq,
            player,
            at: e.origin,
            ragdoll,
            seen: now,
            settled: false,
        })
    }

    /// The models of every corpse entity in `ents`, simulated by `dt`. Bodies the snapshots no longer carry go after
    /// [`KEEP`].
    pub(super) fn corpse_models(
        &mut self,
        dt: f32,
        ents: &[EntityState],
        now: Instant,
    ) -> Vec<ModelInstance> {
        let mut out = Vec::new();
        let mut drawn = 0;
        for e in ents.iter().filter(|e| e.etype == etype::CORPSE) {
            if self
                .corpses
                .get(&e.number)
                .is_none_or(|b| b.seq != e.event_seq)
            {
                self.corpses.remove(&e.number);
                if let Some(b) = self.make_body(e, now) {
                    self.corpses.insert(e.number, b);
                }
            }
            let Some(b) = self.corpses.get_mut(&e.number) else {
                continue;
            };
            b.seen = now;
            b.settled = false;
            drawn += 1;
            match &mut b.ragdoll {
                Some(r) => {
                    r.update(dt, self.boxes.world());
                    if let Some(hit) = r.take_impact() {
                        self.ragdoll_hits.push(crate::props::heard(
                            self.boxes.world(),
                            crate::ragdoll::SOUND.into(),
                            &hit,
                        ));
                    }
                    out.extend(b.player.instances_posed(b.at, r.yaw(), &r.bones()));
                }
                None => out.extend(b.player.instances(b.at)),
            }
        }
        self.c.corpses_max = self.c.corpses_max.max(drawn);
        let world = self.boxes.world();
        for b in self.corpses.values_mut() {
            if !b.settled && now - b.seen >= KEEP {
                b.settled = true;
                if let Some(r) = &mut b.ragdoll {
                    // Fast-forward to rest: a body's life is bounded, so this is.
                    for _ in 0..64 {
                        if r.at_rest() {
                            break;
                        }
                        r.update(0.1, world);
                    }
                    r.freeze();
                }
            }
        }
        self.corpses.retain(|_, b| now - b.seen < FORGET);
        out
    }
}

/// A blast or physics event near the bodies wakes and throws them, as many as may simulate at once.
pub(super) fn blast(
    corpses: &mut HashMap<u16, Body>,
    ev: &crate::events::ClientEvent,
    weapon: &dyn Fn(u16) -> Option<std::sync::Arc<assets::zone::weapon::WeaponDef>>,
) {
    let Some((at, b)) = blast_of(ev, weapon) else {
        return;
    };
    let mut awake = corpses
        .values()
        .filter(|c| c.ragdoll.as_ref().is_some_and(|r| !r.at_rest()))
        .count();
    for r in corpses.values_mut().filter_map(|c| c.ragdoll.as_mut()) {
        // A sleeping body wakes only if there is room for it to simulate.
        if r.at_rest() && awake >= MAX_SIMULATING {
            continue;
        }
        let was_asleep = r.at_rest();
        if r.explode(at, &b) && was_asleep {
            awake += 1;
        }
    }
}
