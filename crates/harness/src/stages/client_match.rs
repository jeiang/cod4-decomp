// SPDX-License-Identifier: GPL-3.0-only
//! Stage 4, `client-match`: the real `cod4e` client plays a team deathmatch against bots on the server it starts
//! in-process (`--listen --autoplay`). The scripted player connects over UDP, gets a body, walks, sees the bots,
//! turns toward enemies it can see and shoots them, for 90 seconds. The stage reads the client's report and asserts
//! what makes a match playable: connected and spawned, the player moved by prediction, the others were drawn,
//! snapshots kept coming, prediction was rarely corrected, shots were fired and their events (impacts, flashes, pain) became effects, and the server ran without
//! script errors. Bandwidth, frame times and the server's tick cost are reported as metrics. Needs a display and the
//! install; skips cleanly without.
use super::client_flythrough::locate_client;
use super::client_models::{decode, no_display};
use super::client_session::{profile_args, render_problem};
use crate::perf::Percentiles;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::Value;
use std::fs::{self, File};
use std::io;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const NAME: &str = "client-match";
const BOTS: usize = 9;
const SECS: u64 = 90;
const LIMIT: Duration = Duration::from_secs(300);
/// Share of the last frame's pixels that may look magenta.
const MAX_MAGENTA: f64 = 0.002;

/// A number at `path`, 0 when the report has none there: for the per-name tallies (`events.<kind>`,
/// `sound.by_channel.<name>`, `fx.played.<name>`), where an entry that never happened is not written. A counter the
/// client always writes is read through [`Counters`], so that its absence is a failure and not a zero.
pub(super) fn num(v: &Value, path: &[&str]) -> f64 {
    path.iter()
        .try_fold(v, |v, k| v.get(k))
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
}

/// The counters the client's report always writes, read for the checks. One that is not there is a failure of the
/// report, not a zero: a check that read a missing counter as 0 passed when the client stopped writing it (#375).
/// Such a read returns NaN, which no comparison passes or fails, so the one failure [`Counters::missing`] builds
/// names the cause.
pub(super) struct Counters<'a> {
    of: &'a Value,
    what: &'static str,
    missing: Vec<String>,
}

impl<'a> Counters<'a> {
    /// Reads under `of`, naming it `what` (`net`, `server`) in the failure.
    pub(super) fn new(what: &'static str, of: &'a Value) -> Self {
        Self {
            of,
            what,
            missing: Vec::new(),
        }
    }

    pub(super) fn get(&mut self, path: &[&str]) -> f64 {
        match path
            .iter()
            .try_fold(self.of, |v, k| v.get(k))
            .and_then(Value::as_f64)
        {
            Some(n) => n,
            None => {
                self.missing
                    .push(format!("{}.{}", self.what, path.join(".")));
                f64::NAN
            }
        }
    }

    /// The failure for the counters that were read and not found, if any.
    pub(super) fn missing(&self) -> Option<String> {
        (!self.missing.is_empty()).then(|| {
            format!(
                "the client report lacks the counter(s) {}",
                self.missing.join(", ")
            )
        })
    }
}

fn fail(mut r: StageReport, why: String) -> StageReport {
    r.status = Status::Failed;
    r.reason = Some(why);
    r
}

/// One column of `frames.raw.csv`.
pub(super) fn column(csv: &str, i: usize) -> Vec<f64> {
    csv.lines()
        .skip(1)
        .filter_map(|l| l.split(',').nth(i)?.parse().ok())
        .collect()
}

/// `frames.phases.csv` (`client/src/app.rs` `PHASES`): the columns by name, one value per frame, in the rows'
/// order, which is `frames.raw.csv`'s. A column the client did not write is absent.
pub(super) struct Phases {
    names: Vec<String>,
    rows: Vec<Vec<f64>>,
}

impl Phases {
    pub(super) fn parse(csv: &str) -> Self {
        let mut lines = csv.lines();
        let names: Vec<String> = lines
            .next()
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
            .collect();
        let rows = lines
            .map(|l| {
                l.split(',')
                    .map(|v| v.trim().parse().unwrap_or(0.0))
                    .collect()
            })
            .collect();
        Self { names, rows }
    }

    fn col(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }

    /// The column's value in frame `i`, 0 where the client did not write the column.
    fn at(&self, i: usize, name: &str) -> f64 {
        self.col(name)
            .and_then(|c| self.rows.get(i)?.get(c))
            .copied()
            .unwrap_or(0.0)
    }

    /// The largest value of the column over the frames, with the frame it is in.
    fn max(&self, name: &str) -> Option<(usize, f64)> {
        let c = self.col(name)?;
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| Some((i, *r.get(c)?)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
    }
}

/// The phases a frame's `cpu_ms` is made of, as `frames.phases.csv` splits it: the render by its laps rather than
/// as a whole. What the columns do not cover (console lines, menu sound, the frame's tail) is `other`.
const LEAVES: &[&str] = &[
    "input_ms",
    "net_ms",
    "warm_ms",
    "r_setup",
    "r_dynamic",
    "r_sun_shadow",
    "r_spot_shadow",
    "r_main_draws",
    "r_lights",
    "r_floatz",
    "r_post_upload",
    "r_encode",
    "r_submit",
    "ui_ms",
    "present_ms",
];

/// A frame that prepared this many material passes at once drew a precache list, not one model that turned up: the
/// passes of the models the server precached are prepared a material per frame (`Renderer::warm_late`), a draw's
/// first sight of a model costs that model's materials only (a dozen passes).
const MAX_PASSES_PER_FRAME: f64 = 64.0;

/// Where the hitching frames' time went: the excess over each phase's median summed over the `spikes` frames, the
/// worst frame's phases, and whether one phase held the stalls or every phase stretched alike. A stall that sits
/// in one phase is a call the client blocks in; excess spread over the phases in proportion to their usual cost is
/// the process not being scheduled (the machine, not the client).
fn where_it_went(phases: &Phases, cpu_ms: &[f64], spikes: &[usize]) -> String {
    let n = cpu_ms.len().min(phases.rows.len());
    if n == 0 || spikes.is_empty() {
        return String::new();
    }
    // `other` is what the columns leave of the frame; the render's laps stand for `render_ms`.
    let value = |i: usize, leaf: &str| -> f64 {
        if leaf == "other" {
            let known: f64 = LEAVES.iter().map(|l| phases.at(i, l)).sum();
            (cpu_ms[i] - known).max(0.0)
        } else {
            phases.at(i, leaf)
        }
    };
    let leaves: Vec<&str> = LEAVES.iter().copied().chain(["other"]).collect();
    let medians: Vec<f64> = leaves
        .iter()
        .map(|l| {
            let mut v: Vec<f64> = (0..n).map(|i| value(i, l)).collect();
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        })
        .collect();
    let usual: f64 = medians.iter().sum::<f64>().max(f64::EPSILON);
    let excess: Vec<f64> = leaves
        .iter()
        .enumerate()
        .map(|(k, l)| {
            spikes
                .iter()
                .filter(|&&i| i < n)
                .map(|&i| (value(i, l) - medians[k]).max(0.0))
                .sum()
        })
        .collect();
    let total: f64 = excess.iter().sum::<f64>().max(f64::EPSILON);
    let mut order: Vec<usize> = (0..leaves.len()).collect();
    order.sort_by(|a, b| excess[*b].total_cmp(&excess[*a]));
    let top = order[0];
    let (top_share, top_usual) = (excess[top] / total, medians[top] / usual);
    let list = |idx: &[usize], f: &dyn Fn(usize) -> f64| -> String {
        idx.iter()
            .filter(|&&k| f(k) >= 1.0)
            .map(|&k| format!("{} {:.0}", leaves[k].trim_end_matches("_ms"), f(k)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let worst = spikes
        .iter()
        .copied()
        .filter(|&i| i < n)
        .max_by(|a, b| cpu_ms[*a].total_cmp(&cpu_ms[*b]))
        .unwrap_or(spikes[0].min(n - 1));
    let mut worst_order: Vec<usize> = (0..leaves.len()).collect();
    worst_order.sort_by(|a, b| value(worst, leaves[*b]).total_cmp(&value(worst, leaves[*a])));
    let verdict = if top_share >= 0.6 && top_share >= 2.0 * top_usual {
        format!(
            "{} held the stalls ({:.0}% of the excess against {:.0}% of a usual frame): the client blocks there",
            leaves[top].trim_end_matches("_ms"),
            top_share * 100.0,
            top_usual * 100.0
        )
    } else {
        format!(
            "every phase stretched alike (the largest, {}, holds {:.0}% of the excess and {:.0}% of a usual frame): the process was not being scheduled, which is the machine, not a call the client blocks in",
            leaves[top].trim_end_matches("_ms"),
            top_share * 100.0,
            top_usual * 100.0
        )
    };
    format!(
        "; ms over the median across them: {}; the worst, frame {worst} ({:.0} ms): {}; {verdict}",
        list(&order[..order.len().min(5)], &|k| excess[k]),
        cpu_ms[worst],
        list(&worst_order[..worst_order.len().min(5)], &|k| value(
            worst, leaves[k]
        )),
    )
}

/// A frame costing this many times the median and this much more (ms) is a hitch the player feels: a frame of a
/// 60 Hz game is 16 ms, so tens of milliseconds over the usual is a dropped frame, however fast or slow the machine.
const SPIKE_FACTOR: f64 = 2.5;
const SPIKE_EXTRA_MS: f64 = 25.0;
/// Share of the frames that may be hitches (the odd scheduler stall of a busy machine; the screenshot frame).
const SPIKE_SHARE: f64 = 0.01;
/// The client's clock may jump to the server's this often (the first reading, a level change); every other
/// jump moves everything it drives by up to the snapshot gap at once.
const MAX_CLOCK_JUMPS: u64 = 3;

/// Sound aliases a match may ask for and the tables not have, with the reason each is benign. Empty: no miss is
/// expected in a stock match, so a new entry needs the alias and why it is sound to leave it out.
const ALLOWED_MISSING_SOUND: &[&str] = &[];
/// The same for the effect names the scripts and the engine ask for (`PlayFx`).
const ALLOWED_MISSING_EFFECTS: &[&str] = &[];

/// Why a client's `missing` list fails the run: every name the game asked for and the tables did not have, but
/// the allow-listed ones. An absent list is a failure too (the counter proves the check ran at all).
fn misses(what: &str, list: &Value, allowed: &[&str]) -> Option<String> {
    // The client reports the set either as a list of names or as the keys of an object, depending on where the
    // counter comes from; a report with neither says nothing about it, which is not a failure of the match.
    let names: Vec<&str> = match list {
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        Value::Object(o) => o.keys().map(String::as_str).collect(),
        _ => return None,
    };
    let names: Vec<&str> = names.into_iter().filter(|n| !allowed.contains(n)).collect();
    (!names.is_empty()).then(|| format!("{} names missing: {}", what, names.join(", ")))
}

/// Why the frames hitched, if they did. `cpu_ms` is every frame's own cost (`frames.raw.csv`), `phases` where it
/// went. The report's `late_builds` lists what was built after the load, as "frame what": a pipeline listed was
/// missing when a frame drew (its draws were skipped for the frames the compile took), so the load's warm-up
/// missed it. None may be, whatever the runner's speed; the spike share is the relative check on what that misses.
/// `clock_jumps` counts how often the clock the view follows jumped (a server running at the wrong pace does it
/// every few seconds).
fn hitch_failures(report: &Value, cpu_ms: &[f64], phases: &Phases) -> Vec<String> {
    hitches(report, cpu_ms, phases, Some(SPIKE_SHARE))
}

/// The share of frames that hitched by the rule of [`hitch_failures`].
pub(super) fn spike_share(cpu_ms: &[f64]) -> Option<f64> {
    let p = Percentiles::from_samples(cpu_ms)?;
    Some(spikes(cpu_ms, p.p50).len() as f64 / cpu_ms.len() as f64)
}

/// The frames costing far over the median: [`SPIKE_FACTOR`] times it and [`SPIKE_EXTRA_MS`] more.
fn spikes(cpu_ms: &[f64], p50: f64) -> Vec<usize> {
    let limit = (p50 * SPIKE_FACTOR).max(p50 + SPIKE_EXTRA_MS);
    (0..cpu_ms.len()).filter(|&i| cpu_ms[i] > limit).collect()
}

/// [`hitch_failures`] with the frame-spike gate optional: a stage that shares its machine with a server and whatever
/// else runs there is stalled in bursts, and leaves frame times to the stage that has the client alone.
pub(super) fn hitches(
    report: &Value,
    cpu_ms: &[f64],
    phases: &Phases,
    max_spike_share: Option<f64>,
) -> Vec<String> {
    let mut out = Vec::new();
    let late: Vec<&str> = report["late_builds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|w| {
            w.split_once(' ')
                .is_some_and(|(_, what)| what.starts_with("pipeline "))
        })
        .collect();
    if let Some(first) = late.first() {
        out.push(format!(
            "{} pipelines were missing while playing (their draws were skipped, or a frame compiled them), first: {first}",
            late.len()
        ));
    }
    // The models the server precached are warmed a material per frame; a frame that prepared a whole list itself
    // stalled on it (31 ms for 241 passes on the Linux test host).
    if let Some((frame, passes)) = phases.max("late_passes")
        && passes > MAX_PASSES_PER_FRAME
    {
        out.push(format!(
            "frame {frame} prepared {passes:.0} material passes itself ({:.0} ms): the precached models were not warmed ahead of their first draw",
            phases.at(frame, "late_pass_ms")
        ));
    }
    let jumps: u64 = report["net"]["clock_jumps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .sum();
    if jumps > MAX_CLOCK_JUMPS {
        out.push(format!(
            "the view's clock jumped {jumps} times to catch up with the server's: the server runs off the client's pace"
        ));
    }
    if let (Some(max), Some(p)) = (max_spike_share, Percentiles::from_samples(cpu_ms)) {
        let limit = (p.p50 * SPIKE_FACTOR).max(p.p50 + SPIKE_EXTRA_MS);
        let spikes = spikes(cpu_ms, p.p50);
        if spikes.len() as f64 > cpu_ms.len() as f64 * max {
            out.push(format!(
                "{} of {} frames cost over {limit:.0} ms (median {:.1} ms, worst {:.0} ms): the frames hitch{}",
                spikes.len(),
                cpu_ms.len(),
                p.p50,
                p.max,
                where_it_went(phases, cpu_ms, &spikes)
            ));
        }
    }
    out
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(client) = locate_client() else {
        return Ok(StageReport::new(NAME, Status::Skipped)
            .with_reason("cod4e client binary not found next to the harness"));
    };
    let Some(install) = &ctx.install else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("no install"));
    };
    if let Some(r) = no_display(&client, NAME)? {
        return Ok(r);
    }
    let dir = ctx.dir.join("match");
    fs::create_dir_all(&dir)?;
    let log = File::create(dir.join("client.log"))?;
    let mut child = Command::new(&client)
        .arg("--install")
        .arg(install)
        .args(profile_args(&dir.join("config")))
        .args(["--map", "mp_crash", "--listen", "--bots"])
        .arg(BOTS.to_string())
        .args(["--autoplay", "--duration"])
        .arg(SECS.to_string())
        .args(["--screenshot", "--size", "1280x720", "--out"])
        .arg(&dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    let end = Instant::now() + LIMIT;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(StageReport::new(NAME, Status::Failed)
                .with_reason(format!("client hung (killed after {} s)", LIMIT.as_secs())));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut report = StageReport::new(NAME, Status::Passed);
    report.files.extend([
        "match/client.log".into(),
        "match/client.json".into(),
        "match/screenshot.png".into(),
    ]);
    if !status.success() {
        return Ok(fail(
            report,
            format!("client exited with {status} (see match/client.log)"),
        ));
    }
    let json: Value = match fs::read(dir.join("client.json"))
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
    {
        Ok(v) => v,
        Err(e) => return Ok(fail(report, format!("no client report: {e}"))),
    };
    let (net, srv) = (&json["net"], &json["server"]);
    let mut failures = Vec::new();
    let secs = SECS as f64;
    let mut c = Counters::new("net", net);
    let mut sv = Counters::new("server", srv);
    let snaps = c.get(&["snapshots"]);
    let corrected = c.get(&["prediction_corrections"]);
    let predicted = c.get(&["predictions"]);
    let (shots, hits) = (sv.get(&["person", "shots"]), sv.get(&["person", "hits"]));
    if net["connected"] != true {
        failures.push("the client never connected".to_owned());
    }
    if net["spawned"] != true {
        failures.push("the server never gave the player a body".to_owned());
    }
    let (moved, path) = (c.get(&["moved"]), c.get(&["path"]));
    if moved < 100.0 && path < 500.0 {
        failures.push(format!("the player barely moved ({path:.0} units walked)"));
    }
    let players_seen = c.get(&["players_seen_max"]);
    if players_seen < (BOTS / 2) as f64 {
        failures.push(format!(
            "at most {players_seen} other players drawn of {BOTS} bots"
        ));
    }
    if snaps < secs * 20.0 {
        failures.push(format!("only {snaps} snapshots in {SECS} s"));
    }
    if predicted == 0.0 || corrected * 7.0 > predicted {
        failures.push(format!(
            "the server corrected {corrected} of {predicted} predictions"
        ));
    }
    // A steady walk moves the eye at a steady speed; a view that jerks (the clock following snapshot jitter, a stair
    // snap) has frames that cover twice the usual ground. The jittery clock put about 5% of frames there.
    let jerky = num(net, &["eye_speed", "over_twice_median"]);
    if net["eye_speed"].is_object() && jerky > 0.02 {
        failures.push(format!(
            "{:.1}% of the drawn frames moved the eye over twice as fast as the median: the view is jerky",
            jerky * 100.0
        ));
    }
    // A frame that hitches: pipelines compiled inside a frame, a precache list prepared in one, or frames far over
    // the usual cost; the phases say where a hitch went.
    let cpu = fs::read_to_string(dir.join("frames.raw.csv"))
        .map(|csv| column(&csv, 0))
        .unwrap_or_default();
    let phases =
        Phases::parse(&fs::read_to_string(dir.join("frames.phases.csv")).unwrap_or_default());
    failures.extend(hitch_failures(&json, &cpu, &phases));
    // The camera eases over stair steps: a frame that began a step on the logical eye did not jump the drawn one.
    // The match's walking decides whether a step is climbed at all; with none the stage notes it as untested.
    let (stairs, snapped) = (
        c.get(&["view", "stair_frames"]),
        c.get(&["view", "stair_snaps"]),
    );
    if snapped > 0.0 {
        failures.push(format!(
            "{snapped} of {stairs} stair steps jumped the drawn eye instead of easing it (last: logical jump, drawn jump, offset, dt = {})",
            net["view"]["stair_last_snap"]
        ));
    }
    if c.get(&["view", "steps"]) >= 1.0 && c.get(&["view", "step_max"]) <= 0.0 {
        failures.push("the player climbed steps but the camera never smoothed one".into());
    }
    // Leaning out moved the eye sideways and a landing dipped it, when the bot did either.
    if c.get(&["view", "lean_frames"]) >= 1.0 && c.get(&["view", "lean_max"]) < 1.0 {
        failures.push("the player leaned but the camera did not move sideways".into());
    }
    if c.get(&["view", "landings"]) >= 1.0 && c.get(&["view", "dip_max"]) <= 0.0 {
        failures.push("the player landed from a jump but the view never dipped".into());
    }
    if c.get(&["viewmodel_frames"]) < secs {
        failures.push("the first-person weapon was hardly drawn".into());
    }
    if net["weapons_without_models"]
        .as_object()
        .is_some_and(|m| !m.is_empty())
    {
        failures.push(format!(
            "weapons without a model: {}",
            net["weapons_without_models"]
        ));
    }
    // Every scripted model the server published (props, cars, objectives) must be drawn.
    let (seen, drawn) = (
        c.get(&["script_models_seen"]),
        c.get(&["script_models_drawn"]),
    );
    if seen < 1.0 {
        failures.push("the server published no script_model for the client to draw".into());
    } else if drawn < seen {
        failures.push(format!(
            "{drawn} of {seen} script_models drawn; models missing: {}",
            net["script_models_unloaded"]
        ));
    }
    // The scripts dress each player (`_teams` attaches a head to the body): the models they attached reach the client
    // and are drawn, not a head the client picked by faction.
    if players_seen >= 1.0
        && net["player_attachments"]
            .as_array()
            .is_none_or(|a| a.is_empty())
    {
        failures.push("players were drawn but none carried a model the scripts attached".into());
    }
    let script_errors = sv.get(&["script_errors"]);
    if script_errors > 0.0 {
        failures.push(format!(
            "{script_errors} script runtime errors on the server"
        ));
    }
    if shots < 1.0 {
        failures.push("the player never fired a shot the server ran".into());
    }
    // Recoil kicks the view and the kick is in the cmds the server aims the shots by.
    if shots >= 1.0 && c.get(&["view_kick_max"]) <= 0.0 {
        failures.push("shots were fired but the view never kicked".into());
    }
    if shots >= 1.0 && c.get(&["view_kick_in_cmd_max"]) <= 0.0 {
        failures.push("shots were fired but no cmd carried the kick".into());
    }
    if shots >= 1.0 && c.get(&["view_kick_settled"]) < 1.0 {
        failures.push("the view kicked but never came back to rest".into());
    }
    // The shots' kick moves the gun and the gun comes back. A weapon that cannot aim down sights has no recoil
    // spring, and a weapon whose file gives the gun no kick has nothing to move: both are untested, not failed.
    if c.get(&["gun_speed_given"]) > 0.0 && net["gun_has_spring"].as_bool() == Some(true) {
        if c.get(&["gun_recoil_max"]) <= 0.0 {
            failures.push("shots kicked the gun but its recoil never moved".into());
        } else if c.get(&["gun_recoil_settled"]) < 1.0 {
            failures.push("the gun recoiled but never came back to rest".into());
        }
    }
    // A hit the player took (the match is bots shooting back) showed on the screen: the red flash and a wedge.
    // Whether a bot lands one within the run is up to the match, with no hit the stage notes it as untested.
    let taken = c.get(&["damage_events"]);
    if taken >= 1.0 && c.get(&["damage_flash_max"]) <= 0.0 && c.get(&["damage_wedges_max"]) < 1.0 {
        failures.push(format!(
            "the player state counted {taken} hits but the HUD showed neither a flash nor a damage wedge"
        ));
    }
    let snd = &net["sound"];
    let heard = |names: &[&str]| -> f64 {
        names
            .iter()
            .map(|n| num(snd, &["by_channel", n]))
            .sum::<f64>()
    };
    if snd["ready"] != true {
        failures.push(format!("the sound tables never loaded: {}", snd["error"]));
    } else {
        let started = c.get(&["sound", "started"]);
        if started < 50.0 {
            failures.push(format!("only {started} sounds started"));
        }
        if heard(&["weapon", "weapon2d"]) < 1.0 {
            failures.push("no weapon fire was heard".into());
        }
        if heard(&["body", "body2d"]) < 1.0 {
            failures.push("no footsteps were heard".into());
        }
        if heard(&["ambient", "music"]) < 1.0 {
            failures.push("neither the map's ambience nor music played".into());
        }
        if c.get(&["sound", "lost_commands"]) > 0.0 {
            failures.push("the mixer's command queue overflowed".into());
        }
        if snd["failed"].as_array().is_some_and(|f| !f.is_empty()) {
            failures.push(format!("sound files failed: {}", snd["failed"]));
        }
        // An alias or effect name the game asks for that the tables do not have is the turret-class bug: it is
        // counted by the client and was never asserted. `ALLOWED_MISSING` names the ones a run may have (empty:
        // every miss is a defect today, and a known-benign one is added here with its reason).
        failures.extend(misses("sound", &snd["missing"], ALLOWED_MISSING_SOUND));
    }
    // What the shots did must have reached the client as events.
    let (impacts, pains) = (
        num(net, &["events", "bullet_impact"]),
        num(net, &["events", "player_pain"]),
    );
    if shots >= 1.0 && impacts < 1.0 {
        failures.push("shots were fired but the client saw no bullet impact event".into());
    }
    if hits >= 1.0 && pains < 1.0 {
        failures.push("enemies were hit but the client saw no pain event".into());
    }
    if impacts >= 1.0 && heard(&["bulletimpact"]) < 1.0 {
        failures.push(format!(
            "{impacts} bullet impacts reached the client but none was heard: {}",
            snd["by_channel"]
        ));
    }
    // The events became effects on screen.
    let fx = &net["fx"];
    let fires = num(net, &["events", "weapon_fire"]);
    if shots >= 1.0 && fires < 1.0 {
        failures.push("shots were fired but the client saw no weapon fire event".into());
    }
    if fires >= 1.0 && num(fx, &["played", "muzzle_flash"]) < 1.0 {
        failures.push("weapons fired but no muzzle flash played".into());
    }
    // A hit on the world, on a player (`bullet_impact_flesh`) or on one with blood off all play an impact effect.
    let impact_fx = num(fx, &["played", "bullet_impact"])
        + num(fx, &["played", "bullet_impact_flesh"])
        + num(fx, &["played", "bullet_impact_noblood"]);
    if impacts >= 1.0 && impact_fx < 1.0 {
        failures.push("bullet impacts happened but none played an effect".into());
    }
    if impact_fx >= 1.0 && num(fx, &["quads_max"]) < 1.0 {
        failures.push("impact effects played but no sprite was ever drawn".into());
    }
    if fx["look_missing"].as_array().is_some_and(|f| !f.is_empty()) {
        failures.push(format!(
            "vision or shock files the scripts named are missing: {}",
            fx["look_missing"]
        ));
    }
    failures.extend(misses("effect", &fx["missing"], ALLOWED_MISSING_EFFECTS));
    let skin_faults = c.get(&["skin_faults"]);
    if skin_faults > 0.0 {
        failures.push(format!(
            "{skin_faults} skinned surfaces had vertices outside their model (stretched triangles)"
        ));
    }
    failures.extend(c.missing());
    failures.extend(sv.missing());
    failures.extend(render_problem(&json));
    // A missing texture or a model outside the light grid shows as magenta: the last frame must hold none.
    match decode(&dir.join("screenshot.png")) {
        Ok((_, _, rgb)) => {
            let magenta = rgb
                .as_chunks::<3>()
                .0
                .iter()
                .filter(|p| {
                    let (r, g, b) = (i32::from(p[0]), i32::from(p[1]), i32::from(p[2]));
                    r > 60 && b > 60 && g * 3 < r.min(b)
                })
                .count();
            let share = magenta as f64 / (rgb.len() / 3).max(1) as f64;
            report
                .metrics
                .insert("client.magenta_pixel_share".into(), share);
            if share > MAX_MAGENTA {
                failures.push(format!(
                    "{:.2}% of the last frame is magenta (a missing texture or lighting)",
                    share * 100.0
                ));
            }
        }
        Err(e) => failures.push(format!("no screenshot to check for magenta: {e}")),
    }
    let m = &mut report.metrics;
    m.insert("sound.impacts_heard".into(), heard(&["bulletimpact"]));
    m.insert("fx.ragdolls".into(), num(fx, &["ragdolls"]));
    m.insert("client.script_models_drawn".into(), drawn);
    m.insert(
        "client.player_attachment_models".into(),
        net["player_attachments"]
            .as_array()
            .map_or(0.0, |a| a.len() as f64),
    );
    m.insert("fx.quads_max".into(), num(fx, &["quads_max"]));
    m.insert("fx.decals_max".into(), num(fx, &["decals_max"]));
    m.insert("fx.live_elems_max".into(), num(fx, &["live_elems_max"]));
    m.insert("sound.started".into(), num(snd, &["started"]));
    m.insert("sound.refused".into(), num(snd, &["refused"]));
    m.insert("sound.replaced".into(), num(snd, &["replaced"]));
    m.insert("sound.underruns".into(), num(snd, &["underruns"]));
    m.insert("client.snapshots_per_s".into(), snaps / secs);
    m.insert(
        "client.bytes_in_per_s".into(),
        num(net, &["bytes_in"]) / secs,
    );
    m.insert(
        "client.bytes_out_per_s".into(),
        num(net, &["bytes_out"]) / secs,
    );
    m.insert("client.prediction_corrections".into(), corrected);
    m.insert(
        "client.players_seen_max".into(),
        num(net, &["players_seen_max"]),
    );
    m.insert("client.walked".into(), num(net, &["path"]));
    m.insert("client.event_bullet_impacts".into(), impacts);
    m.insert("client.event_pains".into(), pains);
    m.insert("match.shots".into(), shots);
    m.insert("match.hits".into(), hits);
    m.insert("match.eye_jerk".into(), jerky);
    m.insert("match.kills".into(), num(srv, &["stats", "kills"]));
    m.insert("server.tick_ms_p50".into(), num(srv, &["tick_ms_p50"]));
    m.insert("server.tick_ms_p99".into(), num(srv, &["tick_ms_p99"]));
    m.insert(
        "server.net_ms_per_client_p50".into(),
        num(srv, &["net_ms_p50"]),
    );
    // Frame pacing of the client while playing.
    if let Ok(csv) = fs::read_to_string(dir.join("frames.raw.csv")) {
        let col = |i: usize| column(&csv, i);
        if let Some(p) = Percentiles::from_samples(&col(0)) {
            m.insert("client.cpu_ms_p50".into(), p.p50);
            m.insert("client.cpu_ms_p99".into(), p.p99);
            m.insert("client.cpu_ms_max".into(), p.max);
        }
        if let Some(p) = Percentiles::from_samples(&col(2)) {
            m.insert("client.frame_interval_ms_p50".into(), p.p50);
            m.insert("client.frame_interval_ms_p99".into(), p.p99);
            m.insert("client.frame_interval_ms_max".into(), p.max);
        }
        // Where each frame's milliseconds went: the only way to read what a hitch was (#436).
        if dir.join("frames.phases.csv").is_file() {
            report.files.push("match/frames.phases.csv".into());
        }
    }
    report.notes.push(format!(
        "{BOTS} bots, {SECS} s: {:.1} KB/s down, {:.1} KB/s up, server tick p50 {:.2} ms p99 {:.2} ms, {shots} shots {hits} hits",
        num(net, &["bytes_in"]) / secs / 1000.0,
        num(net, &["bytes_out"]) / secs / 1000.0,
        num(srv, &["tick_ms_p50"]),
        num(srv, &["tick_ms_p99"]),
    ));
    if taken < 1.0 {
        report
            .notes
            .push("damage feedback on screen untested: no bot hit the player".into());
    }
    if !failures.is_empty() {
        return Ok(fail(report, failures.join("; ")));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_counter_the_report_lacks_is_a_failure_not_a_zero() {
        let net = json!({"moved": 12.5, "view": {"steps": 3}, "snapshots": null});
        let mut c = Counters::new("net", &net);
        assert_eq!(c.get(&["moved"]), 12.5);
        assert_eq!(c.get(&["view", "steps"]), 3.0);
        assert_eq!(c.missing(), None);
        // A missing counter compares as neither over nor under any threshold, and is named once at the end.
        let gone = c.get(&["view", "step_max"]);
        assert!(gone.is_nan() && gone.partial_cmp(&0.0).is_none());
        assert!(c.get(&["snapshots"]).is_nan(), "null is not a number");
        let why = c.missing().unwrap();
        assert!(
            why.contains("net.view.step_max") && why.contains("net.snapshots"),
            "{why}"
        );
        // The lenient reader is for the tallies that are only written when something happened.
        assert_eq!(num(&net, &["events", "weapon_fire"]), 0.0);
    }

    #[test]
    fn a_name_the_tables_lack_fails_and_an_absent_list_fails() {
        assert_eq!(misses("sound", &json!([]), &[]), None);
        assert!(
            misses("sound", &json!(["weap_ak47_fire"]), &[])
                .unwrap()
                .contains("weap_ak47_fire")
        );
        assert_eq!(
            misses("sound", &json!(["benign_alias"]), &["benign_alias"]),
            None,
            "an allow-listed name does not fail"
        );
        assert_eq!(misses("sound", &Value::Null, &[]), None);
        assert!(
            misses("sound", &json!({"weap_x_fire": 1}), &[])
                .unwrap()
                .contains("weap_x_fire"),
            "a count object's names are the misses"
        );
    }

    /// A phases file of `n` steady frames with the given columns, `(name, value)`, and the rest zero.
    fn steady_phases(n: usize, cols: &[(&str, f64)]) -> Phases {
        let header = LEAVES.join(",");
        let row: Vec<String> = LEAVES
            .iter()
            .map(|l| {
                cols.iter()
                    .find(|(c, _)| c == l)
                    .map_or("0".to_owned(), |(_, v)| v.to_string())
            })
            .collect();
        let rows = vec![row.join(","); n];
        Phases::parse(&format!("{header}\n{}\n", rows.join("\n")))
    }

    #[test]
    fn a_pipeline_missing_while_playing_is_a_hitch_a_pass_is_not() {
        let steady = vec![16.0; 1000];
        let none = Phases::parse("");
        let passes = json!({"late_builds": ["7 pass wc/x tech 9 World"]});
        assert!(hitch_failures(&passes, &steady, &none).is_empty());
        let pipe =
            json!({"late_builds": ["7 pass wc/x tech 9 World", "7 pipeline wc/x Target {}"]});
        let why = hitch_failures(&pipe, &steady, &none);
        assert_eq!(why.len(), 1);
        assert!(why[0].contains("1 pipelines"), "{why:?}");
    }

    #[test]
    fn a_precache_list_prepared_in_one_frame_is_a_hitch_a_model_is_not() {
        let steady = vec![16.0; 1000];
        let mut phases = steady_phases(1000, &[("r_submit", 5.0)]);
        phases.names.push("late_passes".into());
        phases.names.push("late_pass_ms".into());
        for r in &mut phases.rows {
            r.extend([0.0, 0.0]);
        }
        let (lp, lpm) = (phases.names.len() - 2, phases.names.len() - 1);
        // One model's dozen passes at first sight.
        (phases.rows[105][lp], phases.rows[105][lpm]) = (14.0, 2.0);
        assert!(hitch_failures(&json!({}), &steady, &phases).is_empty());
        // The server's whole list in one frame (241 passes, 31 ms on the test host).
        (phases.rows[105][lp], phases.rows[105][lpm]) = (241.0, 31.3);
        let why = hitch_failures(&json!({}), &steady, &phases);
        assert_eq!(why.len(), 1, "{why:?}");
        assert!(
            why[0].contains("frame 105 prepared 241 material passes itself (31 ms)"),
            "{why:?}"
        );
    }

    #[test]
    fn a_clock_that_keeps_jumping_is_a_hitch_the_first_reading_is_not() {
        let steady = vec![16.0; 1000];
        let none = Phases::parse("");
        assert!(
            hitch_failures(&json!({"net": {"clock_jumps": [1, 0]}}), &steady, &none).is_empty()
        );
        assert_eq!(
            hitch_failures(&json!({"net": {"clock_jumps": [36, 0]}}), &steady, &none).len(),
            1
        );
    }

    #[test]
    fn spikes_are_counted_against_the_median_not_a_fixed_speed() {
        let report = json!({});
        let none = Phases::parse("");
        // A slow machine: everything at 90 ms is steady, not a hitch.
        assert!(hitch_failures(&report, &vec![90.0; 1000], &none).is_empty());
        let mut frames = vec![16.0; 1000];
        frames[10] = 300.0;
        assert!(
            hitch_failures(&report, &frames, &none).is_empty(),
            "one in a thousand is the scheduler"
        );
        // Tens of milliseconds over the usual, a few times a second: dropped frames.
        for f in frames.iter_mut().step_by(50) {
            *f = 60.0;
        }
        assert_eq!(hitch_failures(&report, &frames, &none).len(), 1);
    }

    #[test]
    fn a_hitch_names_the_phase_that_held_it_or_the_machine() {
        // A usual frame: 1 ms of net, 5 ms of submit, 4 ms of draws, 6 ms of present.
        let usual = [
            ("net_ms", 1.0),
            ("r_submit", 5.0),
            ("r_main_draws", 4.0),
            ("present_ms", 6.0),
        ];
        let mut frames = vec![16.0; 1000];
        for f in frames.iter_mut().step_by(40) {
            *f = 76.0;
        }
        // The 60 ms extra sits in the network frame every time: the client blocks there.
        let mut phases = steady_phases(1000, &usual);
        let net = phases.col("net_ms").unwrap();
        for i in (0..1000).step_by(40) {
            phases.rows[i][net] = 61.0;
        }
        let why = hitch_failures(&json!({}), &frames, &phases);
        assert_eq!(why.len(), 1, "{why:?}");
        assert!(why[0].contains("net held the stalls"), "{why:?}");
        assert!(why[0].contains("(76 ms): net 61"), "{why:?}");
        // Every phase four times its usual cost at once: the process was not scheduled.
        let mut phases = steady_phases(1000, &usual);
        for i in (0..1000).step_by(40) {
            for v in &mut phases.rows[i] {
                *v *= 4.75;
            }
        }
        let why = hitch_failures(&json!({}), &frames, &phases);
        assert_eq!(why.len(), 1, "{why:?}");
        assert!(why[0].contains("every phase stretched alike"), "{why:?}");
        assert!(why[0].contains("not being scheduled"), "{why:?}");
    }
}
