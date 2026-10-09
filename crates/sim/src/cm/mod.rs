// SPDX-License-Identifier: GPL-3.0-only
//! Collision world, box/capsule traces, point contents.

// The trace code indexes several parallel per-axis arrays; iterators would obscure it.
#![allow(clippy::needless_range_loop)]

use crate::Vec3;

mod brushes;
mod capsule;
mod map;
mod mesh;
mod query;
mod sight;
mod transform;
mod tw;
mod vec;

#[cfg(test)]
mod install_tests;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(test)]
mod tests;

pub use map::{ClipModel, CollisionWorld};
pub use query::Pvs;
pub(crate) use tw::Tw;

pub const ENTITYNUM_NONE: u16 = 1023;
pub const ENTITYNUM_WORLD: u16 = 1022;

/// The result of a swept-bounds trace (`trace_t`). `fraction == 1.0` means nothing was hit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trace {
    pub fraction: f32,
    pub normal: Vec3,
    pub surface_flags: i32,
    pub contents: i32,
    /// Index into `Clipmap::materials` of the surface hit, or `u32::MAX`.
    pub material: u32,
    /// The entity hit: [`ENTITYNUM_WORLD`] for the static map, [`ENTITYNUM_NONE`] for nothing.
    pub hit_id: u16,
    pub all_solid: bool,
    pub start_solid: bool,
    pub walkable: bool,
}

impl Trace {
    pub const MISS: Self = Self {
        fraction: 1.0,
        normal: [0.0; 3],
        surface_flags: 0,
        contents: 0,
        material: u32::MAX,
        hit_id: ENTITYNUM_NONE,
        all_solid: false,
        start_solid: false,
        walkable: false,
    };
}

/// What player movement (server and client prediction) needs from the world.
/// The server implements it with every linked entity; the client with the static map plus the
/// entities it knows of.
pub trait Collide {
    /// Swept `mins..maxs` from `start` to `end` against everything matching `mask`, ignoring
    /// entity `pass_ent` (and anything it owns).
    fn trace(
        &self,
        start: Vec3,
        end: Vec3,
        mins: Vec3,
        maxs: Vec3,
        pass_ent: u16,
        mask: i32,
    ) -> Trace;
    /// Union of the contents of everything at `p`.
    fn point_contents(&self, p: Vec3, pass_ent: u16, mask: i32) -> i32;
}
