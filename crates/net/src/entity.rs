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
