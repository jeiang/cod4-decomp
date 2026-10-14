// SPDX-License-Identifier: GPL-3.0-only
//! Runs scenarios and writes run bundles. See `cod4e-harness --help`.

#![cfg_attr(test, allow(clippy::print_stderr, clippy::print_stdout))]
pub mod bundle;
pub mod child;
pub mod diff;
pub mod logcheck;
pub mod manifest;
pub mod perf;
pub mod script;
pub mod scrub;
pub mod stage;
pub mod stages;
pub mod suite;
pub mod time;
pub mod trace;
pub mod watchdog;
