// SPDX-License-Identifier: GPL-3.0-only
//! Server-side player skeletons and locational hit detection.
//!
//! The server never renders a player but its bullets still hit individual body parts, so it
//! poses the same skeleton the client draws: [`anim`] samples `XAnimParts`, [`rig`] composes
//! bones from weighted animations plus the view-angle controllers, [`trace`] intersects a
//! segment with the per-bone hit volumes, and [`hitloc`] names the result.
//!
//! Fact sources are the engine's `DObjCalcSkel`, `XAnimCalcParts`, `DObjTraceline` and
//! `BG_Player_DoControllers*`; the stock player models' hit volumes are oriented boxes in
//! `bone_info`, not triangles (they have no `coll_surfs`).

pub mod anim;
pub mod controllers;
pub mod hitloc;
pub mod quat;
pub mod rig;
pub mod trace;
pub mod turret;

pub use hitloc::{HitLocation, Stance, box_hit_location};
pub use rig::{
    AnimBinding, AnimLayer, BoneMat, Controllers, MAX_BONES, Pose, Rig, RigModel, TURRET_BONES,
};
pub use trace::{LocHit, Placement, locational_trace, trace_player};

#[cfg(test)]
mod install_tests;
#[cfg(test)]
mod tests;
