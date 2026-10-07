// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared simulation: collision traces, player movement, weapons.
//!
//! Everything here is plain data and pure functions: it builds for `wasm32` (client prediction
//! in the browser) and does not allocate per trace or per `pmove` call.

pub mod cm;
pub mod contents;
pub mod pm;

pub type Vec3 = [f32; 3];
