// SPDX-License-Identifier: GPL-3.0-only
//! Stages that misbehave on purpose, to prove the watchdog catches each way a
//! child can go wrong.
use crate::stage::{StageCtx, StageReport};
use std::io;

pub fn crash(_: &StageCtx) -> io::Result<StageReport> {
    // SAFETY: none; this is a deliberate access violation, caught by the
    // crash handler the child installed.
    unsafe { std::ptr::null_mut::<u8>().write_volatile(1) };
    unreachable!("the write above faults")
}

pub fn hang(_: &StageCtx) -> io::Result<StageReport> {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

pub fn panic(_: &StageCtx) -> io::Result<StageReport> {
    panic!("selftest panic");
}
