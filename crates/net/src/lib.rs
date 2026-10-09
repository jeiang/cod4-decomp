// SPDX-License-Identifier: GPL-3.0-only
//! Netchan, snapshot delta codec, and transport trait.
//!
//! Layers, bottom up: a [`Transport`] moves datagrams; a [`Netchan`] sequences, fragments and
//! reassembles messages; inside a message [`reliable`] commands, [`usercmd`]s and [`snapshot`]s are
//! bit-packed with the [`bits`] writer and the [`field`] delta tables. [`oob`] packets carry the
//! challenge-response connect. Wire compatibility with the original engine is not a goal.

pub mod bits;
pub mod client;
pub mod connect;
pub mod demo;
pub mod download;
pub mod entity;
pub mod field;
pub mod netchan;
pub mod oob;
pub mod predict;
pub mod ps;
pub mod reliable;
pub mod session;
pub mod snapshot;
pub mod transport;
pub mod ui;
pub mod usercmd;
pub mod view;
pub mod voice;
#[cfg(feature = "webtransport")]
pub mod wt;

pub use bits::{BitReader, BitWriter, Overflow};
pub use entity::EntityState;
pub use netchan::Netchan;
pub use oob::Oob;
pub use session::{ClientLink, ServerLink, packet_qport};
pub use snapshot::Snapshot;
pub use transport::{MemNet, Transport, UdpTransport};
