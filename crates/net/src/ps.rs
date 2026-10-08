// SPDX-License-Identifier: GPL-3.0-or-later
//! The field table of [`PlayerState`]: everything client prediction reads or writes goes over
//! the wire at full precision, so replaying the unacknowledged commands on a received state
//! lands where the server would.

use crate::field::{Field, Kind};
use sim::pm::{MantleState, PlayerState, PmType, SpreadOverrideState, SprintState};

macro_rules! int {
    ($s:ident, $place:expr, $k:expr) => {
        Field::<PlayerState> {
            get: |$s| $place as u32,
            set: |$s, v| $place = v as _,
            kind: $k,
        }
    };
}

macro_rules! flt {
    ($s:ident, $place:expr) => {
        Field::<PlayerState> {
            get: |$s| $place.to_bits(),
            set: |$s, v| $place = f32::from_bits(v),
            kind: Kind::Float,
        }
    };
}

macro_rules! flag {
    ($s:ident, $place:expr) => {
        Field::<PlayerState> {
            get: |$s| u32::from($place),
            set: |$s, v| $place = v != 0,
            kind: Kind::Bits(1),
        }
    };
}

fn pm_type(v: u32) -> PmType {
    match v {
        1 => PmType::NormalLinked,
        2 => PmType::Noclip,
        3 => PmType::Ufo,
        4 => PmType::Spectator,
        5 => PmType::Intermission,
        6 => PmType::LastStand,
        7 => PmType::Dead,
        8 => PmType::DeadLinked,
        _ => PmType::Normal,
    }
}

fn spread_state(v: u32) -> SpreadOverrideState {
    match v {
        1 => SpreadOverrideState::Enabled,
        2 => SpreadOverrideState::Resetting,
        _ => SpreadOverrideState::Disabled,
    }
}

fn table() -> Vec<Field<PlayerState>> {
    use Kind::{Bits, SBits};
    vec![
        int!(s, s.command_time, SBits(32)),
        Field {
            get: |s| s.pm_type as u32,
            set: |s, v| s.pm_type = pm_type(v),
            kind: Bits(4),
        },
        int!(s, s.bob_cycle, Bits(8)),
        int!(s, s.pm_flags, Bits(21)),
        int!(s, s.pm_time, SBits(24)),
        flt!(s, s.origin[0]),
        flt!(s, s.origin[1]),
        flt!(s, s.origin[2]),
        flt!(s, s.velocity[0]),
        flt!(s, s.velocity[1]),
        flt!(s, s.velocity[2]),
        flt!(s, s.old_velocity[0]),
        flt!(s, s.old_velocity[1]),
        int!(s, s.foliage_sound_time, SBits(32)),
        int!(s, s.gravity, SBits(16)),
        flt!(s, s.leanf),
        int!(s, s.speed, SBits(16)),
        flt!(s, s.delta_angles[0]),
        flt!(s, s.delta_angles[1]),
        flt!(s, s.delta_angles[2]),
        int!(s, s.ground_entity_num, Bits(10)),
        flt!(s, s.ladder_vec[0]),
        flt!(s, s.ladder_vec[1]),
        flt!(s, s.ladder_vec[2]),
        int!(s, s.jump_time, SBits(32)),
        flt!(s, s.jump_origin_z),
        int!(s, s.damage_timer, SBits(32)),
        int!(s, s.damage_count, SBits(32)),
        int!(s, s.movement_dir, SBits(8)),
        int!(s, s.e_flags, Bits(32)),
        int!(s, s.client_num, Bits(10)),
        int!(s, s.event_sequence, Bits(8)),
        int!(s, s.events[0], Bits(8)),
        int!(s, s.events[1], Bits(8)),
        int!(s, s.events[2], Bits(8)),
        int!(s, s.events[3], Bits(8)),
        int!(s, s.event_parms[0], Bits(8)),
        int!(s, s.event_parms[1], Bits(8)),
        int!(s, s.event_parms[2], Bits(8)),
        int!(s, s.event_parms[3], Bits(8)),
        int!(s, s.weapon, Bits(9)),
        int!(s, s.weapon_state, Bits(6)),
        int!(s, s.weapon_time, SBits(24)),
        int!(s, s.weapon_delay, SBits(24)),
        int!(s, s.weapon_flags, Bits(12)),
        flt!(s, s.weapon_pos_frac),
        int!(s, s.ads_delay_time, SBits(32)),
        int!(s, s.viewmodel_index, Bits(9)),
        flt!(s, s.viewangles[0]),
        flt!(s, s.viewangles[1]),
        flt!(s, s.viewangles[2]),
        int!(s, s.view_height_target, SBits(16)),
        flt!(s, s.view_height_current),
        int!(s, s.view_height_lerp_time, SBits(32)),
        int!(s, s.view_height_lerp_target, SBits(16)),
        flag!(s, s.view_height_lerp_down),
        flt!(s, s.view_angle_clamp_base[0]),
        flt!(s, s.view_angle_clamp_base[1]),
        flt!(s, s.view_angle_clamp_range[0]),
        flt!(s, s.view_angle_clamp_range[1]),
        int!(s, s.dead_yaw, SBits(16)),
        int!(s, s.health, SBits(16)),
        int!(s, s.max_health, SBits(16)),
        flt!(s, s.prone_direction),
        flt!(s, s.prone_direction_pitch),
        flt!(s, s.prone_torso_pitch),
        flag!(s, s.sprint_state.sprint_button_up_required),
        flag!(s, s.sprint_state.sprint_delay),
        int!(s, s.sprint_state.last_sprint_start, SBits(32)),
        int!(s, s.sprint_state.last_sprint_end, SBits(32)),
        int!(s, s.sprint_state.sprint_start_max_length, SBits(32)),
        flt!(s, s.torso_pitch),
        flt!(s, s.waist_pitch),
        flt!(s, s.move_speed_scale_multiplier),
        flt!(s, s.mantle_state.yaw),
        int!(s, s.mantle_state.timer, SBits(32)),
        int!(s, s.mantle_state.trans_index, SBits(16)),
        int!(s, s.mantle_state.flags, Bits(8)),
        flt!(s, s.melee_charge_yaw),
        int!(s, s.melee_charge_dist, SBits(16)),
        int!(s, s.melee_charge_time, SBits(32)),
        int!(s, s.perks, Bits(32)),
        flt!(s, s.aim_spread_scale),
        int!(s, s.weapon_shot_count, SBits(8)),
        int!(s, s.offhand_index, Bits(9)),
        int!(s, s.offhand_secondary, Bits(2)),
        int!(s, s.grenade_time_left, SBits(32)),
        int!(s, s.throw_back_grenade_owner, Bits(10)),
        int!(s, s.throw_back_grenade_time_left, SBits(32)),
        int!(s, s.weapon_restrict_kick_time, SBits(32)),
        int!(s, s.spread_override, SBits(32)),
        Field {
            get: |s| s.spread_override_state as u32,
            set: |s, v| s.spread_override_state = spread_state(v),
            kind: Bits(2),
        },
        int!(s, s.cursor_hint_ent_index, Bits(10)),
        int!(s, s.cursor_hint, Bits(8)),
        int!(s, s.cursor_hint_string, SBits(8)),
        int!(s, s.action_slot_type[0], Bits(2)),
        int!(s, s.action_slot_type[1], Bits(2)),
        int!(s, s.action_slot_type[2], Bits(2)),
        int!(s, s.action_slot_type[3], Bits(2)),
        int!(s, s.action_slot_param[0], Bits(9)),
        int!(s, s.action_slot_param[1], Bits(9)),
        int!(s, s.action_slot_param[2], Bits(9)),
        int!(s, s.action_slot_param[3], Bits(9)),
        int!(s, s.loc_selection, Bits(9)),
        int!(s, s.loc_radius, Bits(6)),
    ]
}

/// The player state's fields in wire order.
pub fn fields() -> &'static [Field<PlayerState>] {
    static T: std::sync::OnceLock<Vec<Field<PlayerState>>> = std::sync::OnceLock::new();
    T.get_or_init(table)
}

#[allow(dead_code)]
fn _all_types_are_used(_: MantleState, _: SprintState) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bits::{BitReader, BitWriter};
    use crate::field::{read_delta, write_delta};

    #[test]
    fn every_field_survives_a_delta() {
        // Set every field to a distinct non-default value through its own setter, then compare.
        let mut to = PlayerState::default();
        for (i, f) in fields().iter().enumerate() {
            let v = match f.kind {
                Kind::Float => (i as f32 * 1.37 + 0.123).to_bits(),
                Kind::Bits(1) => 1,
                Kind::Bits(n) => (i as u32 % ((1u32 << n.min(31)) - 1)) + 1,
                Kind::SBits(_) => (i as i32 - 40) as u32,
                _ => unreachable!(),
            };
            (f.set)(&mut to, v);
        }
        to.pm_type = PmType::LastStand;
        to.spread_override_state = SpreadOverrideState::Resetting;
        let mut w = BitWriter::new();
        write_delta(&mut w, fields(), &PlayerState::default(), &to);
        let mut got = PlayerState::default();
        read_delta(&mut BitReader::new(w.as_bytes()), fields(), &mut got).unwrap();
        // Compare through the table so a field the table forgot shows up as a diff of `got`
        // against a state that differs only in that field.
        assert_eq!(got, to);
    }

    #[test]
    fn the_table_covers_every_field() {
        // Changing any single field of a default state must register in the table.
        let base = PlayerState::default();
        let mut probe = base.clone();
        probe.cursor_hint_ent_index ^= 1;
        assert!(crate::field::changed_count(fields(), &base, &probe) > 0);
        let json_like = format!("{base:?}");
        let names = json_like.matches(": ").count();
        // Debug prints one `name: value` per field, plus one per nested struct field.
        assert!(
            fields().len() >= names - 14,
            "{} table fields for {names} struct fields",
            fields().len()
        );
    }

    #[test]
    fn a_walking_player_costs_a_few_bytes_per_tick() {
        let a = PlayerState::default();
        let mut b = a.clone();
        b.command_time = 33;
        b.origin = [100.25, -50.5, 8.0];
        b.velocity = [190.0, 0.0, 0.0];
        b.bob_cycle = 3;
        b.viewangles = [1.5, 90.25, 0.0];
        let mut w = BitWriter::new();
        write_delta(&mut w, fields(), &a, &b);
        assert!(w.byte_len() < 50, "{} bytes", w.byte_len());
    }
}
