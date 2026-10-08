// SPDX-License-Identifier: GPL-3.0-only
//! IWD (zip) reader: central directory index plus ranged entry reads.

use super::ReadAt;
use super::normalize;
use std::collections::HashMap;
use std::io;

const EOCD_SIG: u32 = 0x0605_4b50;
const CD_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;
const EOCD_LEN: usize = 22;
const MAX_COMMENT: usize = 0xffff;

fn bad(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn u16_at(b: &[u8], o: usize) -> usize {
    u16::from_le_bytes([b[o], b[o + 1]]) as usize
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// One central-directory record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Name as stored in the archive.
    pub name: String,
    pub method: u16,
    pub crc32: u32,
    pub compressed_size: u32,
    pub size: u32,
    pub header_offset: u32,
}

/// An opened IWD: the central directory is indexed, entry data is read on demand.
pub struct Iwd {
    source: Box<dyn ReadAt>,
    entries: Vec<Entry>,
    /// normalized name -> index into `entries`; the last duplicate wins.
    index: HashMap<String, usize>,
}

impl Iwd {
    pub fn new(source: Box<dyn ReadAt>) -> io::Result<Self> {
        let len = source.len();
        let tail_len = len.min((EOCD_LEN + MAX_COMMENT) as u64) as usize;
        let tail_start = len - tail_len as u64;
        let mut tail = vec![0; tail_len];
        source.read_exact_at(tail_start, &mut tail)?;
        let eocd = tail
            .len()
            .checked_sub(EOCD_LEN)
            .and_then(|last| {
                (0..=last).rev().find(|&i| {
                    u32_at(&tail, i) == EOCD_SIG
                        && i + EOCD_LEN + u16_at(&tail, i + 20) == tail.len()
                })
            })
            .ok_or_else(|| bad("zip end-of-central-directory not found"))?;
        let count = u16_at(&tail, eocd + 10);
        let cd_size = u32_at(&tail, eocd + 12);
        let cd_offset = u32_at(&tail, eocd + 16);
        if count == 0xffff || cd_size == u32::MAX || cd_offset == u32::MAX {
            return Err(bad("zip64 is not supported"));
        }
        if u64::from(cd_offset) + u64::from(cd_size) > len {
            return Err(bad("central directory out of range"));
        }
        let mut cd = vec![0; cd_size as usize];
        source.read_exact_at(u64::from(cd_offset), &mut cd)?;

        let mut entries = Vec::with_capacity(count);
        let mut index = HashMap::with_capacity(count);
        let mut pos = 0;
        for _ in 0..count {
            let h = cd
                .get(pos..pos + 46)
                .ok_or_else(|| bad("truncated central directory"))?;
            if u32_at(h, 0) != CD_SIG {
                return Err(bad("bad central directory signature"));
            }
            let (name_len, extra_len, comment_len) = (u16_at(h, 28), u16_at(h, 30), u16_at(h, 32));
            let name = cd
                .get(pos + 46..pos + 46 + name_len)
                .ok_or_else(|| bad("truncated central directory"))?;
            let entry = Entry {
                name: String::from_utf8_lossy(name).into_owned(),
                method: u16_at(h, 10) as u16,
                crc32: u32_at(h, 16),
                compressed_size: u32_at(h, 20),
                size: u32_at(h, 24),
                header_offset: u32_at(h, 42),
            };
            // Prepending to a hash bucket in the original means the last record wins.
            index.insert(normalize(&entry.name), entries.len());
            entries.push(entry);
            pos += 46 + name_len + extra_len + comment_len;
        }
        Ok(Self {
            source,
            entries,
            index,
        })
    }

    pub fn open(path: &std::path::Path) -> io::Result<Self> {
        Self::new(Box::new(super::FileSource::open(path)?))
    }

    /// Central-directory records in archive order, duplicates included.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Case-insensitive lookup; `\` and `/` are equivalent.
    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.index.get(&normalize(name)).map(|&i| &self.entries[i])
    }

    /// Read and decompress one entry with ranged reads (no whole-archive read).
    pub fn read(&self, entry: &Entry) -> io::Result<Vec<u8>> {
        if entry.method != 0 && entry.method != 8 {
            return Err(bad("unsupported zip compression method"));
        }
        let mut local = [0; 30];
        self.source
            .read_exact_at(u64::from(entry.header_offset), &mut local)?;
        if u32_at(&local, 0) != LOCAL_SIG {
            return Err(bad("bad local header signature"));
        }
        let data_offset = u64::from(entry.header_offset)
            + 30
            + u16_at(&local, 26) as u64
            + u16_at(&local, 28) as u64;
        let mut raw = vec![0; entry.compressed_size as usize];
        self.source.read_exact_at(data_offset, &mut raw)?;
        let out = if entry.method == 0 {
            raw
        } else {
            miniz_oxide::inflate::decompress_to_vec_with_limit(&raw, entry.size as usize)
                .map_err(|_| bad("corrupt deflate stream"))?
        };
        if out.len() != entry.size as usize {
            return Err(bad("entry size mismatch"));
        }
        Ok(out)
    }
}
