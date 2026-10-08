// SPDX-License-Identifier: GPL-3.0-or-later
//! The wasm32 side of [`crate::fs`]: an index of mounted [`ReadAt`] sources.

use crate::vfs::ReadAt;
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Where the mounted install appears: `install` is `WEB_ROOT`, `install/main/iw_00.iwd` a file.
pub const WEB_ROOT: &str = "/install";

#[derive(Default)]
struct Mount {
    files: HashMap<String, Arc<dyn ReadAt>>,
    /// Directory path (no trailing slash) to its entry names, files and subdirectories.
    dirs: HashMap<String, Vec<String>>,
}

static MOUNT: RwLock<Option<Mount>> = RwLock::new(None);

/// Replaces the mounted install. `files` pairs an install-relative `/`-separated path (`main/iw_00.iwd`) with its
/// source; directories are implied by the paths.
pub fn mount(files: impl IntoIterator<Item = (String, Arc<dyn ReadAt>)>) {
    let mut m = Mount::default();
    m.dirs.entry(WEB_ROOT.to_owned()).or_default();
    for (rel, src) in files {
        let mut dir = WEB_ROOT.to_owned();
        let mut parts = rel.split('/').filter(|p| !p.is_empty()).peekable();
        while let Some(part) = parts.next() {
            let entries = m.dirs.entry(dir.clone()).or_default();
            if !entries.iter().any(|e| e == part) {
                entries.push(part.to_owned());
            }
            dir = format!("{dir}/{part}");
            if parts.peek().is_some() {
                m.dirs.entry(dir.clone()).or_default();
            }
        }
        m.files.insert(dir, src);
    }
    *MOUNT.write().unwrap() = Some(m);
}

/// The path as the mount keys it: `/`-separated, no trailing slash.
fn key(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/');
    if s.starts_with('/') {
        s.to_owned()
    } else {
        format!("/{s}")
    }
}

fn with<T>(f: impl FnOnce(&Mount) -> Option<T>) -> Option<T> {
    MOUNT.read().unwrap().as_ref().and_then(f)
}

fn not_found(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{}: not found", path.display()),
    )
}

pub fn exists(path: &Path) -> bool {
    is_file(path) || is_dir(path)
}

pub fn is_dir(path: &Path) -> bool {
    let k = key(path);
    with(|m| m.dirs.contains_key(&k).then_some(())).is_some()
}

pub fn is_file(path: &Path) -> bool {
    let k = key(path);
    with(|m| m.files.contains_key(&k).then_some(())).is_some()
}

/// A mounted file, read on demand.
pub struct File {
    src: Arc<dyn ReadAt>,
    pos: u64,
}

impl File {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let k = key(path);
        with(|m| m.files.get(&k).cloned())
            .map(|src| Self { src, pos: 0 })
            .ok_or_else(|| not_found(path))
    }

    pub fn metadata(&self) -> io::Result<Metadata> {
        Ok(Metadata(self.src.len()))
    }

    /// The shared source behind the file, for readers that want ranged reads.
    pub fn source(&self) -> Arc<dyn ReadAt> {
        self.src.clone()
    }
}

pub struct Metadata(u64);

impl Metadata {
    pub fn len(&self) -> u64 {
        self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
}

impl Read for File {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = (self.src.len().saturating_sub(self.pos)).min(buf.len() as u64) as usize;
        self.src.read_exact_at(self.pos, &mut buf[..n])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for File {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let base = match to {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::End(d) => i128::from(self.src.len()) + i128::from(d),
            SeekFrom::Current(d) => i128::from(self.pos) + i128::from(d),
        };
        self.pos = u64::try_from(base)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        Ok(self.pos)
    }
}

pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    let mut out = vec![0; f.src.len() as usize];
    f.src.read_exact_at(0, &mut out)?;
    f.pos = out.len() as u64;
    Ok(out)
}

pub fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
    String::from_utf8(read(path)?).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "stream did not contain valid UTF-8",
        )
    })
}

pub struct ReadDir {
    dir: PathBuf,
    names: std::vec::IntoIter<String>,
}

pub struct DirEntry {
    dir: PathBuf,
    name: String,
}

impl DirEntry {
    pub fn file_name(&self) -> OsString {
        OsString::from(&self.name)
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(&self.name)
    }
}

impl Iterator for ReadDir {
    type Item = io::Result<DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        self.names.next().map(|name| {
            Ok(DirEntry {
                dir: self.dir.clone(),
                name,
            })
        })
    }
}

pub fn read_dir(path: impl AsRef<Path>) -> io::Result<ReadDir> {
    let path = path.as_ref();
    let k = key(path);
    let names = with(|m| m.dirs.get(&k).cloned()).ok_or_else(|| not_found(path))?;
    Ok(ReadDir {
        dir: path.to_owned(),
        names: names.into_iter(),
    })
}
