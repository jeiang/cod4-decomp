// SPDX-License-Identifier: GPL-3.0-only
//! What the server tells the client happened: the one-shot events of `server::tempev` turned into [`ClientEvent`]s,
//! each delivered once however many snapshots repeat it.
//!
//! Effects, sound and the interface all consume the same list ([`crate::netplay::NetFrame::events`]). The server
//! announces effect names with `fx <index> <name>` reliable commands; [`Events::take_commands`] swallows those and
//! hands every other command on.

use net::Snapshot;
use net::entity::{EntityState, etype};
use server::tempev::ev;
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
    },
    /// A missile blew up; `owner` is 1023 when it had none.
    Explosion {
        origin: [f32; 3],
        normal: [f32; 3],
        weapon: u16,
        owner: u16,
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
    /// `physicsexplosionsphere`: loose bodies within `radius` are thrown from `origin`.
    PhysicsExplosion {
        origin: [f32; 3],
        radius: f32,
        strength: f32,
    },
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
            Self::PlayFx { .. } => "play_fx",
            Self::PlayerDeath { .. } => "player_death",
            Self::PlayerPain { .. } => "player_pain",
            Self::PhysicsExplosion { .. } => "physics_explosion",
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
            },
            ev::EXPLOSION => ClientEvent::Explosion {
                origin: e.origin,
                normal,
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
            ev::PHYSICS_EXPLOSION => ClientEvent::PhysicsExplosion {
                origin: e.origin,
                radius: e.velocity[0],
                strength: f32::from(e.weapon) / 10.0,
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
    fn non_event_entities_and_unknown_kinds_are_ignored() {
        let mut ev = Events::default();
        let mut p = EntityState::new(5);
        p.etype = etype::PLAYER;
        p.event = super::ev::EXPLOSION;
        p.event_seq = 9;
        let unknown = event(963, 0x7f, 1);
        assert!(ev.scan(&snap(&[p, unknown])).is_empty());
    }
}
