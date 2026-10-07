// SPDX-License-Identifier: GPL-3.0-or-later
//! Random-access byte sources. Object-safe so other backends (browser OPFS,
//! `File`) can implement it later.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// A fixed-length byte source supporting ranged reads.
pub trait ReadAt: Send + Sync {
    /// Total length in bytes.
    fn len(&self) -> u64;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Fill `buf` from `offset`. Fails with `UnexpectedEof` if the range
    /// extends past the end.
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
}

/// A [`ReadAt`] over an open file.
pub struct FileSource {
    file: File,
    len: u64,
}

impl FileSource {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { file, len })
    }
}

impl ReadAt for FileSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::FileExt::read_exact_at(&self.file, buf, offset)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            let mut done = 0;
            while done < buf.len() {
                match self
                    .file
                    .seek_read(&mut buf[done..], offset + done as u64)?
                {
                    0 => return Err(io::ErrorKind::UnexpectedEof.into()),
                    n => done += n,
                }
            }
            Ok(())
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (&self.file, offset, buf);
            Err(io::ErrorKind::Unsupported.into())
        }
    }
}

impl ReadAt for Vec<u8> {
    fn len(&self) -> u64 {
        self.as_slice().len() as u64
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| io::ErrorKind::UnexpectedEof)?;
        let end = start
            .checked_add(buf.len())
            .filter(|&e| e <= self.as_slice().len())
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        buf.copy_from_slice(&self[start..end]);
        Ok(())
    }
}

/// A sequential [`Read`] over a [`ReadAt`], for [`Zone::open`](crate::zone::Zone::open) on sources that are not
/// `std::fs::File`s. Reads in `CHUNK`-sized ranges, so a source with an expensive call (a browser `File`) is
/// asked for megabytes at a time.
pub struct SourceReader<S> {
    source: S,
    pos: u64,
    buf: Vec<u8>,
    at: usize,
}

impl<S: std::ops::Deref<Target: ReadAt>> SourceReader<S> {
    const CHUNK: usize = 1 << 20;

    pub fn new(source: S) -> Self {
        Self {
            source,
            pos: 0,
            buf: Vec::new(),
            at: 0,
        }
    }
}

impl<S: std::ops::Deref<Target: ReadAt>> Read for SourceReader<S> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.at == self.buf.len() {
            let n = (self.source.len() - self.pos).min(Self::CHUNK as u64) as usize;
            self.buf.resize(n, 0);
            self.source.read_exact_at(self.pos, &mut self.buf)?;
            self.pos += n as u64;
            self.at = 0;
        }
        let n = out.len().min(self.buf.len() - self.at);
        out[..n].copy_from_slice(&self.buf[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}
