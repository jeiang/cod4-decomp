// SPDX-License-Identifier: GPL-3.0-or-later
//! Random-access byte sources. Object-safe so other backends (the browser's picked files and OPFS) can implement it.

use std::io;
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

/// A [`ReadAt`] over an open file (on the web, over the mounted source of that path).
pub struct FileSource {
    #[cfg(not(target_arch = "wasm32"))]
    file: std::fs::File,
    #[cfg(target_arch = "wasm32")]
    source: std::sync::Arc<dyn ReadAt>,
    len: u64,
}

impl FileSource {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { file, len })
    }

    #[cfg(target_arch = "wasm32")]
    pub fn open(path: &Path) -> io::Result<Self> {
        let source = crate::fs::File::open(path)?.source();
        Ok(Self {
            len: source.len(),
            source,
        })
    }
}

impl ReadAt for FileSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        #[cfg(target_arch = "wasm32")]
        {
            self.source.read_exact_at(offset, buf)
        }
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
        #[cfg(not(any(unix, windows, target_arch = "wasm32")))]
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
