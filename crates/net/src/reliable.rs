// SPDX-License-Identifier: GPL-3.0-only
//! Reliable command strings carried inside the unreliable message stream: each packet repeats
//! every command the peer has not yet acknowledged, the peer takes them in order exactly once.

use crate::bits::{BitReader, BitWriter, Overflow};
use std::collections::VecDeque;

/// Unacknowledged commands a peer may fall behind by before it is dropped.
pub const WINDOW: usize = 512;
/// Longest command, bytes.
pub const MAX_COMMAND: usize = 1000;

#[derive(Debug, Default, Clone)]
pub struct ReliableOut {
    /// Sequence of the oldest unacknowledged command is `acked + 1`.
    acked: u32,
    pending: VecDeque<String>,
}

impl ReliableOut {
    /// Queues a command. `Err` means the peer is `WINDOW` commands behind.
    pub fn push(&mut self, cmd: impl Into<String>) -> Result<(), Overflow> {
        if self.pending.len() >= WINDOW {
            return Err(Overflow);
        }
        let mut c: String = cmd.into();
        if c.len() > MAX_COMMAND {
            let mut cut = MAX_COMMAND;
            while !c.is_char_boundary(cut) {
                cut -= 1;
            }
            c.truncate(cut);
        }
        self.pending.push_back(c);
        Ok(())
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// The peer says it has taken commands up to and including `seq`.
    pub fn ack(&mut self, seq: u32) {
        let n = seq.wrapping_sub(self.acked);
        if n == 0 || n as usize > self.pending.len() {
            return;
        }
        self.pending.drain(..n as usize);
        self.acked = seq;
    }

    /// Writes as many unacknowledged commands as fit in `budget` bytes, oldest first.
    pub fn write(&self, w: &mut BitWriter, budget: usize) {
        let mut used = 0;
        let mut n = 0;
        for c in &self.pending {
            used += c.len() + 3;
            if used > budget && n > 0 {
                break;
            }
            n += 1;
        }
        w.write_u32(self.acked.wrapping_add(1));
        w.write_uvar(n as u32);
        for c in self.pending.iter().take(n) {
            w.write_string(c);
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ReliableIn {
    received: u32,
}

impl ReliableIn {
    /// Highest command sequence taken; send it back as the acknowledgement.
    pub fn received(&self) -> u32 {
        self.received
    }

    /// Reads a block written by [`ReliableOut::write`], appending the new commands to `out`.
    pub fn read(&mut self, r: &mut BitReader, out: &mut Vec<String>) -> Result<(), Overflow> {
        let first = r.read_u32()?;
        let n = r.read_uvar()?;
        if n as usize > WINDOW {
            return Err(Overflow);
        }
        for i in 0..n {
            let cmd = r.read_string()?;
            if first.wrapping_add(i) == self.received.wrapping_add(1) {
                self.received = self.received.wrapping_add(1);
                out.push(cmd);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(o: &ReliableOut, budget: usize) -> Vec<u8> {
        let mut w = BitWriter::new();
        o.write(&mut w, budget);
        w.into_bytes()
    }

    #[test]
    fn commands_arrive_once_in_order_despite_repeats_and_loss() {
        let (mut o, mut i) = (ReliableOut::default(), ReliableIn::default());
        for c in ["a", "b", "c"] {
            o.push(c).unwrap();
        }
        let mut got = Vec::new();
        let b1 = block(&o, 1000);
        i.read(&mut BitReader::new(&b1), &mut got).unwrap();
        i.read(&mut BitReader::new(&b1), &mut got).unwrap();
        assert_eq!(got, ["a", "b", "c"]);
        o.ack(2);
        o.push("d").unwrap();
        // The packet carrying "d" repeats "c" (unacked) and is the only one that arrives.
        let b2 = block(&o, 1000);
        i.read(&mut BitReader::new(&b2), &mut got).unwrap();
        assert_eq!(got, ["a", "b", "c", "d"]);
        assert_eq!(i.received(), 4);
        o.ack(i.received());
        assert_eq!(o.pending(), 0);
    }

    #[test]
    fn a_block_that_skips_ahead_is_ignored() {
        let (mut o, mut i) = (ReliableOut::default(), ReliableIn::default());
        o.push("a").unwrap();
        o.push("b").unwrap();
        o.ack(0);
        let mut w = BitWriter::new();
        w.write_u32(2);
        w.write_uvar(1);
        w.write_string("b");
        let mut got = Vec::new();
        i.read(&mut BitReader::new(w.as_bytes()), &mut got).unwrap();
        assert!(got.is_empty(), "command 2 before command 1");
    }

    #[test]
    fn budget_limits_a_block_but_always_sends_one() {
        let mut o = ReliableOut::default();
        for _ in 0..10 {
            o.push("x".repeat(400)).unwrap();
        }
        let mut got = Vec::new();
        let mut i = ReliableIn::default();
        let b = block(&o, 900);
        i.read(&mut BitReader::new(&b), &mut got).unwrap();
        assert_eq!(got.len(), 2);
        let tiny = block(&o, 1);
        let mut j = ReliableIn::default();
        let mut one = Vec::new();
        j.read(&mut BitReader::new(&tiny), &mut one).unwrap();
        assert_eq!(one.len(), 1);
    }

    #[test]
    fn the_window_bounds_a_stalled_peer() {
        let mut o = ReliableOut::default();
        for _ in 0..WINDOW {
            o.push("x").unwrap();
        }
        assert!(o.push("x").is_err());
        o.ack(10);
        assert!(o.push("x").is_ok());
    }

    #[test]
    fn a_stale_or_future_ack_changes_nothing() {
        let mut o = ReliableOut::default();
        o.push("a").unwrap();
        o.ack(5);
        assert_eq!(o.pending(), 1);
        o.ack(1);
        o.ack(1);
        assert_eq!(o.pending(), 0);
    }
}
