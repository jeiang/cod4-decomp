// SPDX-License-Identifier: GPL-3.0-only
//! Original-install access: VFS, IWD, fastfile decoding, IWI, typed assets.

// Not yet converted to `tracing` (#377): the print lints come on for this crate when its prints are gone.
#![allow(clippy::print_stderr, clippy::print_stdout)]
pub mod crc;
pub mod fs;
pub mod paths;
pub mod vfs;

pub mod zone;

pub mod iwi;
