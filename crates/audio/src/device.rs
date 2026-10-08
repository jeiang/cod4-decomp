// SPDX-License-Identifier: GPL-3.0-or-later
//! The sound output: [`Config::probe`], [`Config::start`], and an [`Output`] that plays until dropped.
//!
//! Natively that is a cpal stream running [`Mixer::fill`](crate::mixer::Mixer::fill) in its callback; in a
//! browser (`wasm32`) it is an AudioWorklet fed from the page's main thread. Both expose the same surface.

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{Config, Output};

#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
pub use web::{Config, Output};
