// SPDX-License-Identifier: GPL-3.0-or-later
//! The flythrough video recorder (harness) is native only: it needs a software AV1 encoder and a file to write.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct VideoStats {
    pub path: PathBuf,
    pub frames: u64,
    pub dropped: u64,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

pub struct Recorder;

impl Recorder {
    pub fn start(
        _path: &Path,
        _src: (u32, u32),
        _max_height: u32,
        _fps: u32,
    ) -> io::Result<Recorder> {
        Err(io::ErrorKind::Unsupported.into())
    }

    pub fn capture(&mut self, _: &wgpu::Device, _: &wgpu::Queue, _: &wgpu::Texture, _: Duration) {}

    pub fn finish(self, _: &wgpu::Device) -> io::Result<VideoStats> {
        Err(io::ErrorKind::Unsupported.into())
    }
}
