// SPDX-License-Identifier: GPL-3.0-only
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

    pub fn with_status(mut self, status: Status) -> Self {
        self.status = status;
        self
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
        needs_install: false,
        timeout: Duration::from_secs(10 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::net_match::run),
    },
    StageDef {
        name: "net-objective",
        description: "two real UDP clients play Search and Destroy: one plants the bomb holding +activate at the zone's hint, the other defuses it",
        needs_install: false,
        timeout: Duration::from_secs(10 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::net_objective::run),
    },
    StageDef {
        name: "net-vote",
        description: "two real UDP clients vote: a kick vote drops one, a typemap vote changes the gametype and map",
        needs_install: false,
        timeout: Duration::from_secs(10 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::net_vote::run),
    },
    StageDef {
        name: "script-pool",
        description: "headless server with bots plays 32 Search and Destroy rounds and a team deathmatch rotating two maps: the script variable pool must return to the same level every round",
        needs_install: false,
        timeout: Duration::from_secs(20 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::script_pool::run),
    },
    StageDef {
        name: "net-ui",
        description: "a real UDP client answers the menus the stock scripts open and checks the hud elements, objectives, configstrings, client dvars and print lines it is sent",
        needs_install: false,
        timeout: Duration::from_secs(10 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::net_ui::run),
    },
    StageDef {
        name: "net-killcam",
        description: "a real UDP client is killed by a bot and watches the stock killcam replayed from the server's state ring, then respawns",
        needs_install: false,
        timeout: Duration::from_secs(10 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::net_killcam::run),
    },
    StageDef {
        name: "headless-bots-sd",
        description: "headless server, 20 bots play Search and Destroy",
        needs_install: false,
        timeout: Duration::from_secs(5 * MINUTES),
        default: true,
        kind: Kind::Server(include_str!("../scenarios/headless-bots-sd.cfg")),
    },
    StageDef {
        name: "headless-sd-objective",
        description: "headless server, 12 bots plant the Search and Destroy bomb and the other team defuses it",
        needs_install: false,
        timeout: Duration::from_secs(5 * MINUTES),
        default: true,
        kind: Kind::Server(include_str!("../scenarios/headless-sd-objective.cfg")),
    },
    StageDef {
        name: "headless-sab-objective",
        description: "headless server, 16 bots plant a Sabotage bomb and defuse one",
        needs_install: false,
        timeout: Duration::from_secs(5 * MINUTES),
        default: true,
        kind: Kind::Server(include_str!("../scenarios/headless-sab-objective.cfg")),
    },
    StageDef {
        name: "client-objective",
        description: "the real client plays Search and Destroy: the zone's hint is drawn and the player plants the bomb holding +activate",
        needs_install: false,
        timeout: Duration::from_secs(6 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_objective::run),
    },
    StageDef {
        name: "headless-hardpoint",
        description: "headless server, bots earn a killstreak hardpoint into action slot 4 and call it in",
        needs_install: false,
        timeout: Duration::from_secs(5 * MINUTES),
        default: true,
        kind: Kind::Server(include_str!("../scenarios/headless-hardpoint.cfg")),
    },
    StageDef {
        name: "headless-airstrike",
        description: "headless server, bots earn the airstrike, pick a point on the map and call it in",
        needs_install: false,
        timeout: Duration::from_secs(5 * MINUTES),
        default: true,
        kind: Kind::Server(include_str!("../scenarios/headless-airstrike.cfg")),
    },
    StageDef {
        name: "headless-helicopter",
        description: "headless server, a bot calls in the helicopter hardpoint; it flies its path, fires and is shot down",
        needs_install: false,
        timeout: Duration::from_secs(5 * MINUTES),
        default: true,
        kind: Kind::Server(include_str!("../scenarios/headless-helicopter.cfg")),
    },
    StageDef {
        name: "client-hardpoint",
        description: "the real client calls in an airstrike: +actionslot 4, the d-pad pieces, the map pick",
        needs_install: false,
        timeout: Duration::from_secs(6 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_hardpoint::run),
    },
    StageDef {
        name: "client-heli",
        description: "the real client sees a helicopter: a bot calls it in, the player looks at it, the picture must differ from the same view without vehicles",
        needs_install: false,
        timeout: Duration::from_secs(7 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_heli::run),
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
        name: "client-models",
        description: "client --show-models twice from one camera, with and without players: the skinned player models must change the picture",
        needs_install: false,
        timeout: Duration::from_secs(8 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_models::run),
    },
    StageDef {
        name: "client-viewmodel",
        description: "cod4e --viewmodel-tour on three maps: the first-person weapon and hands drawn from spawn points and paths must not be one colour (a wrong reflection probe painted them red) nor black",
        needs_install: false,
        timeout: Duration::from_secs(8 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_viewmodel::run),
    },
    StageDef {
        name: "audio",
        description: "cod4e --audio-selftest on the real sound tables: direction, falloff, range, voice caps, footsteps, streamed ambience and music (no window or sound card)",
        needs_install: false,
        timeout: Duration::from_secs(3 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::audio::run),
    },
    StageDef {
        name: "client-ui",
        description: "client --ui-tour: the stock menus (main menu, server setup, options, team/class, scoreboard) open with no world and draw: fonts, strings and images resolve",
        needs_install: false,
        timeout: Duration::from_secs(8 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_ui::run),
    },
    StageDef {
        name: "client-session",
        description: "the real client gets a person into the world: direct --listen (default join answers) and the stock menus (Start New Server, team, class); the local player must spawn",
        needs_install: false,
        timeout: Duration::from_secs(10 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_session::run),
    },
    StageDef {
        name: "client-gfx",
        description: "the options menus' graphics settings take effect: vsync, aspect, antialiasing, specular, depth of field, glow and shadows through vid_restart, at the menu and in a running match",
        needs_install: false,
        timeout: Duration::from_secs(8 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_gfx::run),
    },
    StageDef {
        name: "client-hud",
        description: "the HUD the client draws over the stock menus in a team deathmatch against bots: scoreboard rows, print and kill lines in the message windows, script hud elements, the killcam",
        needs_install: false,
        timeout: Duration::from_secs(10 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_hud::run),
    },
    StageDef {
        name: "client-ingame-menu",
        description: "changing class and team mid-round through the stock menus with the mouse: Escape and each pick return to the game, the team changes and the new class spawns",
        needs_install: false,
        timeout: Duration::from_secs(8 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_ingame_menu::run),
    },
    StageDef {
        name: "client-frontend",
        description: "the front end with the install's own player profile: the main menu rows, Select Profile, Create a Class opening, the game mode order, and the install's players folder unchanged",
        needs_install: false,
        timeout: Duration::from_secs(4 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_frontend::run),
    },
    StageDef {
        name: "client-leave",
        description: "leaving a match from its in-game menu returns to a drawn main menu with no world; Quit from there exits with status 0",
        needs_install: false,
        timeout: Duration::from_secs(8 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_leave::run),
    },
    StageDef {
        name: "client-custom-class",
        description: "a saved profile's custom class spawns with its weapon (a P90), the profile's perk and experience survive the join, a class change in the grace period applies, and the other players are drawn",
        needs_install: false,
        timeout: Duration::from_secs(8 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_custom_class::run),
    },
    StageDef {
        name: "client-menu-match",
        description: "the client with no flags plays a whole match from the stock menus (Start New Server with a short score limit, team, class), sees the end-of-match scoreboard, follows the server's map rotation into the next map in the same process and spawns there again",
        needs_install: false,
        timeout: Duration::from_secs(8 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_menu_match::run),
    },
    StageDef {
        name: "client-fx",
        description: "effects on the real content, headless: an explosion draws and ends, an impact leaves a decal, a shot flashes and ejects a shell, vision and shock files work",
        needs_install: false,
        timeout: Duration::from_secs(5 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_fx::run),
    },
    StageDef {
        name: "client-load",
        description: "loading a map must not freeze the window: the real client starts a match on mp_crash and on the largest map; the longest gap between presented frames during the load stays under 100 ms",
        needs_install: false,
        timeout: Duration::from_secs(10 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_load::run),
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
        name: "client-match",
        description: "stage 4: the client plays a team deathmatch against bots on its own listen server: connects, spawns, predicts, draws the others, shoots and hits; bandwidth and tick cost",
        needs_install: false,
        timeout: Duration::from_secs(10 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::client_match::run),
    },
    StageDef {
        name: "web-client",
        description: "the browser client (web/pkg) in a headless Chromium on WebGL2 and, where it has it, WebGPU: draws mp_crash; records load time, frame cost and wasm memory (skipped without a built web/pkg or a Chromium)",
        needs_install: false,
        timeout: Duration::from_secs(12 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::web_client::run),
    },
    StageDef {
        name: "web-join",
        description: "the browser client (web/pkg) in a headless Chromium joins a native server over WebTransport, is given a body and gets snapshots, among bots; checked from the page's report and from the server (skipped without a built web/pkg, a Chromium or an install)",
        needs_install: false,
        timeout: Duration::from_secs(12 * MINUTES),
        default: true,
        kind: Kind::Builtin(crate::stages::web_join::run),
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
