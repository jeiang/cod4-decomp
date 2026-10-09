// SPDX-License-Identifier: GPL-3.0-only
//! Demo files: what a client received, replayed later as if the server were sending it again. Wire compatibility
//! with the original's `.dm_1` is not a goal, so the file is native: a header, then records of what one server message
//! delivered, each stamped with the client clock (ms since the recording began).
//!
//! ```text
//! "C4EDEMO\0"  u32 PROTOCOL
//! record: u8 kind, u32 time_ms, u32 length, length bytes
//!   kind 0: a reliable command line (UTF-8), user interface commands included
//!   kind 1: a snapshot, delta-coded against the one recorded before it (or whole, every [`KEY_EVERY`])
//!   kind 2: the level restarted its clock: the playback's snapshot history is forgotten
//! ```
//!
//! A recording that starts in the middle of a match begins with the commands that rebuild the interface state
//! ([`crate::ui::ClientUiState::state_commands`]); playback needs the same game content as the recorder (the map is
//! named in those commands).

use crate::bits::{BitReader, BitWriter};
use crate::oob::PROTOCOL;
use crate::snapshot::{Snapshot, read_snapshot, write_snapshot};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

const MAGIC: &[u8; 8] = b"C4EDEMO\0";
/// A snapshot written whole this often, so a damaged tail loses only what follows the damage up to the next one.
const KEY_EVERY: u32 = 120;
/// Largest record a reader accepts (a configstring dump of a full table is a few hundred KiB at most).
const MAX_RECORD: u32 = 4 << 20;

/// What one record of a demo is.
#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    Command(String),
    Snapshot(Box<Snapshot>),
    NewMap,
}

/// Writes a demo as it is received.
pub struct DemoWriter {
    out: BufWriter<File>,
    prev: Option<Snapshot>,
    since_key: u32,
    scratch: BitWriter,
}

impl DemoWriter {
    pub fn create(path: &Path) -> io::Result<Self> {
        let mut out = BufWriter::new(File::create(path)?);
        out.write_all(MAGIC)?;
        out.write_all(&PROTOCOL.to_le_bytes())?;
        Ok(Self {
            out,
            prev: None,
            since_key: 0,
            scratch: BitWriter::new(),
        })
    }

    fn record(&mut self, kind: u8, time_ms: u64, body: &[u8]) -> io::Result<()> {
        self.out.write_all(&[kind])?;
        self.out.write_all(&(time_ms as u32).to_le_bytes())?;
        self.out.write_all(&(body.len() as u32).to_le_bytes())?;
        self.out.write_all(body)
    }

    pub fn command(&mut self, time_ms: u64, line: &str) -> io::Result<()> {
        self.record(0, time_ms, line.as_bytes())
    }

    pub fn new_map(&mut self, time_ms: u64) -> io::Result<()> {
        self.prev = None;
        self.record(2, time_ms, &[])
    }

    pub fn snapshot(&mut self, time_ms: u64, snap: &Snapshot) -> io::Result<()> {
        let base = self
            .prev
            .as_ref()
            .filter(|b| self.since_key < KEY_EVERY && snap.num.wrapping_sub(b.num) < 32);
        self.scratch.clear();
        write_snapshot(&mut self.scratch, base, snap);
        self.since_key = if base.is_some() {
            self.since_key + 1
        } else {
            0
        };
        let body = std::mem::take(&mut self.scratch);
        let r = self.record(1, time_ms, body.as_bytes());
        self.scratch = body;
        self.prev = Some(snap.clone());
        r
    }

    /// Flushes what is buffered. A demo is whole up to its last flush; dropping the writer flushes too.
    pub fn finish(mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Reads a demo record by record.
pub struct DemoReader {
    input: BufReader<File>,
    prev: Option<Snapshot>,
}

impl DemoReader {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut input = BufReader::new(File::open(path)?);
        let mut head = [0u8; 12];
        input.read_exact(&mut head)?;
        let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, why.to_owned());
        if &head[..8] != MAGIC {
            return Err(bad("not a demo file"));
        }
        if head[8..] != PROTOCOL.to_le_bytes() {
            return Err(bad("recorded by a different protocol version"));
        }
        Ok(Self { input, prev: None })
    }

    /// The next record and its time; `Ok(None)` at the end. A cut-off last record ends the demo like the end of the
    /// file (a recording that was killed mid-write still plays).
    pub fn next(&mut self) -> io::Result<Option<(u64, Record)>> {
        let mut head = [0u8; 9];
        if let Err(e) = self.input.read_exact(&mut head) {
            return if e.kind() == io::ErrorKind::UnexpectedEof {
                Ok(None)
            } else {
                Err(e)
            };
        }
        let time = u64::from(u32::from_le_bytes([head[1], head[2], head[3], head[4]]));
        let len = u32::from_le_bytes([head[5], head[6], head[7], head[8]]);
        let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, why.to_owned());
        if len > MAX_RECORD {
            return Err(bad("a record is too large"));
        }
        let mut body = vec![0u8; len as usize];
        if let Err(e) = self.input.read_exact(&mut body) {
            return if e.kind() == io::ErrorKind::UnexpectedEof {
                Ok(None)
            } else {
                Err(e)
            };
        }
        let record = match head[0] {
            0 => {
                Record::Command(String::from_utf8(body).map_err(|_| bad("a command is not text"))?)
            }
            1 => {
                let prev = self.prev.as_ref();
                let snap =
                    read_snapshot(&mut BitReader::new(&body), |n| prev.filter(|s| s.num == n))
                        .map_err(|_| bad("a snapshot does not decode"))?;
                self.prev = Some(snap.clone());
                Record::Snapshot(Box::new(snap))
            }
            2 => {
                self.prev = None;
                Record::NewMap
            }
            _ => return Err(bad("an unknown record")),
        };
        Ok(Some((time, record)))
    }
}

/// The level a demo was recorded on: the first `map` command it holds.
pub fn map_of(path: &Path) -> io::Result<String> {
    let mut r = DemoReader::open(path)?;
    while let Some((_, rec)) = r.next()? {
        if let Record::Command(line) = rec
            && let Some(crate::ui::ServerCmd::Map { name }) = crate::ui::ServerCmd::parse(&line)
        {
            return Ok(name);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "the demo names no level",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{EntityState, etype};

    fn snap(num: u32) -> Snapshot {
        let mut s = Snapshot::empty();
        s.num = num;
        s.server_time = num as i32 * 50;
        s.ps.origin = [num as f32, 2.0, 3.0];
        s.entities.push(EntityState {
            number: 5,
            etype: etype::PLAYER,
            origin: [num as f32 * 2.0, 1.0, 0.0],
            ..EntityState::default()
        });
        s.canonical()
    }

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "cod4e-demo-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn what_was_written_reads_back_in_order_through_deltas_a_gap_and_a_new_level() {
        let path = dir().join("a.demo");
        let nums = [1u32, 2, 3, 200, 201];
        let mut w = DemoWriter::create(&path).unwrap();
        w.command(0, "map \"mp_x\"").unwrap();
        for (i, n) in nums.iter().enumerate() {
            w.snapshot(i as u64 * 50, &snap(*n)).unwrap();
        }
        w.new_map(300).unwrap();
        w.snapshot(350, &snap(1)).unwrap();
        w.finish().unwrap();

        let mut r = DemoReader::open(&path).unwrap();
        assert_eq!(
            r.next().unwrap(),
            Some((0, Record::Command("map \"mp_x\"".into())))
        );
        for (i, n) in nums.iter().enumerate() {
            assert_eq!(
                r.next().unwrap(),
                Some((i as u64 * 50, Record::Snapshot(Box::new(snap(*n)))))
            );
        }
        assert_eq!(r.next().unwrap(), Some((300, Record::NewMap)));
        assert_eq!(
            r.next().unwrap(),
            Some((350, Record::Snapshot(Box::new(snap(1)))))
        );
        assert_eq!(r.next().unwrap(), None);
        assert_eq!(map_of(&path).unwrap(), "mp_x");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_cut_off_recording_plays_up_to_the_cut_and_a_foreign_file_is_refused() {
        let d = dir();
        let path = d.join("b.demo");
        let mut w = DemoWriter::create(&path).unwrap();
        w.snapshot(0, &snap(1)).unwrap();
        w.snapshot(50, &snap(2)).unwrap();
        w.finish().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 3]).unwrap();
        let mut r = DemoReader::open(&path).unwrap();
        assert!(matches!(r.next().unwrap(), Some((0, Record::Snapshot(_)))));
        assert_eq!(r.next().unwrap(), None);

        let junk = d.join("c.demo");
        std::fs::write(&junk, b"definitely not a demo").unwrap();
        assert!(DemoReader::open(&junk).is_err());
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(junk);
    }
}
