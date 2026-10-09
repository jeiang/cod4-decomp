// SPDX-License-Identifier: GPL-3.0-only
//! What a client is told about one entity: enough to draw it, interpolate it and animate a
//! player body. Positions are quantized (1/16 unit); prediction uses the player state instead.

use crate::field::{Field, Kind};

/// `EntityState::etype` values.
pub mod etype {
    pub const GENERAL: u8 = 0;
    pub const PLAYER: u8 = 1;
    pub const CORPSE: u8 = 2;
    /// A `script_model`: drawn from its model index.
    pub const SCRIPT_MODEL: u8 = 3;
    /// A grenade, rocket or other projectile: the client draws the weapon's projectile model at `origin` with `angles`
    /// and points its trail along `velocity`; `weapon` is the weapon index and `client` the owner.
    pub const MISSILE: u8 = 4;
    pub const ITEM: u8 = 5;
    /// A one-shot happening (impact, explosion, effect): see `server::tempev`.
    pub const EVENT: u8 = 6;
    /// A script vehicle (the helicopter hardpoint): drawn from its model index at `origin` and `angles` (pitch, yaw,
    /// roll), interpolated between snapshots; `velocity` is its motion, `pm_type` its damage stage (3 whole, 2 light
    /// smoke, 1 heavy smoke, 0 crashing) and `client` its owner's client number.
    pub const VEHICLE: u8 = 7;
    /// A script effect entity (`spawnFx`): an effect the scripts start with `triggerFx`. `origin` and `angles` place it,
    /// `model` is the effect index, `event_seq` counts the triggers (0 until the first) and `eflags` is the server
    /// time (ms, low 24 bits) the latest trigger plays at.
    pub const FX: u8 = 8;
    /// A looping script effect (`playLoopedFX`): `origin`, `angles` and `model` as for [`FX`], `pm_flags` the repeat
    /// period in milliseconds and `velocity[0]` the distance beyond which it is not played (0 for always).
    pub const LOOP_FX: u8 = 9;
}

pub const MAX_ENTITIES: usize = 1024;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EntityState {
    /// Not part of the delta; the entity list codes it.
    pub number: u16,
    pub etype: u8,
    pub eflags: u32,
    pub origin: [f32; 3],
    /// Degrees. A player's pitch is the view pitch, roll the lean.
    pub angles: [f32; 3],
    pub velocity: [f32; 3],
    /// Model precache index (0 = none).
    pub model: u16,
    /// Held weapon index of a player, weapon of a missile.
    pub weapon: u16,
    /// Player or corpse: the client number. Missile: the owner.
    pub client: u16,
    pub pm_type: u8,
    pub pm_flags: u32,
    pub weapon_state: u8,
    /// Aim-down-sights fraction, 0..=255.
    pub ads: u8,
    pub move_dir: i8,
    /// A one-shot event (`ev::*`), repeated by bumping `event_seq`.
    pub event: u8,
    pub event_parm: u8,
    pub event_seq: u8,
    /// Player: the prone-on-a-slope body tilt (`ps.torso_pitch`, `ps.waist_pitch`), degrees.
    pub torso_pitch: f32,
    pub waist_pitch: f32,
    /// Player: `ps.damage_timer` and `ps.damage_duration`, milliseconds; the hit's flinch and stumble windows are the
    /// first part of the duration.
    pub damage_timer: u16,
    pub damage_duration: u16,
    /// Player: the torso animation channel the server decided: the clip (0 none), its cap in 10 ms (0 none) and a
    /// counter that changes whenever a clip starts or ends. See `server::playeranim::TorsoWire`.
    pub torso_clip: u8,
    pub torso_cap: u8,
    pub torso_seq: u8,
    /// A launched script model (`eflags::PHYSICS_LAUNCH`, in `server::netsv`): where the launch struck it, in the
    /// world. `origin` and `angles` are where it was launched from and `velocity` is the launch force.
    pub launch_point: [f32; 3],
    /// Perk bits (`bg_perkNames` order); remote clients pick footstep sounds by them.
    pub perks: u32,
    /// Player: the material index (`cs::MATERIALS`, 0 none) of the head icon the scripts set, shown over the head to
    /// the players of `head_icon_team`.
    pub head_icon: u16,
    /// Who sees `head_icon`: 0 everyone, 1 axis, 2 allies, 3 spectators.
    pub head_icon_team: u8,
}

macro_rules! int {
    ($s:ident, $place:expr, $k:expr) => {
        Field::<EntityState> {
            get: |$s| $place as u32,
            set: |$s, v| $place = v as _,
            kind: $k,
        }
    };
}

macro_rules! num {
    ($s:ident, $place:expr, $k:expr) => {
        Field::<EntityState> {
            get: |$s| $place.to_bits(),
            set: |$s, v| $place = f32::from_bits(v),
            kind: $k,
        }
    };
}

fn table() -> Vec<Field<EntityState>> {
    use Kind::{Angle16, Bits, Fixed, SBits};
    let pos = Fixed {
        bits: 22,
        step: 1.0 / 16.0,
    };
    let vel = Fixed {
        bits: 17,
        step: 0.25,
    };
    vec![
        int!(s, s.etype, Bits(4)),
        num!(s, s.origin[0], pos),
        num!(s, s.origin[1], pos),
        num!(s, s.origin[2], pos),
        num!(s, s.angles[0], Angle16),
        num!(s, s.angles[1], Angle16),
        num!(s, s.angles[2], Angle16),
        int!(s, s.eflags, Bits(24)),
        int!(s, s.model, Bits(10)),
        int!(s, s.weapon, Bits(9)),
        int!(s, s.client, Bits(10)),
        num!(s, s.velocity[0], vel),
        num!(s, s.velocity[1], vel),
        num!(s, s.velocity[2], vel),
        int!(s, s.pm_type, Bits(4)),
        int!(s, s.pm_flags, Bits(21)),
        int!(s, s.weapon_state, Bits(6)),
        int!(s, s.ads, Bits(8)),
        int!(s, s.move_dir, SBits(8)),
        int!(s, s.event, Bits(8)),
        int!(s, s.event_parm, Bits(8)),
        int!(s, s.event_seq, Bits(8)),
        num!(s, s.torso_pitch, Angle16),
        num!(s, s.waist_pitch, Angle16),
        int!(s, s.damage_timer, Bits(16)),
        int!(s, s.damage_duration, Bits(16)),
        int!(s, s.torso_clip, Bits(8)),
        int!(s, s.torso_cap, Bits(8)),
        int!(s, s.torso_seq, Bits(8)),
        num!(s, s.launch_point[0], pos),
        num!(s, s.launch_point[1], pos),
        num!(s, s.launch_point[2], pos),
        int!(s, s.perks, Bits(20)),
        int!(s, s.head_icon, Bits(8)),
        int!(s, s.head_icon_team, Bits(2)),
    ]
}

pub fn fields() -> &'static [Field<EntityState>] {
    static T: std::sync::OnceLock<Vec<Field<EntityState>>> = std::sync::OnceLock::new();
    T.get_or_init(table)
}

impl EntityState {
    pub fn new(number: u16) -> Self {
        Self {
            number,
            ..Self::default()
        }
    }

    /// Rounds every quantized field the way the wire does.
    pub fn canonical(mut self) -> Self {
        crate::field::canonicalize(fields(), &mut self);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bits::{BitReader, BitWriter};
    use crate::field::{read_delta, write_delta};

    #[test]
    fn the_animation_fields_survive_the_wire() {
        let to = EntityState {
            torso_pitch: 30.0,
            waist_pitch: 200.0,
            damage_timer: 750,
            damage_duration: 900,
            torso_clip: 12,
            torso_cap: 40,
            torso_seq: 200,
            ..EntityState::new(5)
        }
        .canonical();
        let mut w = BitWriter::new();
        write_delta(&mut w, fields(), &EntityState::new(5), &to);
        let bytes = w.into_bytes();
        let mut got = EntityState::new(5);
        read_delta(&mut BitReader::new(&bytes), fields(), &mut got).unwrap();
        assert_eq!(got, to);
    }

    #[test]
    fn a_launched_models_launch_point_and_force_reach_the_client() {
        let mut sent = EntityState::new(40);
        sent.etype = etype::SCRIPT_MODEL;
        sent.origin = [100.0, 200.0, 30.0];
        sent.launch_point = [101.5, 199.25, 34.0];
        sent.velocity = [120.0, -40.0, 300.0];
        sent.eflags = 1 << 3;
        let sent = sent.canonical();
        let mut w = BitWriter::new();
        write_delta(&mut w, fields(), &EntityState::new(40), &sent);
        let bytes = w.into_bytes();
        let mut got = EntityState::new(40);
        read_delta(&mut BitReader::new(&bytes), fields(), &mut got).unwrap();
        assert_eq!(got, sent);
        assert_eq!(got.launch_point, [101.5, 199.25, 34.0]);
    }

    #[test]
    fn a_head_icon_and_who_sees_it_survive_the_wire() {
        let from = EntityState::new(5);
        let to = EntityState {
            etype: etype::PLAYER,
            head_icon: 255,
            head_icon_team: 3,
            ..EntityState::new(5)
        }
        .canonical();
        let mut w = BitWriter::new();
        write_delta(&mut w, fields(), &from, &to);
        let bytes = w.into_bytes();
        let mut got = from.clone();
        read_delta(&mut BitReader::new(&bytes), fields(), &mut got).unwrap();
        assert_eq!(got, to);
        assert_eq!((got.head_icon, got.head_icon_team), (255, 3));
    }
}
