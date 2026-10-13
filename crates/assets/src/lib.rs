// SPDX-License-Identifier: GPL-3.0-only
//! Original-install access: VFS, IWD, fastfile decoding, IWI, typed assets.

#![cfg_attr(test, allow(clippy::print_stderr, clippy::print_stdout))]
pub mod crc;
pub mod fs;
pub mod paths;
pub mod vfs;

pub mod zone;

pub mod iwi;
