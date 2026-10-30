// SPDX-License-Identifier: GPL-3.0-only
//! The client's half of the dev control channel: where the channel's requests are answered against the running game.
//!
//! [`Control`] is all the game state and input the requests need, so the request handling ([`handle`]) can be read
//! and tested without a window: it names the player's own paths (the menus' pointer, the console's field, the input
//! layer's binds) rather than the internals the `--ui-script` steps reach for.
//!
//! A request that cannot be answered yet (a wait, a capture, a step) returns [`devctrl::Outcome::Wait`] and is offered
//! again next frame; the frame count and the wait clock are the host's.

pub mod host;

pub use host::{Control, handle};

/// The state document's sections: what `{"cmd":"state"}` without a `what` returns, and what a `wait` or an `assert`
/// builds for the paths it names.
pub const PARTS: &[&str] = &[
    "client",
    "menu",
    "player",
    "entities",
    "scoreboard",
    "hud",
    "draw",
    "sound",
    "fx",
    "events",
    "net",
    "console",
    "cvars",
    "binds",
    "view",
    "server",
];

use crate::Cli;
use devctrl::Channel;

/// Opens the channel the command line asked for. `None` when it did not: the channel is off unless a flag turns it
/// on, and then it is bound to loopback or to this process's stdin. In the browser the page's `?control` turns it
/// on, and the page is the wire (`crate::web::PageTransport`).
pub fn open(cli: &Cli) -> Option<Channel> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = cli;
        if crate::web::control_wanted() {
            return Some(Channel::new(Box::new(crate::web::PageTransport)));
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(port) = cli.control {
            let file = cli.control_file.as_deref().map(std::path::Path::new);
            return match devctrl::TcpTransport::bind(port, file) {
                Ok(t) => Some(Channel::new(Box::new(t))),
                Err(e) => {
                    tracing::error!(error = %e, "the control channel did not open");
                    None
                }
            };
        }
        if cli.control_stdio {
            return Some(Channel::new(Box::new(devctrl::StdioTransport::new())));
        }
    }
    None
}
