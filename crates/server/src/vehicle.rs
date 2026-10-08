// SPDX-License-Identifier: GPL-3.0-or-later
//! Script vehicles: the helicopter of the helicopter hardpoint (`spawnhelicopter` and the methods the
//! stock `_helicopter.gsc` calls on it).
//!
//! A vehicle is an entity with a [`Vehicle`] beside the engine fields. Every 50 ms (`Scr_Vehicle_Think`) it flies
//! toward the goal the script set (`VEH_UpdateAIMove`): the speed and acceleration come from `setspeed`, the
//! heading and tilt from the acceleration it is under, and a turret on the model tracks its target. The script
//! hears of progress through notifies: `goal` when it arrives, `near_goal` inside the notify distance, `goal_yaw`
//! when a goal yaw is reached and `turret_on_target` while the barrel points at its target. Damage is the
//! ordinary `G_Damage`: the entity takes damage and the script hears `damage`.
//!
//! Fact sources: `VEH_UpdateMoveToGoal`, `VEH_UpdateMoveOrientation`, `VEH_UpdateYawAndNotify`, `VEH_UpdateAim`,
//! `CMD_VEH_*` and `CMD_VEH_FireWeapon`. Ceilings: the turret is aimed with the model's rest-pose tags (the
//! barrel and rotor bones do not animate on the server), a hit does not jolt the body, vehicles do not collide with
//! the map and there is no player-driven helicopter.

use std::rc::Rc;

use gsc::Vm;
use sim::Vec3;
use sim::contents;
use sim::pm::math;
use sim::weapon::fire::AimBasis;
use sim::weapon::{WeaponParams, WeaponType};

use crate::bullet::{BulletHit, BulletParams, dot, length, lerp, mad, normalized, sub};
use crate::game::{Ent, EntKind, Game};
use crate::tags;

/// `MPH_TO_INCHES_PER_SEC`: scripts speak in miles per hour, the simulation in inches per second.
pub const MPH: f32 = 17.6;
/// A think step, seconds and milliseconds.
const DT: f32 = 0.05;
const THINK_MS: i32 = 50;
/// The heliocopter's collision box half-size (`G_SpawnHelicopter`).
const HALF_BOX: f32 = 50.0;
/// What the stock helicopter's health is (`Heli_InitFirstThink`); scripts keep their own damage tally.
const HEALTH: i32 = 99_999;

/// What `vehicles/<name>` says that matters here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VehicleInfo {
    pub name: String,
    /// Inches per second squared.
    pub accel: f32,
    pub turret_weapon: String,
    pub span_left: f32,
    pub span_right: f32,
    pub span_up: f32,
    pub span_down: f32,
    pub turret_rot_rate: f32,
}

impl VehicleInfo {
    /// Parses a `VEHICLEFILE` info string (`\key\value\key\value...`).
    pub fn parse(name: &str, bytes: &[u8]) -> Result<Self, String> {
        let text = String::from_utf8_lossy(bytes);
        let mut it = text.split('\\').map(str::trim_end);
        match it.next().map(str::trim) {
            Some("VEHICLEFILE") => {}
            _ => return Err(format!("vehicles/{name}: not a VEHICLEFILE")),
        }
        let mut info = Self {
            name: name.to_owned(),
            ..Self::default()
        };
        while let (Some(k), Some(v)) = (it.next(), it.next()) {
            let num = || v.trim().parse::<f32>().unwrap_or(0.0);
            match k.to_ascii_lowercase().as_str() {
                "accel" => info.accel = num() * MPH,
                "turretweapon" => info.turret_weapon = v.trim().to_owned(),
                "turrethorizspanleft" => info.span_left = num(),
                "turrethorizspanright" => info.span_right = num(),
                "turretvertspanup" => info.span_up = num(),
                "turretvertspandown" => info.span_down = num(),
                "turretrotrate" => info.turret_rot_rate = num(),
                _ => {}
            }
        }
        if info.accel <= 0.0 {
            return Err(format!("vehicles/{name}: no acceleration"));
        }
        Ok(info)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveState {
    /// No goal yet.
    Stop,
    Move,
    /// Arrived at a goal it stops at: drifts around it within the hover radius.
    Hover,
}

/// What a script `waittill`s on, raised by a think.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notify {
    Goal,
    NearGoal,
    GoalYaw,
}

impl Notify {
    pub fn name(self) -> &'static str {
        match self {
            Self::Goal => "goal",
            Self::NearGoal => "near_goal",
            Self::GoalYaw => "goal_yaw",
        }
    }
}

/// What the turret points at (`hasTarget`, `targetEnt`, `targetOrigin`, `targetOffset`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    None,
    Ent(u16, Vec3),
    Point(Vec3),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurretState {
    Stopped,
    Moving,
    Stopping,
}

#[derive(Debug, Clone)]
pub struct Vehicle {
    pub info: Rc<VehicleInfo>,
    /// The player the hardpoint belongs to (`spawnHelicopter`'s first argument).
    pub owner: u16,
    /// `setvehicleteam`: 0 none, 1 axis, 2 allies.
    pub team: u8,
    /// The weapon `setvehweapon` chose; 0 is none.
    pub weapon: u16,
    /// Level time of the next think.
    pub next_think: i32,
    /// `setdamagestage`: 3 whole, 2 light smoke, 1 heavy smoke, 0 crashing. Clients draw it.
    pub stage: u8,

    // motion
    pub vel: Vec3,
    /// The smoothed acceleration the tilt follows (`phys.accel`).
    pub accel: Vec3,
    pub speed: f32,
    /// Pitch, yaw, roll.
    pub angles: Vec3,
    pub rot_vel: Vec3,
    pub max_angle_vel: Vec3,
    pub yaw_accel: f32,
    pub yaw_decel: f32,
    pub max_pitch: f32,
    pub max_roll: f32,
    pub manual_speed: f32,
    pub manual_accel: f32,
    pub manual_decel: f32,
    pub max_drag_speed: f32,
    pub turning_ability: f32,
    pub state: MoveState,
    pub goal: Vec3,
    pub stop_at_goal: bool,
    pub stopping: bool,
    pub goal_yaw: Option<f32>,
    pub target_yaw: Option<f32>,
    pub prev_goal_yaw: f32,
    pub yaw_slow_down: bool,
    pub yaw_overshoot: f32,
    pub near_goal_dist: f32,
    pub hover_radius: f32,
    pub hover_speed: f32,
    pub hover_accel: f32,
    pub hover_goal: Vec3,
    pub hover_accel_for_angles: bool,
    pub look_at: Option<u16>,
    /// Toward the look-at entity, set by each think.
    pub look_at_dir: Option<Vec3>,

    // turret
    pub target: Target,
    /// The barrel's angles relative to the body, degrees.
    pub gun_pitch: f32,
    pub gun_yaw: f32,
    pub turret: TurretState,
}

impl Vehicle {
    pub fn new(info: Rc<VehicleInfo>, owner: u16, angles: Vec3, weapon: u16) -> Self {
        Self {
            info,
            owner,
            team: 0,
            weapon,
            next_think: 0,
            stage: 3,
            vel: [0.0; 3],
            accel: [0.0; 3],
            speed: 0.0,
            angles,
            rot_vel: [0.0; 3],
            max_angle_vel: [45.0, 90.0, 45.0],
            yaw_accel: 25.0,
            yaw_decel: 15.0,
            // `VEH_InitPhysics` in the MP binary: 45/90/45 degrees a second, yaw 25 up and 15 down, 25 degrees
            // of pitch and roll until the script's `setmaxpitchroll`.
            max_pitch: 25.0,
            max_roll: 25.0,
            manual_speed: 0.0,
            manual_accel: 0.0,
            manual_decel: 0.0,
            max_drag_speed: 60.0 * MPH,
            turning_ability: 0.5,
            state: MoveState::Stop,
            goal: [0.0; 3],
            stop_at_goal: false,
            stopping: false,
            goal_yaw: None,
            target_yaw: None,
            prev_goal_yaw: -1.0,
            yaw_slow_down: false,
            yaw_overshoot: 0.1,
            near_goal_dist: 0.0,
            hover_radius: 30.0,
            hover_speed: 0.8 * MPH,
            hover_accel: 0.5 * MPH,
            hover_goal: [0.0; 3],
            hover_accel_for_angles: false,
            look_at: None,
            look_at_dir: None,
            target: Target::None,
            gun_pitch: 0.0,
            gun_yaw: 0.0,
            turret: TurretState::Stopped,
        }
    }

    /// `VEH_AccelerateSpeed`.
    fn accelerate(speed: f32, target: f32, accel: f32, dt: f32) -> f32 {
        if target <= speed {
            let s = speed - accel * dt;
            if target > s { target } else { s }
        } else {
            let s = speed + accel * dt;
            if target < s { target } else { s }
        }
    }

    /// `VEH_UpdateMove_CheckStop`: the step length, shortened when the brakes bring it to rest mid-step.
    fn check_stop(&mut self, dist: f32) -> f32 {
        let new_speed = Self::accelerate(self.speed, self.manual_speed, self.manual_accel, DT);
        let time = new_speed / self.manual_decel;
        let stop_dist = new_speed * 0.5 * time;
        let check = dist - new_speed * DT;
        if stop_dist < check || self.speed <= 0.0 {
            self.stopping = false;
            return DT;
        }
        let mut dt = DT;
        if stop_dist < dist {
            dt = (DT - (dist - stop_dist) / self.speed).clamp(0.0, DT);
        }
        self.stopping = true;
        dt
    }

    /// `VEH_GetNewSpeedAndAccel`.
    fn new_speed_and_accel(&self, dt: f32, hovering: bool) -> (f32, f32) {
        let (speed, accel, decel) = if hovering {
            (self.hover_speed, self.hover_accel, self.hover_accel * 0.5)
        } else {
            (self.manual_speed, self.manual_accel, self.manual_decel)
        };
        if self.stopping {
            (Self::accelerate(self.speed, 0.0, decel, dt), decel)
        } else {
            (Self::accelerate(self.speed, speed, accel, dt), accel)
        }
    }

    /// `VEH_UpdateMoveToGoal`.
    fn move_to_goal(&mut self, pos: &mut Vec3, goal: Vec3, ev: &mut Vec<Notify>) {
        let hovering = self.state == MoveState::Hover;
        let to_goal = sub(goal, *pos);
        let dist = length(to_goal);
        if dist > 0.0 {
            let prev = self.vel;
            let mut dt = DT;
            if self.stop_at_goal || hovering {
                dt = self.check_stop(dist);
            }
            let (new_speed, accel_max) = self.new_speed_and_accel(dt, hovering);
            let desired_dir = mad([0.0; 3], 1.0 / dist, to_goal);
            let desired_vel = mad([0.0; 3], new_speed, desired_dir);
            let mut accel_vec = sub(desired_vel, prev);
            if !self.stopping && self.manual_speed >= self.speed {
                let d = desired_vel[1] * prev[1] + desired_vel[0] * prev[0];
                if d > 0.0 && accel_vec[1] * prev[1] + accel_vec[0] * prev[0] < 0.0 {
                    let flat = (prev[0] * prev[0] + prev[1] * prev[1]).sqrt();
                    if flat > 0.0 {
                        let dir = [prev[0] / flat, prev[1] / flat];
                        let along = accel_vec[1] * dir[1] + accel_vec[0] * dir[0];
                        accel_vec[0] -= along * dir[0];
                        accel_vec[1] -= along * dir[1];
                    }
                }
            }
            let mag = length(accel_vec);
            let max_dt = accel_max * dt;
            if max_dt < mag {
                accel_vec = mad([0.0; 3], max_dt / mag, accel_vec);
            }
            if !hovering {
                self.check_horizontal(to_goal, accel_max, &mut accel_vec);
                if to_goal[2] != 0.0 {
                    self.check_vertical(to_goal[2], &mut accel_vec);
                }
            }
            self.vel = mad(prev, 1.0, accel_vec);
            self.speed = length(self.vel);
            self.accel = lerp(self.accel, accel_vec, 0.5);
            let mag = length(self.accel);
            if max_dt < mag && !self.stop_at_goal {
                self.accel = mad([0.0; 3], max_dt / mag, self.accel);
            }
            let average = mad([0.0; 3], 0.5, mad(prev, 1.0, self.vel));
            if dt < DT {
                *pos = mad(*pos, DT - dt, prev);
            }
            *pos = mad(*pos, dt, average);
            self.update_orientation(desired_dir, ev);
        }
        if !hovering {
            if self.near_goal_dist != 0.0 && self.near_goal_dist > dist {
                ev.push(Notify::NearGoal);
            }
            self.check_goal_reached(dist, ev);
        }
    }

    /// `VEH_CheckHorizontalVelocityToGoal`: brake to stop at the goal, or to make a turn the speed would overshoot.
    fn check_horizontal(&mut self, to_goal: Vec3, accel_max: f32, accel_vec: &mut Vec3) {
        let dist = (to_goal[0] * to_goal[0] + to_goal[1] * to_goal[1]).sqrt();
        if dist < 1.0 {
            return;
        }
        let speed = (self.vel[0] * self.vel[0] + self.vel[1] * self.vel[1]).sqrt();
        let new_vel = [self.vel[0] + accel_vec[0], self.vel[1] + accel_vec[1]];
        let new_speed = (new_vel[0] * new_vel[0] + new_vel[1] * new_vel[1]).sqrt();
        if self.stop_at_goal && new_speed > 0.0 {
            let required = speed * speed / (dist * 2.0) * DT;
            if (new_speed - speed).abs() < required {
                let f = (speed - required) / new_speed;
                accel_vec[0] = new_vel[0] * f - self.vel[0];
                accel_vec[1] = new_vel[1] * f - self.vel[1];
            }
            return;
        }
        let perp = [self.vel[1], -self.vel[0]];
        let side = (perp[0] * to_goal[0] + perp[1] * to_goal[1]).abs();
        if dist < side && speed > 0.0 {
            let along = to_goal[1] * self.vel[1] + to_goal[0] * self.vel[0];
            let (r0, r1) = (side / speed, along / speed);
            let radius = (r1 * r1 + r0 * r0) / (r0 * 2.0);
            if radius > 1.0 && accel_max * radius < speed * speed {
                let braking = (speed * speed / radius).min(speed);
                let ability = if self.stop_at_goal {
                    1.0
                } else {
                    self.turning_ability
                };
                let f = -ability * braking / speed * DT;
                let (bx, by) = (f * self.vel[0], f * self.vel[1]);
                self.vel[0] += bx;
                self.vel[1] += by;
                accel_vec[0] += bx;
                accel_vec[1] += by;
            }
        }
    }

    /// `VEH_CheckVerticalVelocityToGoal`.
    fn check_vertical(&mut self, vertical: f32, accel_vec: &mut Vec3) {
        let vs = self.vel[2];
        if accel_vec[2].abs() < 0.001 || vs.abs() < 0.001 || vs * accel_vec[2] >= 0.0 {
            return;
        }
        if vertical * vs <= 0.0 {
            return;
        }
        let current = vs * DT / -accel_vec[2];
        let desired = vertical / (vs * 0.5);
        if desired < current {
            let braking = -vs * DT / desired;
            if accel_vec[2] * accel_vec[2] < braking * braking {
                let cap = self.manual_accel * DT * 3.0;
                let v = if braking - cap < 0.0 { braking } else { cap };
                let v = if -cap - braking < 0.0 { v } else { -cap };
                self.vel[2] += v - accel_vec[2];
                accel_vec[2] = v;
            }
        }
    }

    /// `VEH_UpdateMove_CheckGoalReached`.
    fn check_goal_reached(&mut self, dist: f32, ev: &mut Vec<Notify>) {
        if self.stop_at_goal {
            let reached = if self.hover_radius == 0.0 {
                (self.stopping || dist == 0.0) && self.speed == 0.0
            } else {
                self.hover_radius >= dist && 2.0 * MPH > self.speed
            };
            if reached {
                self.state = MoveState::Hover;
                if self.hover_radius == 0.0 {
                    self.accel = [0.0; 3];
                    self.vel = [0.0; 3];
                }
                ev.push(Notify::Goal);
            }
        } else if dist <= self.speed * DT {
            ev.push(Notify::Goal);
        }
    }

    /// `VEH_UpdateHover`: keeps moving toward a point near the goal that wanders inside the hover radius.
    fn update_hover(&mut self, pos: &mut Vec3, rnd: &mut dyn FnMut() -> f32, ev: &mut Vec<Notify>) {
        let hover_pos = mad(self.goal, 1.0, self.hover_goal);
        self.move_to_goal(pos, hover_pos, ev);
        let near = self.hover_radius * 0.25;
        let d = sub(hover_pos, *pos);
        if dot(d, d) < near * near {
            if self.hover_radius == 0.0 {
                self.hover_goal = [0.0; 3];
            } else {
                let r = self.hover_radius;
                let random = [
                    -r + rnd() * 2.0 * r,
                    -r + rnd() * 2.0 * r,
                    -r + rnd() * 2.0 * r,
                ];
                self.hover_goal = mad(random, -0.5, self.hover_goal);
            }
        }
    }

    /// `VEH_UpdateMoveOrientation`: heading, then pitch and roll from the acceleration.
    fn update_orientation(&mut self, desired_dir: Vec3, ev: &mut Vec<Notify>) {
        let desired_yaw = self.desired_yaw(desired_dir);
        self.update_yaw(desired_yaw, ev);
        let mut accel_vec = self.accel;
        self.add_fake_drag(&mut accel_vec);
        let horizontal = (accel_vec[0] * accel_vec[0] + accel_vec[1] * accel_vec[1]).sqrt() / DT;
        accel_vec = normalized(accel_vec);
        let (body_sin, body_cos) = math::sincos_deg(self.angles[1]);
        let fraction = self.accel_fraction(horizontal);
        let mut stopping = 1.0;
        if self.stopping && horizontal > 0.0 {
            let time_to_goal =
                (self.vel[0] * self.vel[0] + self.vel[1] * self.vel[1]).sqrt() / horizontal;
            let stopping_time = (1.0 - fraction) * 3.5 + fraction * 2.5;
            if stopping_time > time_to_goal {
                stopping = time_to_goal / stopping_time;
            }
        }
        let dot_fwd = (accel_vec[1] * body_sin + accel_vec[0] * body_cos) * stopping;
        let angle_factor = fraction + (1.0 - fraction) * 0.1;
        let accel = self.accel_for_angles();
        let fraction = self.accel_fraction(accel);
        let angular_accel = fraction * 45.0 + (1.0 - fraction);
        let angular_decel = angular_accel * 0.4;
        let pitch = self.max_pitch * dot_fwd * angle_factor;
        self.update_angle(0, pitch, angular_accel, angular_decel, 0.0);
        let dot_side = (accel_vec[1] * -body_cos + accel_vec[0] * body_sin) * stopping;
        let roll = self.max_roll * dot_side * angle_factor;
        self.update_angle(2, roll, angular_accel, angular_decel, 0.0);
    }

    /// `VEH_CalcAccelFraction`: how hard the acceleration pushes, 0..1 of the vehicle's own.
    fn accel_fraction(&self, accel: f32) -> f32 {
        accel.clamp(0.0, self.info.accel) / self.info.accel
    }

    /// `VEH_GetAccelForAngles`.
    fn accel_for_angles(&mut self) -> f32 {
        if self.state != MoveState::Hover {
            self.hover_accel_for_angles = false;
            return self.manual_accel;
        }
        if self.hover_accel_for_angles {
            return self.hover_accel;
        }
        let steady = self.angles[0].abs() <= 5.0
            && self.angles[2].abs() <= 5.0
            && self.rot_vel[0].abs() <= 3.0
            && self.rot_vel[2].abs() <= 3.0;
        if steady {
            self.hover_accel_for_angles = true;
            self.hover_accel
        } else {
            self.manual_accel
        }
    }

    /// `VEH_AddFakeDrag`.
    fn add_fake_drag(&self, accel_vec: &mut Vec3) {
        let horizontal = (self.vel[0] * self.vel[0] + self.vel[1] * self.vel[1]).sqrt();
        let clamped = horizontal.min(self.max_drag_speed);
        let drag = (clamped / self.max_drag_speed).powi(2) * 5.0;
        if horizontal > 0.0 {
            accel_vec[0] += drag * self.vel[0] / horizontal;
            accel_vec[1] += drag * self.vel[1] / horizontal;
        }
    }

    /// `VEH_UpdateMove_GetDesiredYaw`.
    fn desired_yaw(&self, desired_dir: Vec3) -> f32 {
        if let Some(look) = self.look_at_dir {
            return math::vec_to_yaw(&look);
        }
        if let Some(goal) = self.goal_yaw
            && (self.stopping || self.state == MoveState::Hover)
        {
            let time_to_stop = self.speed / self.manual_decel;
            let turn = self.max_angle_vel[1] / self.yaw_accel;
            let mut time_to_turn = turn + turn;
            let stop_angle = self.max_angle_vel[1] * 0.5 * time_to_turn;
            let diff = math::angle_delta(goal, self.angles[1]).abs();
            if time_to_stop > time_to_turn && diff > stop_angle {
                time_to_turn += (diff - stop_angle) / self.max_angle_vel[1];
            }
            if time_to_stop <= time_to_turn {
                return goal;
            }
        }
        if let Some(y) = self.target_yaw {
            y
        } else if self.state == MoveState::Hover {
            self.angles[1]
        } else {
            math::vec_to_yaw(&desired_dir)
        }
    }

    /// `VEH_UpdateYawAndNotify`.
    fn update_yaw(&mut self, desired: f32, ev: &mut Vec<Notify>) {
        const EPSILON: f32 = 0.001;
        let goal = self.goal_yaw.unwrap_or(0.0);
        let initial = self.angles[1] - goal;
        let watch = self.goal_yaw.is_some() && initial.abs() > EPSILON;
        let initial_vel = self.rot_vel[1];
        let (mut accel, mut decel) = (self.yaw_accel, self.yaw_decel);
        if self.prev_goal_yaw != desired {
            self.yaw_slow_down = false;
            self.prev_goal_yaw = desired;
        }
        if self.yaw_slow_down {
            accel *= 0.2;
            decel *= 0.2;
        }
        let overshoot = if self.goal_yaw.is_some() || self.target_yaw.is_some() {
            self.yaw_overshoot
        } else {
            0.0
        };
        self.update_angle(1, desired, accel, decel, overshoot);
        if self.goal_yaw.is_some() && self.rot_vel[1] * initial_vel < 0.0 {
            self.yaw_slow_down = true;
        }
        if watch {
            let fin = self.angles[1] - goal;
            if fin * initial < 0.0 || fin.abs() < EPSILON {
                ev.push(Notify::GoalYaw);
            }
        }
    }

    /// `VEH_UpdateAngleAndAngularVel`: one axis chases its desired angle, braking so it stops on it.
    fn update_angle(&mut self, i: usize, desired: f32, accel: f32, decel: f32, overshoot: f32) {
        let diff = math::angle_delta(desired, self.angles[i]);
        if diff == 0.0 && self.rot_vel[i] == 0.0 {
            return;
        }
        let speed = self.rot_vel[i].abs();
        let mut target = self.max_angle_vel[i];
        let effective = if diff * self.rot_vel[i] < 0.0 {
            accel
        } else {
            let stop_time = speed / decel;
            let stop_angle = (1.0 - overshoot) * (speed * 0.5 * stop_time);
            if stop_angle < diff.abs() {
                accel
            } else {
                target = 0.0;
                decel
            }
        };
        if diff < 0.0 {
            target = -target;
        }
        if speed >= effective * DT || diff.abs() >= speed * DT {
            self.rot_vel[i] = Self::accelerate(self.rot_vel[i], target, effective, DT);
            self.angles[i] = math::angle_wrap_180(self.rot_vel[i] * DT + self.angles[i]);
        } else {
            self.angles[i] = desired;
            self.rot_vel[i] = 0.0;
        }
    }

    /// One `Scr_Vehicle_Think` of the movement: the position and the notifies it raised.
    fn think_move(&mut self, pos: &mut Vec3, rnd: &mut dyn FnMut() -> f32, ev: &mut Vec<Notify>) {
        match self.state {
            MoveState::Move => self.move_to_goal(pos, self.goal, ev),
            MoveState::Hover => self.update_hover(pos, rnd, ev),
            MoveState::Stop => {}
        }
    }
}

/// `LinearTrackAngle`: `current` moves toward `target` by at most `rate` degrees a second.
fn track_angle(target: f32, current: f32, rate: f32) -> f32 {
    let d = math::angle_delta(target, current);
    let step = rate * DT;
    if d.abs() <= step {
        target
    } else {
        current + step * d.signum()
    }
}

impl Game {
    /// `vehicles/<name>` from the zones.
    fn vehicle_info(&self, name: &str) -> Result<Rc<VehicleInfo>, String> {
        let bytes = self
            .content
            .rawfile(&format!("vehicles/{name}"))
            .ok_or_else(|| format!("Can't find info for script vehicle [{name}]"))?;
        VehicleInfo::parse(name, bytes).map(Rc::new)
    }

    /// `GScr_SpawnHelicopter`: a helicopter of vehicle type `vehicle` drawn with `model` for player `owner`.
    pub fn spawn_helicopter(
        &mut self,
        owner: u16,
        origin: Vec3,
        angles: Vec3,
        vehicle: &str,
        model: &str,
    ) -> Result<u16, String> {
        if !self.is_client(owner) {
            return Err("Owner entity is not a player".into());
        }
        let info = self.vehicle_info(vehicle)?;
        let weapon = self.weapons.index(&info.turret_weapon);
        let mut e = Ent::new(EntKind::Plain, "script_vehicle");
        e.origin = origin;
        e.angles = angles;
        e.model = model.into();
        e.health = HEALTH;
        e.takedamage = true;
        e.contents = contents::CLIPSHOT | contents::MISSILECLIP;
        e.mins = [-HALF_BOX; 3];
        e.maxs = [HALF_BOX; 3];
        let mut v = Vehicle::new(info, owner, angles, weapon);
        v.next_think = self.level.time + THINK_MS;
        e.veh = Some(Box::new(v));
        let n = self.spawn(e)?;
        self.note_model(model);
        self.relink(n);
        Ok(n)
    }

    /// `G_RunFrameForEntity` of a vehicle: a think every 50 ms.
    pub fn run_vehicle(&mut self, vm: &mut Vm, n: u16) {
        let now = self.level.time;
        let Some(mut v) = self.ent_mut(n).and_then(|e| e.veh.take()) else {
            return;
        };
        if now >= v.next_think {
            v.next_think = now + THINK_MS;
            self.think_vehicle(vm, n, &mut v);
        }
        if let Some(e) = self.ent_mut(n)
            && e.veh.is_none()
        {
            e.veh = Some(v);
        }
    }

    fn think_vehicle(&mut self, vm: &mut Vm, n: u16, v: &mut Vehicle) {
        let Some(e) = self.ent(n) else { return };
        let mut pos = e.origin;
        v.look_at_dir = v
            .look_at
            .and_then(|t| self.ent(t))
            .map(|t| sub(t.origin, pos));
        let mut events = Vec::new();
        let mut rng = || self.random_f32();
        v.think_move(&mut pos, &mut rng, &mut events);
        if let Some(e) = self.ent_mut(n) {
            e.origin = pos;
            e.angles = v.angles;
        }
        self.relink(n);
        self.update_aim(vm, n, v);
        for ev in events {
            vm.notify_entity(n, ev.name(), &[]);
        }
    }

    /// `VEH_UpdateAim`: the barrel follows the target within the turret's span.
    fn update_aim(&mut self, vm: &mut Vm, n: u16, v: &mut Vehicle) {
        let alive = self.ent(n).is_some_and(|e| e.health > 0);
        let target = match v.target {
            Target::Ent(t, off) if alive => {
                self.ent(t).map(|te| (Some(t), mad(te.origin, 1.0, off)))
            }
            Target::Point(p) if alive => Some((None, p)),
            _ => None,
        };
        let (Some((tgt_ent, tgt_pos)), Some(barrel)) = (target, self.world_tag(n, "tag_barrel"))
        else {
            match v.turret {
                TurretState::Moving => v.turret = TurretState::Stopping,
                TurretState::Stopping => v.turret = TurretState::Stopped,
                TurretState::Stopped => {}
            }
            return;
        };
        let barrel_pos = barrel[3];
        let dir = normalized(sub(tgt_pos, barrel_pos));
        let want = [math::vec_to_pitch(&dir), math::vec_to_yaw(&dir), 0.0];
        // The target angles relative to the body.
        let body = tags::angles_to_axis(v.angles);
        let rel = tags::mul3(&tags::angles_to_axis(want), &tags::transpose3(&body));
        let rel = tags::axis_to_angles(&rel);
        let delta = [
            math::angle_delta(rel[0], v.gun_pitch).abs(),
            math::angle_delta(rel[1], v.gun_yaw).abs(),
        ];
        let info = v.info.clone();
        let mut pitch = track_angle(rel[0], v.gun_pitch, info.turret_rot_rate);
        let mut yaw = track_angle(rel[1], v.gun_yaw, info.turret_rot_rate);
        let (free_pitch, free_yaw) = (pitch, yaw);
        pitch = pitch.clamp(-info.span_up, info.span_down);
        yaw = yaw.clamp(-info.span_right, info.span_left);
        v.gun_pitch = pitch;
        v.gun_yaw = yaw;
        let stuck = [
            math::angle_delta(free_pitch, pitch),
            math::angle_delta(free_yaw, yaw),
        ];
        if (delta[0] >= 2.0 && stuck[0] == 0.0) || (delta[1] >= 2.0 && stuck[1] == 0.0) {
            v.turret = TurretState::Moving;
        } else if v.turret == TurretState::Moving {
            v.turret = TurretState::Stopping;
        } else if v.turret == TurretState::Stopping {
            vm.notify_entity(n, "turret_rotate_stopped", &[]);
            v.turret = TurretState::Stopped;
        }
        if delta[0] >= 1.0 || delta[1] >= 1.0 {
            vm.notify_entity(n, "turret_not_on_target", &[]);
            vm.notify_entity(n, "turret_no_vis", &[]);
            return;
        }
        vm.notify_entity(n, "turret_on_target", &[]);
        let seen = match (tgt_ent, self.world.as_ref()) {
            (Some(t), Some(w)) => {
                w.sight_trace(0, barrel_pos, tgt_pos, [0.0; 3], [0.0; 3], n, t, 2049) == 0
            }
            _ => false,
        };
        vm.notify_entity(
            n,
            if seen {
                "turret_on_vistarget"
            } else {
                "turret_no_vis"
            },
            &[],
        );
    }

    /// `CMD_VEH_FireWeapon`: one shot from `tag` along the barrel (a bullet) or the tag (a projectile, homing
    /// on `target` when the weapon steers). Returns the missile entity.
    pub fn vehicle_fire(
        &mut self,
        vm: &mut Vm,
        n: u16,
        tag: Option<&str>,
        target: Option<(u16, Vec3)>,
    ) -> Result<Option<u16>, String> {
        let e = self.ent(n).ok_or("not a vehicle")?;
        if e.health <= 0 {
            return Err("Vehicle must have health to control the turret".into());
        }
        let v = e.veh.as_ref().ok_or("not a vehicle")?;
        let weapon = v.weapon;
        let (gun_pitch, gun_yaw, angles) = (v.gun_pitch, v.gun_yaw, v.angles);
        if weapon == 0 {
            return Err("Invalid weapon specified for vehicle".into());
        }
        let info = self
            .weapons
            .get(weapon)
            .ok_or("Invalid weapon specified for vehicle")?
            .clone();
        let tag = tag.unwrap_or("tag_flash").to_ascii_lowercase();
        let m = self
            .world_tag(n, &tag)
            .ok_or_else(|| format!("vehicle has no tag '{tag}'"))?;
        let origin = m[3];
        // A bullet leaves along the barrel, which the turret has turned; a projectile along its own tag.
        let body = tags::angles_to_axis(angles);
        let gun = tags::mul3(&tags::angles_to_axis([gun_pitch, gun_yaw, 0.0]), &body);
        let gun_angles = tags::axis_to_angles(&gun);
        let now = self.level.time;
        self.stats.heli_shots += 1;
        let spent = match info.weap_type {
            WeaponType::Bullet => {
                let aim = AimBasis::from_angles(origin, &gun_angles);
                self.vehicle_bullets(vm, n, weapon, &aim);
                None
            }
            WeaponType::Projectile => {
                let fwd = m[0];
                let spread = info.ads_spread.to_radians().tan() * 16.0;
                let theta = self.random_f32() * 360.0;
                let r = self.random_f32();
                let (s, c) = math::sincos_deg(theta);
                let mut dir = mad([0.0; 3], 16.0, fwd);
                dir = mad(dir, r * c * spread, m[1]);
                dir = mad(dir, r * s * spread, m[2]);
                let rocket = self.launch_rocket(n, weapon, origin, normalized(dir))?;
                if let Some(e) = self.ent_mut(rocket)
                    && let Some(mi) = e.missile.as_mut()
                    && let Some((t, off)) = target
                {
                    mi.target = Some(t);
                    mi.target_offset = off;
                }
                Some(rocket)
            }
            _ => return Err("Vehicles only support bullet and projectile weapons".into()),
        };
        self.tempev.add(now, crate::tempev::ev::WEAPON_FIRE, |s| {
            s.origin = origin;
            s.angles = gun_angles;
            s.weapon = weapon;
            s.client = n;
        });
        vm.notify_entity(n, "weapon_fired", &[]);
        Ok(spent)
    }

    /// `Bullet_Fire` from a vehicle: no spread, no perks; the vehicle is the attacker.
    fn vehicle_bullets(&mut self, vm: &mut Vm, n: u16, weapon: u16, aim: &AimBasis) {
        self.ensure_player_anims();
        let penetration = self.penetration_table();
        let params = WeaponParams::default();
        let mut hits: Vec<BulletHit> = Vec::new();
        {
            let Some(info) = self.weapons.get(weapon) else {
                return;
            };
            let p = BulletParams {
                attacker: n,
                info,
                perks: 0,
                params: &params,
                penetration: &penetration,
                friendly_fire: self.cvars.int("scr_friendlyfire") != 0,
            };
            self.bullet_hits(&p, aim, 0.0, self.level.time, &mut hits);
        }
        self.apply_bullet_hits(vm, n, weapon, hits);
    }

    /// Every vehicle entity with its number.
    pub fn vehicles(&self) -> impl Iterator<Item = (u16, &Ent, &Vehicle)> {
        self.in_use()
            .filter_map(|(n, e)| e.veh.as_deref().map(|v| (n, e, v)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cobra() -> Vehicle {
        let info = VehicleInfo {
            name: "cobra_mp".into(),
            accel: 20.0 * MPH,
            turret_weapon: "cobra_20mm_mp".into(),
            span_left: 120.0,
            span_right: 120.0,
            span_up: 25.0,
            span_down: 120.0,
            turret_rot_rate: 80.0,
        };
        let mut v = Vehicle::new(Rc::new(info), 0, [0.0; 3], 1);
        v.manual_speed = 60.0 * MPH;
        v.manual_accel = 25.0 * MPH;
        v.manual_decel = 12.5 * MPH;
        v.max_angle_vel[1] = 75.0;
        v.yaw_accel = 45.0;
        v.yaw_decel = 45.0;
        v.near_goal_dist = 256.0;
        v
    }

    fn fly(v: &mut Vehicle, pos: &mut Vec3, steps: usize) -> Vec<(usize, Notify)> {
        let mut seen = Vec::new();
        let mut r = || 0.5;
        for i in 0..steps {
            let mut ev = Vec::new();
            v.think_move(pos, &mut r, &mut ev);
            seen.extend(ev.into_iter().map(|e| (i, e)));
        }
        seen
    }

    #[test]
    fn parses_the_stock_vehicle_file() {
        let src = b"VEHICLEFILE\\type\\helicopter\\maxSpeed\\60\\accel\\20\\turretWeapon\\cobra_20mm_mp\\turretHorizSpanLeft\\120\\turretHorizSpanRight\\110\\turretVertSpanUp\\25\\turretVertSpanDown\\100\\turretRotRate\\80\\";
        let i = VehicleInfo::parse("cobra_mp", src).unwrap();
        assert_eq!(i.accel, 352.0);
        assert_eq!(
            (i.span_left, i.span_right, i.span_up, i.span_down),
            (120.0, 110.0, 25.0, 100.0)
        );
        assert_eq!(i.turret_weapon, "cobra_20mm_mp");
        assert!(VehicleInfo::parse("x", b"WEAPONFILE\\a\\b").is_err());
    }

    /// A stop at the goal comes to rest on it, hovers there, and the script hears `goal` once on the way in.
    #[test]
    fn stops_at_the_goal_and_hovers() {
        let mut v = cobra();
        v.state = MoveState::Move;
        v.stop_at_goal = true;
        v.goal = [3000.0, 0.0, 0.0];
        let mut pos = [0.0; 3];
        let ev = fly(&mut v, &mut pos, 600);
        assert!(ev.iter().any(|(_, e)| *e == Notify::Goal), "{ev:?}");
        assert_eq!(v.state, MoveState::Hover);
        assert!(length(sub(pos, v.goal)) < 60.0, "at {pos:?}");
        assert!(v.speed < 2.0 * MPH + 5.0);
        assert!(v.angles[1].abs() < 1.0);
    }

    /// Without a stop it passes the goal at speed (notifying `goal` as it gets within one step).
    #[test]
    fn flies_through_a_waypoint() {
        let mut v = cobra();
        v.state = MoveState::Move;
        v.goal = [4000.0, 0.0, 0.0];
        let mut pos = [0.0; 3];
        let ev = fly(&mut v, &mut pos, 400);
        let near = ev.iter().filter(|(_, e)| *e == Notify::NearGoal).count();
        assert!(near > 0);
        let first = ev.iter().find(|(_, e)| *e == Notify::Goal).expect("goal");
        assert!(first.0 < 400);
        assert!(v.speed > 20.0 * MPH, "speed {}", v.speed);
        assert!(v.speed <= 60.0 * MPH + 1.0);
    }

    /// It faces where it flies and leans into the acceleration (nose down speeding up).
    #[test]
    fn turns_to_its_heading_and_pitches_into_acceleration() {
        let mut v = cobra();
        v.state = MoveState::Move;
        v.goal = [0.0, 6000.0, 0.0];
        let mut pos = [0.0; 3];
        fly(&mut v, &mut pos, 40);
        assert!(v.angles[0] > 5.0, "pitch {}", v.angles[0]);
        assert!(v.angles[0] <= 30.5);
        fly(&mut v, &mut pos, 100);
        assert!((v.angles[1] - 90.0).abs() < 5.0, "yaw {}", v.angles[1]);
        assert!(pos[1] > 500.0);
    }

    /// However hard it is pushed, in a straight run or a turn, pitch and roll stay within `setmaxpitchroll`, and
    /// the leaning never turns faster than the angular velocity limits.
    #[test]
    fn pitch_and_roll_stay_within_the_limits() {
        let mut v = cobra();
        v.max_pitch = 30.0;
        v.max_roll = 20.0;
        v.state = MoveState::Move;
        let mut pos = [0.0; 3];
        let (mut pitch, mut roll) = (0.0f32, 0.0f32);
        // Out, then hard back and across, so it brakes, turns and leans every way.
        for goal in [
            [6000.0, 0.0, 0.0],
            [-4000.0, 3000.0, 0.0],
            [500.0, -5000.0, 0.0],
        ] {
            v.goal = goal;
            for _ in 0..300 {
                fly(&mut v, &mut pos, 1);
                pitch = pitch.max(v.angles[0].abs());
                roll = roll.max(v.angles[2].abs());
                assert!(
                    v.rot_vel[0].abs() <= v.max_angle_vel[0] + 0.1,
                    "{:?}",
                    v.rot_vel
                );
                assert!(
                    v.rot_vel[2].abs() <= v.max_angle_vel[2] + 0.1,
                    "{:?}",
                    v.rot_vel
                );
            }
        }
        // The brake may carry it a little past its target before it settles, never far.
        assert!(pitch > 10.0 && pitch <= 33.0, "pitch {pitch}");
        assert!(roll > 3.0 && roll <= 22.0, "roll {roll}");
    }

    /// A vehicle nobody told otherwise has the original's physics defaults.
    #[test]
    fn new_vehicles_have_the_original_defaults() {
        let v = Vehicle::new(cobra().info, 0, [0.0; 3], 1);
        assert_eq!((v.max_pitch, v.max_roll), (25.0, 25.0));
        assert_eq!(v.max_angle_vel, [45.0, 90.0, 45.0]);
        assert_eq!((v.yaw_accel, v.yaw_decel), (25.0, 15.0));
    }

    /// A goal yaw is reached and announced.
    #[test]
    fn announces_the_goal_yaw() {
        let mut v = cobra();
        v.state = MoveState::Hover;
        v.goal = [0.0; 3];
        v.hover_goal = [10.0, 0.0, 0.0];
        v.goal_yaw = Some(60.0);
        let mut pos = [0.0; 3];
        let ev = fly(&mut v, &mut pos, 200);
        assert!(
            ev.iter().any(|(_, e)| *e == Notify::GoalYaw),
            "yaw {}",
            v.angles[1]
        );
        assert!((v.angles[1] - 60.0).abs() < 2.0);
    }
}
