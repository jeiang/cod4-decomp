// SPDX-License-Identifier: GPL-3.0-or-later
//! The player profiles: the persistent stats (rank, unlocks, class setups; `stat(n)` in the menus and `setstat` in the
//! scripts) kept between runs, one set per profile.
//!
//! The original keeps a profile in `players/profiles/<profile>/mpdata` (the stats), next to `config_mp.cfg`, and the
//! last used profile's name in `players/profiles/active.txt`. `mpdata` is a 0x211C byte container: a 4 byte magic, a
//! 4 byte nonce, a 16 byte hash, a 260 byte game directory, then a CRC-32 of the stats (4 bytes) and the stats (2000
//! byte stats, then 1498 little-endian 32 bit stats, then padding). The original encrypts everything after the nonce
//! (magic `iwm0`, a key made from the CD key); a CoD 4 X install stores it decrypted (magic `ice0`), which is what this
//! client reads and writes. An encrypted file needs the CD key, which this client does not have, so such a profile is
//! listed but starts with fresh stats.
//!
//! Profiles are read from the install's `players/profiles` and from the user's own folder next to the config file
//! (`--config` moves it). Everything this client saves goes to the user's folder, in the `ice0` format; the install is
//! never written. A profile of the same name in the user's folder shadows the install's. The profile is `com_playerProfile`.
//!
//! The client owns the stats like the original's: it uploads them to the server after joining ([`upload_commands`]),
//! and the server's later changes ([`StatSync`]) flow back into the profile.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

/// Number of persistent stats the client keeps (the file holds 3498).
pub const STAT_COUNT: usize = 4000;
/// Stats below this index are one byte in the file, the rest 32 bit.
const BYTE_STATS: usize = 2000;
/// Stats the file holds.
const FILE_STATS: usize = 3498;
/// Stat pairs in one upload command (the server accepts at most 64).
const PAIRS_PER_COMMAND: usize = 24;
/// Size of an `mpdata` file.
const FILE_LEN: usize = 0x211C;
/// Where the stats checksum and the stats start in the file; the checksum covers [`CHECKED_LEN`] bytes of stats.
const CRC_AT: usize = 0x11C;
const STATS_AT: usize = CRC_AT + 4;
const CHECKED_LEN: usize = 0x1FFC;
/// The stats version the stock scripts check (`stat_version`); stat 299 holds it.
const STAT_VERSION: i32 = 10;
/// The most profiles the menu lists.
pub const MAX_PROFILES: usize = 64;
/// The longest profile name.
const NAME_LEN: usize = 31;

/// Why an `mpdata` file gave no stats.
#[derive(Debug, PartialEq, Eq)]
pub enum MpdataError {
    /// Wrong size or magic, or the checksum does not match.
    Corrupt,
    /// The original's encrypted container (needs the CD key).
    Encrypted,
}

/// A new profile's stats, as the original's first run sets them (`LiveStorage_StatsInit`): the stats version and the
/// five default classes in their five slots with the equipment they unlock. The stock scripts treat a profile whose
/// class slots are empty as tampered with.
pub fn default_stats() -> Vec<i32> {
    /// The first stat of a slot, the slot's items, and the stats the class unlocks.
    type Class = (usize, [i32; 8], &'static [(usize, i32)]);
    const CLASSES: [Class; 5] = [
        (
            201,
            [25, 5, 0, 0, 191, 160, 154, 103],
            &[
                (3025, 0x321),
                (3020, 0x321),
                (190, 1),
                (160, 1),
                (154, 1),
                (103, 1),
            ],
        ),
        (
            211,
            [10, 0, 2, 3, 184, 156, 162, 101],
            &[
                (3010, 0x301),
                (3011, 0x301),
                (184, 1),
                (156, 1),
                (162, 1),
                (101, 1),
            ],
        ),
        (
            221,
            [81, 0, 2, 0, 176, 167, 161, 103],
            &[
                (3081, 0x301),
                (3080, 0x301),
                (176, 1),
                (167, 1),
                (161, 1),
                (103, 1),
            ],
        ),
        (
            231,
            [71, 0, 0, 0, 186, 156, 154, 102],
            &[(3071, 0x301), (186, 1), (156, 1), (154, 1), (102, 1)],
        ),
        (
            241,
            [61, 0, 0, 3, 176, 160, 161, 101],
            &[(3061, 0x301), (176, 1), (160, 1), (161, 1), (101, 1)],
        ),
    ];
    let mut s = vec![0; STAT_COUNT];
    for (first, items, extra) in CLASSES {
        for (k, v) in items.iter().enumerate() {
            s[first + k] = *v;
        }
        for (i, v) in extra {
            s[*i] = *v;
        }
    }
    s[3000] = 9;
    s[3002] = 9;
    s[299] = STAT_VERSION;
    s
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// The `ice0` file for `stats` (stats the file has no room for are dropped, bytes keep their low 8 bits).
pub fn encode(stats: &[i32]) -> Vec<u8> {
    let mut out = vec![0u8; FILE_LEN];
    out[..4].copy_from_slice(b"ice0");
    for (i, &v) in stats.iter().enumerate().take(FILE_STATS) {
        if i < BYTE_STATS {
            out[STATS_AT + i] = v as u8;
        } else {
            let at = STATS_AT + BYTE_STATS + 4 * (i - BYTE_STATS);
            out[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
    }
    let crc = crc32(&out[STATS_AT..STATS_AT + CHECKED_LEN]);
    out[CRC_AT..CRC_AT + 4].copy_from_slice(&crc.to_le_bytes());
    out
}

/// Whether the stats checksum of an `mpdata` file matches its stats.
pub fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.len() == FILE_LEN
        && u32::from_le_bytes(bytes[CRC_AT..CRC_AT + 4].try_into().unwrap())
            == crc32(&bytes[STATS_AT..STATS_AT + CHECKED_LEN])
}

/// The stats of an `mpdata` file. Only the size and the magic must be right: the original's own check (the `iwm0`
/// magic, the key, the hashes) is stricter, and it throws away a file written by CoD 4 X (`ice0`) as corrupt; this
/// client reads such a file, and a file whose checksum is off, as it stands.
pub fn decode(bytes: &[u8]) -> Result<Vec<i32>, MpdataError> {
    if bytes.len() != FILE_LEN {
        return Err(MpdataError::Corrupt);
    }
    match &bytes[..4] {
        b"iwm0" => return Err(MpdataError::Encrypted),
        b"ice0" => {}
        _ => return Err(MpdataError::Corrupt),
    }
    // A wrong checksum is not fatal: the stats are used as they are (see `checksum_ok`).
    let mut stats = vec![0; STAT_COUNT];
    for (i, s) in stats.iter_mut().enumerate().take(FILE_STATS) {
        *s = if i < BYTE_STATS {
            i32::from(bytes[STATS_AT + i])
        } else {
            let at = STATS_AT + BYTE_STATS + 4 * (i - BYTE_STATS);
            i32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
        };
    }
    Ok(stats)
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

/// Folder-safe profile name, or `None` when nothing usable is left: letters, digits, space, `-`, `_` and `.` stay,
/// the rest becomes `_`; at most 31 characters.
pub fn clean_name(name: &str) -> Option<String> {
    let n: String = name
        .trim()
        .chars()
        .take(NAME_LEN)
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let n = n.trim().to_owned();
    (!n.is_empty() && !n.chars().all(|c| c == '.')).then_some(n)
}

/// What creating a profile can come to.
#[derive(Debug, PartialEq, Eq)]
pub enum CreateError {
    TooMany,
    Exists,
    Failed,
}

/// The profiles on disk, the active one and the stats last written for it.
pub struct Profiles {
    install_root: Option<PathBuf>,
    user_root: Option<PathBuf>,
    /// The active profile's name (empty: none chosen yet).
    active: String,
    saved: Vec<i32>,
    /// A listing sorted by name, ascending unless the menu flipped it.
    ascending: bool,
}

impl Profiles {
    /// No profiles anywhere and nothing persisted (the menus still run).
    pub fn none() -> Self {
        Profiles {
            install_root: None,
            user_root: None,
            active: String::new(),
            saved: Vec::new(),
            ascending: true,
        }
    }

    /// `install` is the game folder (its `players/profiles` is read), `config_dir` where the user's profiles live
    /// (`None`: nothing is saved). The active profile is the one `active.txt` names (the user's, else the install's),
    /// else `fallback` if that exists. A first run with no profile at all starts one named `fallback` (the original
    /// asks for a name; there is no way to ask before the menus exist).
    /// Returns the profiles and the active profile's stats.
    pub fn open(install: &Path, config_dir: Option<PathBuf>, fallback: &str) -> (Self, Vec<i32>) {
        let mut p = Profiles {
            install_root: Some(install.join("players").join("profiles")),
            user_root: config_dir.map(|d| d.join("players").join("profiles")),
            active: String::new(),
            saved: Vec::new(),
            ascending: true,
        };
        let listed = p.list();
        let named = [&p.user_root, &p.install_root]
            .into_iter()
            .flatten()
            .find_map(|r| std::fs::read_to_string(r.join("active.txt")).ok())
            .and_then(|t| clean_name(t.lines().next().unwrap_or("")));
        let fallback = clean_name(fallback);
        let find = |n: &Option<String>| {
            n.as_ref()
                .and_then(|n| listed.iter().find(|l| l.eq_ignore_ascii_case(n)).cloned())
        };
        p.active = match find(&named).or_else(|| find(&fallback)) {
            Some(n) => n,
            None if listed.is_empty() => fallback.unwrap_or_else(|| "default".into()),
            None => String::new(),
        };
        let stats = p.load_active();
        (p, stats)
    }

    pub fn active(&self) -> &str {
        &self.active
    }

    fn dir_of(root: &Option<PathBuf>, name: &str) -> Option<PathBuf> {
        root.as_ref().map(|r| r.join(name))
    }

    /// The folder of a profile in one root (names match without regard to case).
    fn find_in(root: &Option<PathBuf>, name: &str) -> Option<PathBuf> {
        let want = clean_name(name)?.to_lowercase();
        std::fs::read_dir(root.as_ref()?)
            .ok()?
            .flatten()
            .find_map(|e| {
                let n = e.file_name().to_string_lossy().to_lowercase();
                (n == want && e.path().is_dir()).then(|| e.path())
            })
    }

    /// Where the named profile lives: the user's folder wins, then the install's.
    fn find_dir(&self, name: &str) -> Option<PathBuf> {
        Self::find_in(&self.user_root, name).or_else(|| Self::find_in(&self.install_root, name))
    }

    /// The active profile's `config_mp.cfg`: the file to read (the user's copy, else the install's) and the file
    /// to write (always the user's folder). `None` where there is no such place.
    pub fn config_paths(&self) -> (Option<PathBuf>, Option<PathBuf>) {
        if self.active.is_empty() {
            return (None, None);
        }
        let write = Self::dir_of(&self.user_root, &self.active).map(|d| d.join("config_mp.cfg"));
        let read = write.clone().filter(|p| p.is_file()).or_else(|| {
            let d = Self::find_in(&self.install_root, &self.active)?;
            Some(d.join("config_mp.cfg")).filter(|p| p.is_file())
        });
        (read, write)
    }

    /// The profile names found, sorted by name (ascending unless flipped), the active one included.
    pub fn list(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for root in [&self.user_root, &self.install_root].into_iter().flatten() {
            let Ok(rd) = std::fs::read_dir(root) else {
                continue;
            };
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if e.path().is_dir() && !names.iter().any(|x| x.eq_ignore_ascii_case(&n)) {
                    names.push(n);
                }
            }
        }
        if !self.active.is_empty() && !names.iter().any(|x| x.eq_ignore_ascii_case(&self.active)) {
            names.push(self.active.clone());
        }
        names.sort_by_key(|n| n.to_lowercase());
        if !self.ascending {
            names.reverse();
        }
        names.truncate(MAX_PROFILES);
        names
    }

    /// Picks the listing order (the column header of the profile menu flips it).
    pub fn set_ascending(&mut self, ascending: bool) {
        self.ascending = ascending;
    }

    pub fn ascending(&self) -> bool {
        self.ascending
    }

    fn read_stats(&self, name: &str) -> Vec<i32> {
        self.find_dir(name)
            .and_then(|d| std::fs::read(d.join("mpdata")).ok())
            .and_then(|b| match decode(&b) {
                Ok(s) => {
                    if !checksum_ok(&b) {
                        eprintln!("profile {name}: the stats checksum does not match; using the stats anyway");
                    }
                    Some(s)
                }
                Err(e) => {
                    eprintln!("profile {name}: cannot read its stats ({e:?}); starting fresh");
                    None
                }
            })
            .unwrap_or_else(default_stats)
    }

    fn load_active(&mut self) -> Vec<i32> {
        let stats = if self.active.is_empty() {
            default_stats()
        } else {
            self.read_stats(&self.active)
        };
        self.saved.clone_from(&stats);
        stats
    }

    /// Writes `stats` to the active profile if they differ from what was last loaded or written (to a temp file, then
    /// renamed, so a crash never leaves half a file). Returns whether a file was written.
    pub fn save_if_changed(&mut self, stats: &[i32]) -> io::Result<bool> {
        if self.active.is_empty() || self.saved == stats {
            return Ok(false);
        }
        let Some(dir) = Self::dir_of(&self.user_root, &self.active) else {
            return Ok(false);
        };
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("mpdata");
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, encode(stats))?;
        std::fs::rename(&tmp, path)?;
        self.saved = stats.to_vec();
        Ok(true)
    }

    /// Makes `name` the active profile: the current stats are saved first, the new profile's stats are returned, and
    /// `active.txt` records the choice. `None` if the name is not a profile.
    pub fn switch(&mut self, current: &[i32], name: &str) -> Option<Vec<i32>> {
        let name = self
            .list()
            .into_iter()
            .find(|n| n.eq_ignore_ascii_case(name))?;
        if let Err(e) = self.save_if_changed(current) {
            eprintln!("cannot save the profile: {e}");
        }
        self.active = name;
        if let Some(root) = &self.user_root
            && std::fs::create_dir_all(root).is_ok()
            && let Err(e) = std::fs::write(root.join("active.txt"), &self.active)
        {
            eprintln!("cannot record the active profile: {e}");
        }
        Some(self.load_active())
    }

    /// Starts a profile with a new profile's stats.
    pub fn create(&mut self, name: &str) -> Result<String, CreateError> {
        let name = clean_name(name).ok_or(CreateError::Failed)?;
        let listed = self.list();
        if listed.len() >= MAX_PROFILES {
            return Err(CreateError::TooMany);
        }
        if listed.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
            return Err(CreateError::Exists);
        }
        let dir = Self::dir_of(&self.user_root, &name).ok_or(CreateError::Failed)?;
        std::fs::create_dir_all(&dir).map_err(|_| CreateError::Failed)?;
        std::fs::write(dir.join("mpdata"), encode(&default_stats()))
            .map_err(|_| CreateError::Failed)?;
        Ok(name)
    }

    /// Deletes a profile of the user's folder; the install's profiles are never touched (`false`). Deleting the
    /// active profile leaves none chosen.
    pub fn delete(&mut self, name: &str) -> bool {
        let Some(user) = &self.user_root else {
            return false;
        };
        let Some(dir) = clean_name(name).map(|n| user.join(n)) else {
            return false;
        };
        if !dir.is_dir() || std::fs::remove_dir_all(&dir).is_err() {
            return false;
        }
        if self.active.eq_ignore_ascii_case(name) {
            self.active.clear();
            self.saved.clear();
        }
        true
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

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cod4e-profile-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn stats_round_trip_through_the_file() {
        let mut s = vec![0; STAT_COUNT];
        s[0] = 7;
        s[1999] = 255;
        s[2000] = -3;
        s[2301] = 125_490;
        s[3497] = i32::MAX;
        let bytes = encode(&s);
        assert_eq!(bytes.len(), 0x211C);
        assert_eq!(&bytes[..4], b"ice0");
        assert_eq!(decode(&bytes).unwrap(), s);
    }

    #[test]
    fn decode_rejects_damaged_and_foreign_files() {
        let good = encode(&default_stats());
        assert_eq!(decode(&good[..100]), Err(MpdataError::Corrupt));
        let mut bad = good.clone();
        bad[STATS_AT + 5] ^= 1;
        assert!(!checksum_ok(&bad));
        assert_eq!(
            decode(&bad).unwrap()[5],
            default_stats()[5] ^ 1,
            "a bad checksum still reads"
        );
        let mut enc = good.clone();
        enc[..4].copy_from_slice(b"iwm0");
        assert_eq!(decode(&enc), Err(MpdataError::Encrypted));
        let mut other = good;
        other[..4].copy_from_slice(b"xxxx");
        assert_eq!(decode(&other), Err(MpdataError::Corrupt));
    }

    #[test]
    fn crc32_is_the_standard_one() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn a_new_profile_has_the_default_classes() {
        let s = default_stats();
        assert_eq!(s[299], 10);
        assert_eq!((s[201], s[205], s[209]), (25, 191, 0));
        assert_eq!((s[241], s[245]), (61, 176));
    }

    #[test]
    fn install_profiles_are_listed_read_and_never_written() {
        let root = tmp("install");
        let (install, cfg) = (root.join("install"), root.join("cfg"));
        let dir = install.join("players/profiles/Lagahoo");
        std::fs::create_dir_all(&dir).unwrap();
        let mut theirs = default_stats();
        theirs[2301] = 125_490;
        std::fs::write(dir.join("mpdata"), encode(&theirs)).unwrap();
        std::fs::write(install.join("players/profiles/active.txt"), "Lagahoo").unwrap();

        let (mut p, stats) = Profiles::open(&install, Some(cfg.clone()), "default");
        assert_eq!(p.active(), "Lagahoo");
        assert_eq!(p.list(), ["Lagahoo"]);
        assert_eq!(stats, theirs);

        let mut mine = stats.clone();
        mine[2301] += 5;
        assert!(p.save_if_changed(&mine).unwrap());
        assert_eq!(
            decode(&std::fs::read(dir.join("mpdata")).unwrap()).unwrap(),
            theirs
        );
        let (again, back) = Profiles::open(&install, Some(cfg), "default");
        assert_eq!(again.active(), "Lagahoo");
        assert_eq!(back, mine, "the user's copy shadows the install's");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn loading_and_saving_leave_the_install_untouched() {
        let root = tmp("untouched");
        let (install, cfg) = (root.join("install"), root.join("cfg"));
        let store = install.join("players/profiles");
        let dir = store.join("Lagahoo");
        std::fs::create_dir_all(&dir).unwrap();
        // A file the original rejects: CoD 4 X's magic and a checksum that does not match.
        let mut theirs = encode(&default_stats());
        theirs[CRC_AT] ^= 0xFF;
        std::fs::write(dir.join("mpdata"), &theirs).unwrap();
        std::fs::write(store.join("active.txt"), "Lagahoo").unwrap();
        let snapshot = || {
            let mut v: Vec<_> = walk(&install)
                .into_iter()
                .map(|p| (p.clone(), std::fs::read(&p).ok()))
                .collect();
            v.sort();
            v
        };
        let before = snapshot();
        let (mut p, mut s) = Profiles::open(&install, Some(cfg), "default");
        assert_eq!(s, default_stats(), "read despite the checksum");
        p.list();
        s[2301] = 5;
        p.save_if_changed(&s).unwrap();
        p.switch(&s, "Lagahoo").unwrap();
        assert_eq!(snapshot(), before, "no file renamed, rewritten or added");
        let _ = std::fs::remove_dir_all(root);
    }

    fn walk(d: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            out.push(p.clone());
            if p.is_dir() {
                out.extend(walk(&p));
            }
        }
        out
    }

    #[test]
    fn a_profile_config_is_the_users_copy_else_the_installs_and_saves_to_the_users_folder() {
        let root = tmp("cfg");
        let (install, cfg) = (root.join("install"), root.join("cfg"));
        let theirs = install.join("players/profiles/Lagahoo");
        std::fs::create_dir_all(&theirs).unwrap();
        std::fs::write(install.join("players/profiles/active.txt"), "Lagahoo").unwrap();
        let (p, _) = Profiles::open(&install, Some(cfg.clone()), "default");
        let mine = cfg.join("players/profiles/Lagahoo/config_mp.cfg");
        // No config anywhere: nothing to read, the user's file is where it will be written.
        assert_eq!(p.config_paths(), (None, Some(mine.clone())));
        std::fs::write(theirs.join("config_mp.cfg"), "bind w \"+a\"").unwrap();
        assert_eq!(
            p.config_paths(),
            (Some(theirs.join("config_mp.cfg")), Some(mine.clone()))
        );
        std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
        std::fs::write(&mine, "bind w \"+b\"").unwrap();
        assert_eq!(p.config_paths(), (Some(mine.clone()), Some(mine)));
        assert_eq!(Profiles::none().config_paths(), (None, None));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn create_switch_and_delete() {
        let root = tmp("crud");
        let (install, cfg) = (root.join("install"), root.join("cfg"));
        let (mut p, stats) = Profiles::open(&install, Some(cfg.clone()), "default");
        assert_eq!(
            (p.active(), p.list()),
            ("default", vec!["default".to_string()])
        );
        assert_eq!(stats, default_stats());
        assert_eq!(p.create("Bo/b").unwrap(), "Bo_b");
        assert_eq!(p.create("bo_b"), Err(CreateError::Exists));
        assert_eq!(p.create("  "), Err(CreateError::Failed));
        let mut s = stats;
        s[2301] = 9;
        let bob = p.switch(&s, "BO_B").unwrap();
        assert_eq!(bob, default_stats());
        assert_eq!(p.active(), "Bo_b");
        assert!(p.switch(&bob, "nobody").is_none());
        assert_eq!(
            p.switch(&bob, "default").unwrap()[2301],
            9,
            "the stats were saved on leaving"
        );
        assert!(p.delete("Bo_b"));
        assert!(!p.delete("Bo_b"));
        assert_eq!(p.list(), ["default"]);
        let (reopened, _) = Profiles::open(&install, Some(cfg), "other");
        assert_eq!(
            reopened.active(),
            "default",
            "active.txt wins over the fallback"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn with_profiles_but_no_active_none_is_chosen() {
        let root = tmp("none");
        let install = root.join("install");
        std::fs::create_dir_all(install.join("players/profiles/Ann")).unwrap();
        let (p, _) = Profiles::open(&install, None, "default");
        assert_eq!(p.active(), "");
        assert_eq!(p.list(), ["Ann"]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn without_a_config_dir_nothing_is_saved_and_install_profiles_cannot_be_deleted() {
        let root = tmp("nocfg");
        let install = root.join("install");
        std::fs::create_dir_all(install.join("players/profiles/Ann")).unwrap();
        std::fs::write(install.join("players/profiles/active.txt"), "Ann").unwrap();
        let (mut p, mut s) = Profiles::open(&install, None, "default");
        s[1] = 1;
        assert!(!p.save_if_changed(&s).unwrap());
        assert!(!p.delete("Ann"));
        assert_eq!(p.create("x"), Err(CreateError::Failed));
        let _ = std::fs::remove_dir_all(root);
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
