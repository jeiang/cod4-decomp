// SPDX-License-Identifier: GPL-3.0-or-later
//! The suite runner: detects the install, runs each selected stage under the
//! watchdog, and writes the bundle.
use crate::bundle::{self, Written};
use crate::install::{self, Detection};
use crate::manifest;
use crate::scrub::Scrubber;
use crate::stage::{self, StageReport, Status};
use crate::stages::asset_load;
use crate::watchdog::{self, DumpServer, Selection, StageRun};
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
}

pub struct Outcome {
    pub bundle: Written,
    pub reports: Vec<StageReport>,
}

impl Outcome {
    pub fn ok(&self) -> bool {
        !self.reports.iter().any(|r| r.status.is_bad())
    }
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
    fn say(&self, msg: impl AsRef<str>) {
        let msg = msg.as_ref();
        println!("{msg}");
        let _ = writeln!(self.0.lock().unwrap(), "{msg}");
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
    let _ = fs::remove_dir_all(&scratch);
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
        .and_then(|p| asset_load::mp_zones(p).ok())
        .map(|z| z.len());

    let dumps = DumpServer::start(scratch);
    if let Err(e) = &dumps {
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
    let summary = json!({
        "schema": manifest::SCHEMA,
        "started": crate::time::iso(started),
        "finished": crate::time::iso(finished),
        "build": manifest::build_hash(),
        "ok": !reports.iter().any(|r| r.status.is_bad()),
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
    Ok(Outcome { bundle, reports })
}
