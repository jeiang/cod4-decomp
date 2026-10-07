// SPDX-License-Identifier: GPL-3.0-or-later
//! User commands on the wire: each is a delta against the one before it in the packet.

use crate::bits::{BitReader, BitWriter, Overflow};
use crate::field::{Field, Kind, read_delta, write_delta};
use sim::pm::UserCmd;

macro_rules! int {
    ($s:ident, $place:expr, $k:expr) => {
        Field::<UserCmd> {
            get: |$s| $place as u32,
            set: |$s, v| $place = v as _,
            kind: $k,
        }
    };
}

macro_rules! flt {
    ($s:ident, $place:expr) => {
        Field::<UserCmd> {
            get: |$s| $place.to_bits(),
            set: |$s, v| $place = f32::from_bits(v),
            kind: Kind::Float,
        }
    };
}

/// Every command field except the time, which is coded as a difference.
fn fields() -> &'static [Field<UserCmd>] {
    static T: std::sync::OnceLock<Vec<Field<UserCmd>>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        use Kind::{Bits, SBits};
        vec![
            int!(s, s.buttons, Bits(24)),
            int!(s, s.angles[0], Bits(16)),
            int!(s, s.angles[1], Bits(16)),
            int!(s, s.angles[2], Bits(16)),
            int!(s, s.forwardmove, SBits(8)),
            int!(s, s.rightmove, SBits(8)),
            int!(s, s.weapon, Bits(8)),
            int!(s, s.offhand_index, Bits(8)),
            int!(s, s.upmove, SBits(8)),
            int!(s, s.pitchmove, SBits(8)),
            int!(s, s.yawmove, SBits(8)),
            flt!(s, s.gun_pitch),
            flt!(s, s.gun_yaw),
            flt!(s, s.gun_offset[0]),
            flt!(s, s.gun_offset[1]),
            flt!(s, s.gun_offset[2]),
            flt!(s, s.melee_charge_yaw),
            int!(s, s.melee_charge_dist, Bits(8)),
            int!(s, s.selected_location[0], SBits(8)),
            int!(s, s.selected_location[1], SBits(8)),
        ]
    })
}

/// Writes `cmd` as a difference from `prev`.
pub fn write_cmd(w: &mut BitWriter, prev: &UserCmd, cmd: &UserCmd) {
    w.write_ivar(cmd.server_time.wrapping_sub(prev.server_time));
    write_delta(w, fields(), prev, cmd);
}

pub fn read_cmd(r: &mut BitReader, prev: &UserCmd) -> Result<UserCmd, Overflow> {
    let mut cmd = *prev;
    cmd.server_time = prev.server_time.wrapping_add(r.read_ivar()?);
    read_delta(r, fields(), &mut cmd)?;
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_of_commands_round_trips_in_a_few_bytes_each() {
        let mut cmds = Vec::new();
        for i in 0..6 {
            cmds.push(UserCmd {
                server_time: 1000 + i * 8,
                buttons: if i == 3 { 1 } else { 0 },
                angles: [100 + i, 40000 - 5 * i, 0],
                forwardmove: 127,
                rightmove: if i > 2 { -127 } else { 0 },
                weapon: 4,
                gun_pitch: 0.5 * i as f32,
                ..UserCmd::default()
            });
        }
        let mut w = BitWriter::new();
        let mut prev = UserCmd::default();
        let mut sizes = Vec::new();
        for c in &cmds {
            let before = w.bit_len();
            write_cmd(&mut w, &prev, c);
            sizes.push(w.bit_len() - before);
            prev = *c;
        }
        let mut r = BitReader::new(w.as_bytes());
        let mut prev = UserCmd::default();
        for c in &cmds {
            let got = read_cmd(&mut r, &prev).unwrap();
            assert_eq!(&got, c);
            prev = got;
        }
        assert!(sizes[1..].iter().all(|&b| b < 130), "{sizes:?}");
    }
}
