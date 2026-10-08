// SPDX-License-Identifier: GPL-3.0-only
//! The `--show-models` smoke scene: a row of stock player models in different poses and the first-person weapon in
//! front of a fixed camera, so a screenshot shows whether skinning, textures and lighting are right.

use crate::flythrough;
use crate::models::{Library, Player, PlayerModelSet, Team};
use crate::viewmodel::ViewModel;
use glam::Vec3;
use render::ModelInstance;
use render::lightgrid::SightTrace;
use server::playeranim::{PlayerPoseInput, StanceInput};
use sim::pm::{PlayerState, weapon_state};

/// What a showcase player does.
#[derive(Clone, Copy, Debug)]
pub struct Pose {
    pub label: &'static str,
    pub input: PlayerPoseInput,
}

fn pose(label: &'static str, f: impl FnOnce(&mut PlayerPoseInput)) -> Pose {
    let mut input = PlayerPoseInput::default();
    f(&mut input);
    Pose { label, input }
}

/// The poses the row cycles through.
pub fn poses() -> [Pose; 7] {
    [
        pose("stand idle", |_| {}),
        pose("run", |i| {
            i.speed = 190.0;
            i.trying_to_move = true;
        }),
        pose("crouch idle", |i| i.stance = StanceInput::Crouch),
        pose("prone idle", |i| i.stance = StanceInput::Prone),
        pose("sprint", |i| {
            i.speed = 280.0;
            i.trying_to_move = true;
            i.sprinting = true;
        }),
        pose("crouch walk", |i| {
            i.stance = StanceInput::Crouch;
            i.speed = 80.0;
            i.trying_to_move = true;
            i.walking = true;
        }),
        pose("death", |i| i.dead = true),
    ]
}

struct Actor {
    player: Player,
    pose: Pose,
    origin: [f32; 3],
    yaw: f32,
    started: bool,
}

pub struct Showcase {
    actors: Vec<Actor>,
    vm: ViewModel,
    ps: PlayerState,
    camera: (Vec3, f32, f32),
    clock: f32,
    fire_ms: i32,
    reload_ms: i32,
}

/// Height of the first solid surface under `p` (searching `up` above and `down` below), by bisection on a sight trace.
fn ground(world: &dyn SightTrace, p: Vec3, up: f32, down: f32) -> Option<f32> {
    let top = [p.x, p.y, p.z + up];
    let bottom = [p.x, p.y, p.z - down];
    if world.blocked(top, [top[0], top[1], top[2] - 0.01]) || !world.blocked(top, bottom) {
        return None;
    }
    let (mut lo, mut hi) = (0.0f32, up + down);
    for _ in 0..24 {
        let mid = (lo + hi) * 0.5;
        if world.blocked(top, [top[0], top[1], top[2] - mid]) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Some(top[2] - hi)
}

/// A camera on the flythrough path with a clear stage ahead: ground under the whole row and a free line of sight.
fn find_camera(
    world: &dyn SightTrace,
    tour: &flythrough::Tour,
    reach: f32,
    lit: &dyn Fn(Vec3) -> bool,
) -> Option<(Vec3, f32, f32)> {
    for step in 0..192 {
        let p = tour.pose(step as f32 * tour.period() / 192.0);
        let eye = p.origin;
        let (fx, fy) = (p.yaw.cos(), p.yaw.sin());
        let Some(gz) = ground(world, eye, 40.0, 400.0) else {
            continue;
        };
        let eye = Vec3::new(eye.x, eye.y, gz + 60.0);
        // Models stand where the light grid has data; elsewhere the renderer paints them in its "missing
        // light grid" colour on purpose, which is what a stage outside the playable area looks like.
        let clear = lit(eye)
            && [0.25f32, 0.5, 1.0].iter().all(|k| {
                let d = reach * k;
                let at = Vec3::new(eye.x + fx * d, eye.y + fy * d, eye.z);
                lit(Vec3::new(at.x, at.y, gz + 36.0))
                    && !world.blocked(eye.to_array(), at.to_array())
                    && !world.blocked(eye.to_array(), [at.x - fy * 150.0, at.y + fx * 150.0, at.z])
                    && !world.blocked(eye.to_array(), [at.x + fy * 150.0, at.y - fx * 150.0, at.z])
                    && ground(world, at, 40.0, 100.0).is_some_and(|z| (z - gz).abs() < 6.0)
            });
        if clear {
            return Some((eye, p.yaw, 0.0));
        }
    }
    None
}

impl Showcase {
    /// `count` players (the 7 poses, cycling) at `reach` units from the camera, and the view model of `weapon`.
    pub fn new(
        lib: &mut Library,
        world: &dyn SightTrace,
        tour: &flythrough::Tour,
        weapon: &str,
        count: usize,
        lit: &dyn Fn(Vec3) -> bool,
    ) -> Result<Showcase, String> {
        let reach = 190.0 + 40.0 * (count.div_ceil(7).saturating_sub(1)) as f32;
        let (eye, yaw, pitch) = find_camera(world, tour, reach, lit)
            .ok_or("no clear spot for the stage on the flythrough path")?;
        let def = lib
            .content
            .weapon(weapon)
            .cloned()
            .ok_or_else(|| format!("weapon {weapon} not loaded"))?;
        let held = def
            .world_models
            .first()
            .cloned()
            .flatten()
            .and_then(|m| m.name.as_deref().map(str::to_owned));
        let armed = |s: PlayerModelSet| PlayerModelSet {
            weapon: held.clone(),
            ..s
        };
        let allies = lib
            .team_models(Team::Allies)
            .map(armed)
            .ok_or("no allied player models in the loaded zones")?;
        let axis = lib.team_models(Team::Axis).map(armed);
        let poses = poses();
        let (fx, fy) = (yaw.cos(), yaw.sin());
        let ground_z = eye.z - 60.0;
        let mut actors = Vec::new();
        for i in 0..count {
            let (row, col) = (i / 7, i % 7);
            let set = if i % 2 == 1 {
                axis.as_ref().unwrap_or(&allies)
            } else {
                &allies
            };
            let along = 190.0 + 40.0 * row as f32;
            let across = (col as f32 - 3.0) * 48.0;
            let origin = [
                eye.x + fx * along - fy * across,
                eye.y + fy * along + fx * across,
                ground_z,
            ];
            actors.push(Actor {
                player: lib.player(set)?,
                pose: poses[i % poses.len()],
                origin,
                // Face the camera.
                yaw: yaw.to_degrees() + 180.0 + (col as f32 - 3.0) * 6.0,
                started: false,
            });
        }
        let vm = ViewModel::new(&lib.content, &def, None)?;
        let ps = PlayerState {
            origin: [eye.x, eye.y, ground_z],
            view_height_current: 60.0,
            viewangles: [-pitch.to_degrees(), yaw.to_degrees(), 0.0],
            ..PlayerState::default()
        };
        Ok(Showcase {
            actors,
            vm,
            ps,
            camera: (eye, yaw, pitch),
            clock: 0.0,
            fire_ms: def.fire_time.max(50),
            reload_ms: def.reload_time.max(500),
        })
    }

    /// Camera position, yaw and pitch (radians).
    pub fn camera(&self) -> (Vec3, f32, f32) {
        self.camera
    }

    /// `label: animation` of every actor and the view model's animation slot, for the run log.
    pub fn describe(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .actors
            .iter()
            .map(|a| format!("{}: {}", a.pose.label, a.player.animation().unwrap_or("-")))
            .collect();
        let p = self.vm.playing();
        v.push(format!("viewmodel: slot {} at {:.2}", p.slot, p.time));
        v
    }

    /// Advances `dt` seconds and returns every model to draw.
    pub fn update(&mut self, dt: f32) -> Vec<ModelInstance> {
        self.clock += dt;
        let mut out = Vec::new();
        for a in &mut self.actors {
            let mut input = a.pose.input;
            input.yaw = a.yaw;
            if input.dead && !a.started {
                // A death is chosen from the frame before: stand first.
                a.player.update(
                    0.0,
                    &PlayerPoseInput {
                        dead: false,
                        ..input
                    },
                );
            }
            a.started = true;
            a.player.update(dt, &input);
            out.extend(a.player.instances(a.origin));
        }
        // Idle, three shots, then a reload, repeating.
        let (idle, shots) = (1.0, 3);
        let fire = self.fire_ms as f32 / 1000.0;
        let reload = self.reload_ms as f32 / 1000.0;
        let t = self.clock % (idle + shots as f32 * fire + reload);
        let (state, left) = if t < idle {
            (weapon_state::READY, 0.0)
        } else if t < idle + shots as f32 * fire {
            let k = (t - idle) % fire;
            (weapon_state::FIRING, fire - k)
        } else {
            (
                weapon_state::RELOADING,
                reload - (t - idle - shots as f32 * fire),
            )
        };
        self.ps.weapon_state = state;
        self.ps.weapon_time = (left * 1000.0) as i32;
        out.extend(self.vm.update(&self.ps, dt));
        out
    }
}
