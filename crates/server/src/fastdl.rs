// SPDX-License-Identifier: GPL-3.0-only
//! What a server gives a joining client for the mod and the map it runs (`sv_allowDownload`): the mod's IWDs and
//! `mod.ff`, and the map's zone, loading zone and IWD. The list is published in a manifest ([`net::manifest`]) with each
//! file's size and CRC-32; a file is served by its name in the list and by nothing else, so a client cannot name a
//! path of its own. The map's files head the list and the list has the limits a client holds it to.

use assets::crc::file_crc32;
use net::manifest::{File, dest};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::SystemTime;

/// The files on offer, and where each is.
#[derive(Default)]
pub struct Offer {
    pub files: Vec<File>,
    paths: HashMap<String, PathBuf>,
    /// What each file looked like when it was read: a changed one is hashed again ([`Offer::stale`]).
    stamps: Vec<(PathBuf, u64, Option<SystemTime>)>,
    /// Files left out and why, for the server's log.
    pub skipped: Vec<String>,
    bytes: u64,
}

impl Offer {
    /// The list for the install at `root` running the mod `game` and `map`: the map's files first, then the mod's, so
    /// that what a full list leaves out is never the map. A file over [`net::download::MAX_TOTAL`], and any that would
    /// take the list past [`net::manifest::MAX_FILES`] files or [`net::manifest::MAX_SUM`] bytes, is left out and
    /// noted in [`Offer::skipped`]. Reads every file once to get its CRC, so it belongs on a thread of its own.
    pub fn build(root: &Path, game: &str, map: Option<&str>) -> Self {
        let mut offer = Self::default();
        let mod_dir = assets::paths::game_rel(root, game).map(|rel| root.join(rel));
        if let Some(map) = map.filter(|m| assets::paths::valid_map(m)) {
            for zone in [map.to_owned(), format!("{map}_load")] {
                let Some(path) = assets::paths::zone_file(root, game, &zone) else {
                    continue;
                };
                // A zone of the mod is the mod's file; any other is given as a custom map's, which is where a client
                // that lacks it keeps it.
                let name = match &mod_dir {
                    Some(dir) if path.parent() == Some(dir.as_path()) => {
                        format!("{game}/{zone}.ff")
                    }
                    _ => format!("usermaps/{map}/{zone}.ff"),
                };
                offer.add(game, &name, &path);
            }
            if let Some(path) = assets::paths::usermap_iwd(root, map) {
                offer.add(game, &format!("usermaps/{map}/{map}.iwd"), &path);
            }
        }
        if let Some(dir) = &mod_dir {
            let mut names: Vec<String> = assets::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| {
                    let n = n.to_ascii_lowercase();
                    n.ends_with(".iwd") || n == "mod.ff"
                })
                .collect();
            names.sort();
            for n in names {
                offer.add(game, &format!("{game}/{n}"), &dir.join(&n));
            }
        }
        offer
    }

    /// Lists `path` as `name` when a client could write it ([`dest`]) and it can be read.
    fn add(&mut self, game: &str, name: &str, path: &Path) {
        if dest(game, name).is_none() || self.paths.contains_key(name) {
            return;
        }
        let Ok(meta) = std::fs::metadata(path) else {
            return;
        };
        let why = if meta.len() > net::download::MAX_TOTAL {
            Some("it is larger than a client takes in one file")
        } else if self.files.len() >= net::manifest::MAX_FILES {
            Some("the list is full")
        } else if self.bytes + meta.len() > net::manifest::MAX_SUM {
            Some("the list would be larger than a client takes")
        } else {
            None
        };
        if let Some(why) = why {
            self.skipped
                .push(format!("download: not offering {name}: {why}\n"));
            return;
        }
        if let Ok((size, crc)) = file_crc32(path) {
            self.bytes += size;
            self.files.push(File {
                name: name.to_owned(),
                size,
                crc,
            });
            self.paths.insert(name.to_owned(), path.to_owned());
            self.stamps
                .push((path.to_owned(), meta.len(), meta.modified().ok()));
        }
    }

    /// The file on offer under `name`.
    pub fn path(&self, name: &str) -> Option<&Path> {
        self.paths.get(name).map(PathBuf::as_path)
    }

    /// Whether a file of the list changed (or went) since it was read: the CRCs it promises are no longer true.
    pub fn stale(&self) -> bool {
        self.stamps.iter().any(|(path, len, at)| {
            std::fs::metadata(path).is_ok_and(|m| m.len() != *len || m.modified().ok() != *at)
                || !path.exists()
        })
    }
}

/// The offer for the running map, built on a thread of its own: hashing the files of a big mod takes seconds, which
/// must not stall the game. Until it is ready a request goes unanswered, and the client asks again.
#[derive(Default)]
pub struct Offers {
    current: Option<(Option<String>, State)>,
    notes: Vec<String>,
}

enum State {
    Building(Receiver<Offer>),
    Ready(Offer),
}

impl Offers {
    /// Brings the offer up to date for `map`: a map change starts a new one; with `recheck`, so does a file that changed
    /// since the list was made (a request for the list, not one for a file already being fetched).
    pub fn refresh(&mut self, root: &Path, game: &str, map: Option<&str>, recheck: bool) {
        let same = self
            .current
            .as_ref()
            .is_some_and(|(m, _)| m.as_deref() == map);
        let stale =
            same && recheck && matches!(&self.current, Some((_, State::Ready(o))) if o.stale());
        if !same || stale {
            let (root, game, name) = (root.to_owned(), game.to_owned(), map.map(str::to_owned));
            let (tx, rx) = std::sync::mpsc::channel();
            let on = name.clone();
            let spawned = std::thread::Builder::new()
                .name("download-list".into())
                .spawn(move || {
                    let _ = tx.send(Offer::build(&root, &game, on.as_deref()));
                })
                .is_ok();
            if spawned {
                self.current = Some((name, State::Building(rx)));
            }
        }
        if let Some((_, state)) = self.current.as_mut()
            && let State::Building(rx) = state
            && let Ok(offer) = rx.try_recv()
        {
            self.notes.extend(offer.skipped.iter().cloned());
            *state = State::Ready(offer);
        }
    }

    /// The offer, once it is built.
    pub fn offer(&self) -> Option<&Offer> {
        match &self.current {
            Some((_, State::Ready(o))) => Some(o),
            _ => None,
        }
    }

    /// What the server should log about the lists built since the last call.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(root: &Path, rel: &str, bytes: &[u8]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    #[test]
    fn a_server_offers_its_mod_and_its_map_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for f in [
            "main/iw_00.iwd",
            "zone/english/mp_stock.ff",
            "zone/english/common_mp.ff",
            "usermaps/mp_custom/mp_custom.ff",
            "usermaps/mp_custom/mp_custom_load.ff",
            "usermaps/mp_custom/mp_custom.iwd",
            "mods/m/mod.ff",
            "mods/m/Z_One.iwd",
            "mods/m/notes.txt",
            "mods/m/odd name.iwd",
            "mods/other/z_two.iwd",
        ] {
            touch(root, f, f.as_bytes());
        }
        let names = |o: &Offer| o.files.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
        let offer = Offer::build(root, "mods/m", Some("mp_custom"));
        assert_eq!(
            names(&offer),
            [
                "usermaps/mp_custom/mp_custom.ff",
                "usermaps/mp_custom/mp_custom_load.ff",
                "usermaps/mp_custom/mp_custom.iwd",
                "mods/m/Z_One.iwd",
                "mods/m/mod.ff",
            ]
        );
        let one = &offer.files[3];
        assert_eq!(
            (one.size, one.crc),
            (
                "mods/m/Z_One.iwd".len() as u64,
                assets::crc::crc32(b"mods/m/Z_One.iwd")
            )
        );
        assert!(offer.path("mods/m/Z_One.iwd").is_some());
        for no in [
            "main/iw_00.iwd",
            "zone/english/common_mp.ff",
            "mods/m/notes.txt",
            "mods/m/odd name.iwd",
            "mods/other/z_two.iwd",
            "../main/iw_00.iwd",
            "mods/m/../../main/iw_00.iwd",
        ] {
            assert!(offer.path(no).is_none(), "{no}");
        }

        // A stock map is given as a custom map's files; the map of a stock server brings no mod files.
        let stock = Offer::build(root, "", Some("mp_stock"));
        assert_eq!(names(&stock), ["usermaps/mp_stock/mp_stock.ff"]);
        assert!(Offer::build(root, "", None).files.is_empty());
        // A map the server does not have, or a name that is not one, offers nothing.
        assert!(Offer::build(root, "", Some("mp_gone")).files.is_empty());
        assert!(Offer::build(root, "", Some("../main")).files.is_empty());
    }

    #[test]
    fn a_zone_in_the_mod_folder_is_the_mods_file() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "mods/m/mp_inmod.ff", b"zone");
        let offer = Offer::build(dir.path(), "mods/m", Some("mp_inmod"));
        assert_eq!(offer.files[0].name, "mods/m/mp_inmod.ff");
    }

    #[test]
    fn the_list_is_built_on_a_thread_and_rebuilt_for_a_new_map_or_a_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(root, "mods/m/a.iwd", b"one");
        touch(root, "usermaps/mp_x/mp_x.ff", b"zone");
        let mut offers = Offers::default();
        let wait = |offers: &mut Offers, map: Option<&str>, recheck: bool| {
            for _ in 0..2000 {
                offers.refresh(root, "mods/m", map, recheck);
                if let Some(o) = offers.offer() {
                    return o.files.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!("the list never came");
        };
        assert_eq!(wait(&mut offers, None, true), ["mods/m/a.iwd"]);
        assert_eq!(
            wait(&mut offers, Some("mp_x"), true),
            ["usermaps/mp_x/mp_x.ff", "mods/m/a.iwd"]
        );
        // A file that changed is listed again with its new CRC, but only when the list is asked for.
        touch(root, "mods/m/a.iwd", b"three");
        offers.refresh(root, "mods/m", Some("mp_x"), false);
        let old = offers.offer().unwrap().files[1].crc;
        assert_eq!(old, assets::crc::crc32(b"one"));
        wait(&mut offers, Some("mp_x"), true);
        let new = offers.offer().unwrap().files[1].crc;
        assert_eq!(new, assets::crc::crc32(b"three"));
    }

    #[test]
    fn a_full_list_drops_the_mods_files_never_the_maps_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "usermaps/mp_x/mp_x.ff", b"zone");
        touch(dir.path(), "usermaps/mp_x/mp_x.iwd", b"iwd");
        for i in 0..net::manifest::MAX_FILES + 5 {
            touch(dir.path(), &format!("mods/m/a{i:03}.iwd"), b"x");
        }
        let offer = Offer::build(dir.path(), "mods/m", Some("mp_x"));
        assert_eq!(offer.files.len(), net::manifest::MAX_FILES);
        for f in ["usermaps/mp_x/mp_x.ff", "usermaps/mp_x/mp_x.iwd"] {
            assert!(offer.path(f).is_some(), "{f}");
        }
        assert_eq!(offer.skipped.len(), 7);
        assert!(offer.skipped[0].contains("mods/m/a"), "{:?}", offer.skipped);
    }

    #[test]
    fn a_file_a_client_would_refuse_is_left_out_and_a_changed_file_makes_the_list_stale() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "mods/m/small.iwd", b"abc");
        let big = std::fs::File::create(dir.path().join("mods/m/huge.iwd")).unwrap();
        big.set_len(net::download::MAX_TOTAL + 1).unwrap();
        let offer = Offer::build(dir.path(), "mods/m", None);
        assert_eq!(
            offer
                .files
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            ["mods/m/small.iwd"]
        );
        assert_eq!(offer.skipped.len(), 1);
        assert!(!offer.stale());
        touch(dir.path(), "mods/m/small.iwd", b"abcd");
        assert!(offer.stale());
    }
}
