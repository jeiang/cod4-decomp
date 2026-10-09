// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (GPL-3.0, KisakCOD contributors): `game_mp/g_active_mp.cpp`
// (`P_DamageFeedback`) and `cgame/cg_playerstate.cpp` (`CG_DamageFeedback`).
//! What the server and the client agree on about a hit: the direction bytes of the player state, the view kick
//! a hit's damage percent makes and how long it lasts.

use super::math::{angle_vectors, vec_to_pitch, vec_to_yaw};
use crate::Vec3;

/// How long `damage_count` stays after a hit, ms.
pub const DAMAGE_COUNT_MS: i32 = 500;
/// How long the view kick of a hit lasts, ms (`v_dmg_time`).
pub const VIEW_KICK_MS: i32 = 500;
/// `bg_viewKickScale`, `bg_viewKickMin` and `bg_viewKickMax` of a stock server.
pub const VIEW_KICK_SCALE: f32 = 0.2;
pub const VIEW_KICK_MIN: f32 = 5.0;
pub const VIEW_KICK_MAX: f32 = 90.0;

/// The pitch and yaw bytes of `damage_pitch` and `damage_yaw` for a hit pushing along `from` (the way the player is
/// pushed); `None` is a hit with no direction, which is told as 255 and 255.
pub fn direction_bytes(from: Option<Vec3>) -> (u8, u8) {
    let Some(v) = from else { return (255, 255) };
    // `(int)(angle / 360 * 256)` of an angle below 360 is at most 255.
    let byte = |a: f32| ((a / 360.0 * 256.0) as i32).clamp(0, 255) as u8;
    (byte(vec_to_pitch(&v)), byte(vec_to_yaw(&v)))
}

/// The unit direction of the damage bytes, `None` for the no-direction mark. The bytes are read as 1/255 turns
/// (`CG_DamageFeedback`) though the server writes 1/256 turns.
pub fn direction_of(pitch: u8, yaw: u8) -> Option<Vec3> {
    if pitch == 255 && yaw == 255 {
        return None;
    }
    let angles = [
        f32::from(pitch) / 255.0 * 360.0,
        f32::from(yaw) / 255.0 * 360.0,
        0.0,
    ];
    Some(angle_vectors(&angles).0)
}

/// The kick of a hit that took `percent` of the maximum health, degrees.
pub fn view_kick(percent: i32) -> f32 {
    (percent as f32 * VIEW_KICK_SCALE).clamp(VIEW_KICK_MIN, VIEW_KICK_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_direction_survives_the_bytes_to_within_a_byte() {
        for v in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [-0.6, -0.8, 0.0]] {
            let (p, y) = direction_bytes(Some(v));
            let d = direction_of(p, y).unwrap();
            let dot = d[0] * v[0] + d[1] * v[1] + d[2] * v[2];
            assert!(dot > 0.98, "{v:?} came back as {d:?}");
        }
        assert_eq!(direction_bytes(None), (255, 255));
        assert_eq!(direction_of(255, 255), None);
    }

    #[test]
    fn a_hit_kicks_between_the_least_and_the_most() {
        assert_eq!(view_kick(1), VIEW_KICK_MIN);
        assert_eq!(view_kick(1000), VIEW_KICK_MAX);
        assert!((view_kick(50) - 10.0).abs() < 1e-5);
    }
}
