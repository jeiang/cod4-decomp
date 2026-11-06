// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (cgame_mp/cg_players_mp.cpp CG_Corpse, cgame_mp/cg_ents_mp.cpp corpse and ragdoll creation, ragdoll/ragdoll_update.cpp Ragdoll_EnterRunning; GPL-3.0, copyright the KisakCOD contributors and Activision).
//! The bodies of dead players. The server's `clonePlayer` leaves a corpse entity that outlives its owner's life; each
//! client turns the entity into a ragdoll the first time it sees it (from the pose the player was last seen in, thrown
//! by the death's impulse) and draws that ragdoll as long as the entity is in the snapshots. A blast near a body wakes
//! it and throws it.

use super::{NetPlay, pose_input};
use crate::models::Player;
use crate::props::blast_of;
use crate::ragdoll::Ragdoll;
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
    /// The input layer owns definitions for the life of the client. Only an edit copies a definition; only stock's
    /// resetting commands re-capture bodies, including bodies which have settled or hit their simulation limit.
    /// Stock retries a body without a ragdoll every frame (`CG_Corpse` → `Ragdoll_CreateRagdollForDObj`, which fails
    /// while the definition does not bind); the attempt can only succeed after an edit, so it runs on edits only.
    pub fn sync_ragdolls(&mut self, input: &mut crate::input::Input) {
        if self.ragdoll_revision.is_none() {
            // A new level's library: its zones' strings replace the previous map's for `ragdoll_pin_bone`.
            input.match_script_strings = self.lib.content.script_strings().clone();
        } else if self.ragdoll_revision == Some(input.ragdoll_revision) {
            return;
        }
        self.ragdoll_def = input.ragdoll_defs.get(1).clone();
        self.ragdoll_revision = Some(input.ragdoll_revision);
        let reset = self.ragdoll_reset_revision != input.ragdoll_reset_revision;
        self.ragdoll_reset_revision = input.ragdoll_reset_revision;
        let enabled = self.switches.ragdoll;
        let tun = self.switches.ragdoll_tunables;
        let mut running = if reset {
            0
        } else {
            self.corpses
                .values()
                .filter(|b| b.ragdoll.as_ref().is_some_and(|r| !r.at_rest()))
                .count()
        };
        for b in self.corpses.values_mut() {
            match b.ragdoll.take() {
                Some(old) if reset => {
                    self.ragdoll_resets += 1;
                    self.ragdoll_resting_resets += u64::from(old.at_rest());
                    b.at = old.root();
                    b.player.capture_ragdoll_pose(&old.bones(), old.yaw());
                    b.ragdoll = b.player.ragdoll(&self.ragdoll_def, b.at, [0.0; 3], tun);
                }
                Some(old) => {
                    b.ragdoll = Some(old);
                    continue;
                }
                None if enabled => {
                    b.ragdoll = b.player.ragdoll(&self.ragdoll_def, b.at, [0.0; 3], tun);
                    if b.ragdoll.is_some() {
                        self.c.ragdolls += 1;
                    }
                }
                None => continue,
            }
            if let Some(r) = &mut b.ragdoll {
                if running >= tun.max_simulating {
                    r.freeze();
                } else {
                    running += 1;
                }
            }
            b.settled = false;
        }
    }

    /// Live bodies, resting bodies, and cumulative resets (all/resting), for the command assertion.
    pub fn ragdoll_bodies_fact(&self) -> String {
        let live = self.corpses.values().filter_map(|b| b.ragdoll.as_ref());
        let (mut count, mut resting) = (0, 0);
        for r in live {
            count += 1;
            resting += usize::from(r.at_rest());
        }
        format!(
            "{count} {resting} {} {}",
            self.ragdoll_resets, self.ragdoll_resting_resets
        )
    }

    /// Makes the body of the corpse entity `e`: from the pose its owner was last seen in if the client has it, else
    /// from the end of its death clip.
    fn make_body(&mut self, e: &EntityState, now: Instant) -> Option<Body> {
        let set = self.body_set(e)?;
        let mut player = match self.lib.player(&set) {
            Ok(p) => p,
            Err(f) => {
                super::player_fault(&mut self.c.player_faults, e.client, f);
                return None;
            }
        };
        player.hide_parts(e.part_bits);
        let push = self.pushes.remove(&e.client).unwrap_or_default();
        // `ragdoll_enable` off: the body keeps the pose its death clip ends in, as it does without a ragdoll definition.
        let enabled = self.switches.ragdoll;
        let tun = self.switches.ragdoll_tunables;
        let live = self
            .remotes
            .get(&e.client)
            .filter(|r| enabled && r.body == set.body)
            .and_then(|r| r.player.ragdoll(&self.ragdoll_def, e.origin, push, tun));
        let mut ragdoll = live.or_else(|| {
            let input = pose_input(e, None, true);
            for _ in 0..SETTLE_STEPS {
                player.update(SETTLE_STEP, &input);
            }
            enabled
                .then(|| player.ragdoll(&self.ragdoll_def, e.origin, push, tun))
                .flatten()
        });
        if let Some(r) = &mut ragdoll {
            self.c.ragdolls += 1;
            let running = self
                .corpses
                .values()
                .filter(|b| b.ragdoll.as_ref().is_some_and(|r| !r.at_rest()))
                .count();
            if running >= self.switches.ragdoll_tunables.max_simulating {
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
            let before = out.len();
            match &mut b.ragdoll {
                Some(r) => {
                    r.tune(self.switches.ragdoll_tunables);
                    r.update(dt, self.boxes.world());
                    if let Some(hit) = r.take_impact() {
                        self.ragdoll_hits.push(crate::props::heard(
                            self.boxes.world(),
                            crate::ragdoll::SOUND.into(),
                            &hit,
                        ));
                    }
                    out.extend(b.player.instances_posed(r.root(), r.yaw(), &r.bones()));
                }
                None => out.extend(b.player.instances(b.at)),
            }
            // A script's `playfxontag` on the body rides its posed bones.
            self.effects
                .script_model_tags(e.number, e.model, &out[before..], &self.lib.content);
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
    max_simulating: usize,
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
        if r.at_rest() && awake >= max_simulating {
            continue;
        }
        let was_asleep = r.at_rest();
        if r.explode(at, &b) && was_asleep {
            awake += 1;
        }
    }
}
