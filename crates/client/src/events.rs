// SPDX-License-Identifier: GPL-3.0-only
//! What the server tells the client happened: the one-shot events of `server::tempev` turned into [`ClientEvent`]s,
//! each delivered once however many snapshots repeat it.
//!
//! Effects, sound and the interface all consume the same list ([`crate::netplay::NetFrame::events`]). The server
//! announces effect names with `fx <index> <name>` reliable commands; [`Events::take_commands`] swallows those and
//! hands every other command on.

use net::Snapshot;
use net::entity::{EntityState, etype};
use server::tempev::{Earthquake, Physics, ev};
use std::collections::HashMap;

/// A happening in the world, at server time of the snapshot that first carried it.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientEvent {
    /// A bullet struck something. `surface` is the surface type index (7 = flesh); `shooter` is a client number.
    BulletImpact {
        origin: [f32; 3],
        normal: [f32; 3],
        surface: u8,
        weapon: u16,
        shooter: u16,
        /// A penetrating bullet came out of a wall here.
        exit: bool,
    },
    /// A missile blew up; `owner` is 1023 when it had none.
    Explosion {
        origin: [f32; 3],
        normal: [f32; 3],
        /// The surface type the blast sat on.
        surface: u8,
        weapon: u16,
        owner: u16,
    },
    /// A missile that will not go off hit or settled on a surface (a grenade that never armed, a `dud` explosion).
    Dud {
        origin: [f32; 3],
        normal: [f32; 3],
        surface: u8,
        weapon: u16,
        owner: u16,
        /// An unarmed grenade that came to rest: the dud table's effect only, no sound.
        settled: bool,
    },
    MissileBounce {
        origin: [f32; 3],
        normal: [f32; 3],
        surface: u8,
        weapon: u16,
        owner: u16,
    },
    /// A script effect. `name` is empty if the effect name has not arrived (it follows in a reliable command).
    PlayFx {
        origin: [f32; 3],
        forward: [f32; 3],
        /// Where the effect's up points (the roll `playfx` gave it).
        up: [f32; 3],
        name: String,
        /// The entity the effect was played on (`playfxontag`), 1023 for none.
        entity: u16,
    },
    PlayerDeath {
        origin: [f32; 3],
        client: u16,
        /// The impulse to push the body with.
        push: [f32; 3],
    },
    PlayerPain {
        origin: [f32; 3],
        client: u16,
        damage: u8,
    },
    /// A physics world event (`physicsexplosionsphere`, `physicsexplosioncylinder`, `physicsjolt`, `physicsjitter`):
    /// loose bodies near `origin` are pushed.
    Physics { origin: [f32; 3], what: Physics },
    /// `earthquake`: the camera shakes for whoever is within `radius` of `origin`.
    Earthquake { origin: [f32; 3], quake: Earthquake },
    /// A weapon fired: from the shooter's `eye`, looking along `angles` (degrees). A `vehicle` shooter is an entity
    /// that is no player: `eye` is its muzzle, and its shot has no player event to make the sound.
    WeaponFire {
        eye: [f32; 3],
        angles: [f32; 3],
        weapon: u16,
        shooter: u16,
        vehicle: bool,
    },
}

impl ClientEvent {
    /// A short name for reports.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::BulletImpact { .. } => "bullet_impact",
            Self::Explosion { .. } => "explosion",
            Self::MissileBounce { .. } => "missile_bounce",
            Self::Dud { .. } => "dud",
            Self::PlayFx { .. } => "play_fx",
            Self::PlayerDeath { .. } => "player_death",
            Self::PlayerPain { .. } => "player_pain",
            Self::Physics { .. } => "physics",
            Self::Earthquake { .. } => "earthquake",
            Self::WeaponFire { .. } => "weapon_fire",
        }
    }
}

#[derive(Default)]
pub struct Events {
    /// Effect names by index - 1.
    fx: Vec<String>,
    /// Last sequence acted on, by event entity number.
    seen: HashMap<u16, u8>,
}

impl Events {
    /// Takes the `fx` commands out of `cmds`, returning the rest in order.
    pub fn take_commands(&mut self, cmds: &mut Vec<String>) -> Vec<String> {
        let mut rest = Vec::new();
        for c in cmds.drain(..) {
            let Some(r) = c.strip_prefix("fx ") else {
                rest.push(c);
                continue;
            };
            if let Some((i, name)) = r.split_once(' ')
                && let Ok(i) = i.parse::<usize>()
                && i >= 1
            {
                if self.fx.len() < i {
                    self.fx.resize(i, String::new());
                }
                self.fx[i - 1] = name.to_owned();
            }
        }
        rest
    }

    /// The name of effect `index` (1-based), if announced.
    pub fn fx_name(&self, index: usize) -> Option<&str> {
        self.fx
            .get(index.checked_sub(1)?)
            .map(String::as_str)
            .filter(|n| !n.is_empty())
    }

    /// The events in `snap` not delivered before. Call once per snapshot, oldest first.
    pub fn scan(&mut self, snap: &Snapshot) -> Vec<ClientEvent> {
        let mut out = Vec::new();
        let mut live = Vec::new();
        let vehicles: Vec<u16> = snap
            .entities
            .iter()
            .filter(|e| e.etype == etype::VEHICLE)
            .map(|e| e.number)
            .collect();
        for e in snap.entities.iter().filter(|e| e.etype == etype::EVENT) {
            live.push(e.number);
            if self.seen.get(&e.number) == Some(&e.event_seq) {
                continue;
            }
            self.seen.insert(e.number, e.event_seq);
            out.extend(self.decode(e, &vehicles));
        }
        // An entity that left the snapshot starts over, so a restarted server's sequence 1 is not skipped.
        self.seen.retain(|n, _| live.contains(n));
        out
    }

    fn decode(&self, e: &EntityState, vehicles: &[u16]) -> Option<ClientEvent> {
        let normal = direction(e.angles);
        Some(match e.event {
            ev::BULLET_IMPACT => ClientEvent::BulletImpact {
                origin: e.origin,
                normal,
                surface: e.event_parm,
                weapon: e.weapon,
                shooter: e.client,
                exit: e.eflags & server::tempev::IMPACT_EXIT != 0,
            },
            ev::EXPLOSION => ClientEvent::Explosion {
                origin: e.origin,
                normal,
                surface: e.event_parm,
                weapon: e.weapon,
                owner: e.client,
            },
            ev::DUD | ev::CHANGE_TO_DUD => ClientEvent::Dud {
                settled: e.event == ev::CHANGE_TO_DUD,
                origin: e.origin,
                normal,
                surface: e.event_parm,
                weapon: e.weapon,
                owner: e.client,
            },
            ev::MISSILE_BOUNCE => ClientEvent::MissileBounce {
                origin: e.origin,
                normal,
                surface: e.event_parm,
                weapon: e.weapon,
                owner: e.client,
            },
            ev::PLAY_FX => ClientEvent::PlayFx {
                origin: e.origin,
                forward: normal,
                up: sim::pm::math::angle_vectors(&e.angles).2,
                name: self
                    .fx_name(usize::from(e.model))
                    .unwrap_or_default()
                    .to_owned(),
                entity: e.client,
            },
            ev::PLAYER_DEATH => ClientEvent::PlayerDeath {
                origin: e.origin,
                client: e.client,
                push: e.velocity,
            },
            ev::PLAYER_PAIN => ClientEvent::PlayerPain {
                origin: e.origin,
                client: e.client,
                damage: e.event_parm,
            },
            ev::PHYSICS_EXPLOSION
            | ev::PHYSICS_EXPLOSION_CYLINDER
            | ev::PHYSICS_JOLT
            | ev::PHYSICS_JITTER => ClientEvent::Physics {
                origin: e.origin,
                what: Physics::decode(e)?,
            },
            ev::EARTHQUAKE => ClientEvent::Earthquake {
                origin: e.origin,
                quake: Earthquake::decode(e)?,
            },
            ev::WEAPON_FIRE => ClientEvent::WeaponFire {
                eye: e.origin,
                angles: e.angles,
                weapon: e.weapon,
                shooter: e.client,
                vehicle: vehicles.contains(&e.client),
            },
            _ => return None,
        })
    }
}

/// The unit vector a pitch/yaw/roll in degrees points along.
fn direction(angles: [f32; 3]) -> [f32; 3] {
    sim::pm::math::angle_vectors(&angles).0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(events: &[EntityState]) -> Snapshot {
        let mut s = Snapshot::empty();
        s.entities = events.to_vec();
        s
    }

    fn event(number: u16, kind: u8, seq: u8) -> EntityState {
        let mut e = EntityState::new(number);
        e.etype = etype::EVENT;
        e.event = kind;
        e.event_seq = seq;
        e
    }

    #[test]
    fn an_event_repeated_by_later_snapshots_is_delivered_once() {
        let mut ev = Events::default();
        let mut e = event(960, super::ev::BULLET_IMPACT, 3);
        e.origin = [1.0, 2.0, 3.0];
        e.angles = [-90.0, 0.0, 0.0];
        e.event_parm = 7;
        let first = ev.scan(&snap(std::slice::from_ref(&e)));
        assert_eq!(first.len(), 1);
        let ClientEvent::BulletImpact {
            origin,
            normal,
            surface,
            ..
        } = &first[0]
        else {
            panic!("{first:?}");
        };
        assert_eq!((*origin, *surface), ([1.0, 2.0, 3.0], 7));
        assert!((normal[2] - 1.0).abs() < 1e-4, "{normal:?}");
        assert!(ev.scan(&snap(std::slice::from_ref(&e))).is_empty());
        // The slot reused with a new sequence is a new event.
        e.event_seq = 4;
        assert_eq!(ev.scan(&snap(&[e])).len(), 1);
    }

    #[test]
    fn a_sequence_seen_again_after_the_slot_left_the_snapshot_is_new() {
        let mut ev = Events::default();
        let e = event(961, super::ev::EXPLOSION, 1);
        assert_eq!(ev.scan(&snap(std::slice::from_ref(&e))).len(), 1);
        assert!(ev.scan(&snap(&[])).is_empty());
        assert_eq!(ev.scan(&snap(&[e])).len(), 1);
    }

    #[test]
    fn fx_commands_name_effects_and_other_commands_pass_through() {
        let mut ev = Events::default();
        let mut cmds = vec![
            "fx 2 fx/explosions/grenadeexp_dirt".to_owned(),
            "d 3".to_owned(),
            "fx 1 fx/misc/a b".to_owned(),
        ];
        let rest = ev.take_commands(&mut cmds);
        assert_eq!(rest, ["d 3"]);
        assert!(cmds.is_empty());
        assert_eq!(ev.fx_name(1), Some("fx/misc/a b"));
        assert_eq!(ev.fx_name(2), Some("fx/explosions/grenadeexp_dirt"));
        assert_eq!(ev.fx_name(3), None);
        assert_eq!(ev.fx_name(0), None);

        let mut e = event(962, super::ev::PLAY_FX, 1);
        e.model = 2;
        match &ev.scan(&snap(&[e]))[0] {
            ClientEvent::PlayFx { name, .. } => assert_eq!(name, "fx/explosions/grenadeexp_dirt"),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn a_weapon_fire_event_carries_the_eye_the_aim_and_the_shooter() {
        let mut ev = Events::default();
        let mut e = event(964, super::ev::WEAPON_FIRE, 1);
        e.origin = [1.0, 2.0, 60.0];
        e.angles = [5.0, 90.0, 0.0];
        e.weapon = 7;
        e.client = 3;
        let out = ev.scan(&snap(&[e]));
        assert!(
            matches!(
                out[..],
                [ClientEvent::WeaponFire {
                    eye: [1.0, 2.0, 60.0],
                    angles: [5.0, 90.0, 0.0],
                    weapon: 7,
                    shooter: 3,
                    vehicle: false
                }]
            ),
            "{out:?}"
        );
    }

    #[test]
    fn a_shot_of_a_vehicle_entity_is_marked() {
        let mut ev = Events::default();
        let mut e = event(964, super::ev::WEAPON_FIRE, 1);
        e.client = 70;
        let mut v = EntityState::new(70);
        v.etype = etype::VEHICLE;
        let out = ev.scan(&snap(&[e, v]));
        assert!(
            matches!(out[..], [ClientEvent::WeaponFire { vehicle: true, .. }]),
            "{out:?}"
        );
    }

    #[test]
    fn earthquake_and_physics_events_arrive_with_their_parameters() {
        let mut t = server::tempev::TempEvents::default();
        t.add_earthquake(
            0,
            [1.0, 2.0, 3.0],
            &Earthquake {
                scale: 0.5,
                duration_ms: 3000,
                radius: 800.0,
            },
        );
        let jolt = Physics::Jolt {
            outer: 400.0,
            inner: 50.0,
            impulse: [0.0, 0.0, 2.0],
        };
        t.add_physics(0, [4.0, 5.0, 6.0], &jolt);
        let states: Vec<_> = t.live(1).cloned().collect();
        let out = Events::default().scan(&snap(&states));
        assert!(
            matches!(&out[0], ClientEvent::Earthquake { quake, origin }
                if quake.duration_ms == 3000 && quake.radius == 800.0 && *origin == [1.0, 2.0, 3.0]),
            "{out:?}"
        );
        assert_eq!(
            out[1],
            ClientEvent::Physics {
                origin: [4.0, 5.0, 6.0],
                what: jolt
            }
        );
    }

    #[test]
    fn non_event_entities_and_unknown_kinds_are_ignored() {
        let mut ev = Events::default();
        let mut p = EntityState::new(5);
        p.etype = etype::PLAYER;
        p.event = super::ev::EXPLOSION;
        p.event_seq = 9;
        let unknown = event(963, 0x7f, 1);
        assert!(ev.scan(&snap(&[p, unknown])).is_empty());
    }

    #[test]
    fn a_blast_and_a_dud_carry_the_surface_they_were_on() {
        let mut ev = Events::default();
        let mut blast = event(960, super::ev::EXPLOSION, 1);
        blast.event_parm = 5;
        blast.weapon = 3;
        let mut dud = event(961, super::ev::DUD, 1);
        dud.event_parm = 9;
        let mut settled = event(962, super::ev::CHANGE_TO_DUD, 1);
        settled.event_parm = 2;
        let out = ev.scan(&snap(&[blast, dud, settled]));
        assert!(matches!(out[2], ClientEvent::Dud { settled: true, .. }));
        assert!(matches!(
            out[0],
            ClientEvent::Explosion {
                surface: 5,
                weapon: 3,
                ..
            }
        ));
        assert!(matches!(
            out[1],
            ClientEvent::Dud {
                surface: 9,
                settled: false,
                ..
            }
        ));
    }
}
