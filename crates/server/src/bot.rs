// SPDX-License-Identifier: GPL-3.0-or-later
//! Server-side bots: they inject usercmds directly (no netchan).

use sim::pm::{UserCmd, button};

use crate::client::Session;
use crate::game::Game;

/// Per-bot driving state.
pub struct Brain {
    pub num: u16,
}

impl Brain {
    pub fn new(num: u16) -> Self {
        Self { num }
    }

    /// The command the bot sends for the tick ending at `server_time`.
    pub fn usercmd(&mut self, g: &Game, n: u16, server_time: i32) -> UserCmd {
        let mut cmd = UserCmd {
            server_time,
            ..UserCmd::default()
        };
        let playing = g.client(n).is_some_and(|c| c.session == Session::Playing);
        if !playing {
            // Waiting to respawn: tap use on alternate ticks so the script sees a press.
            if (server_time / 33) % 2 == 0 {
                cmd.buttons |= button::USE;
            }
        }
        if let Some(c) = g.client(n) {
            cmd.angles = [
                (c.ps.viewangles[0] / sim::pm::ANGLE_UNIT) as i32,
                (c.ps.viewangles[1] / sim::pm::ANGLE_UNIT) as i32,
                0,
            ];
        }
        cmd
    }
}
