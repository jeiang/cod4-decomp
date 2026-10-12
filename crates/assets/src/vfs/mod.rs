// SPDX-License-Identifier: GPL-3.0-only
//! Virtual file system over the original install: IWDs and loose directories
//! resolved in the original engine's search order (first hit wins).
//!
//! `fs_basepath == fs_homepath` and there is no `fs_cdpath`. `fs_game` (a `mods/<name>`
//! directory) is added last, so it has the highest priority, as the original does
//! (`FS_Startup`); [`Builder::add_game_dir`] adds one more game directory.

mod iwd;
mod source;

pub use iwd::{Entry, Iwd};
pub use source::{FileSource, ReadAt};

use std::io;
use std::path::{Path, PathBuf};

/// Languages of the original engine, in table order (`loc_language` indexes this).
pub const LANGUAGES: [&str; 15] = [
    "english",
    "french",
    "german",
    "italian",
    "spanish",
    "british",
    "russian",
    "polish",
    "korean",
    "taiwanese",
    "japanese",
    "chinese",
    "thai",
    "leet",
    "czech",
];

/// Original cap on IWDs per game directory.
const MAX_IWDS: usize = 1024;
const LOCALIZED_PREFIX: &str = "localized_";

/// Lookup key: ASCII case-folded, `\` as `/`.
fn normalize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == '\\' {
                '/'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect()
}

fn language_index(name: &str) -> Option<usize> {
    LANGUAGES.iter().position(|l| l.eq_ignore_ascii_case(name))
}

pub enum NodeKind {
    /// Loose files under a directory.
    Dir(PathBuf),
    Iwd {
        path: PathBuf,
        iwd: Iwd,
    },
}

/// One search-path node. `language` is `Some` for localized nodes, which are
/// consulted only when it equals the current language.
pub struct Node {
    pub kind: NodeKind,
    pub language: Option<usize>,
}

/// A resolved file, not yet read.
pub enum Hit<'a> {
    Iwd { iwd: &'a Iwd, entry: &'a Entry },
    File(PathBuf),
}

impl Hit<'_> {
    pub fn read(&self) -> io::Result<Vec<u8>> {
        match self {
            Hit::Iwd { iwd, entry } => iwd.read(entry),
            Hit::File(path) => crate::fs::read(path),
        }
    }
}

pub struct Vfs {
    nodes: Vec<Node>,
    language: usize,
}

impl Vfs {
    /// Stock search path rooted at the install directory: `players`,
    /// `main_shared`, `main` (see the boot-load research for the order).
    pub fn open_stock(root: &Path, language: usize) -> io::Result<Self> {
        Self::open(root, language, "", None)
    }

    /// The stock search path plus `game` (`fs_game`, such as `mods/promod`; empty for none) and the
    /// package of the custom map `usermap` (`usermaps/<map>`: its IWDs and loose files). The mod
    /// directory sits above the usermap's, which sits above `main`.
    pub fn open(
        root: &Path,
        language: usize,
        game: &str,
        usermap: Option<&str>,
    ) -> io::Result<Self> {
        if !crate::paths::valid_game(game) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid fs_game '{game}'"),
            ));
        }
        let mut b = Builder::new(root);
        b.add_game_dir("players")?;
        b.add_game_dir("main_shared")?;
        b.add_game_dir("main")?;
        if let Some(map) = usermap.filter(|m| crate::paths::valid_map(m)) {
            b.add_game_dir(&format!("usermaps/{map}"))?;
        }
        if !game.is_empty() {
            let rel = crate::paths::game_rel(root, game).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("fs_game '{game}': no such mod folder"),
                )
            })?;
            b.add_game_dir(&rel)?;
        }
        b.finish(language)
    }

    /// Nodes in search order, highest priority first.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn language(&self) -> usize {
        self.language
    }

    /// First node holding `name`, in search order.
    pub fn locate(&self, name: &str) -> Option<Hit<'_>> {
        self.locate_node(name).map(|(_, hit)| hit)
    }

    /// Like [`Vfs::locate`], also returning the index into [`Vfs::nodes`].
    pub fn locate_node(&self, name: &str) -> Option<(usize, Hit<'_>)> {
        let norm = normalize(name);
        self.nodes.iter().enumerate().find_map(|(i, node)| {
            if node.language.is_some_and(|l| l != self.language) {
                return None;
            }
            let hit = match &node.kind {
                NodeKind::Iwd { iwd, .. } => {
                    let entry = iwd.find(&norm)?;
                    Hit::Iwd { iwd, entry }
                }
                NodeKind::Dir(dir) => Hit::File(find_file(dir, &norm)?),
            };
            Some((i, hit))
        })
    }

    /// Every file below `prefix` (a directory, such as `maps/`) ending in `suffix`, in the lookup
    /// form of [`Vfs::locate`] (lowercase, `/`), each once. Localized nodes of another language
    /// are left out.
    pub fn list(&self, prefix: &str, suffix: &str) -> Vec<String> {
        let prefix = normalize(prefix);
        let mut out = std::collections::BTreeSet::new();
        for node in &self.nodes {
            if node.language.is_some_and(|l| l != self.language) {
                continue;
            }
            match &node.kind {
                NodeKind::Iwd { iwd, .. } => out.extend(
                    iwd.entries()
                        .iter()
                        .map(|e| normalize(&e.name))
                        .filter(|n| n.starts_with(&prefix) && n.ends_with(suffix)),
                ),
                NodeKind::Dir(dir) => walk(dir, &prefix, suffix, &mut out),
            }
        }
        out.into_iter().collect()
    }

    /// Read `name` through the search path; `Ok(None)` if absent.
    pub fn read(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        self.locate(name).map(|h| h.read()).transpose()
    }
}

/// Adds the files below `dir/<prefix>` ending in `suffix`, named from `dir` on (lowercase, `/`).
fn walk(dir: &Path, prefix: &str, suffix: &str, out: &mut std::collections::BTreeSet<String>) {
    fn rec(path: &Path, name: &str, suffix: &str, out: &mut std::collections::BTreeSet<String>) {
        let Ok(entries) = crate::fs::read_dir(path) else {
            return;
        };
        for e in entries.flatten() {
            let n = format!(
                "{name}{}",
                e.file_name().to_string_lossy().to_ascii_lowercase()
            );
            let p = e.path();
            if crate::fs::is_dir(&p) {
                rec(&p, &format!("{n}/"), suffix, out);
            } else if n.ends_with(suffix) {
                out.insert(n);
            }
        }
    }
    let rel = prefix.trim_end_matches('/');
    let start = if rel.is_empty() {
        Some(dir.to_path_buf())
    } else {
        find_dir(dir, rel)
    };
    if let Some(start) = start {
        let name = if rel.is_empty() {
            String::new()
        } else {
            format!("{rel}/")
        };
        rec(&start, &name, suffix, out);
    }
}

/// `rel` below `dir`, each part matched case-insensitively, when it is a directory.
fn find_dir(dir: &Path, rel: &str) -> Option<PathBuf> {
    let mut cur = dir.to_path_buf();
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        cur = crate::fs::read_dir(&cur)
            .ok()?
            .flatten()
            .find(|e| e.file_name().to_string_lossy().to_ascii_lowercase() == part)?
            .path();
    }
    crate::fs::is_dir(&cur).then_some(cur)
}

/// Case-insensitive path lookup below `dir` (the install may sit on a
/// case-sensitive file system). Directory entries are not files.
fn find_file(dir: &Path, norm: &str) -> Option<PathBuf> {
    let mut cur = dir.to_path_buf();
    for part in norm.split('/').filter(|p| !p.is_empty()) {
        let direct = cur.join(part);
        cur = if crate::fs::exists(&direct) {
            direct
        } else {
            crate::fs::read_dir(&cur)
                .ok()?
                .flatten()
                .find(|e| e.file_name().to_string_lossy().to_ascii_lowercase() == part)?
                .path()
        };
    }
    crate::fs::is_file(&cur).then_some(cur)
}

/// Builds the search path the way `AddGameDirectory` does: every addition is
/// prepended, localized nodes sit behind all non-localized ones.
pub struct Builder {
    base: PathBuf,
    /// Insertion order; reversed in `finish`.
    normal: Vec<Node>,
    localized: Vec<Node>,
}

impl Builder {
    pub fn new(base: &Path) -> Self {
        Self {
            base: base.to_path_buf(),
            normal: vec![],
            localized: vec![],
        }
    }

    /// Add `<base>/<dir>` for every language subfolder, then plain.
    /// Later additions take priority.
    pub fn add_game_dir(&mut self, dir: &str) -> io::Result<()> {
        for (i, lang) in LANGUAGES.iter().enumerate() {
            self.add_dir(dir, &Path::new(dir).join(lang), Some(i))?;
        }
        self.add_dir(dir, Path::new(dir), None)
    }

    fn push(&mut self, node: Node) {
        if node.language.is_some() {
            &mut self.localized
        } else {
            &mut self.normal
        }
        .push(node);
    }

    fn add_dir(
        &mut self,
        game_dir: &str,
        rel: &Path,
        subfolder_lang: Option<usize>,
    ) -> io::Result<()> {
        let path = self.base.join(rel);
        if !crate::fs::is_dir(&path) {
            return Ok(());
        }
        self.push(Node {
            kind: NodeKind::Dir(path.clone()),
            language: subfolder_lang,
        });

        let mut found: Vec<(String, Option<usize>)> = vec![];
        for e in crate::fs::read_dir(&path)? {
            let name = e?.file_name().to_string_lossy().into_owned();
            if !name.to_ascii_lowercase().ends_with(".iwd") {
                continue;
            }
            let lower = name.to_ascii_lowercase();
            let language = match (subfolder_lang, lower.strip_prefix(LOCALIZED_PREFIX)) {
                (Some(l), _) => Some(l),
                (None, Some(rest)) => match language_index(rest.split('_').next().unwrap_or("")) {
                    Some(l) => Some(l),
                    None => continue, // invalid localized name: original warns and skips
                },
                (None, None) => None,
            };
            // Basepath `main` accepts only `iw_*` for non-localized IWDs.
            if language.is_none()
                && game_dir.eq_ignore_ascii_case("main")
                && !lower.starts_with("iw_")
            {
                continue;
            }
            found.push((name, language));
        }
        if found.len() > MAX_IWDS {
            found.sort_by_key(|(n, _)| n.to_ascii_lowercase());
            found.truncate(MAX_IWDS);
        }
        if found.is_empty() && subfolder_lang.is_none() && game_dir.eq_ignore_ascii_case("main") {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "No IWD files found in /main",
            ));
        }
        found.sort_by_cached_key(|(n, lang)| iwd_sort_key(n, *lang));
        for (name, language) in found {
            let iwd_path = path.join(name);
            let iwd = Iwd::open(&iwd_path)?;
            self.push(Node {
                kind: NodeKind::Iwd {
                    path: iwd_path,
                    iwd,
                },
                language,
            });
        }
        Ok(())
    }

    pub fn finish(mut self, language: usize) -> io::Result<Vfs> {
        self.normal.reverse();
        self.localized.reverse();
        self.normal.append(&mut self.localized);
        Ok(Vfs {
            nodes: self.normal,
            language,
        })
    }
}

/// Original IWD order (ascending = lowest priority first): localized names
/// sort before all others, English before other languages among them, then
/// by name with ASCII case folded to upper and `\`, `:` as `/`.
fn iwd_sort_key(name: &str, language: Option<usize>) -> (u8, u8, String) {
    let fold = |s: &str| {
        s.chars()
            .map(|c| match c {
                '\\' | ':' => '/',
                c => c.to_ascii_uppercase(),
            })
            .collect::<String>()
    };
    match language {
        Some(l) => {
            let stripped = name
                .get(..LOCALIZED_PREFIX.len())
                .filter(|p| p.eq_ignore_ascii_case(LOCALIZED_PREFIX))
                .map_or(name, |_| &name[LOCALIZED_PREFIX.len()..]);
            (0, u8::from(l != 0), fold(stripped))
        }
        None => (1, 0, fold(name)),
    }
}

#[cfg(test)]
mod tests;
