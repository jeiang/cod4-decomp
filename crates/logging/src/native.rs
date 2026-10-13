// SPDX-License-Identifier: GPL-3.0-only
//! The native outputs: stderr, a log file per run (rotated), and a panic hook that logs the panic.

// This crate is where output is written.
#![allow(clippy::print_stderr)]

use crate::{BoxLayer, KEEP};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing_subscriber::fmt;

pub fn layers(
    app: &str,
    dir: Option<PathBuf>,
    stderr: bool,
    out: &mut Vec<BoxLayer>,
) -> Option<PathBuf> {
    if stderr {
        out.push(Box::new(
            fmt::layer().with_ansi(false).with_writer(std::io::stderr),
        ));
    }
    let dir = std::env::var_os("COD4E_LOG_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or(dir)?;
    match open(app, &dir) {
        Ok((file, path)) => {
            out.push(Box::new(
                fmt::layer().with_ansi(false).with_writer(Mutex::new(file)),
            ));
            Some(path)
        }
        Err(e) => {
            // The subscriber is not up yet, so this is the one place a plain print is the only way out.
            eprintln!("cannot write a log file in {}: {e}", dir.display());
            None
        }
    }
}

fn open(app: &str, dir: &Path) -> std::io::Result<(File, PathBuf)> {
    fs::create_dir_all(dir)?;
    rotate(app, dir);
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let path = dir.join(format!("{app}-{secs:012}-{}.log", std::process::id()));
    Ok((File::create(&path)?, path))
}

/// Deletes the oldest `<app>-*.log` files so that, with the one about to be made, [`KEEP`] remain.
fn rotate(app: &str, dir: &Path) {
    let prefix = format!("{app}-");
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut old: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".log"))
        })
        .collect();
    old.sort();
    let excess = (old.len() + 1).saturating_sub(KEEP);
    for p in old.into_iter().take(excess) {
        let _ = fs::remove_file(p);
    }
}

/// A panic goes to the log (with its location) before the previous hook runs.
pub fn log_panics() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(panic = %info, "panic");
        prev(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_keeps_the_newest() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..12 {
            File::create(d.path().join(format!("app-{i:012}-1.log"))).unwrap();
        }
        File::create(d.path().join("other-000000000000-1.log")).unwrap();
        let (_, path) = open("app", d.path()).unwrap();
        let mut names: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.starts_with("app-"))
            .collect();
        names.sort();
        assert_eq!(names.len(), KEEP);
        assert!(names.contains(&path.file_name().unwrap().to_str().unwrap().to_owned()));
        assert!(!names.contains(&"app-000000000000-1.log".to_owned()));
        assert!(d.path().join("other-000000000000-1.log").exists());
    }
}
