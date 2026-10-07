// SPDX-License-Identifier: GPL-3.0-or-later
//! The sequenced channel between a client and a server (Q3 `netchan` style): each datagram
//! carries a sequence number; messages larger than a datagram are split into fragments and
//! reassembled; stale and duplicate datagrams are dropped. The message inside is opaque here.
//!
//! Layout of a datagram: `u32` sequence, top bit set for a fragment; a fragment adds `u16` start
//! and `u16` length. A fragment shorter than [`FRAGMENT_SIZE`] ends its message.

/// Payload bytes in one full fragment.
pub const FRAGMENT_SIZE: usize = 1200;
/// Largest message the channel carries.
pub const MAX_MESSAGE: usize = 64 * 1024;
/// Marks the sequence word of a fragment.
const FRAGMENT_BIT: u32 = 1 << 31;
/// First word of an out-of-band packet; never a valid sequence (it has the fragment bit set and
/// would need a fragment header: the receiver tells them apart before calling the channel).
pub const OOB_MARKER: u32 = 0xffff_ffff;
const HEADER: usize = 4;
const FRAGMENT_HEADER: usize = 8;

/// Bytes a regular datagram adds to its payload.
pub const OVERHEAD: usize = HEADER;

#[derive(Debug, Default, Clone)]
pub struct Netchan {
    out_seq: u32,
    in_seq: u32,
    /// Datagrams skipped before the last accepted one.
    pub dropped: u32,
    frag_seq: u32,
    frag_buf: Vec<u8>,
    /// Bytes handed to the transport, headers included.
    pub bytes_out: u64,
    pub packets_out: u64,
    pub bytes_in: u64,
    pub packets_in: u64,
}

impl Netchan {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn out_sequence(&self) -> u32 {
        self.out_seq
    }

    pub fn in_sequence(&self) -> u32 {
        self.in_seq
    }

    /// Sends `message` as one datagram, or as fragments when it does not fit one. `send` gets each
    /// datagram in order.
    pub fn transmit(&mut self, message: &[u8], mut send: impl FnMut(&[u8])) {
        debug_assert!(message.len() <= MAX_MESSAGE);
        self.out_seq = self.out_seq.wrapping_add(1) & !FRAGMENT_BIT;
        let seq = self.out_seq;
        let mut buf = Vec::with_capacity(FRAGMENT_HEADER + FRAGMENT_SIZE.min(message.len()));
        if message.len() < FRAGMENT_SIZE {
            buf.extend_from_slice(&seq.to_le_bytes());
            buf.extend_from_slice(message);
            self.account_out(buf.len());
            send(&buf);
            return;
        }
        let mut start = 0;
        loop {
            let len = FRAGMENT_SIZE.min(message.len() - start);
            buf.clear();
            buf.extend_from_slice(&(seq | FRAGMENT_BIT).to_le_bytes());
            buf.extend_from_slice(&(start as u16).to_le_bytes());
            buf.extend_from_slice(&(len as u16).to_le_bytes());
            buf.extend_from_slice(&message[start..start + len]);
            self.account_out(buf.len());
            send(&buf);
            start += len;
            // A message that ends exactly on a boundary needs a final empty fragment.
            if len < FRAGMENT_SIZE {
                break;
            }
            if start == message.len() {
                buf.clear();
                buf.extend_from_slice(&(seq | FRAGMENT_BIT).to_le_bytes());
                buf.extend_from_slice(&(start as u16).to_le_bytes());
                buf.extend_from_slice(&0u16.to_le_bytes());
                self.account_out(buf.len());
                send(&buf);
                break;
            }
        }
    }

    fn account_out(&mut self, n: usize) {
        self.bytes_out += n as u64;
        self.packets_out += 1;
    }

    /// Takes one received datagram. Returns true and fills `out` with the whole message when it
    /// completes one that is newer than anything accepted so far.
    pub fn process(&mut self, packet: &[u8], out: &mut Vec<u8>) -> bool {
        if packet.len() < HEADER {
            return false;
        }
        self.bytes_in += packet.len() as u64;
        self.packets_in += 1;
        let word = u32::from_le_bytes([packet[0], packet[1], packet[2], packet[3]]);
        let seq = word & !FRAGMENT_BIT;
        // Wrap-safe "is newer": the 31-bit forward distance is in the first half.
        let ahead = seq.wrapping_sub(self.in_seq) & !FRAGMENT_BIT;
        if ahead == 0 || ahead >= 1 << 30 {
            return false;
        }
        if word & FRAGMENT_BIT == 0 {
            self.dropped = ahead - 1;
            self.in_seq = seq;
            out.clear();
            out.extend_from_slice(&packet[HEADER..]);
            return true;
        }
        if packet.len() < HEADER + 4 {
            return false;
        }
        let start = usize::from(u16::from_le_bytes([packet[4], packet[5]]));
        let len = usize::from(u16::from_le_bytes([packet[6], packet[7]]));
        let body = &packet[FRAGMENT_HEADER..];
        if body.len() < len || len > FRAGMENT_SIZE {
            return false;
        }
        if seq != self.frag_seq || (start == 0 && !self.frag_buf.is_empty()) {
            self.frag_seq = seq;
            self.frag_buf.clear();
        }
        if start != self.frag_buf.len() || start + len > MAX_MESSAGE {
            return false;
        }
        self.frag_buf.extend_from_slice(&body[..len]);
        if len == FRAGMENT_SIZE {
            return false;
        }
        self.dropped = ahead - 1;
        self.in_seq = seq;
        out.clear();
        out.append(&mut self.frag_buf);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(c: &mut Netchan, m: &[u8]) -> Vec<Vec<u8>> {
        let mut v = Vec::new();
        c.transmit(m, |p| v.push(p.to_vec()));
        v
    }

    fn msg(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 + 3) as u8).collect()
    }

    #[test]
    fn small_message_is_one_datagram() {
        let (mut a, mut b) = (Netchan::new(), Netchan::new());
        let p = collect(&mut a, b"hello");
        assert_eq!(p.len(), 1);
        let mut out = Vec::new();
        assert!(b.process(&p[0], &mut out));
        assert_eq!(out, b"hello");
        assert!(!b.process(&p[0], &mut out), "a duplicate is dropped");
    }

    #[test]
    fn fragments_reassemble_including_exact_multiples() {
        for n in [
            FRAGMENT_SIZE - 1,
            FRAGMENT_SIZE,
            FRAGMENT_SIZE + 1,
            FRAGMENT_SIZE * 3,
            FRAGMENT_SIZE * 5 + 17,
        ] {
            let (mut a, mut b) = (Netchan::new(), Netchan::new());
            let m = msg(n);
            let p = collect(&mut a, &m);
            assert_eq!(p.len(), n / FRAGMENT_SIZE + 1, "size {n}");
            let mut out = Vec::new();
            let done: Vec<bool> = p.iter().map(|d| b.process(d, &mut out)).collect();
            assert_eq!(done.iter().filter(|d| **d).count(), 1, "size {n}");
            assert!(*done.last().unwrap());
            assert_eq!(out, m, "size {n}");
        }
    }

    #[test]
    fn a_lost_fragment_loses_the_message_not_the_next_one() {
        let (mut a, mut b) = (Netchan::new(), Netchan::new());
        let first = collect(&mut a, &msg(FRAGMENT_SIZE * 3));
        let second = collect(&mut a, &msg(FRAGMENT_SIZE * 2 + 5));
        let mut out = Vec::new();
        assert!(!b.process(&first[0], &mut out));
        // first[1] lost
        assert!(!b.process(&first[2], &mut out));
        assert!(!b.process(&first[3], &mut out));
        let done: Vec<bool> = second.iter().map(|d| b.process(d, &mut out)).collect();
        assert_eq!(done.last(), Some(&true));
        assert_eq!(out, msg(FRAGMENT_SIZE * 2 + 5));
        assert_eq!(b.dropped, 1);
    }

    #[test]
    fn out_of_order_and_old_datagrams_are_dropped() {
        let (mut a, mut b) = (Netchan::new(), Netchan::new());
        let p1 = collect(&mut a, b"one");
        let p2 = collect(&mut a, b"two");
        let mut out = Vec::new();
        assert!(b.process(&p2[0], &mut out));
        assert!(!b.process(&p1[0], &mut out));
        assert_eq!(b.in_sequence(), 2);
    }

    #[test]
    fn sequence_wrap_still_orders() {
        let mut a = Netchan {
            out_seq: !FRAGMENT_BIT - 1,
            ..Netchan::new()
        };
        let mut b = Netchan {
            in_seq: !FRAGMENT_BIT - 1,
            ..Netchan::new()
        };
        let mut out = Vec::new();
        for _ in 0..4 {
            let p = collect(&mut a, b"x");
            assert!(b.process(&p[0], &mut out));
        }
    }

    #[test]
    fn malformed_datagrams_are_ignored() {
        let mut b = Netchan::new();
        let mut out = Vec::new();
        assert!(!b.process(&[1, 2], &mut out));
        let mut bad = (1u32 | FRAGMENT_BIT).to_le_bytes().to_vec();
        bad.extend_from_slice(&[0, 0, 0xff, 0xff, 1]);
        assert!(!b.process(&bad, &mut out));
    }
}
