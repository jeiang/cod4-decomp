// SPDX-License-Identifier: GPL-3.0-only
//! The dev control channel: one line of JSON per request, one line of JSON per reply.
//!
//! It is how an agent, the harness or an MCP wrapper drives a running client or server without a screen
//! recorder or OS-level tools: inject the player's input, capture the presented frame, read the game's state and
//! check it against what was expected.
//!
//! The channel is off unless a flag turns it on (see the client's `--control`). The wire is a [`Transport`]:
//! loopback TCP or stdin for the native binaries, a request/reply queue for the browser, whose page polls it.
//! The requests are the same in every case; only the bytes' route differs.
//!
//! Requests, parsed by [`Request`] and answered by a [`Handler`]:
//!
//! ```text
//! {"id": 1, "cmd": "state", "what": "player"}
//! {"id": 2, "cmd": "key", "name": "w", "action": "down"}
//! {"id": 3, "cmd": "text", "text": "bots 3"}
//! {"id": 4, "cmd": "wait", "state": {"player.health": {"gt": 0}}, "timeout_ms": 5000}
//! ```
//!
//! A handler that cannot answer yet returns [`Outcome::Wait`]; the channel keeps the request at the front of its
//! queue and offers it again on the next frame, so a wait costs no thread and no sleep. Replies keep the request's
//! `id`, so a client can pair them even when it batches requests.

mod channel;
mod protocol;
mod transport;

pub use channel::{Channel, Handler, Outcome};
pub use protocol::{Reply, Request, base64, lookup, matches, num, reply, text_lines};
pub use transport::{QueueTransport, StdioTransport, TcpTransport, Transport};

/// The most bytes one request or reply line may hold; a longer line is refused rather than buffered for ever.
pub const MAX_LINE: usize = 4 * 1024 * 1024;

/// The JSON `null`, for a request without an `id`.
pub fn null() -> serde_json::Value {
    serde_json::Value::Null
}
