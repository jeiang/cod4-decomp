// SPDX-License-Identifier: GPL-3.0-only
//! The files a server gives for the mod and the map it runs, and what a client may do with the list. Wire
//! compatibility with the original's download protocol is not a goal: the list comes in [`Oob::Manifest`] packets, one
//! per file, and each file is then fetched over the out-of-band `getfile` ([`crate::download`]) or, when the server
//! names a fast-download URL, over HTTP(S).
//!
//! A server is not trusted: [`dest`] decides where a file may be written, and only the mod the server runs, and a
//! custom map's own folder, qualify.

use crate::oob::Oob;
use crate::transport::Transport;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Most files a manifest lists: a mod's IWDs, its `mod.ff` and a custom map's three. A server lists the map's files
/// first and says in its log which it left out.
pub const MAX_FILES: usize = 64;
/// Most bytes all the files of a manifest add up to; a client refuses a longer list, so a server leaves out what would
/// pass it (and any one file over [`crate::download::MAX_TOTAL`]).
pub const MAX_SUM: u64 = 2 << 30;
/// What a server answers a `getmanifest` or `getfile` when `sv_allowDownload` is off.
pub const DOWNLOADS_OFF: &str = "Downloads are not enabled on this server";
/// Longest fast-download URL.
pub const MAX_URL: usize = 256;

/// One file of a manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct File {
    /// Where the file lives below the install, `/`-separated: `mods/<name>/<file>` or `usermaps/<map>/<file>`.
    pub name: String,
    pub size: u64,
    /// CRC-32 of the whole file.
    pub crc: u32,
}

/// A file name that cannot name a path: letters, digits, `_`, `-` and `.`, not hidden.
fn plain_file(f: &str) -> bool {
    !f.is_empty()
        && f.len() <= 64
        && !f.starts_with('.')
        && f.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// Where `name`, a file of a manifest of a server running the mod `game` (`fs_game`), goes below the install; `None`
/// for a name the client will not write: only an IWD or a zone (`mod.ff`, a map's) of that mod, and a custom map's `<map>.iwd`,
/// `<map>.ff` and `<map>_load.ff` in its own folder. No path part is taken from the server but the file's name.
pub fn dest(game: &str, name: &str) -> Option<String> {
    let (dir, file) = name.rsplit_once('/')?;
    if !plain_file(file) {
        return None;
    }
    if !game.is_empty() && assets::paths::valid_game(game) && dir.eq_ignore_ascii_case(game) {
        let lower = file.to_ascii_lowercase();
        // A map of the mod: its zone and loading zone.
        let zone = lower
            .strip_suffix(".ff")
            .is_some_and(|z| assets::paths::valid_map(z.strip_suffix("_load").unwrap_or(z)));
        return (lower.ends_with(".iwd") || zone).then(|| format!("{game}/{file}"));
    }
    let map = dir.strip_prefix("usermaps/")?;
    (assets::paths::valid_map(map)
        && [
            format!("{map}.iwd"),
            format!("{map}.ff"),
            format!("{map}_load.ff"),
        ]
        .iter()
        .any(|f| f == file))
    .then(|| name.to_owned())
}

/// Whether `url` may be a fast-download base: `http://` or `https://`, one word, no control characters.
pub fn valid_www(url: &str) -> bool {
    url.len() <= MAX_URL
        && (url.starts_with("http://") || url.starts_with("https://"))
        && url
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'"' | b'\\' | b'<' | b'>' | b'`'))
}

/// The URL of the file `name` below the base `www`.
pub fn url(www: &str, name: &str) -> String {
    format!("{}/{name}", www.trim_end_matches('/'))
}

/// The packets that answer a `getmanifest` with `files` (a server cuts the list at [`MAX_FILES`]).
pub fn packets(files: &[File], www: &str) -> Vec<Oob> {
    let files = &files[..files.len().min(MAX_FILES)];
    if files.is_empty() {
        return vec![Oob::Manifest {
            index: 0,
            count: 0,
            file: File::default(),
            www: www.to_owned(),
        }];
    }
    files
        .iter()
        .enumerate()
        .map(|(i, f)| Oob::Manifest {
            index: i as u16,
            count: files.len() as u16,
            file: f.clone(),
            www: www.to_owned(),
        })
        .collect()
}

/// Collects the [`Oob::Manifest`] packets of one answer, in any order and with repeats.
#[derive(Default)]
pub struct Assembler {
    files: Option<Vec<Option<File>>>,
    www: String,
}

impl Assembler {
    /// Takes one packet; `Err` for an answer that cannot be believed.
    pub fn add(&mut self, index: u16, count: u16, file: File, www: String) -> Result<(), String> {
        let count = usize::from(count);
        if count > MAX_FILES {
            return Err("The server's file list is too long".into());
        }
        let files = self.files.get_or_insert_with(|| vec![None; count]);
        if files.len() != count {
            return Err("The server's file list changed".into());
        }
        if count > 0 {
            let slot = files
                .get_mut(usize::from(index))
                .ok_or("The server's file list is malformed")?;
            if file.size > crate::download::MAX_TOTAL {
                return Err("A file of the server is too large".into());
            }
            *slot = Some(file);
            if files.iter().flatten().map(|f| f.size).sum::<u64>() > MAX_SUM {
                return Err("The server's files are too large".into());
            }
        }
        self.www = www;
        Ok(())
    }

    /// The files and the fast-download base once every packet is in.
    pub fn done(&self) -> Option<(Vec<File>, &str)> {
        let files = self.files.as_ref()?;
        let all: Option<Vec<File>> = files.iter().cloned().collect();
        Some((all?, &self.www))
    }
}

/// How long a server may stay silent before the request is made again, and how long the whole answer may take (a
/// server that has not listed its files yet is hashing them, and stays silent).
const RESEND: Duration = Duration::from_millis(500);
const GIVE_UP: Duration = Duration::from_secs(60);

/// Asks `server` for its manifest (taking a challenge first) and waits for every packet of the answer, asking again
/// when it goes quiet. Blocks until it is done, fails or `cancel` is set. The files, and the fast-download base
/// (empty when the server has none).
pub fn fetch_list(
    t: &mut dyn Transport,
    server: SocketAddr,
    cancel: &AtomicBool,
) -> Result<(Vec<File>, String), String> {
    let mut buf = vec![0u8; 2048];
    let mut challenge: Option<u32> = None;
    let mut asm = Assembler::default();
    let (started, mut sent) = (Instant::now(), None::<Instant>);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Download cancelled".into());
        }
        if started.elapsed() > GIVE_UP {
            return Err("The server does not answer".into());
        }
        if sent.is_none_or(|s| s.elapsed() > RESEND) {
            let ask = match challenge {
                Some(challenge) => Oob::GetManifest { challenge },
                None => Oob::GetChallenge,
            };
            t.send_to(server, &ask.encode());
            sent = Some(Instant::now());
        }
        match t.recv_from(&mut buf, Some(Duration::from_millis(50))) {
            Ok(Some((n, from))) if from == server => match Oob::parse(&buf[..n]) {
                Some(Oob::Challenge(c)) => {
                    challenge = Some(c);
                    sent = None;
                }
                Some(Oob::Manifest {
                    index,
                    count,
                    file,
                    www,
                }) => {
                    asm.add(index, count, file, www)?;
                    if let Some((files, www)) = asm.done() {
                        return Ok((files, www.to_owned()));
                    }
                }
                Some(Oob::Error(why)) if why == crate::download::BAD_CHALLENGE => {
                    challenge = None;
                    sent = None;
                }
                Some(Oob::Error(why)) => return Err(why),
                _ => {}
            },
            Ok(_) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(name: &str) -> File {
        File {
            name: name.into(),
            size: 1,
            crc: 2,
        }
    }

    #[test]
    fn only_the_servers_mod_and_a_maps_own_files_have_a_destination() {
        let game = "mods/synth";
        for (name, to) in [
            ("mods/synth/a.iwd", Some("mods/synth/a.iwd")),
            ("mods/Synth/Z_B.IWD", Some("mods/synth/Z_B.IWD")),
            ("mods/synth/mod.ff", Some("mods/synth/mod.ff")),
            ("usermaps/mp_x/mp_x.ff", Some("usermaps/mp_x/mp_x.ff")),
            (
                "usermaps/mp_x/mp_x_load.ff",
                Some("usermaps/mp_x/mp_x_load.ff"),
            ),
            ("usermaps/mp_x/mp_x.iwd", Some("usermaps/mp_x/mp_x.iwd")),
            // Another mod, the stock folders, a map's foreign file, executables and escapes.
            ("mods/other/a.iwd", None),
            ("main/iw_00.iwd", None),
            ("zone/english/mp_x.ff", None),
            ("usermaps/mp_x/mp_y.ff", None),
            ("usermaps/mp_x/evil.exe", None),
            ("mods/synth/evil.exe", None),
            ("mods/synth/sub/a.iwd", None),
            ("mods/synth/../a.iwd", None),
            ("mods/synth/..iwd", None),
            ("mods/synth/.hidden.iwd", None),
            ("mods/synth/a b.iwd", None),
            ("mods/synth/a\\b.iwd", None),
            ("usermaps/../mp_x.ff", None),
            ("usermaps/MP_X/MP_X.ff", None),
            ("/etc/passwd", None),
            ("a.iwd", None),
            ("", None),
        ] {
            assert_eq!(dest(game, name).as_deref(), to, "{name:?}");
        }
        // A server with no mod gives no mod files.
        assert_eq!(dest("", "mods/synth/a.iwd"), None);
        assert_eq!(
            dest("", "usermaps/mp_x/mp_x.ff").as_deref(),
            Some("usermaps/mp_x/mp_x.ff")
        );
        // A mod name that is not a valid fs_game matches nothing.
        assert_eq!(dest("../x", "../x/a.iwd"), None);
    }

    #[test]
    fn a_manifest_is_whole_once_every_file_has_come() {
        let files = vec![f("mods/m/a.iwd"), f("mods/m/b.iwd"), f("mods/m/mod.ff")];
        let mut a = Assembler::default();
        let packets = packets(&files, "http://h/x/");
        assert_eq!(packets.len(), 3);
        for p in [&packets[2], &packets[0], &packets[2]] {
            let Oob::Manifest {
                index,
                count,
                file,
                www,
            } = p
            else {
                panic!()
            };
            assert!(a.done().is_none());
            a.add(*index, *count, file.clone(), www.clone()).unwrap();
        }
        assert!(a.done().is_none());
        let Oob::Manifest {
            index,
            count,
            file,
            www,
        } = &packets[1]
        else {
            panic!()
        };
        a.add(*index, *count, file.clone(), www.clone()).unwrap();
        let (got, www) = a.done().unwrap();
        assert_eq!((got, www), (files, "http://h/x/"));

        let mut empty = Assembler::default();
        let Oob::Manifest {
            index,
            count,
            file,
            www,
        } = &super::packets(&[], "")[0]
        else {
            panic!()
        };
        empty
            .add(*index, *count, file.clone(), www.clone())
            .unwrap();
        assert_eq!(empty.done().unwrap().0, vec![]);
    }

    #[test]
    fn a_hostile_list_is_refused() {
        let mut a = Assembler::default();
        assert!(
            a.add(0, MAX_FILES as u16 + 1, f("x"), String::new())
                .is_err()
        );
        let mut a = Assembler::default();
        a.add(0, 2, f("mods/m/a.iwd"), String::new()).unwrap();
        assert!(a.add(1, 3, f("mods/m/b.iwd"), String::new()).is_err());
        assert!(a.add(5, 2, f("mods/m/b.iwd"), String::new()).is_err());
        let big = File {
            size: crate::download::MAX_TOTAL + 1,
            ..f("mods/m/b.iwd")
        };
        assert!(a.add(1, 2, big, String::new()).is_err());
        // Each file passes, the list as a whole does not.
        let mut a = Assembler::default();
        let each = crate::download::MAX_TOTAL;
        let fits = (MAX_SUM / each) as u16;
        for i in 0..=fits {
            let file = File {
                size: each,
                ..f("mods/m/x.iwd")
            };
            let r = a.add(i, fits + 1, file, String::new());
            assert_eq!(r.is_err(), i == fits, "{i}");
        }
    }

    #[test]
    fn only_plain_http_urls_pass() {
        assert!(valid_www("http://example.org/fastdl"));
        assert!(valid_www("https://example.org:8080/a/"));
        for bad in [
            "",
            "ftp://x/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "http://a b/",
            "http://a\"b/",
            "http://x/\n",
            &format!("http://{}", "a".repeat(MAX_URL)),
        ] {
            assert!(!valid_www(bad), "{bad:?}");
        }
        assert_eq!(
            url("http://h/x/", "mods/m/a.iwd"),
            "http://h/x/mods/m/a.iwd"
        );
        assert_eq!(url("http://h/x", "mods/m/a.iwd"), "http://h/x/mods/m/a.iwd");
    }
}
