// SPDX-License-Identifier: GPL-3.0-only
//! Stage 1: load every MP zone, then scan the VFS.
//!
//! Each zone is opened (header and asset list) and decoded as far as the
//! decoders go. A zone that stops at an asset type with no decoder yet counts
//! as `partial`: it is inflated to the end instead, which still verifies the
//! container. Anything else that goes wrong fails the stage. As decoders land
//! in `assets`, partial zones turn into decoded ones with no change here.
use crate::perf::{Percentiles, process_rss};
use crate::stage::{StageCtx, StageReport, Status};
use assets::vfs::{NodeKind, Vfs};
use assets::zone::{Asset, KeepAll, Zone, ZoneError};
use sim::skel::anim::{self, Accum};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::{info, info_span, warn};

const LANGUAGE: &str = "english";

/// MP zones: map zones and their load screens (`mp_*`) and the shared
/// `*_mp` zones, including localized ones.
pub fn is_mp_zone(stem: &str) -> bool {
    stem.starts_with("mp_") || stem.ends_with("_mp")
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

pub fn mp_zones(install: &Path) -> io::Result<Vec<PathBuf>> {
    let mut dir = install.join("zone");
    let lang = std::fs::read_dir(&dir)?
        .flatten()
        .find(|e| {
            e.file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(LANGUAGE)
        })
        .map(|e| e.path());
    dir = lang.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "zone/english missing"))?;
    let mut zones: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ff"))
                && p.file_stem()
                    .is_some_and(|s| is_mp_zone(&s.to_string_lossy().to_ascii_lowercase()))
        })
        .collect();
    zones.sort();
    Ok(zones)
}

enum ZoneOutcome {
    Decoded,
    Partial(&'static str),
    Failed(String),
}

struct ZoneRow {
    name: String,
    open_ms: f64,
    decode_ms: f64,
    inflated: u64,
    assets: usize,
    anims: usize,
    anim_samples: usize,
    takeoff_anims: usize,
    outcome: ZoneOutcome,
}

fn load_zone(path: &Path) -> ZoneRow {
    let name = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let mut row = ZoneRow {
        name,
        open_ms: 0.0,
        decode_ms: 0.0,
        inflated: 0,
        assets: 0,
        anims: 0,
        anim_samples: 0,
        takeoff_anims: 0,
        outcome: ZoneOutcome::Decoded,
    };
    let open =
        || -> Result<Zone<'static>, ZoneError> { Zone::open(BufReader::new(File::open(path)?)) };
    let t = Instant::now();
    let zone = match open() {
        Ok(z) => z,
        Err(e) => {
            row.outcome = ZoneOutcome::Failed(format!("open: {e}"));
            return row;
        }
    };
    row.open_ms = ms(t);
    row.assets = zone.asset_types().len();
    let t = Instant::now();
    let mut animation_error = None;
    match zone.decode(&KeepAll, |asset| {
        if let Asset::XAnimParts(a) = asset {
            row.anims += 1;
            row.takeoff_anims += usize::from(a.name.as_deref() == Some("pb_runjump_takeoff"));
            let binding: [u8; 255] = std::array::from_fn(|i| i as u8);
            // Every integer frame and midpoint, including the held non-looping endpoint.
            for half_frame in 0..=u32::from(a.num_frames).max(1) * 2 {
                let time = half_frame as f32 / (f32::from(a.num_frames).max(1.0) * 2.0);
                let mut pose = [Accum::ZERO; 255];
                let in_range = anim::accumulate(&a, &binding, time, 1.0, &mut pose);
                row.anim_samples += 1;
                let finite = pose.iter().all(|bone| {
                    let (q, t) = bone.finish();
                    bone.quat.iter().chain(&bone.trans).all(|v| v.is_finite())
                        && q.iter()
                            .flatten()
                            .chain(t.iter().flatten())
                            .all(|v| v.is_finite())
                });
                if !in_range || !finite || a.names.len() > binding.len() {
                    animation_error = Some(format!(
                        "animation {} at {time}: in_range={in_range}, finite={finite}",
                        a.name.as_deref().unwrap_or("?")
                    ));
                    break;
                }
            }
        }
    }) {
        Ok(stats) => row.inflated = stats.consumed,
        Err(ZoneError::Unsupported(ty)) => {
            row.outcome = ZoneOutcome::Partial(ty.name());
            match open().and_then(Zone::inflate_rest) {
                Ok(n) => row.inflated = n,
                Err(e) => row.outcome = ZoneOutcome::Failed(format!("inflate: {e}")),
            }
        }
        Err(e) => row.outcome = ZoneOutcome::Failed(format!("decode: {e}")),
    }
    if let Some(error) = animation_error {
        row.outcome = ZoneOutcome::Failed(error);
    }
    row.decode_ms = ms(t);
    row
}

struct VfsScan {
    open_ms: f64,
    iwds: usize,
    entries: usize,
    bytes: u64,
    read_ms: f64,
    errors: usize,
    unresolved: usize,
}

fn scan_vfs(install: &Path) -> io::Result<VfsScan> {
    let t = Instant::now();
    let vfs = Vfs::open_stock(install, 0)?;
    let mut scan = VfsScan {
        open_ms: ms(t),
        iwds: 0,
        entries: 0,
        bytes: 0,
        read_ms: 0.0,
        errors: 0,
        unresolved: 0,
    };
    let t = Instant::now();
    for node in vfs.nodes() {
        let NodeKind::Iwd { iwd, path } = &node.kind else {
            continue;
        };
        scan.iwds += 1;
        for entry in iwd.entries() {
            scan.entries += 1;
            // The first hit in search order must exist for every entry.
            if vfs.locate(&entry.name).is_none() {
                scan.unresolved += 1;
            }
            match iwd.read(entry) {
                Ok(data) => scan.bytes += data.len() as u64,
                Err(e) => {
                    scan.errors += 1;
                    warn!("{}:{}: {e}", path.display(), entry.name);
                }
            }
        }
    }
    scan.read_ms = ms(t);
    Ok(scan)
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let name = "asset-load";
    let Some(install) = &ctx.install else {
        return Ok(StageReport::new(name, Status::Failed).with_reason("install not found"));
    };
    let zones = mp_zones(install)?;
    if zones.is_empty() {
        return Ok(StageReport::new(name, Status::Failed)
            .with_reason(format!("no MP zones in {}", install.join("zone").display())));
    }
    let mut csv = io::BufWriter::new(File::create(ctx.dir.join("zones.csv"))?);
    writeln!(
        csv,
        "zone,open_ms,decode_ms,inflated_bytes,assets,status,detail"
    )?;
    let (mut open_ms, mut decode_ms) = (Vec::new(), Vec::new());
    let (mut decoded, mut partial, mut failed) = (0usize, 0usize, 0usize);
    let (mut inflated, mut assets_listed) = (0u64, 0usize);
    let (mut anims, mut anim_samples) = (0, 0);
    let mut takeoff_anims = 0;
    let mut stopped_at = BTreeSet::new();
    let mut failures = Vec::new();
    for path in &zones {
        let row = {
            let _s = info_span!("zone", zone = %path.display()).entered();
            load_zone(path)
        };
        let (status, detail) = match &row.outcome {
            ZoneOutcome::Decoded => {
                decoded += 1;
                ("decoded", String::new())
            }
            ZoneOutcome::Partial(ty) => {
                partial += 1;
                stopped_at.insert(*ty);
                ("partial", format!("no decoder for {ty}"))
            }
            ZoneOutcome::Failed(e) => {
                failed += 1;
                warn!("zone {}: {e}", row.name);
                failures.push(format!("{}: {e}", row.name));
                ("failed", e.clone())
            }
        };
        info!(
            "zone {} {status}: {} assets, {} bytes, open {:.1} ms, decode {:.1} ms",
            row.name, row.assets, row.inflated, row.open_ms, row.decode_ms
        );
        writeln!(
            csv,
            "{},{:.3},{:.3},{},{},{status},\"{}\"",
            row.name,
            row.open_ms,
            row.decode_ms,
            row.inflated,
            row.assets,
            detail.replace('"', "'")
        )?;
        open_ms.push(row.open_ms);
        decode_ms.push(row.decode_ms);
        inflated += row.inflated;
        assets_listed += row.assets;
        anims += row.anims;
        anim_samples += row.anim_samples;
        takeoff_anims += row.takeoff_anims;
    }
    csv.flush()?;

    let mut report = StageReport::new(name, Status::Passed);
    report.files.push("zones.csv".into());
    let scan = {
        let _s = info_span!("vfs-scan").entered();
        scan_vfs(install)
    };
    match &scan {
        Ok(s) => {
            info!(
                "vfs: {} iwds, {} entries, {} bytes read in {:.0} ms, {} read errors, {} unresolved",
                s.iwds, s.entries, s.bytes, s.read_ms, s.errors, s.unresolved
            );
            for (k, v) in [
                ("vfs.open_ms", s.open_ms),
                ("vfs.iwds", s.iwds as f64),
                ("vfs.entries", s.entries as f64),
                ("vfs.bytes_read", s.bytes as f64),
                ("vfs.read_ms", s.read_ms),
                ("vfs.read_errors", s.errors as f64),
                ("vfs.unresolved", s.unresolved as f64),
            ] {
                report.metrics.insert(k.into(), v);
            }
            if s.errors > 0 || s.unresolved > 0 {
                failures.push(format!(
                    "vfs: {} read errors, {} unresolved names",
                    s.errors, s.unresolved
                ));
            }
        }
        Err(e) => failures.push(format!("vfs: {e}")),
    }
    for (k, v) in [
        ("zones.total", zones.len()),
        ("zones.decoded", decoded),
        ("zones.partial", partial),
        ("zones.failed", failed),
        ("zones.assets_listed", assets_listed),
        ("xanim.decoded", anims),
        ("xanim.samples", anim_samples),
        ("xanim.takeoff_anims", takeoff_anims),
    ] {
        report.metrics.insert(k.into(), v as f64);
    }
    report
        .metrics
        .insert("zones.inflated_bytes".into(), inflated as f64);
    if let Some(rss) = process_rss() {
        report
            .metrics
            .insert("process.rss_bytes".into(), rss as f64);
    }
    report.series.extend(
        [("zone.open_ms", &open_ms), ("zone.decode_ms", &decode_ms)]
            .into_iter()
            .filter_map(|(k, v)| Some((k.to_owned(), Percentiles::from_samples(v)?))),
    );
    report.notes.push(format!(
        "{decoded} of {} MP zones decode fully; {partial} stop at an asset type with no decoder yet{}",
        zones.len(),
        if stopped_at.is_empty() {
            String::new()
        } else {
            format!(" ({})", stopped_at.into_iter().collect::<Vec<_>>().join(", "))
        }
    ));
    if anims == 0 || anim_samples == 0 || takeoff_anims == 0 {
        failures.push("animation sweep did not cover pb_runjump_takeoff".into());
    }
    if !failures.is_empty() {
        report.status = Status::Failed;
        report.reason = Some(failures.join("; "));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mp_zone_names() {
        for z in [
            "mp_crash",
            "mp_crash_load",
            "common_mp",
            "code_post_gfx_mp",
            "localized_common_mp",
            "ui_mp",
        ] {
            assert!(is_mp_zone(z), "{z}");
        }
        for z in [
            "common",
            "ui",
            "ac130",
            "code_post_gfx",
            "simplecredits",
            "village_assault",
        ] {
            assert!(!is_mp_zone(z), "{z}");
        }
    }

    #[test]
    fn missing_install_dir_is_an_io_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        assert!(mp_zones(dir.path()).is_err());
    }
}
