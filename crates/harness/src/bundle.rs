// SPDX-License-Identifier: GPL-3.0-only
//! Writes the run bundle: a scrubbed zip of the run directory, capped at
//! 100 MB.
use crate::scrub::Scrubber;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;

pub const CAP: u64 = 100 * 1024 * 1024;
/// A single text file is cut to its head and tail beyond this size.
const TEXT_CAP: usize = 8 * 1024 * 1024;
/// Room left free under the cap for the central directory and the longer
/// DROPPED.txt a drop set adds, beyond the measured cost of the files it
/// leaves out.
const SLACK: u64 = 64 * 1024;

const VIDEOS: &[&str] = &["mp4", "mkv", "webm", "ivf", "obu", "avi"];
const IMAGES: &[&str] = &["png", "jpg", "jpeg", "bmp"];

/// What may be left out, in the order the classes are given up when the zip
/// is over its cap. Within a class the largest file goes first, so the
/// bundle keeps as many frames as fit instead of losing a whole class.
const DROP_ORDER: [&[&str]; 4] = [VIDEOS, IMAGES, &["trace.json"], &["csv"]];

const TEXT: [&str; 6] = ["log", "json", "csv", "txt", "cfg", "md"];

pub struct Written {
    pub path: PathBuf,
    pub size: u64,
    /// Bundle-relative names left out to fit the cap, in the order they were
    /// given up.
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

/// The stage a bundle-relative name belongs to: one directory under
/// `stages/` for a staged run, else the top-level directory that holds it.
fn stage_of(name: &str) -> String {
    let mut parts = name.split('/');
    match parts.next() {
        Some("stages") => parts
            .next()
            .map_or_else(String::new, |s| format!("stages/{s}")),
        Some(first) => first.to_owned(),
        None => String::new(),
    }
}

/// What each entry of `zip` costs it: its local header, its deflated data and
/// its central directory record. Deflate works per file, so an entry's cost
/// does not change when its neighbours go.
fn costs(zip: &Path) -> io::Result<HashMap<String, u64>> {
    let zerr = |e: zip::result::ZipError| io::Error::other(e);
    let mut a = zip::ZipArchive::new(File::open(zip)?).map_err(zerr)?;
    let mut out = HashMap::with_capacity(a.len());
    for i in 0..a.len() {
        let f = a.by_index_raw(i).map_err(zerr)?;
        let name = f.name().to_owned();
        // 30 bytes of local header and 46 of central directory record, and
        // the name twice.
        let cost = f.compressed_size() + 76 + 2 * name.len() as u64;
        out.insert(name, cost);
    }
    Ok(out)
}

/// One file that may be left out.
struct Droppable {
    name: String,
    /// Index into `DROP_ORDER`.
    class: usize,
    /// The stage directory it belongs to.
    stage: String,
    cost: u64,
    image: bool,
}

/// The order the droppable files of `files` are given up in. Classes go in
/// `DROP_ORDER`; within a class the largest goes first, so the most files fit
/// under the cap. The smallest image of each stage comes last of all, so a
/// stage keeps one screenshot while the cap allows it.
fn drop_plan(files: &[(String, PathBuf)], cost: &HashMap<String, u64>) -> Vec<(String, u64)> {
    let mut all: Vec<Droppable> = files
        .iter()
        .filter_map(|(name, path)| {
            let class = DROP_ORDER.iter().position(|c| matches_class(name, c))?;
            let cost = cost
                .get(name)
                .copied()
                .or_else(|| fs::metadata(path).ok().map(|m| m.len()))
                .unwrap_or(0);
            Some(Droppable {
                name: name.clone(),
                class,
                stage: stage_of(name),
                cost,
                image: matches_class(name, IMAGES),
            })
        })
        .collect();
    all.sort_by(|a, b| {
        a.class
            .cmp(&b.class)
            .then(b.cost.cmp(&a.cost))
            .then(a.name.cmp(&b.name))
    });
    // In this order the last file of a stage's class group is that stage's
    // smallest, which is the one to keep.
    let mut last: HashMap<(usize, &str), usize> = HashMap::new();
    for (i, d) in all.iter().enumerate() {
        if d.image {
            last.insert((d.class, d.stage.as_str()), i);
        }
    }
    let kept: HashSet<usize> = last.into_values().collect();
    let mut out = Vec::with_capacity(all.len());
    for (i, d) in all.iter().enumerate() {
        if !kept.contains(&i) {
            out.push((d.name.clone(), d.cost));
        }
    }
    for (i, d) in all.iter().enumerate() {
        if kept.contains(&i) {
            out.push((d.name.clone(), d.cost));
        }
    }
    out
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

fn discard(tmp: &Path) {
    match fs::remove_file(tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(path = %tmp.display(), "cannot remove partial bundle: {e}"),
    }
}

/// Zip `work` into `out`, scrubbing text and minidumps, dropping the least
/// valuable files until the result is within `cap`.
pub fn write(work: &Path, out: &Path, scrub: &Scrubber, cap: u64) -> io::Result<Written> {
    let mut files = Vec::new();
    walk(work, work, &mut files)?;
    let tmp = out.with_extension("zip.partial");
    let mut dropped = Vec::new();
    // The first pass writes every file, which measures what each one costs in
    // the zip: deflate works per file, so what leaving one out saves is exact.
    let mut size = write_once(&files, &dropped, scrub, &tmp)?;
    if size > cap {
        let plan = drop_plan(&files, &costs(&tmp)?);
        if plan.is_empty() {
            discard(&tmp);
            return Err(io::Error::other(format!(
                "bundle is {size} bytes and nothing optional can be left out; cap is {cap}"
            )));
        }
        let mut next = 0;
        loop {
            // Give up files until the measured size minus their cost, plus
            // room for the central directory and DROPPED.txt, fits.
            let start = next;
            let mut estimate = size;
            while next < plan.len() && (next == start || estimate.saturating_add(SLACK) > cap) {
                let (name, cost) = &plan[next];
                estimate = estimate
                    .saturating_sub(*cost)
                    .saturating_add(name.len() as u64 + 1);
                dropped.push(name.clone());
                next += 1;
            }
            if next > start {
                let gone: HashSet<&str> = plan[..next].iter().map(|(n, _)| n.as_str()).collect();
                files.retain(|(n, _)| !gone.contains(n.as_str()));
            }
            size = write_once(&files, &dropped, scrub, &tmp)?;
            if size <= cap {
                fs::rename(&tmp, out)?;
                return Ok(Written {
                    path: out.to_owned(),
                    size,
                    dropped,
                });
            }
            if next >= plan.len() {
                // Everything optional is gone and it still does not fit.
                discard(&tmp);
                return Err(io::Error::other(format!(
                    "bundle is {size} bytes with every optional file left out; cap is {cap}"
                )));
            }
        }
    }
    fs::rename(&tmp, out)?;
    Ok(Written {
        path: out.to_owned(),
        size,
        dropped,
    })
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

    /// Bytes deflate cannot shrink, so a file costs about its own size.
    fn noise(n: usize) -> Vec<u8> {
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    fn images(n: &[String]) -> Vec<&String> {
        n.iter().filter(|n| n.ends_with(".png")).collect()
    }

    #[test]
    fn scrubs_and_drops_in_order_until_it_fits() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("w");
        fs::create_dir_all(work.join("stages/01")).unwrap();
        fs::write(work.join("manifest.json"), r#"{"p":"/home/bob/x"}"#).unwrap();
        fs::write(work.join("stages/01/stdout.log"), "ok\n").unwrap();
        fs::write(work.join("stages/01/flight.mp4"), noise(400_000)).unwrap();
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
        assert!(!dir.path().join("c.zip.partial").exists());
    }

    #[test]
    fn a_cap_between_every_image_and_none_keeps_the_ones_that_fit() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("w");
        fs::create_dir_all(work.join("stages/01-a")).unwrap();
        fs::create_dir_all(work.join("stages/02-b")).unwrap();
        fs::write(work.join("summary.json"), "{}").unwrap();
        fs::write(work.join("stages/01-a/big.png"), noise(300 * 1024)).unwrap();
        fs::write(work.join("stages/01-a/small.png"), noise(2 * 1024)).unwrap();
        fs::write(work.join("stages/02-b/mid.png"), noise(100 * 1024)).unwrap();
        let scrub = Scrubber::new(None, None, None);

        // Room for the small and the mid image, not the big one.
        let cap = 200 * 1024;
        let w = write(&work, &dir.path().join("a.zip"), &scrub, cap).unwrap();
        assert!(w.size <= cap);
        assert_eq!(w.dropped, ["stages/01-a/big.png"], "the largest goes first");
        let n = names(&w.path);
        let kept = images(&n);
        assert_eq!(
            kept.len(),
            2,
            "a cap that fits some images keeps some: {kept:?}"
        );
        assert!(n.contains(&"stages/01-a/small.png".to_owned()));
        assert!(n.contains(&"stages/02-b/mid.png".to_owned()));
    }

    #[test]
    fn the_last_image_of_each_stage_goes_only_after_every_other_image() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("w");
        for (stage, count, kb) in [("01-a", 3, 500), ("02-b", 2, 800)] {
            fs::create_dir_all(work.join("stages").join(stage)).unwrap();
            for i in 0..count {
                fs::write(
                    work.join("stages").join(stage).join(format!("{i}.png")),
                    noise(kb * 1024),
                )
                .unwrap();
            }
        }
        let scrub = Scrubber::new(None, None, None);

        // 3.1 MB of images; this cap fits one of each stage but not three of
        // them, so stage 02-b's images (the large ones) go before 01-a loses
        // its last screenshot.
        let cap = 1_600_000;
        let w = write(&work, &dir.path().join("a.zip"), &scrub, cap).unwrap();
        assert!(w.size <= cap, "{} <= {cap}", w.size);
        assert_eq!(
            w.dropped,
            [
                "stages/02-b/0.png",
                "stages/01-a/0.png",
                "stages/01-a/1.png"
            ],
            "largest first, and one image of each stage held back"
        );
        let n = names(&w.path);
        let kept = images(&n);
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert!(kept.iter().any(|k| k.starts_with("stages/01-a/")));
        assert!(kept.iter().any(|k| k.starts_with("stages/02-b/")));
    }
}
