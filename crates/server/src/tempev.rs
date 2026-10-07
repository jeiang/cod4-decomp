// SPDX-License-Identifier: GPL-3.0-or-later
//! One-shot happenings clients show or hear but do not simulate: bullet impacts, explosions, script effects, deaths.
//!
//! They travel as entities of type [`net::entity::etype::EVENT`] in every snapshot for [`LIFETIME_MS`] after they
//! happen, so a lost snapshot does not lose them (the temporary entities of Quake 3). Each carries a sequence number
//! in `event_seq`; a client acts on every (entity number, sequence) pair once.
//!
//! Field use: `event` is one of [`ev`], `origin` where, `angles` the surface normal or direction in degrees, `client`
//! the entity that caused it (the shooter, the owner of a missile, the player a death or pain is about), `weapon` the
//! weapon index, `event_parm` the surface type (or damage for pain), `model` the effect index (script effects) and
//! `velocity` a direction scaled to push a body (deaths).

use net::entity::{EntityState, etype};

/// Event kinds; all above the player movement events of `sim::pm::ev`, which share the field on player entities.
pub mod ev {
    pub const BULLET_IMPACT: u8 = 0x80;
    pub const EXPLOSION: u8 = 0x81;
    pub const MISSILE_BOUNCE: u8 = 0x82;
    pub const PLAY_FX: u8 = 0x83;
    pub const PLAYER_DEATH: u8 = 0x84;
    pub const PLAYER_PAIN: u8 = 0x85;
    /// A physics blast (`physicsexplosionsphere`): `velocity[0]` carries the radius, `weapon` the strength in tenths.
    pub const PHYSICS_EXPLOSION: u8 = 0x86;
}

pub const LIFETIME_MS: i32 = 300;
/// Entity numbers events use, above anything the game allocates.
const FIRST: u16 = 960;
const SLOTS: u16 = 48;

struct Slot {
    state: EntityState,
    expires: i32,
}

#[derive(Default)]
pub struct TempEvents {
    slots: Vec<Option<Slot>>,
    next: u16,
    seq: u8,
}

impl TempEvents {
    /// Records an event at server time `now`; `fill` sets its fields.
    pub fn add(&mut self, now: i32, event: u8, fill: impl FnOnce(&mut EntityState)) {
        if self.slots.is_empty() {
            self.slots.resize_with(usize::from(SLOTS), || None);
        }
        let i = usize::from(self.next % SLOTS);
        self.next = (self.next + 1) % SLOTS;
        self.seq = self.seq.wrapping_add(1).max(1);
        let mut s = EntityState::new(FIRST + i as u16);
        s.etype = etype::EVENT;
        s.event = event;
        s.event_seq = self.seq;
        fill(&mut s);
        self.slots[i] = Some(Slot {
            state: s.canonical(),
            expires: now + LIFETIME_MS,
        });
    }

    /// The events still live at `now`, in entity-number order.
    pub fn live(&self, now: i32) -> impl Iterator<Item = &EntityState> {
        self.slots
            .iter()
            .flatten()
            .filter(move |s| s.expires > now)
            .map(|s| &s.state)
    }
}

/// Angles (degrees) of a direction.
pub fn dir_to_angles(d: [f32; 3]) -> [f32; 3] {
    let yaw = d[1].atan2(d[0]).to_degrees();
    let pitch = -d[2].atan2(d[0].hypot(d[1])).to_degrees();
    [pitch, yaw, 0.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_live_for_the_lifetime_and_get_distinct_sequences() {
        let mut t = TempEvents::default();
        t.add(1000, ev::EXPLOSION, |s| s.origin = [1.0, 2.0, 3.0]);
        t.add(1100, ev::BULLET_IMPACT, |_| {});
        let at = |now| t.live(now).map(|s| (s.event, s.event_seq)).collect::<Vec<_>>();
        assert_eq!(at(1200), [(ev::EXPLOSION, 1), (ev::BULLET_IMPACT, 2)]);
        assert_eq!(at(1350), [(ev::BULLET_IMPACT, 2)]);
        assert!(at(1500).is_empty());
    }

    #[test]
    fn a_full_ring_reuses_the_oldest_slot() {
        let mut t = TempEvents::default();
        for i in 0..=SLOTS {
            t.add(0, ev::PLAY_FX, |s| s.model = i);
        }
        assert_eq!(t.live(1).count(), usize::from(SLOTS));
        assert!(t.live(1).any(|s| s.model == SLOTS));
        assert!(!t.live(1).any(|s| s.model == 0));
    }
}
