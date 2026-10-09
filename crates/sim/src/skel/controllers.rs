// SPDX-License-Identifier: GPL-3.0-only
// The swing, lean and easing dynamics follow KisakCOD (bgame/bg_animation_mp.cpp, bgame/bg_misc.cpp, universal/q_shared.cpp; GPL-3.0, copyright the KisakCOD contributors and Activision).
//! View angles to the six player controller angles (`BG_PlayerAngles`, `BG_SwingAngles`,
//! `BG_Player_DoControllersInternal`, `BG_Player_DoControllersSetup`).
//!
//! The original keeps three persistent angles per player, the legs yaw, the torso yaw and the torso pitch, and swings
//! each toward its goal a little every frame: the torso follows the view, the legs stay put until the view is more
//! than `bg_legYawTolerance` away and then step round, and a strafing player's legs face the view. The controller
//! angles those produce are then eased toward their goals at `0.36` degrees per millisecond (the root offset at
//! `0.1` units per millisecond). [`Swing`] is the persistent part, [`goal`] the controller math, [`ease`] the easing.
//!
//! [`compute`] is the same math with the angles already at their goals: what a pose needs when it has no history.
//! Steady-state: the torso yaw follows the view yaw, the legs the movement direction (standing or crouched) or the
//! view (prone, strafing), the torso pitches by twice the view pitch with the head cancelling the excess, and a lean
//! rolls the spine and head, tilts the root and shifts it sideways (`player_lean_*`).

use super::rig::Controllers;
use crate::pm::math::{angle_delta, angle_normalize_360, get_lean_fraction, sincos_deg};

/// `bg_swingSpeed`.
pub const SWING_SPEED: f32 = 0.2;
/// `bg_legYawTolerance`: degrees the view may turn before an idle player's legs follow.
pub const LEG_YAW_TOLERANCE: f32 = 20.0;
/// Degrees per millisecond the controller angles ease at, and units per millisecond for the root offset.
const ANGLE_EASE: f32 = 0.36;
const OFFSET_EASE: f32 = 0.1;

/// `player_lean_shift_*` defaults: sideways shift of a leaning model, left then right, standing then crouched.
const LEAN_SHIFT: [[f32; 2]; 2] = [[5.0, 2.5], [12.5, 13.0]];
/// `player_lean_rotate_*` defaults, same order.
const LEAN_ROTATE: [[f32; 2]; 2] = [[1.25, 1.25], [1.25, 1.0]];

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ControllerInput {
    /// `viewangles[PITCH]` in degrees (either `[0, 360)` or `[-180, 180)`).
    pub view_pitch: f32,
    /// The view yaw the swing angles are measured against.
    pub view_yaw: f32,
    /// `movementDir`: the direction the legs run in, relative to the view yaw (degrees). Only [`compute`] reads it;
    /// a [`Swing`] has its own.
    pub move_dir: f32,
    pub prone: bool,
    pub crouch: bool,
    /// `ps.torso_pitch` and `ps.waist_pitch`, the prone-on-a-slope body tilt.
    pub torso_pitch: f32,
    pub waist_pitch: f32,
    /// `ps.leanf`, -1 (left) to 1 (right).
    pub lean: f32,
    /// Mounted on a turret, mantling or on a ladder: the controllers stay at rest.
    pub no_aim: bool,
}

/// What `BG_PlayerAngles` reads of a player each frame.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SwingInput {
    pub view_pitch: f32,
    pub view_yaw: f32,
    /// `movementDir` relative to the view.
    pub move_dir: f32,
    pub prone: bool,
    /// Standing or crouched and not moving (movetype `idle` or `idlecr`).
    pub idle: bool,
    pub mounted: bool,
    pub mantle: bool,
    pub ladder: bool,
    pub firing: bool,
    /// The legs clip is a strafe clip (the script's `strafing left` or `right`).
    pub strafing: bool,
}

/// `clientInfo_t`'s `legs.yawAngle`, `torso.yawAngle` and `torso.pitchAngle` with their swinging flags.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Swing {
    pub legs_yaw: f32,
    pub torso_yaw: f32,
    pub torso_pitch: f32,
    legs_yawing: bool,
    torso_yawing: bool,
    torso_pitching: bool,
}

/// `BG_SwingAngles`: moves `angle` toward `dest` by at most `frame_ms * speed * max(0.5, |delta| * 0.05)`, once the
/// two are `tolerance` apart and until they meet, and never lets it trail by more than `clamp`.
fn swing_angle(
    dest: f32,
    tolerance: f32,
    clamp: f32,
    speed: f32,
    frame_ms: f32,
    angle: &mut f32,
    swinging: &mut bool,
) {
    if !*swinging {
        let d = angle_delta(*angle, dest);
        if tolerance < d || d < -tolerance {
            *swinging = true;
        }
    }
    if !*swinging {
        return;
    }
    let swing = angle_delta(dest, *angle);
    let scale = (swing.abs() * 0.05).max(0.5);
    let step = frame_ms * scale * speed;
    let moved = if swing < 0.0 {
        if swing < -step {
            -step
        } else {
            *swinging = false;
            swing
        }
    } else if swing > step {
        step
    } else {
        *swinging = false;
        swing
    };
    *angle = angle_normalize_360(*angle + moved);
    let behind = angle_delta(dest, *angle);
    if behind > clamp {
        *angle = angle_normalize_360(dest - clamp);
    } else if behind < -clamp {
        *angle = angle_normalize_360(dest + clamp);
    }
}

impl Swing {
    /// `BG_PlayerAngles` for one frame of `frame_ms` milliseconds.
    pub fn step(&mut self, frame_ms: f32, i: &SwingInput) {
        let head = angle_normalize_360(i.view_yaw);
        if i.mounted || i.ladder || i.mantle || !i.idle {
            self.torso_yawing = true;
            self.torso_pitching = true;
            self.legs_yawing = true;
        } else if i.firing {
            self.torso_yawing = true;
            self.torso_pitching = true;
        }
        let legs_goal = if i.mantle && !i.ladder && !i.mounted {
            head
        } else {
            head + i.move_dir
        };
        let torso = (&mut self.torso_yaw, &mut self.torso_yawing);
        let swing = |dest, clamp, (a, s): (&mut f32, &mut bool)| {
            swing_angle(dest, 0.0, clamp, SWING_SPEED, frame_ms, a, s);
        };
        if i.ladder {
            swing(legs_goal, 0.0, torso);
        } else if i.mantle || i.prone {
            swing(head, 90.0, torso);
        } else if i.firing {
            swing(head, 45.0, torso);
        } else {
            swing(head, 90.0, torso);
        }
        if i.prone {
            self.legs_yawing = false;
            self.legs_yaw = head;
        } else if i.strafing {
            self.legs_yawing = false;
            swing_angle(
                head,
                0.0,
                150.0,
                SWING_SPEED,
                frame_ms,
                &mut self.legs_yaw,
                &mut self.legs_yawing,
            );
        } else {
            let tolerance = if self.legs_yawing {
                0.0
            } else {
                LEG_YAW_TOLERANCE
            };
            swing_angle(
                legs_goal,
                tolerance,
                150.0,
                SWING_SPEED,
                frame_ms,
                &mut self.legs_yaw,
                &mut self.legs_yawing,
            );
        }
        if i.mounted {
            self.torso_yaw = head;
            self.legs_yaw = head;
        } else if i.ladder {
            self.torso_yaw = angle_normalize_360(head + i.move_dir);
            self.legs_yaw = self.torso_yaw;
        }
        let pitch_goal = if i.mantle || i.mounted || i.ladder {
            0.0
        } else {
            angle_delta(i.view_pitch, 0.0) * 2.0
        };
        swing_angle(
            pitch_goal,
            0.0,
            45.0,
            0.15,
            frame_ms,
            &mut self.torso_pitch,
            &mut self.torso_pitching,
        );
    }

    /// The angles at their goals for `i`, as the first frame of a player with no history.
    pub fn settle(&mut self, i: &SwingInput) {
        *self = Swing {
            legs_yawing: true,
            torso_yawing: true,
            torso_pitching: true,
            ..Swing::default()
        };
        // A swing of this many milliseconds reaches any goal inside the clamps.
        self.step(1.0e5, i);
    }
}

/// Goal controller angles for the swing angles `s` and the player `i`.
pub fn goal(i: &ControllerInput, s: &Swing) -> Controllers {
    if i.no_aim {
        return Controllers::NONE;
    }
    let tilt = i.torso_pitch != 0.0 || i.waist_pitch != 0.0;
    let mut torso_pitch = angle_delta(s.torso_pitch, 0.0);
    if i.prone {
        let t = torso_pitch / 360.0;
        torso_pitch = (t - (t + 0.5).floor()) * 360.0;
        torso_pitch *= if torso_pitch <= 0.0 { 0.25 } else { 0.5 };
    }
    let torso_yaw = angle_delta(s.torso_yaw, s.legs_yaw);
    let head = [
        angle_delta(i.view_pitch, torso_pitch),
        angle_delta(i.view_yaw, s.torso_yaw),
    ];

    let lean = get_lean_fraction(i.lean);
    let side = usize::from(lean > 0.0);
    let stance = usize::from(i.crouch);
    let mut roll = lean * 50.0 * 0.925;
    let mut head_roll = roll;
    let mut tag_angles = [0.0, angle_delta(s.legs_yaw, i.view_yaw), 0.0];
    let mut tag_offset = [0.0; 3];
    if lean != 0.0 {
        tag_offset[1] += -lean * LEAN_SHIFT[stance][side];
    }
    let mut a = [[0.0f32; 3]; 6];

    if i.prone {
        if lean != 0.0 {
            head_roll *= 0.5;
        }
        tag_angles[0] += i.torso_pitch;
        let (s_, c) = sincos_deg(torso_yaw);
        tag_offset[0] += (1.0 - c) * -24.0;
        tag_offset[1] += s_ * -12.0;
        if lean * s_ > 0.0 {
            tag_offset[1] += -lean * (1.0 - c) * 16.0;
        }
        a[0] = [
            if tilt {
                angle_delta(i.torso_pitch, i.waist_pitch)
            } else {
                0.0
            },
            roll * -1.2,
            roll * 0.3,
        ];
        a[1] = [0.0, torso_yaw * 0.1 - roll * 0.2, roll * 0.2];
        a[2] = [torso_pitch, torso_yaw * 0.8 + roll, roll * -0.2];
    } else {
        if lean != 0.0 {
            let rotate = LEAN_ROTATE[stance][side];
            roll *= rotate;
            head_roll *= rotate;
        }
        tag_angles[2] += lean * 50.0 * 0.075;
        a[0] = [torso_pitch * 0.2, torso_yaw * 0.4, roll * 0.5];
        if tilt {
            a[0][0] += angle_delta(i.torso_pitch, i.waist_pitch);
        }
        a[1] = [torso_pitch * 0.3, torso_yaw * 0.4, roll * 0.5];
        a[2] = [torso_pitch * 0.5, torso_yaw * 0.2, roll * -0.6];
    }
    a[3] = [head[0] * 0.3, head[1] * 0.3, 0.0];
    a[4] = [head[0] * 0.7, head[1] * 0.7, head_roll * -0.3];
    if tilt {
        a[5][0] = angle_delta(i.waist_pitch, i.torso_pitch);
    }
    Controllers {
        angles: a,
        tag_origin_angles: tag_angles,
        tag_origin_offset: tag_offset,
    }
}

/// Goal controller angles with the swing angles already at their goals.
pub fn compute(i: &ControllerInput) -> Controllers {
    let mut s = Swing::default();
    s.settle(&SwingInput {
        view_pitch: i.view_pitch,
        view_yaw: i.view_yaw,
        move_dir: i.move_dir,
        prone: i.prone,
        idle: false,
        ..SwingInput::default()
    });
    goal(i, &s)
}

/// `BG_LerpAngles` and `BG_LerpOffset` (`BG_Player_DoControllersSetup`): moves `current` toward `goal` for a frame of
/// `frame_ms` milliseconds.
pub fn ease(current: &mut Controllers, goal: &Controllers, frame_ms: f32) {
    let max = frame_ms * ANGLE_EASE;
    let lerp = |cur: &mut [f32; 3], to: &[f32; 3]| {
        for (c, g) in cur.iter_mut().zip(to) {
            let d = g - *c;
            *c = if d > max {
                *c + max
            } else if d < -max {
                *c - max
            } else {
                *g
            };
        }
    };
    for (c, g) in current.angles.iter_mut().zip(&goal.angles) {
        lerp(c, g);
    }
    lerp(&mut current.tag_origin_angles, &goal.tag_origin_angles);
    let d = [
        goal.tag_origin_offset[0] - current.tag_origin_offset[0],
        goal.tag_origin_offset[1] - current.tag_origin_offset[1],
        goal.tag_origin_offset[2] - current.tag_origin_offset[2],
    ];
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let step = frame_ms * OFFSET_EASE;
    if len <= step {
        current.tag_origin_offset = goal.tag_origin_offset;
    } else {
        let f = step / len;
        for (c, d) in current.tag_origin_offset.iter_mut().zip(d) {
            *c += d * f;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idle(view_yaw: f32) -> SwingInput {
        SwingInput {
            view_yaw,
            idle: true,
            ..SwingInput::default()
        }
    }

    #[test]
    fn an_idle_players_legs_hold_until_the_view_is_past_the_tolerance() {
        let mut s = Swing::default();
        s.settle(&idle(0.0));
        for yaw in [10.0, 19.0] {
            s.step(16.0, &idle(yaw));
            assert_eq!(s.legs_yaw, 0.0, "the legs stay planted at {yaw} degrees");
        }
        for _ in 0..3 {
            s.step(16.0, &idle(40.0));
        }
        assert!(
            s.legs_yaw > 0.0 && s.legs_yaw < 40.0,
            "the legs swing toward the view, not onto it: {}",
            s.legs_yaw
        );
    }

    #[test]
    fn the_torso_follows_the_view_at_the_swing_speed_not_instantly() {
        let mut s = Swing::default();
        s.settle(&idle(0.0));
        s.step(16.0, &idle(90.0));
        assert!(s.torso_yaw > 0.0 && s.torso_yaw < 90.0, "{}", s.torso_yaw);
        for _ in 0..200 {
            s.step(16.0, &idle(90.0));
        }
        assert!((s.torso_yaw - 90.0).abs() < 1e-3);
    }

    #[test]
    fn a_strafing_players_legs_face_the_view_and_a_running_players_follow_the_movement() {
        let run = SwingInput {
            move_dir: 45.0,
            ..SwingInput::default()
        };
        let mut s = Swing::default();
        s.settle(&run);
        assert!((s.legs_yaw - 45.0).abs() < 1e-3);
        let strafe = SwingInput {
            move_dir: 90.0,
            strafing: true,
            ..SwingInput::default()
        };
        s.step(1.0e5, &strafe);
        assert!(s.legs_yaw.abs() < 1e-3);
    }

    #[test]
    fn controllers_ease_toward_the_goal_at_a_fixed_rate() {
        let goal = Controllers {
            angles: [[90.0; 3]; 6],
            tag_origin_angles: [10.0; 3],
            tag_origin_offset: [30.0, 0.0, 0.0],
        };
        let mut c = Controllers::NONE;
        ease(&mut c, &goal, 10.0);
        assert!((c.angles[0][0] - 3.6).abs() < 1e-4);
        assert!((c.tag_origin_angles[1] - 3.6).abs() < 1e-4);
        assert!((c.tag_origin_offset[0] - 1.0).abs() < 1e-4);
        for _ in 0..100 {
            ease(&mut c, &goal, 10.0);
        }
        assert_eq!(c, goal);
    }

    #[test]
    fn a_lean_rolls_the_body_tilts_the_root_and_shifts_it_to_the_side() {
        let base = ControllerInput::default();
        let left = compute(&ControllerInput { lean: -1.0, ..base });
        let right = compute(&ControllerInput { lean: 1.0, ..base });
        let none = compute(&base);
        assert_eq!(none.tag_origin_offset, [0.0; 3]);
        assert_eq!(none.angles[0][2], 0.0);
        assert!(left.angles[0][2] < 0.0 && right.angles[0][2] > 0.0);
        assert!(left.tag_origin_angles[2] < 0.0 && right.tag_origin_angles[2] > 0.0);
        // Leaning left shifts the model by 5, right by 2.5, the other way round; a crouch shifts further.
        assert!((left.tag_origin_offset[1] - 5.0).abs() < 1e-4);
        assert!((right.tag_origin_offset[1] + 2.5).abs() < 1e-4);
        let crouched = compute(&ControllerInput {
            lean: -1.0,
            crouch: true,
            ..base
        });
        assert!((crouched.tag_origin_offset[1] - 12.5).abs() < 1e-4);
        assert!(left.angles[4][2] != 0.0, "the head rolls with the body");
    }

    #[test]
    fn a_player_who_is_not_leaning_has_no_lean_terms() {
        let c = compute(&ControllerInput {
            view_pitch: 20.0,
            ..ControllerInput::default()
        });
        assert!(c.angles.iter().all(|a| a[2] == 0.0));
        assert_eq!(c.tag_origin_angles[2], 0.0);
    }
}
