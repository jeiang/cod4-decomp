// SPDX-License-Identifier: GPL-3.0-only
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
    /// A spherical physics blast (`physicsexplosionsphere`); see [`Physics`] for the fields.
    pub const PHYSICS_EXPLOSION: u8 = 0x86;
    /// A weapon fired (`FireWeapon`): `origin` is the shooter's eye, `angles` where they look, `client` who, `weapon`
    /// which; the muzzle flash and the ejected shell.
    pub const WEAPON_FIRE: u8 = 0x87;
    /// A cylindrical physics blast (`physicsexplosioncylinder`); see [`Physics`].
    pub const PHYSICS_EXPLOSION_CYLINDER: u8 = 0x88;
    /// `physicsjolt`; see [`Physics`].
    pub const PHYSICS_JOLT: u8 = 0x89;
    /// `physicsjitter`; see [`Physics`].
    pub const PHYSICS_JITTER: u8 = 0x8a;
    /// `earthquake`; see [`Earthquake`].
    pub const EARTHQUAKE: u8 = 0x8b;
}

/// Script numbers that travel in a velocity component, which the wire keeps to a quarter unit: scaled up to keep the
/// precision scripts use (0.4 of an earthquake, a jitter of 0.05).
const PARAM: f32 = 16.0;
/// Radii travel as 12 bits each in `eflags`.
const RADIUS_BITS: u32 = 12;
const RADIUS_MAX: f32 = ((1 << RADIUS_BITS) - 1) as f32;

fn pack_radii(outer: f32, inner: f32) -> u32 {
    let r = |v: f32| v.round().clamp(0.0, RADIUS_MAX) as u32;
    r(outer) | (r(inner) << RADIUS_BITS)
}

fn unpack_radii(eflags: u32) -> (f32, f32) {
    let m = (1 << RADIUS_BITS) - 1;
    ((eflags & m) as f32, ((eflags >> RADIUS_BITS) & m) as f32)
}

/// A physics world event the scripts raise: loose bodies within `outer` of the event's `origin` are pushed, fully
/// within `inner` and less with distance out to `outer`.
///
/// Fields: `eflags` holds the radii (12 bits each, whole units), `weapon` the magnitude in tenths, and `velocity` the
/// impulse or the displacements, times 16.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Physics {
    /// `physicsexplosionsphere` and `physicsexplosioncylinder`: thrown away from the origin; a cylinder ignores height.
    Explosion {
        cylinder: bool,
        outer: f32,
        inner: f32,
        magnitude: f32,
    },
    /// `physicsjolt`: thrown along `impulse`.
    Jolt {
        outer: f32,
        inner: f32,
        impulse: [f32; 3],
    },
    /// `physicsjitter`: hopped by a distance between `min` and `max`.
    Jitter {
        outer: f32,
        inner: f32,
        min: f32,
        max: f32,
    },
}

impl Physics {
    fn kind(&self) -> u8 {
        match self {
            Self::Explosion {
                cylinder: false, ..
            } => ev::PHYSICS_EXPLOSION,
            Self::Explosion { cylinder: true, .. } => ev::PHYSICS_EXPLOSION_CYLINDER,
            Self::Jolt { .. } => ev::PHYSICS_JOLT,
            Self::Jitter { .. } => ev::PHYSICS_JITTER,
        }
    }

    fn fill(&self, s: &mut EntityState) {
        match *self {
            Self::Explosion {
                outer,
                inner,
                magnitude,
                ..
            } => {
                s.eflags = pack_radii(outer, inner);
                s.weapon = (magnitude * 10.0).round().clamp(0.0, 511.0) as u16;
            }
            Self::Jolt {
                outer,
                inner,
                impulse,
            } => {
                s.eflags = pack_radii(outer, inner);
                s.velocity = impulse.map(|v| v * PARAM);
            }
            Self::Jitter {
                outer,
                inner,
                min,
                max,
            } => {
                s.eflags = pack_radii(outer, inner);
                s.velocity = [min * PARAM, max * PARAM, 0.0];
            }
        }
    }

    /// The event `s` carries, if it is a physics event.
    pub fn decode(s: &EntityState) -> Option<Self> {
        let (outer, inner) = unpack_radii(s.eflags);
        Some(match s.event {
            ev::PHYSICS_EXPLOSION | ev::PHYSICS_EXPLOSION_CYLINDER => Self::Explosion {
                cylinder: s.event == ev::PHYSICS_EXPLOSION_CYLINDER,
                outer,
                inner,
                magnitude: f32::from(s.weapon) / 10.0,
            },
            ev::PHYSICS_JOLT => Self::Jolt {
                outer,
                inner,
                impulse: s.velocity.map(|v| v / PARAM),
            },
            ev::PHYSICS_JITTER => Self::Jitter {
                outer,
                inner,
                min: s.velocity[0] / PARAM,
                max: s.velocity[1] / PARAM,
            },
            _ => return None,
        })
    }
}

/// `earthquake`: the camera of anyone within `radius` of the origin shakes, harder the nearer, for `duration_ms`.
///
/// Fields: `velocity` holds the scale times 16, the duration in milliseconds and the radius.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Earthquake {
    pub scale: f32,
    pub duration_ms: i32,
    pub radius: f32,
}

impl Earthquake {
    fn fill(&self, s: &mut EntityState) {
        s.velocity = [self.scale * PARAM, self.duration_ms as f32, self.radius];
    }

    /// The earthquake `s` carries, if it is one.
    pub fn decode(s: &EntityState) -> Option<Self> {
        (s.event == ev::EARTHQUAKE).then(|| Self {
            scale: s.velocity[0] / PARAM,
            duration_ms: s.velocity[1].round() as i32,
            radius: s.velocity[2],
        })
    }
}

pub const LIFETIME_MS: i32 = 300;
/// Entity numbers events use, above anything the game allocates.
const FIRST: u16 = 960;
/// Entity numbers 960 to 1021; 1022 and 1023 mean the world and none.
const SLOTS: u16 = 62;

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

    /// Records a physics world event at `origin`.
    pub fn add_physics(&mut self, now: i32, origin: [f32; 3], p: &Physics) {
        self.add(now, p.kind(), |s| {
            s.origin = origin;
            p.fill(s);
        });
    }

    /// Records an earthquake at `origin`.
    pub fn add_earthquake(&mut self, now: i32, origin: [f32; 3], q: &Earthquake) {
        self.add(now, ev::EARTHQUAKE, |s| {
            s.origin = origin;
            q.fill(s);
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
        let at = |now| {
            t.live(now)
                .map(|s| (s.event, s.event_seq))
                .collect::<Vec<_>>()
        };
        assert_eq!(at(1200), [(ev::EXPLOSION, 1), (ev::BULLET_IMPACT, 2)]);
        assert_eq!(at(1350), [(ev::BULLET_IMPACT, 2)]);
        assert!(at(1500).is_empty());
    }

    #[test]
    fn physics_and_earthquake_parameters_survive_the_wire() {
        let mut t = TempEvents::default();
        let sent = [
            Physics::Explosion {
                cylinder: true,
                outer: 600.0,
                inner: 150.0,
                magnitude: 2.5,
            },
            Physics::Jolt {
                outer: 300.0,
                inner: 0.0,
                impulse: [0.0, 0.5, -3.25],
            },
            Physics::Jitter {
                outer: 1200.0,
                inner: 100.0,
                min: 0.05,
                max: 2.0,
            },
        ];
        for p in &sent {
            t.add_physics(0, [1.0, 2.0, 3.0], p);
        }
        let quake = Earthquake {
            scale: 0.4,
            duration_ms: 2000,
            radius: 1000.0,
        };
        t.add_earthquake(0, [4.0, 5.0, 6.0], &quake);
        let got: Vec<_> = t.live(1).collect();
        for (s, p) in got.iter().zip(&sent) {
            let d = Physics::decode(s).expect("physics");
            match (d, *p) {
                (
                    Physics::Jitter { min, max, .. },
                    Physics::Jitter {
                        min: m0, max: x0, ..
                    },
                ) => assert!((min - m0).abs() < 0.02 && (max - x0).abs() < 0.02, "{d:?}"),
                _ => assert_eq!(d, *p),
            }
        }
        let q = Earthquake::decode(got[3]).expect("earthquake");
        assert!((q.scale - 0.4).abs() < 0.02 && q.duration_ms == 2000 && q.radius == 1000.0);
        assert_eq!(got[3].origin, [4.0, 5.0, 6.0]);
        assert!(Earthquake::decode(got[0]).is_none());
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
