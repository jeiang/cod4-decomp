// SPDX-License-Identifier: GPL-3.0-only
//! Player movement (`Pmove`): shared by the server and client prediction.
//!
//! One [`pmove`] call consumes one user command: it splits the time since the player's last
//! command into steps of at most 66 ms and runs the original's `PmoveSingle` for each. Nothing
//! here allocates; the touched-entity list, the event ring and every scratch value are fixed
//! size. The world is reached only through [`Collide`](crate::cm::Collide).
//!
//! Determinism: all arithmetic is `f32` with the operation order of the original, no fused
//! multiply-add, and trigonometry from [`math`] (not the platform libm), so x86-64, ARM64 and
//! wasm32 produce identical states for identical inputs.
//!
//! The weapon state machine (`PM_Weapon`) and the aim-spread update run inside `pmove` when
//! [`Pmove::weapons`] carries a [`WeaponCtx`]; without it movement sees the weapon only through
//! [`Pmove::weapon`] and the weapon fields of [`PlayerState`], as before. Animation script
//! events are not modelled.

// Arithmetic keeps the original's operand order (`a = b * a`, `x * -1`) so results match bit for
// bit, and the per-axis loops index parallel arrays.
#![allow(
    clippy::assign_op_pattern,
    clippy::neg_multiply,
    clippy::needless_range_loop
)]

mod ads;
pub mod damage;
mod duck;
mod footsteps;
mod jump;
mod ladder;
mod mantle;
pub mod math;
mod params;
mod pm_weapon;
mod prone;
mod single;
mod slide;
mod sprint;
mod state;
mod view;
mod walk;

#[cfg(test)]
mod install_tests;
#[cfg(any(test, feature = "test-support"))]
pub mod test_world;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod weapon_install_tests;
#[cfg(test)]
mod weapon_tests;

pub use mantle::{
    MANTLE_ANIM_COUNT, MANTLE_ANIM_NAMES, MantleAnim, MantleAnims, MantleTransition, TRANSITIONS,
    weapon_inactive,
};
pub use params::{Params, WeaponMove};
pub use sprint::{max_sprint_ms, sprint_left_ms};
pub use state::*;

use crate::Vec3;
use crate::cm::{Collide, ENTITYNUM_NONE, ENTITYNUM_WORLD, Trace};
use crate::contents::{MASK_CHARACTER, MASK_PLAYERSOLID};
use crate::weapon::{WeaponCtx, WeaponOut};

/// Most entities one `pmove` call reports as touched.
pub const MAX_TOUCH: usize = 32;

/// Standing bounds (`playerMins` / `playerMaxs`).
pub const PLAYER_MINS: Vec3 = [-15.0, -15.0, 0.0];
pub const PLAYER_MAXS: Vec3 = [15.0, 15.0, 70.0];

/// Input and output of one movement call (`pmove_t`).
#[derive(Debug)]
pub struct Pmove<'a> {
    pub ps: PlayerState,
    pub cmd: UserCmd,
    pub oldcmd: UserCmd,
    pub tracemask: i32,
    pub num_touch: usize,
    pub touch_ents: [u16; MAX_TOUCH],
    /// Current bounds; `PM_CheckDuck` rewrites the height for the stance every step.
    pub mins: Vec3,
    pub maxs: Vec3,
    pub xyspeed: f32,
    pub prone_change: bool,
    pub mantle_started: bool,
    pub mantle_end_pos: Vec3,
    pub mantle_duration: i32,
    /// Smoothed stair-step view offset: set when a step moves the player vertically.
    pub view_change_time: i32,
    pub view_change: f32,
    /// What movement reads about the weapon on show. With [`weapons`](Self::weapons) set,
    /// `pmove` keeps it in step with the player's weapon; otherwise the caller sets it.
    pub weapon: WeaponMove,
    /// The weapon table and inventory: set to run the weapon state machine. The caller sets
    /// `cmd.weapon` (and `cmd.offhand_index`) to the weapons the player asks for.
    pub weapons: Option<WeaponCtx<'a>>,
    /// What the weapon state machine did during this `pmove` call; cleared at its start.
    pub weapon_out: WeaponOut,
    /// The active shellshock slows movement (`shellshock.movement.affect`).
    pub shellshock_slows: bool,
    pub params: &'a Params,
}

impl<'a> Pmove<'a> {
    pub fn new(ps: PlayerState, params: &'a Params) -> Self {
        Self {
            ps,
            cmd: UserCmd::default(),
            oldcmd: UserCmd::default(),
            tracemask: MASK_PLAYERSOLID,
            num_touch: 0,
            touch_ents: [0; MAX_TOUCH],
            mins: PLAYER_MINS,
            maxs: PLAYER_MAXS,
            xyspeed: 0.0,
            prone_change: false,
            mantle_started: false,
            mantle_end_pos: [0.0; 3],
            mantle_duration: 0,
            view_change_time: 0,
            view_change: 0.0,
            weapon: WeaponMove::default(),
            weapons: None,
            weapon_out: WeaponOut::default(),
            shellshock_slows: false,
            params,
        }
    }

    /// The entities touched so far.
    pub fn touched(&self) -> &[u16] {
        &self.touch_ents[..self.num_touch]
    }

    /// `PM_AddTouchEnt`: remember an entity once; the world is not an entity.
    pub(crate) fn add_touch_ent(&mut self, entity: u16) {
        if entity != ENTITYNUM_WORLD
            && entity != ENTITYNUM_NONE
            && self.num_touch != MAX_TOUCH
            && !self.touched().contains(&entity)
        {
            self.touch_ents[self.num_touch] = entity;
            self.num_touch += 1;
        }
    }

    /// `PM_trace`: a plain trace that ignores the player's own entity.
    pub(crate) fn trace(
        &self,
        world: &dyn Collide,
        start: Vec3,
        mins: Vec3,
        maxs: Vec3,
        end: Vec3,
        mask: i32,
    ) -> Trace {
        world.trace(start, end, mins, maxs, self.ps.client_num, mask)
    }

    /// Trace with the current bounds and mask.
    pub(crate) fn trace_body(&self, world: &dyn Collide, start: Vec3, end: Vec3) -> Trace {
        self.trace(world, start, self.mins, self.maxs, end, self.tracemask)
    }

    /// `PM_playerTrace`: when the start is stuck in another character, touch it and retry
    /// without characters for the rest of the call.
    pub(crate) fn player_trace(
        &mut self,
        world: &dyn Collide,
        start: Vec3,
        mins: Vec3,
        maxs: Vec3,
        end: Vec3,
        mask: i32,
    ) -> Trace {
        let mut t = self.trace(world, start, mins, maxs, end, mask);
        if t.start_solid && t.contents & MASK_CHARACTER != 0 {
            self.add_touch_ent(t.hit_id);
            self.tracemask &= !MASK_CHARACTER;
            t = self.trace(world, start, mins, maxs, end, mask & !MASK_CHARACTER);
        }
        t
    }

    /// `PM_playerTrace` with the current bounds and mask.
    pub(crate) fn player_trace_body(
        &mut self,
        world: &dyn Collide,
        start: Vec3,
        end: Vec3,
    ) -> Trace {
        self.player_trace(world, start, self.mins, self.maxs, end, self.tracemask)
    }
}

/// Per-step working data (`pml_t`).
#[derive(Debug, Clone)]
pub(crate) struct Pml {
    pub forward: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub frametime: f32,
    pub msec: i32,
    pub walking: bool,
    pub ground_plane: bool,
    pub almost_ground_plane: bool,
    pub ground_trace: Trace,
    pub impact_speed: f32,
    pub previous_origin: Vec3,
    pub previous_velocity: Vec3,
}

/// Runs one user command (`Pmove`): see the module docs. The command's `server_time` is the
/// time the player's state advances to.
pub fn pmove(pm: &mut Pmove<'_>, world: &impl Collide) {
    single::pmove(pm, world);
}

/// What one command left besides the player state.
pub struct CmdOut {
    pub weapon_out: WeaponOut,
    /// Entities the player's box touched.
    pub touched: Vec<u16>,
    pub mins: Vec3,
    pub maxs: Vec3,
    /// Set when the command began a mantle: where it ends and how long it takes in ms.
    pub mantle: Option<(Vec3, i32)>,
}

/// Runs one user command exactly as the server does, so a client predicting its own player
/// gets the same state from the same inputs: the weapon the player holds or has asked for goes
/// into the command, `g_speed` into the state, and the trace mask follows the state.
#[allow(clippy::too_many_arguments)]
pub fn run_usercmd(
    ps: &mut PlayerState,
    inv: &mut crate::weapon::PlayerWeapons,
    mut cmd: UserCmd,
    old_cmd: UserCmd,
    g_speed: i32,
    table: &crate::weapon::WeaponTable,
    params: &Params,
    world: &impl Collide,
) -> CmdOut {
    // The weapon the player asked for: a script's `switchtoweapon`, else what it holds or last held.
    cmd.weapon = u8::try_from(inv.wanted(ps.weapon as u16)).unwrap_or(0);
    let mut pm = Pmove::new(std::mem::take(ps), params);
    pm.weapons = Some(WeaponCtx::new(table, inv));
    pm.cmd = cmd;
    pm.oldcmd = old_cmd;
    pm.tracemask = if pm.ps.pm_type < PmType::Dead {
        crate::contents::MASK_PLAYERSOLID
    } else {
        crate::contents::MASK_DEADSOLID
    };
    pm.ps.speed = g_speed;
    pmove(&mut pm, world);
    let touched = pm.touched().to_vec();
    let (mins, maxs) = (pm.mins, pm.maxs);
    let weapon_out = pm.weapon_out;
    let mantle = pm
        .mantle_started
        .then_some((pm.mantle_end_pos, pm.mantle_duration));
    *ps = pm.ps;
    inv.remember(ps.weapon as u16);
    CmdOut {
        weapon_out,
        touched,
        mins,
        maxs,
        mantle,
    }
}
