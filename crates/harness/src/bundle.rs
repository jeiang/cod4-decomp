// SPDX-License-Identifier: GPL-3.0-only
//! Writes the run bundle: a scrubbed zip of the run directory, capped at
//! 100 MB.
use crate::scrub::Scrubber;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;

pub const CAP: u64 = 100 * 1024 * 1024;
/// A single text file is cut to its head and tail beyond this size.
const TEXT_CAP: usize = 8 * 1024 * 1024;

/// What may be dropped, first to go first, when the zip exceeds the cap.
const DROP_ORDER: [&[&str]; 4] = [
    &["mp4", "mkv", "webm", "ivf", "obu", "avi"],
    &["png", "jpg", "jpeg", "bmp"],
    &["trace.json"],
    &["csv"],
];

const TEXT: [&str; 6] = ["log", "json", "csv", "txt", "cfg", "md"];

pub struct Written {
    pub path: PathBuf,
    pub size: u64,
    /// Bundle-relative names left out to fit the cap.
    pub dropped: Vec<String>,
}

fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        if p.is_dir() {
            walk(&p, base, out)?;
        } else {
            let rel = p.strip_prefix(base).unwrap_or(&p);
            let rel: Vec<_> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect();
            out.push((rel.join("/"), p));
        }
    }
    Ok(())
}

fn ext(name: &str) -> &str {
    name.rsplit_once('.').map_or("", |(_, e)| e)
}

fn matches_class(name: &str, class: &[&str]) -> bool {
    class
        .iter()
        .any(|c| name.ends_with(&format!(".{c}")) || name.rsplit('/').next() == Some(c))
}

fn load(path: &Path, name: &str, scrub: &Scrubber) -> io::Result<Vec<u8>> {
    let mut data = fs::read(path)?;
    let e = ext(name).to_ascii_lowercase();
    if TEXT.contains(&e.as_str()) {
        if data.len() > TEXT_CAP {
            let (head, tail) = (TEXT_CAP / 2, TEXT_CAP / 2);
            let mut cut = data[..head].to_vec();
            cut.extend_from_slice(
                format!(
                    "\n[... {} bytes cut by the bundle writer ...]\n",
                    data.len() - head - tail
                )
                .as_bytes(),
            );
            cut.extend_from_slice(&data[data.len() - tail..]);
            data = cut;
        }
        Ok(scrub.text(&String::from_utf8_lossy(&data)).into_bytes())
    } else if e == "dmp" {
        scrub.bytes(&mut data);
        Ok(data)
    } else {
        Ok(data)
    }
}

fn write_once(
    files: &[(String, PathBuf)],
    dropped: &[String],
    scrub: &Scrubber,
    out: &Path,
) -> io::Result<u64> {
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(false);
    let mut z = zip::ZipWriter::new(File::create(out)?);
    let zerr = |e: zip::result::ZipError| io::Error::other(e);
    for (name, path) in files {
        z.start_file(name, opts).map_err(zerr)?;
        z.write_all(&load(path, name, scrub)?)?;
    }
    if !dropped.is_empty() {
        z.start_file("DROPPED.txt", opts).map_err(zerr)?;
        writeln!(z, "Left out to keep the bundle under the size cap:")?;
        for d in dropped {
            writeln!(z, "{d}")?;
        }
    }
    z.finish().map_err(zerr)?.sync_all()?;
    Ok(fs::metadata(out)?.len())
}

/// Zip `work` into `out`, scrubbing text and minidumps, dropping the least
/// valuable files until the result is within `cap`.
pub fn write(work: &Path, out: &Path, scrub: &Scrubber, cap: u64) -> io::Result<Written> {
    let mut files = Vec::new();
    walk(work, work, &mut files)?;
    let tmp = out.with_extension("zip.partial");
    let mut dropped = Vec::new();
    let mut classes = DROP_ORDER.iter();
    loop {
        let size = write_once(&files, &dropped, scrub, &tmp)?;
        if size <= cap {
            fs::rename(&tmp, out)?;
            return Ok(Written {
                path: out.to_owned(),
                size,
                dropped,
            });
        }
        let Some(class) = classes.next() else {
            let _ = fs::remove_file(&tmp);
            return Err(io::Error::other(format!(
                "bundle is {size} bytes after dropping everything optional; cap is {cap}"
            )));
        };
        files.retain(|(name, _)| {
            let drop = matches_class(name, class);
            if drop {
                dropped.push(name.clone());
            }
            !drop
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn names(zip: &Path) -> Vec<String> {
        let mut a = zip::ZipArchive::new(File::open(zip).unwrap()).unwrap();
        (0..a.len())
            .map(|i| a.by_index(i).unwrap().name().to_owned())
            .collect()
    }

    #[test]
    fn scrubs_and_drops_in_order_until_it_fits() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("w");
        fs::create_dir_all(work.join("stages/01")).unwrap();
        fs::write(work.join("manifest.json"), r#"{"p":"/home/bob/x"}"#).unwrap();
        fs::write(work.join("stages/01/stdout.log"), "ok\n").unwrap();
        let mut noise = 0x2545_F491_4F6C_DD1Du64;
        let video: Vec<u8> = (0..400_000)
            .map(|_| {
                noise ^= noise << 13;
                noise ^= noise >> 7;
                noise ^= noise << 17;
                noise as u8
            })
            .collect();
        fs::write(work.join("stages/01/flight.mp4"), &video).unwrap();
        fs::write(work.join("stages/01/shot.png"), b"png").unwrap();
        let scrub = Scrubber::new(Some("/home/bob"), Some("bob"), None);

        let big = write(&work, &dir.path().join("a.zip"), &scrub, CAP).unwrap();
        assert!(big.dropped.is_empty());
        assert!(names(&big.path).contains(&"stages/01/flight.mp4".to_owned()));
        let mut m = String::new();
        zip::ZipArchive::new(File::open(&big.path).unwrap())
            .unwrap()
            .by_name("manifest.json")
            .unwrap()
            .read_to_string(&mut m)
            .unwrap();
        assert_eq!(m, r#"{"p":"<home>/x"}"#);

        let small = write(&work, &dir.path().join("b.zip"), &scrub, 100_000).unwrap();
        assert_eq!(small.dropped, ["stages/01/flight.mp4"]);
        let n = names(&small.path);
        assert!(
            n.contains(&"stages/01/shot.png".to_owned()) && n.contains(&"DROPPED.txt".to_owned())
        );
        assert!(small.size <= 100_000);

        assert!(write(&work, &dir.path().join("c.zip"), &scrub, 10).is_err());
        assert!(!dir.path().join("c.zip").exists());
    }
}
