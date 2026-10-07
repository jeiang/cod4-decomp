// SPDX-License-Identifier: GPL-3.0-or-later
//! Stages: the unit the suite runs, each in its own child process.
use crate::perf::Percentiles;
use crate::script::{self, CommandReport, NoEngine, Outcome};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Passed,
    Failed,
    Skipped,
    /// The child died from a signal or exception.
    Crashed,
    /// The child exceeded its timeout and was killed.
    Hung,
}

impl Status {
    pub fn is_bad(self) -> bool {
        matches!(self, Self::Failed | Self::Crashed | Self::Hung)
    }
}

/// What a stage reports; the child writes it as `result.json`, the parent
/// completes it (exit code, timings, files) into `summary.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StageReport {
    pub name: String,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default)]
    pub wall_ms: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Scalar results, compared by `harness diff`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metrics: BTreeMap<String, f64>,
    /// Distributions with p50/p95/p99/max.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub series: BTreeMap<String, Percentiles>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<CommandReport>,
    /// Environment facts only this stage can know (`gpu`, `display_modes`,
    /// `cvars`); merged into `manifest.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<serde_json::Value>,
    /// Bundle-relative paths of this stage's files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

impl StageReport {
    pub fn new(name: &str, status: Status) -> Self {
        Self {
            name: name.to_owned(),
            status,
            reason: None,
            wall_ms: 0.0,
            exit_code: None,
            metrics: BTreeMap::new(),
            series: BTreeMap::new(),
            notes: Vec::new(),
            commands: Vec::new(),
            environment: None,
            files: Vec::new(),
        }
    }

    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

/// What a running stage can use.
pub struct StageCtx {
    /// Where the stage writes its files.
    pub dir: PathBuf,
    pub install: Option<PathBuf>,
}

pub enum Kind {
    Builtin(fn(&StageCtx) -> std::io::Result<StageReport>),
    /// A scenario script compiled into the binary.
    Script(&'static str),
    /// A scenario script run against the headless server (needs the install).
    Server(&'static str),
}

pub struct StageDef {
    pub name: &'static str,
    pub description: &'static str,
    pub needs_install: bool,
    pub timeout: Duration,
    /// In the default suite.
    pub default: bool,
    pub kind: Kind,
}

const MINUTES: u64 = 60;

/// Every stage, default-suite ones first and in suite order.
pub static STAGES: &[StageDef] = &[
    StageDef {
        name: "asset-load",
        description: "decode every MP zone and scan the VFS",
        needs_install: true,
        timeout: Duration::from_secs(30 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::asset_load::run),
    },
    StageDef {
        name: "headless-bots",
        description: "headless server, 18 bots play a team deathmatch round and the map rotates",
        needs_install: false,
        timeout: Duration::from_secs(10 * MINUTES),
        default: true,
        kind: Kind::Server(include_str!("../scenarios/headless-bots.cfg")),
    },
    StageDef {
        name: "net-loopback",
        description: "eight clients over real UDP with loss, duplicates and reordering: handshake, reliable commands, snapshot convergence",
        needs_install: false,
        timeout: Duration::from_secs(5 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::net_loopback::run),
    },
    StageDef {
        name: "net-match",
        description: "headless server with bots, real UDP clients connect, spawn, walk and watch: smooth interpolation, bandwidth and per-client tick cost",
        needs_install: true,
        timeout: Duration::from_secs(10 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::net_match::run),
    },
    StageDef {
        name: "client-flythrough",
        description: "client flythrough per display mode, with video",
        needs_install: false,
        timeout: Duration::from_secs(20 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_flythrough::run),
    },
    StageDef {
        name: "client-input",
        description: "client input layer: default binds, mouse look scaling and config round trip (no display needed)",
        needs_install: false,
        timeout: Duration::from_secs(2 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_input::run),
    },
    StageDef {
        name: "client-bots-match",
        description: "client plus bots match, perf only",
        needs_install: false,
        timeout: Duration::from_secs(10 * MINUTES),
        default: true,
        kind: Kind::Script(include_str!("../scenarios/client-bots-match.cfg")),
    },
    StageDef {
        name: "headless-bots-32",
        description: "headless server with 32 bots (the 32-player server budget)",
        needs_install: false,
        timeout: Duration::from_secs(10 * MINUTES),
        default: false,
        kind: Kind::Server(include_str!("../scenarios/headless-bots-32.cfg")),
    },
    // Self tests of the watchdog; run by name only.
    StageDef {
        name: "selftest-crash",
        description: "writes through a null pointer",
        needs_install: false,
        timeout: Duration::from_secs(60),
        default: false,
        kind: Kind::Builtin(crate::stages::selftest::crash),
    },
    StageDef {
        name: "selftest-hang",
        description: "never returns",
        needs_install: false,
        timeout: Duration::from_secs(3),
        default: false,
        kind: Kind::Builtin(crate::stages::selftest::hang),
    },
    StageDef {
        name: "selftest-panic",
        description: "panics",
        needs_install: false,
        timeout: Duration::from_secs(60),
        default: false,
        kind: Kind::Builtin(crate::stages::selftest::panic),
    },
];

pub fn find(name: &str) -> Option<&'static StageDef> {
    STAGES.iter().find(|s| s.name == name)
}

/// Run a script in the current process and report it. `Skipped` when it holds
/// engine commands and none could run.
pub fn run_script(name: &str, src: &str) -> StageReport {
    run_script_on(name, src, &mut NoEngine, std::thread::sleep)
}

/// [`run_script`] against any console; `sleep` performs the waits.
pub fn run_script_on(
    name: &str,
    src: &str,
    console: &mut dyn script::Console,
    sleep: impl FnMut(Duration),
) -> StageReport {
    let cmds = match script::parse(src) {
        Ok(c) => c,
        Err(e) => return StageReport::new(name, Status::Failed).with_reason(e.to_string()),
    };
    let reports = script::run(&cmds, console, sleep);
    let failed = reports
        .iter()
        .find(|r| matches!(r.outcome, Outcome::Failed(_)));
    let engine_missing: Vec<&str> = reports
        .iter()
        .zip(&cmds)
        .filter(|(r, _)| matches!(&r.outcome, Outcome::Skipped(m) if m.starts_with("command not implemented")))
        .map(|(_, c)| c.name.as_str())
        .collect();
    let ran_engine = reports
        .iter()
        .zip(&cmds)
        .any(|(r, c)| r.outcome == Outcome::Done && !matches!(c.name.as_str(), "echo" | "wait"));
    let (status, reason) = if let Some(f) = failed {
        (
            Status::Failed,
            Some(format!("line {}: {}", f.line, f.command)),
        )
    } else if !engine_missing.is_empty() && !ran_engine {
        let mut uniq = engine_missing.clone();
        uniq.sort_unstable();
        uniq.dedup();
        (
            Status::Skipped,
            Some(format!("engine cannot run it yet: no {}", uniq.join(", "))),
        )
    } else {
        (Status::Passed, None)
    };
    let mut r = StageReport::new(name, status);
    r.reason = reason;
    r.commands = reports;
    r
}

/// Script file stage name: `script:<file stem>`.
pub fn script_stage_name(path: &Path) -> String {
    format!(
        "script:{}",
        path.file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_scripts_skip_until_the_engine_has_their_commands() {
        let r = run_script("t", "map mp_crash; wait 10s; screenshot");
        assert_eq!(r.status, Status::Skipped);
        assert!(r.reason.unwrap().contains("map, screenshot"));
        assert_eq!(r.commands.len(), 3);
    }

    #[test]
    fn harness_only_scripts_pass_and_bad_ones_fail() {
        assert_eq!(run_script("t", "echo hi; wait 1ms").status, Status::Passed);
        assert_eq!(run_script("t", "wait bogus").status, Status::Failed);
        let r = run_script("t", "echo \"x");
        assert_eq!(r.status, Status::Failed);
        assert!(r.reason.unwrap().contains("line 1"));
    }

    #[test]
    fn default_suite_order() {
        let d: Vec<_> = STAGES
            .iter()
            .filter(|s| s.default)
            .map(|s| s.name)
            .collect();
        assert_eq!(d[0], "asset-load");
    }
}
