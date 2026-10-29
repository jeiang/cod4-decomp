// SPDX-License-Identifier: GPL-3.0-only
//! The suite runner: detects the install, runs each selected stage under the
//! watchdog, and writes the bundle.
use crate::bundle::{self, Written};
use crate::manifest;
use crate::scrub::Scrubber;
use crate::stage::{self, StageReport, Status};
use crate::stages::asset_load;
use crate::watchdog::{self, DumpServer, Selection, StageRun};
use install::{self, Detection};
use serde_json::json;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

pub struct Config {
    pub selections: Vec<Selection>,
    pub cod4: Option<PathBuf>,
    pub out_dir: PathBuf,
    pub timeout: Option<Duration>,
    pub prompt: bool,
    /// Directory holding the executable, searched for an install.
    pub exe_dir: Option<PathBuf>,
    /// Stages whose `Skipped` status the caller accepts (`--allow-skip`). A stage that skips without being named
    /// here fails the run: a skipped stage proves nothing, so it is not a pass.
    pub allow_skip: Vec<String>,
}

pub struct Outcome {
    pub bundle: Written,
    pub reports: Vec<StageReport>,
    /// Every stage that skipped, named or not.
    pub skipped: Vec<String>,
    /// The skips `--allow-skip` did not name: these fail the run.
    pub unexpected_skips: Vec<String>,
}

impl Outcome {
    /// The run's verdict: every stage passed, and every skip was named by `--allow-skip`.
    pub fn ok(&self) -> bool {
        !self.reports.iter().any(|r| r.status.is_bad()) && self.unexpected_skips.is_empty()
    }
}

/// Sorts the skips against the allowed list: `(all skips, the ones nothing allowed)`, both in stage order.
pub fn skips(reports: &[StageReport], allow: &[String]) -> (Vec<String>, Vec<String>) {
    let skipped: Vec<String> = reports
        .iter()
        .filter(|r| r.status == Status::Skipped)
        .map(|r| r.name.clone())
        .collect();
    let unexpected = skipped
        .iter()
        .filter(|n| !allow.iter().any(|a| a == *n))
        .cloned()
        .collect();
    (skipped, unexpected)
}

pub fn default_selections() -> Vec<Selection> {
    stage::STAGES
        .iter()
        .filter(|s| s.default)
        .map(|s| Selection::Stage(s.name))
        .collect()
}

struct Log(Mutex<File>);

impl Log {
    /// Stage progress and the verdict are the harness's output: terminal and `harness.log`.
    #[allow(clippy::print_stdout)]
    fn say(&self, msg: impl AsRef<str>) {
        let msg = msg.as_ref();
        println!("{msg}");
        if let Err(e) = writeln!(self.0.lock().unwrap(), "{msg}") {
            tracing::warn!("cannot write harness.log: {e}");
        }
    }
}

fn dir_name(index: usize, name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{:02}-{safe}", index + 1)
}

pub fn run(cfg: &Config) -> io::Result<Outcome> {
    let started = crate::time::unix_now();
    let stamp = crate::time::stamp(started);
    let scratch =
        std::env::temp_dir().join(format!("cod4e-harness-{}-{stamp}", std::process::id()));
    let work = scratch.join("bundle");
    fs::create_dir_all(&work)?;
    let result = run_in(cfg, started, &stamp, &scratch, &work);
    if let Err(e) = fs::remove_dir_all(&scratch) {
        tracing::warn!(path = %scratch.display(), "cannot remove scratch dir: {e}");
    }
    result
}

fn run_in(
    cfg: &Config,
    started: u64,
    stamp: &str,
    scratch: &Path,
    work: &Path,
) -> io::Result<Outcome> {
    let log = Log(Mutex::new(File::create(work.join("harness.log"))?));
    log.say(format!(
        "cod4e-harness {} ({}), {} {}",
        env!("CARGO_PKG_VERSION"),
        manifest::build_hash(),
        std::env::consts::OS,
        std::env::consts::ARCH
    ));

    let detection: Detection = install::detect(&install::Options {
        explicit: cfg.cod4.as_deref(),
        flag: "--cod4",
        prompt: cfg.prompt,
        exe_dir: cfg.exe_dir.as_deref(),
    });
    let install_path = detection.install.as_ref().map(|i| i.path.clone());
    match &detection.install {
        Some(i) => log.say(format!("install: {} ({})", i.path.display(), i.source)),
        None => {
            log.say("install not found. Looked at:");
            for t in &detection.tried {
                log.say(format!("  {t}"));
            }
        }
    }
    let mp_zones = install_path
        .as_deref()
        .and_then(|p| {
            asset_load::mp_zones(p)
                .inspect_err(|e| tracing::warn!(path = %p.display(), "cannot list mp zones: {e}"))
                .ok()
        })
        .map(|z| z.len());

    let dumps = DumpServer::start(scratch);
    if let Err(e) = &dumps {
        tracing::warn!("crash capture unavailable: {e}");
        log.say(format!("crash capture unavailable: {e}"));
    }
    let exe = std::env::current_exe()?;
    let mut reports = Vec::new();
    for (i, sel) in cfg.selections.iter().enumerate() {
        let (name, needs_install, timeout) = match sel {
            Selection::Stage(n) => {
                let def = stage::find(n).expect("selections are validated by the caller");
                (n.to_string(), def.needs_install, def.timeout)
            }
            Selection::Script(p) => (stage::script_stage_name(p), false, Duration::from_secs(600)),
        };
        let timeout = cfg.timeout.unwrap_or(timeout);
        log.say(format!("[{}/{}] {name} ...", i + 1, cfg.selections.len()));
        let report = if needs_install && install_path.is_none() {
            StageReport::new(&name, Status::Failed).with_reason("install not found")
        } else {
            let rel = format!("stages/{}", dir_name(i, &name));
            watchdog::run_stage(&StageRun {
                exe: &exe,
                selection: sel,
                name: &name,
                dir: &work.join(&rel),
                rel: &rel,
                install: install_path.as_deref(),
                timeout,
                dumps: dumps.as_ref().ok(),
            })
        };
        log.say(format!(
            "[{}/{}] {name}: {:?}{} ({:.1} s)",
            i + 1,
            cfg.selections.len(),
            report.status,
            report
                .reason
                .as_deref()
                .map_or(String::new(), |r| format!(": {r}")),
            report.wall_ms / 1000.0
        ));
        for n in &report.notes {
            log.say(format!("    {n}"));
        }
        reports.push(report);
    }

    let finished = crate::time::unix_now();
    let man = manifest::collect(
        started,
        &detection,
        mp_zones,
        dumps.as_ref().map(|_| ()).map_err(String::as_str),
        &reports,
    );
    fs::write(work.join("manifest.json"), serde_json::to_vec_pretty(&man)?)?;
    let (skipped, unexpected_skips) = skips(&reports, &cfg.allow_skip);
    let allowed_skips: Vec<&String> = skipped
        .iter()
        .filter(|n| !unexpected_skips.contains(n))
        .collect();
    for r in &mut reports {
        if r.status == Status::Skipped {
            r.notes.push(if unexpected_skips.contains(&r.name) {
                "skipped: a skip fails the run unless --allow-skip names the stage".into()
            } else {
                "skipped: --allow-skip names the stage, so the run accepts it".into()
            });
        }
    }
    let ok = !reports.iter().any(|r| r.status.is_bad()) && unexpected_skips.is_empty();
    let summary = json!({
        "schema": manifest::SCHEMA,
        "started": crate::time::iso(started),
        "finished": crate::time::iso(finished),
        "build": manifest::build_hash(),
        // A skipped stage is not a pass: `ok` needs every stage to have run, or to be named by --allow-skip.
        "ok": ok,
        "skipped": skipped,
        "allowed_skips": allowed_skips,
        "unexpected_skips": unexpected_skips,
        "stages": reports,
    });
    fs::write(
        work.join("summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    drop(dumps);
    drop(log);

    fs::create_dir_all(&cfg.out_dir)?;
    let zip = cfg.out_dir.join(format!("cod4e-run-{stamp}.zip"));
    let bundle = bundle::write(work, &zip, &Scrubber::from_env(), bundle::CAP)?;
    Ok(Outcome {
        bundle,
        reports,
        skipped,
        unexpected_skips,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reports(statuses: &[(&str, Status)]) -> Vec<StageReport> {
        statuses
            .iter()
            .map(|(n, s)| StageReport::new(n, *s))
            .collect()
    }

    #[test]
    fn a_skip_nothing_allows_is_unexpected() {
        let r = reports(&[
            ("asset-load", Status::Passed),
            ("client-hud", Status::Skipped),
            ("net-match", Status::Failed),
        ]);
        let (skipped, unexpected) = skips(&r, &[]);
        assert_eq!(skipped, ["client-hud"]);
        assert_eq!(unexpected, ["client-hud"]);
    }

    #[test]
    fn an_allowed_skip_is_reported_but_not_unexpected() {
        let r = reports(&[
            ("asset-load", Status::Passed),
            ("client-hud", Status::Skipped),
            ("web-client", Status::Skipped),
        ]);
        let (skipped, unexpected) = skips(&r, &["web-client".to_owned()]);
        assert_eq!(skipped, ["client-hud", "web-client"]);
        assert_eq!(unexpected, ["client-hud"]);
        let (_, none) = skips(&r, &["client-hud".to_owned(), "web-client".to_owned()]);
        assert!(none.is_empty());
    }
}
