// SPDX-License-Identifier: GPL-3.0-only
//! CRC-32 (the zip one): what an IWD's central directory records for each entry, and what the download manifest
//! records for a whole file.

use std::io::Read;

const fn table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static TABLE: [u32; 256] = table();

/// A running CRC-32.
#[derive(Clone, Copy)]
pub struct Crc32(u32);

impl Default for Crc32 {
    fn default() -> Self {
        Self(!0)
    }
}

impl Crc32 {
    pub fn update(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = TABLE[((self.0 ^ u32::from(*b)) & 0xff) as usize] ^ (self.0 >> 8);
        }
    }

    pub fn finish(self) -> u32 {
        !self.0
    }
}

/// CRC-32 of `bytes`.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut c = Crc32::default();
    c.update(bytes);
    c.finish()
}

/// Size and CRC-32 of the file at `path`, read in pieces.
pub fn file_crc32(path: &std::path::Path) -> std::io::Result<(u64, u32)> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 16];
    let (mut crc, mut size) = (Crc32::default(), 0u64);
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok((size, crc.finish()));
        }
        crc.update(&buf[..n]);
        size += n as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_is_the_standard_one() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let mut c = Crc32::default();
        c.update(b"1234");
        c.update(b"56789");
        assert_eq!(c.finish(), 0xCBF4_3926);
    }
}
