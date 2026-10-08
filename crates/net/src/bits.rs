// SPDX-License-Identifier: GPL-3.0-only
//! Bit-packed message buffers. Bits are written least significant first; a message is the byte
//! string of the bits written so far, padded with zero bits.

/// Error from reading past the end of a message or a value outside its encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overflow;

impl std::fmt::Display for Overflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("message ended early or holds an invalid value")
    }
}

impl std::error::Error for Overflow {}

#[derive(Debug, Clone, Default)]
pub struct BitWriter {
    bytes: Vec<u8>,
    /// Bits used in the last byte, 0 when byte aligned.
    used: u8,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(n: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(n),
            used: 0,
        }
    }

    pub fn bit_len(&self) -> usize {
        if self.used == 0 {
            self.bytes.len() * 8
        } else {
            self.bytes.len() * 8 - usize::from(8 - self.used)
        }
    }

    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn clear(&mut self) {
        self.bytes.clear();
        self.used = 0;
    }

    /// Writes the low `n` (at most 32) bits of `v`.
    pub fn write_bits(&mut self, v: u32, n: u32) {
        debug_assert!(n <= 32);
        let mut v = u64::from(v) & ((1u64 << n) - 1);
        let mut left = n;
        while left > 0 {
            if self.used == 0 {
                self.bytes.push(0);
            }
            let room = 8 - u32::from(self.used);
            let take = room.min(left);
            let last = self.bytes.len() - 1;
            self.bytes[last] |= ((v & ((1 << take) - 1)) as u8) << self.used;
            self.used = ((u32::from(self.used) + take) % 8) as u8;
            v >>= take;
            left -= take;
        }
    }

    pub fn write_bool(&mut self, b: bool) {
        self.write_bits(u32::from(b), 1);
    }

    pub fn write_u8(&mut self, v: u8) {
        self.write_bits(u32::from(v), 8);
    }

    pub fn write_u16(&mut self, v: u16) {
        self.write_bits(u32::from(v), 16);
    }

    pub fn write_u32(&mut self, v: u32) {
        self.write_bits(v, 32);
    }

    pub fn write_i32(&mut self, v: i32) {
        self.write_bits(v as u32, 32);
    }

    /// An unsigned value in groups of 4 bits plus a continuation bit: small values are cheap.
    pub fn write_uvar(&mut self, mut v: u32) {
        loop {
            self.write_bits(v & 15, 4);
            v >>= 4;
            self.write_bool(v != 0);
            if v == 0 {
                break;
            }
        }
    }

    /// A signed value as zigzag [`write_uvar`](Self::write_uvar).
    pub fn write_ivar(&mut self, v: i32) {
        self.write_uvar(((v << 1) ^ (v >> 31)) as u32);
    }

    /// Byte-aligned raw bytes (pads to the next byte first).
    pub fn write_bytes(&mut self, b: &[u8]) {
        self.used = 0;
        self.bytes.extend_from_slice(b);
    }

    /// A length-prefixed string (at most 65535 bytes; longer is cut).
    pub fn write_string(&mut self, s: &str) {
        let b = &s.as_bytes()[..s.len().min(usize::from(u16::MAX))];
        self.write_uvar(b.len() as u32);
        self.write_bytes(b);
    }
}

#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    bytes: &'a [u8],
    /// Next bit to read.
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    pub fn bits_left(&self) -> usize {
        self.bytes.len() * 8 - self.pos.min(self.bytes.len() * 8)
    }

    pub fn read_bits(&mut self, n: u32) -> Result<u32, Overflow> {
        debug_assert!(n <= 32);
        if self.bits_left() < n as usize {
            return Err(Overflow);
        }
        let mut v = 0u64;
        let mut got = 0;
        while got < n {
            let byte = u64::from(self.bytes[self.pos / 8]);
            let off = (self.pos % 8) as u32;
            let take = (8 - off).min(n - got);
            v |= ((byte >> off) & ((1 << take) - 1)) << got;
            got += take;
            self.pos += take as usize;
        }
        Ok(v as u32)
    }

    pub fn read_bool(&mut self) -> Result<bool, Overflow> {
        Ok(self.read_bits(1)? != 0)
    }

    pub fn read_u8(&mut self) -> Result<u8, Overflow> {
        Ok(self.read_bits(8)? as u8)
    }

    pub fn read_u16(&mut self) -> Result<u16, Overflow> {
        Ok(self.read_bits(16)? as u16)
    }

    pub fn read_u32(&mut self) -> Result<u32, Overflow> {
        self.read_bits(32)
    }

    pub fn read_i32(&mut self) -> Result<i32, Overflow> {
        Ok(self.read_bits(32)? as i32)
    }

    pub fn read_uvar(&mut self) -> Result<u32, Overflow> {
        let mut v = 0u32;
        let mut shift = 0;
        loop {
            if shift >= 32 {
                return Err(Overflow);
            }
            v |= self.read_bits(4)? << shift;
            shift += 4;
            if !self.read_bool()? {
                return Ok(v);
            }
        }
    }

    pub fn read_ivar(&mut self) -> Result<i32, Overflow> {
        let u = self.read_uvar()?;
        Ok(((u >> 1) as i32) ^ -((u & 1) as i32))
    }

    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], Overflow> {
        let start = self.pos.div_ceil(8);
        let end = start.checked_add(n).ok_or(Overflow)?;
        let b = self.bytes.get(start..end).ok_or(Overflow)?;
        self.pos = end * 8;
        Ok(b)
    }

    pub fn read_string(&mut self) -> Result<String, Overflow> {
        let n = self.read_uvar()? as usize;
        Ok(String::from_utf8_lossy(self.read_bytes(n)?).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_widths_round_trip() {
        let mut w = BitWriter::new();
        w.write_bits(5, 3);
        w.write_bool(true);
        w.write_u32(0xdead_beef);
        w.write_bits(0x1ff, 9);
        w.write_ivar(-1234);
        w.write_uvar(0);
        w.write_uvar(u32::MAX);
        w.write_string("héllo");
        w.write_bits(1, 1);
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(3), Ok(5));
        assert_eq!(r.read_bool(), Ok(true));
        assert_eq!(r.read_u32(), Ok(0xdead_beef));
        assert_eq!(r.read_bits(9), Ok(0x1ff));
        assert_eq!(r.read_ivar(), Ok(-1234));
        assert_eq!(r.read_uvar(), Ok(0));
        assert_eq!(r.read_uvar(), Ok(u32::MAX));
        assert_eq!(r.read_string().as_deref(), Ok("héllo"));
        assert_eq!(r.read_bits(1), Ok(1));
    }

    #[test]
    fn reading_past_the_end_is_an_error() {
        let mut r = BitReader::new(&[0xff]);
        assert_eq!(r.read_bits(9), Err(Overflow));
        assert_eq!(r.read_bits(8), Ok(0xff));
        assert_eq!(r.read_bits(1), Err(Overflow));
        assert!(BitReader::new(&[0xff, 0xff, 0xff]).read_uvar().is_err());
    }

    #[test]
    fn bit_len_counts_partial_bytes() {
        let mut w = BitWriter::new();
        assert_eq!(w.bit_len(), 0);
        w.write_bits(1, 3);
        assert_eq!(w.bit_len(), 3);
        w.write_bits(1, 5);
        assert_eq!(w.bit_len(), 8);
        w.write_bits(1, 1);
        assert_eq!(w.bit_len(), 9);
    }
}
