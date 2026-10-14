// SPDX-License-Identifier: GPL-3.0-only
//! The native outputs: stderr, a log file per run (rotated), and a panic hook that logs the panic.

// This crate is where output is written.
#![allow(clippy::print_stderr)]

use crate::{BoxLayer, KEEP, TRUNCATION_MARK, redact};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing_subscriber::fmt;

/// A log file stops growing here (`developer 2` is chatty); the last line says so.
const MAX_FILE: u64 = 64 << 20;

/// Runs every event's text through [`redact`] on its way to `W` (the fmt layer writes one event per `write`).
struct Redacting<W>(W);

impl<W: Write> Write for Redacting<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        self.0.write_all(redact(&text).as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

struct Capped {
    file: File,
    written: u64,
}

/// One log file shared by every event, behind a lock, with a size cap.
#[derive(Clone)]
struct SharedFile(Arc<Mutex<Capped>>);

impl Write for SharedFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut c = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if c.written >= MAX_FILE {
            return Ok(buf.len());
        }
        c.written += buf.len() as u64;
        if c.written >= MAX_FILE {
            let note = format!(
                "\n{TRUNCATION_MARK} {} MiB: the rest is dropped]\n",
                MAX_FILE >> 20
            );
            let _ = c
                .file
                .write_all(buf)
                .and_then(|()| c.file.write_all(note.as_bytes()));
            return Ok(buf.len());
        }
        c.file.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn layers(
    app: &str,
    dir: Option<PathBuf>,
    stderr: bool,
    out: &mut Vec<BoxLayer>,
) -> Option<PathBuf> {
    if stderr {
        out.push(Box::new(
            fmt::layer()
                .with_ansi(false)
                .with_writer(|| Redacting(io::stderr())),
        ));
    }
    // A directory the environment names belongs to whoever named it (the harness): nothing in it is rotated away.
    let env_dir = std::env::var_os("COD4E_LOG_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from);
    let rotate = env_dir.is_none();
    let dir = env_dir.or(dir)?;
    match open(app, &dir, rotate) {
        Ok((mut file, path)) => {
            // A header, so a run that logged nothing still leaves a file saying whose it is.
            let _ = writeln!(file, "# {app} pid {} started", std::process::id());
            let shared = SharedFile(Arc::new(Mutex::new(Capped { file, written: 0 })));
            out.push(Box::new(
                fmt::layer()
                    .with_ansi(false)
                    .with_writer(move || Redacting(shared.clone())),
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

fn open(app: &str, dir: &Path, rotate: bool) -> io::Result<(File, PathBuf)> {
    fs::create_dir_all(dir)?;
    if rotate {
        rotate_logs(app, dir);
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let path = dir.join(format!("{app}-{secs:012}-{}.log", std::process::id()));
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    // A log can hold what the player typed: readable by the owner only.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    Ok((o.open(&path)?, path))
}

/// Whether `name` is a log file of `app`: `<app>-<12 digits>-...log` (so `harness` does not take `harness-stage`).
fn is_log_of(app: &str, name: &str) -> bool {
    let Some(rest) = name.strip_prefix(app).and_then(|r| r.strip_prefix('-')) else {
        return false;
    };
    let b = rest.as_bytes();
    name.ends_with(".log")
        && b.len() > 12
        && b[..12].iter().all(u8::is_ascii_digit)
        && b[12] == b'-'
}

/// Deletes the oldest log files of `app` so that, with the one about to be made, [`KEEP`] remain.
fn rotate_logs(app: &str, dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut old: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| is_log_of(app, n))
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
    fn rotation_keeps_the_newest_of_this_app_only() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..12 {
            File::create(d.path().join(format!("app-{i:012}-1.log"))).unwrap();
        }
        File::create(d.path().join("other-000000000000-1.log")).unwrap();
        File::create(d.path().join("app-stage-000000000000-1.log")).unwrap();
        let (_, path) = open("app", d.path(), true).unwrap();
        let mut names: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| is_log_of("app", n))
            .collect();
        names.sort();
        assert_eq!(names.len(), KEEP);
        assert!(names.contains(&path.file_name().unwrap().to_str().unwrap().to_owned()));
        assert!(!names.contains(&"app-000000000000-1.log".to_owned()));
        assert!(d.path().join("other-000000000000-1.log").exists());
        assert!(d.path().join("app-stage-000000000000-1.log").exists());
    }

    #[test]
    fn a_named_directory_is_not_rotated() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..12 {
            File::create(d.path().join(format!("app-{i:012}-1.log"))).unwrap();
        }
        open("app", d.path(), false).unwrap();
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 13);
    }

    #[cfg(unix)]
    #[test]
    fn the_log_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let (f, _) = open("app", d.path(), true).unwrap();
        assert_eq!(f.metadata().unwrap().permissions().mode() & 0o077, 0);
    }

    #[test]
    fn secrets_do_not_reach_the_writer() {
        let mut out = Vec::new();
        Redacting(&mut out)
            .write_all(b"WARN unknown command cmd=\"rcon hunter2 status\"\n")
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            !text.contains("hunter2") && text.contains("status"),
            "{text}"
        );
    }
}
