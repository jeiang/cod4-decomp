// SPDX-License-Identifier: GPL-3.0-or-later
//! Virtual file system over the original install: IWDs and loose directories
//! resolved in the original engine's search order (first hit wins).
//!
//! Only stock behavior is built: `fs_basepath == fs_homepath`, no `fs_cdpath`,
//! no `fs_game`. [`Builder::add_game_dir`] is the single extension point for
//! mod directories.

mod iwd;
mod source;

pub use iwd::{Entry, Iwd};
pub use source::{FileSource, ReadAt, SourceReader};

use std::collections::HashMap;
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
            Hit::File(path) => std::fs::read(path),
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
        let mut b = Builder::new(root);
        b.add_game_dir("players")?;
        b.add_game_dir("main_shared")?;
        b.add_game_dir("main")?;
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

    /// Read `name` through the search path; `Ok(None)` if absent.
    pub fn read(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        self.locate(name).map(|h| h.read()).transpose()
    }
}

/// Case-insensitive path lookup below `dir` (the install may sit on a
/// case-sensitive file system). Directory entries are not files.
fn find_file(dir: &Path, norm: &str) -> Option<PathBuf> {
    let mut cur = dir.to_path_buf();
    for part in norm.split('/').filter(|p| !p.is_empty()) {
        let direct = cur.join(part);
        cur = if direct.exists() {
            direct
        } else {
            std::fs::read_dir(&cur)
                .ok()?
                .flatten()
                .find(|e| e.file_name().to_string_lossy().to_ascii_lowercase() == part)?
                .path()
        };
    }
    cur.is_file().then_some(cur)
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
        if !path.is_dir() {
            return Ok(());
        }
        self.push(Node {
            kind: NodeKind::Dir(path.clone()),
            language: subfolder_lang,
        });

        let mut names = vec![];
        for e in std::fs::read_dir(&path)? {
            names.push(e?.file_name().to_string_lossy().into_owned());
        }
        let found = select_iwds(game_dir, subfolder_lang, names)?;
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

    /// Like [`Builder::add_game_dir`] for one directory whose IWDs are not on a file system (a browser's picked
    /// folder): `iwds` are the `.iwd` members of `<game_dir>[/<language>]`, by file name. `Dir` nodes are not built.
    pub fn add_iwds(
        &mut self,
        game_dir: &str,
        subfolder_lang: Option<usize>,
        iwds: Vec<(String, Box<dyn ReadAt>)>,
    ) -> io::Result<()> {
        let mut sources: HashMap<String, Box<dyn ReadAt>> = iwds.into_iter().collect();
        let names = sources.keys().cloned().collect();
        for (name, language) in select_iwds(game_dir, subfolder_lang, names)? {
            let source = sources.remove(&name).expect("selected from the keys");
            self.push(Node {
                kind: NodeKind::Iwd {
                    path: PathBuf::from(&name),
                    iwd: Iwd::new(source)?,
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

/// The IWDs the original engine loads from one directory listing, in load order (ascending priority).
fn select_iwds(
    game_dir: &str,
    subfolder_lang: Option<usize>,
    names: Vec<String>,
) -> io::Result<Vec<(String, Option<usize>)>> {
    let mut found: Vec<(String, Option<usize>)> = vec![];
    for name in names {
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
        if language.is_none() && game_dir.eq_ignore_ascii_case("main") && !lower.starts_with("iw_")
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
    Ok(found)
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
