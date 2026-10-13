// SPDX-License-Identifier: GPL-3.0-only
//! Shared simulation: collision traces, player movement, weapons.
//!
//! Everything here is plain data and pure functions: it builds for `wasm32` (client prediction
//! in the browser) and does not allocate per trace or per `pmove` call.

// Not yet converted to `tracing` (#377): the print lints come on for this crate when its prints are gone.
#![allow(clippy::print_stderr, clippy::print_stdout)]

pub mod cm;
pub mod contents;
pub mod pm;
mod props;
pub mod shake;
pub mod skel;
pub mod traj;
pub mod weapon;
pub mod world;

pub type Vec3 = [f32; 3];
