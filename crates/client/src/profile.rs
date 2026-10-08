// SPDX-License-Identifier: GPL-3.0-or-later
//! The player profile: the persistent stats (rank, unlocks, class setups; `stat(n)` in the menus and `setstat` in the
//! scripts) kept between runs.
//!
//! The original keeps them in `players/profiles/<profile>/mpdata`, an encrypted, checksummed binary block (2000 byte
//! stats and 1498 int stats). This client does not reproduce that format: it writes its own plain text file,
//! `<config dir>/players/profiles/<profile>/stats.txt`, next to the config file (so `--config` moves it too). The first
//! line is the header [`HEADER`], then one `<index> <value>` line per stat that is not zero. The profile is
//! `com_playerProfile` (default `default`).
//!
//! The client owns the stats like the original's: it uploads them to the server after joining ([`upload_commands`]),
//! and the server's later changes ([`StatSync`]) flow back into the profile.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

/// Number of persistent stats.
pub const STAT_COUNT: usize = 4000;
/// First line of a stats file; the number is the format version.
const HEADER: &str = "cod4e-stats 1";
/// Stat pairs in one upload command (the server accepts at most 64).
const PAIRS_PER_COMMAND: usize = 24;

/// A new profile's stats: the five class slots start as the stock scripts expect (a profile with these all zero is
/// treated as tampered with and kicked).
pub fn default_stats() -> Vec<i32> {
    let mut s = vec![0; STAT_COUNT];
    for i in 0..5 {
        s[205 + i * 10] = 1;
    }
    s
}

pub fn encode(stats: &[i32]) -> String {
    let mut out = format!("{HEADER}\n");
    for (i, v) in stats.iter().enumerate().filter(|(_, v)| **v != 0) {
        out.push_str(&format!("{i} {v}\n"));
    }
    out
}

/// `None` when the text is not a stats file of this version. Lines that do not parse or name a stat out of range are
/// skipped; a later line for the same stat wins.
pub fn decode(text: &str) -> Option<Vec<i32>> {
    let mut lines = text.lines();
    if lines.next()?.trim() != HEADER {
        return None;
    }
    let mut stats = vec![0; STAT_COUNT];
    for l in lines {
        let mut it = l.split_whitespace();
        let (Some(i), Some(v)) = (it.next(), it.next()) else {
            continue;
        };
        if let (Ok(i), Ok(v)) = (i.parse::<usize>(), v.parse::<i32>())
            && i < STAT_COUNT
        {
            stats[i] = v;
        }
    }
    Some(stats)
}

/// `statsync` commands that carry every nonzero stat to the server.
pub fn upload_commands(stats: &[i32]) -> Vec<String> {
    let pairs: Vec<(usize, i32)> = stats
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, v)| *v != 0)
        .collect();
    pairs
        .chunks(PAIRS_PER_COMMAND)
        .map(|c| {
            let mut s = String::from("statsync");
            for (i, v) in c {
                s.push_str(&format!(" {i} {v}"));
            }
            s
        })
        .collect()
}

/// Folder-safe profile name: letters, digits, space, `-`, `_` and `.` stay; the rest becomes `_`.
fn clean_name(name: &str) -> String {
    let n: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if n.is_empty() || n.chars().all(|c| c == '.') {
        "default".into()
    } else {
        n
    }
}

/// The stats file of one profile and the stats last written to it.
pub struct Profile {
    path: Option<PathBuf>,
    saved: Vec<i32>,
}

impl Profile {
    /// `config_dir` is where the config file lives (`None`: nothing is persisted).
    pub fn new(config_dir: Option<PathBuf>, name: &str) -> Self {
        Profile {
            path: config_dir.map(|d| {
                d.join("players")
                    .join("profiles")
                    .join(clean_name(name))
                    .join("stats.txt")
            }),
            saved: Vec::new(),
        }
    }

    /// The stored stats, or a new profile's defaults.
    pub fn load(&mut self) -> Vec<i32> {
        let stats = self
            .path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| decode(&t))
            .unwrap_or_else(default_stats);
        self.saved = stats.clone();
        stats
    }

    /// Writes `stats` if they differ from what was last loaded or written (write to a temp file, then rename, so a crash
    /// never leaves half a file). Returns whether a file was written.
    pub fn save_if_changed(&mut self, stats: &[i32]) -> io::Result<bool> {
        let Some(path) = self.path.as_ref().filter(|_| self.saved != stats) else {
            return Ok(false);
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, encode(stats))?;
        std::fs::rename(&tmp, path)?;
        self.saved = stats.to_vec();
        Ok(true)
    }
}

/// Which of the server's stat values are news. The server announces its stats once on joining (stand-in defaults the
/// profile replaces); only changes after that are the server's word.
#[derive(Default)]
pub struct StatSync {
    seen: HashMap<i32, i32>,
}

impl StatSync {
    /// Takes the server's current stats as already known (a new level: nothing to import from the burst).
    pub fn baseline(&mut self, server: &HashMap<i32, i32>) {
        self.seen.clone_from(server);
    }

    /// The `(index, value)` pairs the server changed since the last call.
    pub fn changes(&mut self, server: &HashMap<i32, i32>) -> Vec<(i32, i32)> {
        let mut out = Vec::new();
        for (&i, &v) in server {
            if self.seen.insert(i, v) != Some(v) {
                out.push((i, v));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_round_trip_through_the_file_text() {
        let mut s = vec![0; STAT_COUNT];
        s[0] = 7;
        s[205] = -3;
        s[3999] = i32::MAX;
        let back = decode(&encode(&s)).unwrap();
        assert_eq!(back, s);
        assert_eq!(encode(&vec![0; STAT_COUNT]), format!("{HEADER}\n"));
    }

    #[test]
    fn decode_rejects_foreign_files_and_skips_bad_lines() {
        assert_eq!(decode("garbage\n1 2\n"), None);
        assert_eq!(decode(""), None);
        let s = decode(&format!("{HEADER}\n5 9\nnope\n4000 1\n-1 4\n6 x\n5 10\n")).unwrap();
        assert_eq!((s[5], s[6]), (10, 0));
        assert_eq!(s.iter().filter(|v| **v != 0).count(), 1);
    }

    #[test]
    fn profile_survives_a_restart_and_only_writes_changes() {
        let dir = std::env::temp_dir().join(format!("cod4e-profile-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut p = Profile::new(Some(dir.clone()), "Some/One");
        let mut s = p.load();
        assert_eq!(s, default_stats());
        assert!(!p.save_if_changed(&s).unwrap());
        s[210] = 42;
        assert!(p.save_if_changed(&s).unwrap());
        assert!(!p.save_if_changed(&s).unwrap());
        let mut again = Profile::new(Some(dir.clone()), "Some/One");
        assert_eq!(again.load(), s);
        // A different profile name is a different file.
        assert_eq!(
            Profile::new(Some(dir.clone()), "Other").load(),
            default_stats()
        );
        assert!(
            dir.join("players/profiles/Some_One/stats.txt").is_file(),
            "the name is made folder-safe"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_profile_without_a_config_dir_persists_nothing() {
        let mut p = Profile::new(None, "x");
        let mut s = p.load();
        s[1] = 1;
        assert!(!p.save_if_changed(&s).unwrap());
    }

    #[test]
    fn upload_covers_every_nonzero_stat_in_bounded_commands() {
        let mut s = vec![0; STAT_COUNT];
        for i in 0..60 {
            s[100 + i] = i as i32 + 1;
        }
        let cmds = upload_commands(&s);
        assert_eq!(cmds.len(), 3);
        let pairs: usize = cmds
            .iter()
            .map(|c| {
                let n = c.split_whitespace().count() - 1;
                assert!(n % 2 == 0 && n / 2 <= 24);
                n / 2
            })
            .sum();
        assert_eq!(pairs, 60);
        assert!(upload_commands(&vec![0; STAT_COUNT]).is_empty());
    }

    #[test]
    fn only_changes_after_the_baseline_are_imported() {
        let mut sync = StatSync::default();
        let mut server: HashMap<i32, i32> = [(205, 1), (215, 1)].into();
        sync.baseline(&server);
        assert!(sync.changes(&server).is_empty());
        server.insert(215, 5);
        server.insert(2301, 100);
        let mut ch = sync.changes(&server);
        ch.sort();
        assert_eq!(ch, [(215, 5), (2301, 100)]);
        assert!(sync.changes(&server).is_empty());
    }
}
