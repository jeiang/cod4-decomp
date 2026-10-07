// SPDX-License-Identifier: GPL-3.0-or-later
//! View angles to the six player controller angles (`BG_Player_DoControllersInternal`).
//!
//! The original eases every angle toward its goal at `frametime * 0.36` degrees per step and
//! swings the torso and legs yaw with `BG_SwingAngles`. The server skeleton only needs where a
//! shot lands, so this computes the goal instantly: the torso follows the view yaw, the legs
//! turn to the movement direction (standing or crouched) or stay with the view (prone), the
//! torso pitches by twice the view pitch with the head cancelling the excess, and leaning is
//! ignored (bots never lean). Ceiling: anything the swing smoothing would still be catching up
//! on is a few degrees off at most for one or two frames after a fast turn.

use super::rig::Controllers;
use crate::pm::math::{angle_delta, sincos_deg};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ControllerInput {
    /// `viewangles[PITCH]` in degrees (either `[0, 360)` or `[-180, 180)`).
    pub view_pitch: f32,
    /// `movementDir`: the direction the legs run in, relative to the view yaw (degrees).
    pub move_dir: f32,
    pub prone: bool,
    /// `ps.torso_pitch` and `ps.waist_pitch`, the prone-on-a-slope body tilt.
    pub torso_pitch: f32,
    pub waist_pitch: f32,
    /// Mounted on a turret, mantling or on a ladder: the controllers stay at rest.
    pub no_aim: bool,
}

/// Goal controller angles for `i`.
pub fn compute(i: &ControllerInput) -> Controllers {
    if i.no_aim {
        return Controllers::NONE;
    }
    let tilt = i.torso_pitch != 0.0 || i.waist_pitch != 0.0;
    let view_pitch = angle_delta(i.view_pitch, 0.0);
    let mut torso_pitch = view_pitch * 2.0;

    // Legs yaw relative to the view; torso yaw relative to the legs; head relative to torso.
    let legs_yaw = if i.prone { 0.0 } else { i.move_dir };
    let torso_yaw = -legs_yaw;
    let mut tag_angles = [0.0, legs_yaw, 0.0];
    let mut tag_offset = [0.0; 3];
    let mut a = [[0.0f32; 3]; 6];

    if i.prone {
        let t = torso_pitch / 360.0;
        torso_pitch = (t - (t + 0.5).floor()) * 360.0;
        torso_pitch *= if torso_pitch <= 0.0 { 0.25 } else { 0.5 };
    }
    let head = [angle_delta(view_pitch, torso_pitch), 0.0];
    if i.prone {
        tag_angles[0] += i.torso_pitch;
        let (s, c) = sincos_deg(torso_yaw);
        tag_offset[0] = (1.0 - c) * -24.0;
        tag_offset[1] = s * -12.0;
        a[0][0] = if tilt { angle_delta(i.torso_pitch, i.waist_pitch) } else { 0.0 };
        a[1][1] = torso_yaw * 0.1;
        a[2] = [torso_pitch, torso_yaw * 0.8, 0.0];
    } else {
        a[0] = [torso_pitch * 0.2, torso_yaw * 0.4, 0.0];
        if tilt {
            a[0][0] += angle_delta(i.torso_pitch, i.waist_pitch);
        }
        a[1] = [torso_pitch * 0.3, torso_yaw * 0.4, 0.0];
        a[2] = [torso_pitch * 0.5, torso_yaw * 0.2, 0.0];
    }
    a[3] = [head[0] * 0.3, head[1] * 0.3, 0.0];
    a[4] = [head[0] * 0.7, head[1] * 0.7, 0.0];
    if tilt {
        a[5][0] = angle_delta(i.waist_pitch, i.torso_pitch);
    }
    Controllers {
        angles: a,
        tag_origin_angles: tag_angles,
        tag_origin_offset: tag_offset,
    }
}
