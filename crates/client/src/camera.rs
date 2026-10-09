// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (GPL-3.0, KisakCOD contributors): `cgame_mp/cg_view_mp.cpp`
// (`CG_OffsetFirstPersonView`, `CG_SmoothCameraZ`), `bgame/bg_weapons.cpp` (`BG_GetVerticalBobFactor`,
// `BG_GetHorizontalBobFactor`, `BG_CalculateView_BobAngles`,
// `BG_CalculateView_Velocity`) and `cgame/cg_event.cpp` (the landing dip).
//! The first-person camera layer: what the original's `CG_OffsetFirstPersonView` does to the eye on top of the
//! predicted player.
//!
//! The *logical* eye is the predicted feet plus the view height; it is where shots start, where the listener hears
//! and what the harness measures. The *render* eye is the logical eye moved by [`View::offset`] and turned by
//! [`View::angles`], and is only what is drawn: the stair smoothing (the eye lags the body's step, see
//! [`net::predict::StepView`]), the walk, run and sprint bob, the lean shift, the dip of a landing, and the
//! eye never lower than 8 units over the feet. A scoped weapon bobs the view by angle while aimed and moving, and any
//! weapon with an `adsViewBobMult` moves it with the steps while aimed. The scoped idle sway and the kick of a hit are
//! [`sim::weapon::gun::ViewFx`]'s, in the aim this layer offsets along.

use assets::zone::weapon::WeaponDef;
use sim::pm::bob::{BOB_MAX, MIN_EYE, bob_cycle, bob_speed, horizontal_bob, vertical_bob};
use sim::pm::math::{add_lean_to_position, angle_vectors};
use sim::pm::{Params, PlayerState, ef, ev};
use sim::weapon::gun::{GunParams, view_bob};

/// How far a full lean moves the eye sideways, units, and the roll `AddLeanToPosition` pivots it by, degrees
/// (`AddLeanToPosition(.., 16, 20)`). The original's view itself takes no lean roll.
const LEAN_DIST: f32 = 20.0;
const LEAN_ROLL: f32 = 16.0;
/// A landing's dip goes down over this long, then comes back over [`DIP_UP_MS`] more, milliseconds.
const DIP_DOWN_MS: i32 = 150;
const DIP_UP_MS: i32 = 300;

/// The numbers of a weapon file the camera reads. The scoped idle sway is not here: [`sim::weapon::gun::ViewFx`]
/// has it, in the aim the camera is offset along.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WeaponView {
    /// A scoped weapon (`overlayReticle`): its view bobs by angle while aimed.
    pub scoped: bool,
    pub ads_bob_factor: f32,
    pub ads_view_bob_mult: f32,
}

impl From<&WeaponDef> for WeaponView {
    fn from(w: &WeaponDef) -> Self {
        Self {
            scoped: w.overlay_reticle != 0,
            ads_bob_factor: w.ads_bob_factor,
            ads_view_bob_mult: w.ads_view_bob_mult,
        }
    }
}

/// What one frame's view needs.
pub struct Frame<'a> {
    /// The predicted state, its origin the feet (the prediction error already added).
    pub ps: &'a PlayerState,
    /// The own events no earlier frame delivered ([`net::predict::Predicted::events`]), oldest first.
    pub events: &'a [(u8, u8)],
    /// Milliseconds on the client's clock.
    pub now: i32,
    pub weapon: Option<WeaponView>,
    /// The aim, pitch and yaw degrees, with the recoil kick: the angles the eye is offset along.
    pub aim: [f32; 2],
    /// [`net::predict::Predictor::step_offset`].
    pub step: f32,
    pub params: &'a Params,
}

/// The render eye relative to the logical one.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct View {
    /// Added to the eye position.
    pub offset: [f32; 3],
    /// Added to the view angles: pitch (positive looks down), yaw, roll (positive tilts clockwise), degrees.
    pub angles: [f32; 3],
    /// The parts of the offset, for the harness: the stair smoothing, the lean (sideways), the landing dip.
    pub step: f32,
    pub lean: f32,
    pub dip: f32,
}

#[derive(Debug, Clone, Copy)]
struct Dip {
    time: i32,
    change: f32,
}

#[derive(Debug, Clone, Default)]
pub struct Camera {
    dip: Option<Dip>,
    /// Landings that started a dip, for the report.
    pub landings: u64,
}

impl Camera {
    /// Forgets the dip and the event position (a new level, a respawn into another life).
    pub fn reset(&mut self) {
        *self = Self {
            landings: self.landings,
            ..Self::default()
        };
    }

    /// The effect of the landings among `events` on the view: `LANDING` events start a dip of the event's depth,
    /// and a hard landing (`LANDING_PAIN`) one by the height fallen.
    fn landings(&mut self, events: &[(u8, u8)], now: i32, params: &Params) {
        for &(event, parm) in events {
            let parm = f32::from(parm);
            let soft = (ev::LANDING_FIRST + 1..=ev::LANDING_FIRST + 28).contains(&event);
            let hard = (ev::LANDING_PAIN_FIRST + 1..=ev::LANDING_PAIN_FIRST + 28).contains(&event);
            if soft {
                self.landings += 1;
                self.dip = Some(Dip {
                    time: now,
                    change: -parm,
                });
            } else if hard {
                let (min, max) = (
                    params.bg_fall_damage_min_height,
                    params.bg_fall_damage_max_height,
                );
                let fell = parm * 0.01 * (max - min) + min;
                let dip = if fell > 12.0 {
                    (((fell - 12.0) / 26.0 * 4.0 + 4.0) as i32).min(24)
                } else {
                    0
                };
                if dip > 0 {
                    self.landings += 1;
                    self.dip = Some(Dip {
                        time: now,
                        change: -(dip as f32),
                    });
                }
            }
        }
    }

    /// The vertical shift of a landing at `now`: down over [`DIP_DOWN_MS`], back over [`DIP_UP_MS`].
    fn dip_at(&self, now: i32) -> f32 {
        let Some(d) = self.dip else { return 0.0 };
        let since = (now - d.time) as f32;
        let (down, up) = (DIP_DOWN_MS as f32, DIP_UP_MS as f32);
        if since <= 0.0 || since >= down + up {
            0.0
        } else if since < down {
            d.change * since / down
        } else {
            d.change * (1.0 - (since - down) / up)
        }
    }

    /// The scoped bob and the aimed view bob (`BG_CalculateView_BobAngles`, `BG_CalculateView_Velocity`), degrees:
    /// the same [`view_bob`] the server aims shots with.
    fn angles(f: &Frame<'_>) -> [f32; 3] {
        let Some(w) = f.weapon else { return [0.0; 3] };
        let p = GunParams {
            overlay_reticle: w.scoped,
            ads_bob_factor: w.ads_bob_factor,
            ads_view_bob_mult: w.ads_view_bob_mult,
            ..GunParams::default()
        };
        view_bob(f.ps, bob_speed(f.ps, f.now), &p)
    }

    /// `CG_OffsetFirstPersonView` for one frame. Nothing changes for a dead player, in a turret or at the
    /// intermission, and the landings still count then.
    pub fn view(&mut self, f: &Frame<'_>) -> View {
        self.landings(f.events, f.now, f.params);
        self.offsets(f)
    }

    /// The same for the view of a player being watched: their state is the snapshot's, so no landing is read and
    /// the own dip is dropped.
    pub fn follow(&mut self, f: &Frame<'_>) -> View {
        self.dip = None;
        self.offsets(f)
    }

    fn offsets(&mut self, f: &Frame<'_>) -> View {
        let ps = f.ps;
        let still = ps.e_flags & ef::TURRET_ACTIVE != 0
            || matches!(
                ps.pm_type,
                sim::pm::PmType::Dead | sim::pm::PmType::DeadLinked | sim::pm::PmType::Intermission
            );
        if still {
            return View::default();
        }
        let angles = Self::angles(f);
        let step = f.step;
        let dip = self.dip_at(f.now);
        let (cycle, speed) = (bob_cycle(ps), bob_speed(ps, f.now));
        let rise = vertical_bob(ps, cycle, speed, BOB_MAX);
        let side = horizontal_bob(ps, cycle, speed, BOB_MAX);
        let view = [f.aim[0] + angles[0], f.aim[1] + angles[1], angles[2]];
        let (_, right, _) = angle_vectors(&view);
        // Relative to the logical eye.
        let mut at = [
            side * right[0],
            side * right[1],
            -step + rise + side * right[2] + dip,
        ];
        let lean = ps.leanf;
        let mut pos = at;
        add_lean_to_position(&mut pos, view[1], lean, LEAN_ROLL, LEAN_DIST);
        let moved = (pos[0] - at[0]).hypot(pos[1] - at[1]);
        let sideways = if lean < 0.0 { -moved } else { moved };
        at = pos;
        // Never lower than a step over the feet.
        let floor = MIN_EYE - ps.view_height_current;
        at[2] = at[2].max(floor);
        View {
            offset: at,
            angles,
            step: -step,
            lean: sideways,
            dip,
        }
    }
}

/// Whether the hands' animated camera applies to a view (`CG_ApplyViewAnimation`): not for a spectator, the
/// intermission, a turret or a dead player.
pub fn takes_hands_camera(ps: &PlayerState) -> bool {
    ps.e_flags & ef::TURRET_ACTIVE == 0
        && !matches!(
            ps.pm_type,
            sim::pm::PmType::Spectator
                | sim::pm::PmType::Intermission
                | sim::pm::PmType::Dead
                | sim::pm::PmType::DeadLinked
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walking() -> PlayerState {
        PlayerState {
            view_height_current: 60.0,
            view_height_target: sim::pm::VIEW_STAND,
            velocity: [190.0, 0.0, 0.0],
            bob_cycle: 40,
            ..PlayerState::default()
        }
    }

    fn frame<'a>(ps: &'a PlayerState, params: &'a Params) -> Frame<'a> {
        Frame {
            ps,
            events: &[],
            now: 1000,
            weapon: None,
            aim: [0.0, 0.0],
            step: 0.0,
            params,
        }
    }

    fn scoped() -> WeaponView {
        WeaponView {
            scoped: true,
            ads_bob_factor: 0.3,
            ads_view_bob_mult: 0.0,
        }
    }

    #[test]
    fn standing_still_does_not_move_the_eye() {
        let params = Params::default();
        let ps = PlayerState {
            velocity: [0.0; 3],
            ..walking()
        };
        let v = Camera::default().view(&frame(&ps, &params));
        assert_eq!(v, View::default());
    }

    #[test]
    fn bob_follows_the_cycle_and_is_capped() {
        let ps = walking();
        let mut seen = [f32::MAX, f32::MIN];
        for c in 0..=255u8 {
            let p = PlayerState {
                bob_cycle: c,
                ..ps.clone()
            };
            let z = vertical_bob(&p, bob_cycle(&p), 190.0, BOB_MAX);
            seen = [seen[0].min(z), seen[1].max(z)];
        }
        // Standing: 190 * 0.007 = 1.33 amplitude, times the 0.75 and the two harmonics' peak.
        assert!(seen[1] > 0.5 && seen[1] < 1.4, "{seen:?}");
        assert!(seen[0] < -0.5);
        // A sprint bobs more, and a speed far past the cap stops at it.
        let sprint = PlayerState {
            pm_flags: sim::pm::pmf::SPRINTING,
            ..ps.clone()
        };
        assert!(
            vertical_bob(&sprint, 0.3, 300.0, BOB_MAX).abs()
                > vertical_bob(&ps, 0.3, 300.0, BOB_MAX).abs()
        );
        assert!(vertical_bob(&ps, 0.3, 1.0e6, BOB_MAX).abs() <= BOB_MAX);
    }

    #[test]
    fn the_stair_smoothing_lowers_the_eye_by_what_is_left_of_the_step() {
        let params = Params::default();
        let ps = PlayerState {
            velocity: [0.0; 3],
            ..walking()
        };
        let mut f = frame(&ps, &params);
        f.step = 6.0;
        let v = Camera::default().view(&f);
        assert_eq!(v.offset[2], -6.0);
        assert_eq!(v.step, -6.0);
    }

    #[test]
    fn a_lean_moves_the_eye_sideways_without_rolling_the_view() {
        let params = Params::default();
        let mut ps = PlayerState {
            velocity: [0.0; 3],
            leanf: 0.5,
            ..walking()
        };
        let mut f = frame(&ps, &params);
        f.aim = [0.0, 0.0];
        let right = Camera::default().view(&f);
        // Yaw 0 faces +x, so the player's right is -y: 0.75 of 20 units,.
        assert!((right.offset[1] + 15.0).abs() < 0.5, "{:?}", right.offset);
        assert_eq!(
            right.angles[2], 0.0,
            "upstream's view does not roll with a lean"
        );
        ps.leanf = -0.5;
        let left = Camera::default().view(&frame(&ps, &params));
        assert!(left.offset[1] > 14.0);
        // The pivot lowers the eye a little but it stays over the floor limit.
        assert!(right.offset[2] <= 0.0 && right.offset[2] > MIN_EYE - 60.0);
    }

    #[test]
    fn the_eye_never_goes_below_a_step_over_the_feet() {
        let params = Params::default();
        let ps = PlayerState {
            velocity: [0.0; 3],
            view_height_current: 11.0,
            ..walking()
        };
        let mut f = frame(&ps, &params);
        f.step = 50.0;
        assert_eq!(Camera::default().view(&f).offset[2], MIN_EYE - 11.0);
    }

    #[test]
    fn a_landing_dips_the_view_and_brings_it_back() {
        let params = Params::default();
        let calm = PlayerState {
            velocity: [0.0; 3],
            ..walking()
        };
        let mut cam = Camera::default();
        let mut f = frame(&calm, &params);
        assert_eq!(cam.view(&f).dip, 0.0);
        let landing = [(ev::LANDING_FIRST + 5, 8)];
        f.events = &landing;
        f.now = 1000;
        assert_eq!(cam.view(&f).dip, 0.0);
        f.events = &[];
        f.now = 1075;
        assert!((cam.view(&f).dip + 4.0).abs() < 1e-4);
        f.now = 1150;
        assert!((cam.view(&f).dip + 8.0).abs() < 1e-4);
        f.now = 1300;
        assert!((cam.view(&f).dip + 4.0).abs() < 1e-4);
        f.now = 1450;
        assert_eq!(cam.view(&f).dip, 0.0);
        assert_eq!(cam.landings, 1, "one landing, one dip");
    }

    #[test]
    fn a_hard_landing_dips_by_the_height_fallen() {
        let params = Params::default();
        let calm = PlayerState {
            velocity: [0.0; 3],
            ..walking()
        };
        let mut cam = Camera::default();
        let mut f = frame(&calm, &params);
        let hurt = [(ev::LANDING_PAIN_FIRST + 5, 100)];
        f.events = &hurt;
        f.now = 2000;
        cam.view(&f);
        f.events = &[];
        // 300 units fallen: (300 - 12) / 26 * 4 + 4 = 48, capped at 24.
        f.now = 2000 + DIP_DOWN_MS;
        assert_eq!(cam.view(&f).dip, -24.0);
    }

    #[test]
    fn a_scoped_weapon_bobs_its_view_by_angle_only_while_aimed_and_moving() {
        let params = Params::default();
        let run = PlayerState {
            weapon_pos_frac: 1.0,
            ..walking()
        };
        let mut f = frame(&run, &params);
        f.weapon = Some(scoped());
        let mut peak = 0.0f32;
        let states: Vec<PlayerState> = (0..=255u8)
            .map(|c| PlayerState {
                bob_cycle: c,
                ..run.clone()
            })
            .collect();
        for p in &states {
            f.ps = p;
            let v = Camera::default().view(&f);
            peak = peak.max(v.angles[0].abs()).max(v.angles[1].abs());
        }
        assert!(peak > 0.01 && peak < 1.0, "peak bob {peak}");
        let hip = PlayerState {
            weapon_pos_frac: 0.0,
            ..run.clone()
        };
        f.ps = &hip;
        assert_eq!(Camera::default().view(&f).angles, [0.0; 3]);
        f.ps = &run;
        f.weapon = Some(WeaponView {
            scoped: false,
            ..scoped()
        });
        assert_eq!(Camera::default().view(&f).angles, [0.0; 3]);
    }

    #[test]
    fn an_aimed_weapon_with_a_view_bob_multiplier_moves_the_view_with_the_steps() {
        let params = Params::default();
        let run = PlayerState {
            weapon_pos_frac: 1.0,
            bob_cycle: 20,
            ..walking()
        };
        let mut f = frame(&run, &params);
        f.weapon = Some(WeaponView {
            ads_view_bob_mult: 1.0,
            ..WeaponView::default()
        });
        let v = Camera::default().view(&f);
        assert!(v.angles[0] != 0.0 || v.angles[1] != 0.0);
    }

    #[test]
    fn a_watched_players_view_drops_the_dip_and_reads_no_events() {
        let params = Params::default();
        let calm = PlayerState {
            velocity: [0.0; 3],
            ..walking()
        };
        let mut cam = Camera::default();
        let landing = [(ev::LANDING_FIRST + 5, 8)];
        let mut f = frame(&calm, &params);
        f.events = &landing;
        cam.view(&f);
        f.events = &[];
        f.now = 1075;
        assert!(cam.view(&f).dip < 0.0);
        cam.follow(&f);
        assert_eq!(cam.view(&f).dip, 0.0, "the dip belongs to the own view");
    }

    #[test]
    fn the_hands_camera_is_for_a_player_in_the_world() {
        let mut ps = walking();
        assert!(takes_hands_camera(&ps));
        ps.pm_type = sim::pm::PmType::Spectator;
        assert!(!takes_hands_camera(&ps));
        ps.pm_type = sim::pm::PmType::Normal;
        ps.e_flags = ef::TURRET_ACTIVE;
        assert!(!takes_hands_camera(&ps));
    }

    #[test]
    fn a_turret_or_a_dead_player_gets_no_offsets() {
        let params = Params::default();
        let mut ps = walking();
        ps.leanf = 0.5;
        ps.e_flags = ef::TURRET_ACTIVE;
        assert_eq!(
            Camera::default().view(&frame(&ps, &params)),
            View::default()
        );
        ps.e_flags = 0;
        ps.pm_type = sim::pm::PmType::Dead;
        assert_eq!(
            Camera::default().view(&frame(&ps, &params)),
            View::default()
        );
    }
}
