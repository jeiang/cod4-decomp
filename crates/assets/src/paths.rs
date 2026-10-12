// SPDX-License-Identifier: GPL-3.0-only
// Search order, game directory rule and mod fastfile lookup follow KisakCOD (GPL-3.0): universal/com_files.cpp
// (FS_GameDirDomainFunc, FS_GetModList) and database/db_registry.cpp (DB_TryLoadXFileInternal); Activision and
// the KisakCOD authors.
//! Where the install keeps what is not in the VFS: fastfiles, mods and custom maps.
//!
//! Fastfiles are looked up the way the original's `DB_TryLoadXFileInternal` does for a running mod: in the `fs_game`
//! directory first, then `zone/<language>`.

use crate::vfs::LANGUAGES;
use std::path::{Path, PathBuf};

/// The install's language: line 1 of `localization.txt` (default `english`), as an index into [`LANGUAGES`].
pub fn language_index(root: &Path) -> usize {
    crate::fs::read_to_string(root.join("localization.txt"))
        .ok()
        .and_then(|t| t.lines().next().map(|l| l.trim().to_ascii_lowercase()))
        .and_then(|l| LANGUAGES.iter().position(|n| *n == l))
        .unwrap_or(0)
}

/// Whether `game` may be an `fs_game`: empty, or `mods/<name>` (lowercase, `/`) with no `..` and no `:`. Stricter than
/// `FS_GameDirDomainFunc`, which also takes any case, a backslash and a single `:`: the path is the same on every
/// file system this way.
pub fn valid_game(game: &str) -> bool {
    game.is_empty()
        || (game.len() > 5
            && game.starts_with("mods/")
            && !game.contains("..")
            && !game.contains([':', '\\']))
}

/// `name` in `dir`, matched case-insensitively.
pub fn find_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    let direct = dir.join(name);
    if crate::fs::exists(&direct) {
        return Some(direct);
    }
    crate::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        .map(|e| e.path())
}

/// `rel` (a `/`-separated path) below `root`, each part matched case-insensitively.
fn find_path(root: &Path, rel: &str) -> Option<PathBuf> {
    rel.split(['/', '\\'])
        .filter(|p| !p.is_empty())
        .try_fold(root.to_owned(), |dir, part| find_ci(&dir, part))
}

/// `zone/<language>` of the install at `root`; where a downloaded stock-layout zone goes.
pub fn zone_dir(root: &Path) -> Option<PathBuf> {
    find_path(root, &format!("zone/{}", LANGUAGES[language_index(root)]))
}

/// The fastfile `name`: `<game>/<name>.ff`, then `zone/<language>/<name>.ff`.
pub fn zone_file(root: &Path, game: &str, name: &str) -> Option<PathBuf> {
    let file = format!("{name}.ff");
    let in_game = || {
        let dir = find_path(root, game).filter(|_| !game.is_empty() && valid_game(game))?;
        find_ci(&dir, &file)
    };
    in_game().or_else(|| find_ci(&zone_dir(root)?, &file))
}

/// The directory of `game` as it is named on disk, relative to `root` (`mods/Synth` for `mods/synth` on a
/// case-sensitive file system); `None` when `game` is not valid or not there.
pub fn game_rel(root: &Path, game: &str) -> Option<String> {
    let dir = find_path(root, game).filter(|d| valid_game(game) && crate::fs::is_dir(d))?;
    let rel = dir.strip_prefix(root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

/// Whether the game directory `game` (`mods/<name>`) is in the install at `root`.
pub fn game_dir(root: &Path, game: &str) -> bool {
    valid_game(game) && find_path(root, game).is_some_and(|d| crate::fs::is_dir(&d))
}

/// A mod's own fastfile, `<game>/mod.ff`.
pub fn mod_zone(root: &Path, game: &str) -> Option<PathBuf> {
    (!game.is_empty() && valid_game(game))
        .then(|| find_ci(&find_path(root, game)?, "mod.ff"))
        .flatten()
}

/// The mods in `mods/` (the folders), with the first 48 bytes of each one's `description.txt` or else its name, sorted
/// by name (`FS_GetModList`).
pub fn list_mods(root: &Path) -> Vec<(String, String)> {
    let Some(dir) = find_ci(root, "mods") else {
        return Vec::new();
    };
    let mut mods: Vec<(String, String)> = crate::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| crate::fs::is_dir(&e.path()))
        .filter(|e| valid_game(&format!("mods/{}", e.file_name().to_string_lossy())))
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let desc = find_ci(&e.path(), "description.txt")
                .and_then(|p| crate::fs::read(p).ok())
                .map(|b| {
                    let b = &b[..b.len().min(48)];
                    let end = b.iter().position(|c| *c == 0).unwrap_or(b.len());
                    String::from_utf8_lossy(&b[..end]).into_owned()
                })
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| name.clone());
            (name, desc)
        })
        .collect();
    mods.sort_by_cached_key(|(n, _)| n.to_ascii_lowercase());
    mods
}

/// The names of the maps (`mp_*`, no `_load`) that have a fastfile in `dir`.
fn ff_maps(dir: &Path) -> impl Iterator<Item = String> {
    crate::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let stem = p.file_stem()?.to_str()?.to_ascii_lowercase();
            let is_ff = p.extension()?.eq_ignore_ascii_case("ff");
            (is_ff && stem.starts_with("mp_") && !stem.ends_with("_load")).then_some(stem)
        })
}

/// Every map the install can run under `game`: the stock zone folder and the game directory, sorted.
pub fn maps(root: &Path, game: &str) -> Vec<String> {
    let mut v: Vec<String> = zone_dir(root)
        .into_iter()
        .flat_map(|d| ff_maps(&d).collect::<Vec<_>>())
        .collect();
    v.extend(custom_maps(root, game));
    v.sort();
    v.dedup();
    v
}

/// The maps that are not stock: the ones in the game directory, sorted.
pub fn custom_maps(root: &Path, game: &str) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(dir) = find_path(root, game).filter(|_| !game.is_empty() && valid_game(game)) {
        v.extend(ff_maps(&dir));
    }
    v.sort();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("cod4e-paths-{}-{tag}", std::process::id()));
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
        fn file(&self, rel: &str, bytes: &[u8]) {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, bytes).unwrap();
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn game_directories_stay_inside_mods() {
        for ok in ["", "mods/promod"] {
            assert!(valid_game(ok), "{ok}");
        }
        for bad in [
            "mods",
            "mods/",
            "main",
            "mods/../main",
            "/mods/x",
            "mods/a::b",
            "xmods/a",
            "Mods/x",
            "mods\\x",
            "mods/a:b",
        ] {
            assert!(!valid_game(bad), "{bad}");
        }
    }

    #[test]
    fn a_fastfile_is_found_in_the_mod_then_the_zone_folder() {
        let t = Tmp::new("zones");
        t.file("zone/english/mp_a.ff", b"stock");
        t.file("mods/m/mp_a.ff", b"mod");
        t.file("mods/m/mod.ff", b"");
        t.file("mods/m/mp_m.ff", b"");
        let at = |game, name| zone_file(&t.0, game, name).map(|p| std::fs::read(p).unwrap());
        assert_eq!(at("", "mp_a").unwrap(), b"stock");
        assert_eq!(at("mods/m", "mp_a").unwrap(), b"mod");
        assert!(at("", "mp_m").is_none(), "a mod's own map is not stock");
        assert_eq!(game_rel(&t.0, "mods/m").as_deref(), Some("mods/m"));
        assert!(game_rel(&t.0, "mods/gone").is_none());
        assert!(mod_zone(&t.0, "mods/m").is_some() && mod_zone(&t.0, "").is_none());
        assert!(
            zone_file(&t.0, "mods/../zone", "mp_a").is_some(),
            "a bad game is ignored"
        );
        assert_eq!(maps(&t.0, "mods/m"), ["mp_a", "mp_m"]);
    }

    #[test]
    fn mods_are_listed_by_folder_with_their_description() {
        let t = Tmp::new("mods");
        t.file("mods/b/mod.ff", b"");
        t.file("mods/A/description.txt", b"Alpha mod");
        t.file("mods/loose.txt", b"");
        t.file("mods/a..b/mod.ff", b"");
        assert_eq!(
            list_mods(&t.0),
            [("A".into(), "Alpha mod".into()), ("b".into(), "b".into())]
        );
    }
}
