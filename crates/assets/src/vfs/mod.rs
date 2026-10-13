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

/// A lookup key that names something below a search-path node: not absolute, no drive, no `..`.
fn stays_inside(norm: &str) -> bool {
    !norm.starts_with('/') && !norm.contains(':') && !norm.split('/').any(|p| p == "..")
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
            Hit::Iwd { iwd, entry } => iwd.read(entry).inspect_err(
                |e| tracing::warn!(file = %entry.name, error = %e, "cannot read iwd entry"),
            ),
            Hit::File(path) => crate::fs::read(path).inspect_err(
                |e| tracing::warn!(file = %path.display(), error = %e, "cannot read file"),
            ),
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
        if !stays_inside(&norm) {
            return None;
        }
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

    /// Top-level names (lower-cased, sorted, unique) of the files of the search path that end with `ext`, for the
    /// console's argument completion.
    pub fn names_with_ext(&self, ext: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for node in &self.nodes {
            if node.language.is_some_and(|l| l != self.language) {
                continue;
            }
            match &node.kind {
                NodeKind::Iwd { iwd, .. } => {
                    out.extend(iwd.entries().iter().map(|e| e.name.clone()));
                }
                NodeKind::Dir(dir) => {
                    if let Ok(rd) = crate::fs::read_dir(dir) {
                        out.extend(
                            rd.flatten()
                                .map(|e| e.file_name().to_string_lossy().into_owned()),
                        );
                    }
                }
            }
        }
        out.retain(|n| !n.contains(['/', '\\']) && n.to_ascii_lowercase().ends_with(ext));
        for n in &mut out {
            n.make_ascii_lowercase();
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The IWDs of the search path, by name below `root` (lowercase, `/`) and [`Iwd::pack_checksum`], sorted by name:
    /// what `sv_pure` compares. Localized IWDs are left out: they depend on the player's language.
    pub fn pure_iwds(&self, root: &Path) -> Vec<(String, u32)> {
        let mut out: Vec<(String, u32)> = self
            .nodes
            .iter()
            .filter(|n| n.language.is_none())
            .filter_map(|n| match &n.kind {
                NodeKind::Iwd { path, iwd } => {
                    let rel = path.strip_prefix(root).unwrap_or(path);
                    Some((
                        rel.to_string_lossy()
                            .replace('\\', "/")
                            .to_ascii_lowercase(),
                        iwd.pack_checksum(),
                    ))
                }
                NodeKind::Dir(_) => None,
            })
            .collect();
        out.sort();
        out
    }
}

/// One number for a [`Vfs::pure_iwds`] list: the client sends it with its `connect` and an `sv_pure` server compares it
/// with its own. Never 0, which is what a client that sends none counts as.
pub fn pure_digest(iwds: &[(String, u32)]) -> u32 {
    let mut c = crate::crc::Crc32::default();
    for (name, sum) in iwds {
        c.update(name.as_bytes());
        c.update(&[0]);
        c.update(&sum.to_le_bytes());
    }
    c.finish().max(1)
}

/// Adds the files below `dir` whose name starts with `prefix` and ends with `suffix`, named from `dir` on (lowercase,
/// `/`). Every directory whose name folds to the same lowercase one is walked, as the file system of a platform that
/// ignores case would see one.
fn walk(dir: &Path, prefix: &str, suffix: &str, out: &mut std::collections::BTreeSet<String>) {
    fn rec(
        path: &Path,
        name: &str,
        prefix: &str,
        suffix: &str,
        out: &mut std::collections::BTreeSet<String>,
    ) {
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
                let d = format!("{n}/");
                if prefix.starts_with(&d) || d.starts_with(prefix) {
                    rec(&p, &d, prefix, suffix, out);
                }
            } else if n.starts_with(prefix) && n.ends_with(suffix) {
                out.insert(n);
            }
        }
    }
    rec(dir, "", prefix, suffix, out);
}

/// Case-insensitive path lookup below `dir` (the install may sit on a case-sensitive file system, where directories
/// that differ only in case can both exist: each is tried). Directory entries are not files.
fn find_file(dir: &Path, norm: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = norm.split('/').filter(|p| !p.is_empty()).collect();
    fn rec(cur: &Path, parts: &[&str]) -> Option<PathBuf> {
        let Some((part, rest)) = parts.split_first() else {
            return crate::fs::is_file(cur).then(|| cur.to_path_buf());
        };
        let direct = cur.join(part);
        if crate::fs::exists(&direct)
            && let Some(f) = rec(&direct, rest)
        {
            return Some(f);
        }
        crate::fs::read_dir(cur)
            .ok()?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().to_ascii_lowercase() == *part)
            .find_map(|e| rec(&e.path(), rest))
    }
    rec(dir, &parts)
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
                    None => {
                        tracing::warn!(iwd = %name, "invalid localized iwd name, skipped");
                        continue;
                    }
                },
                (None, None) => None,
            };
            // Basepath `main` accepts only `iw_*` for non-localized IWDs, plus the `jcod4x_` and `xbase_` names the
            // CoD4X loader exempts.
            if language.is_none()
                && game_dir.eq_ignore_ascii_case("main")
                && !["iw_", "jcod4x_", "xbase_"]
                    .iter()
                    .any(|p| lower.starts_with(p))
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
            tracing::error!("no iwd files found in main");
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "No IWD files found in /main",
            ));
        }
        found.sort_by_cached_key(|(n, lang)| iwd_sort_key(n, *lang));
        for (name, language) in found {
            let iwd_path = path.join(name);
            let iwd = Iwd::open(&iwd_path).inspect_err(
                |e| tracing::error!(iwd = %iwd_path.display(), error = %e, "cannot open iwd"),
            )?;
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
