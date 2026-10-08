// SPDX-License-Identifier: GPL-3.0-only
//! Stage 2: the headless server driven by a console scenario.
//!
//! The harness runs the server in this process through its console: `map` boots a map, `wait`
//! runs the 30 Hz loop for that long, commands the server lacks are
//! reported as skipped. Every tick feeds `ticks.csv`; RSS is sampled once a second. Script
//! runtime errors fail the stage.
use crate::perf::{Percentiles, TickRecorder, cgroup_peak, peak_rss, process_rss};
use crate::script::{Command, Console};
use crate::stage::{self, StageCtx, StageReport, Status};
use server::server::{COMMANDS, Server, TICK_SUBSYSTEMS, TickSample};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The dedicated server's memory budget (ticket: 32 players in 512 MB).
const RSS_BUDGET: u64 = 512 << 20;

struct ServerConsole {
    server: Rc<RefCell<Server>>,
    map_loads: Rc<RefCell<Vec<(String, f64)>>>,
}

impl Console for ServerConsole {
    fn has_command(&self, name: &str) -> bool {
        COMMANDS.contains(&name)
    }

    fn exec(&mut self, cmd: &Command) -> Result<(), String> {
        let mut s = self.server.borrow_mut();
        let line: String = std::iter::once(cmd.name.clone())
            .chain(cmd.args.iter().map(|a| format!("\"{a}\"")))
            .collect::<Vec<_>>()
            .join(" ");
        s.exec_line(&line)?;
        if matches!(cmd.name.as_str(), "map" | "devmap") {
            let m = s.map_name().unwrap_or("?").to_owned();
            self.map_loads.borrow_mut().push((m, s.map_load_ms));
        }
        Ok(())
    }
}

pub fn run(ctx: &StageCtx, name: &str, src: &str) -> StageReport {
    let Some(install) = ctx.install.as_deref() else {
        return StageReport::new(name, Status::Skipped).with_reason("install not found");
    };
    let args: Vec<String> = ["+set", "net_port", "0"].map(String::from).to_vec();
    let boot = Instant::now();
    let server = match Server::boot(install, &args, false) {
        Ok(s) => s,
        Err(e) => return StageReport::new(name, Status::Failed).with_reason(e),
    };
    let boot_ms = boot.elapsed().as_secs_f64() * 1000.0;
    let server = Rc::new(RefCell::new(server));
    let recorder = match TickRecorder::create(&ctx.dir.join("ticks.csv"), &TICK_SUBSYSTEMS) {
        Ok(r) => Rc::new(RefCell::new(Some(r))),
        Err(e) => return StageReport::new(name, Status::Failed).with_reason(e.to_string()),
    };
    let rss = Rc::new(RefCell::new(Vec::<f64>::new()));
    {
        let rec = recorder.clone();
        let rss = rss.clone();
        let mut ticks = 0u32;
        let mut last = 0u64;
        server.borrow_mut().on_tick = Some(Box::new(move |t: &TickSample| {
            if ticks.is_multiple_of(30)
                && let Some(r) = process_rss()
            {
                last = r;
                rss.borrow_mut().push(r as f64);
            }
            ticks += 1;
            if let Some(r) = rec.borrow_mut().as_mut() {
                let _ = r.push(t.total_ms, &t.subsystems(), last);
            }
        }));
    }
    let map_loads = Rc::new(RefCell::new(Vec::new()));
    let mut console = ServerConsole {
        server: server.clone(),
        map_loads: map_loads.clone(),
    };
    let sleep = {
        let server = server.clone();
        move |d: Duration| server.borrow_mut().run_for(d)
    };
    let mut report = stage::run_script_on(name, src, &mut console, sleep);
    drop(console);

    let s = server.borrow();
    report.metrics.insert("server.boot_ms".into(), boot_ms);
    report.metrics.insert("server.ticks".into(), s.ticks as f64);
    for (m, ms) in map_loads.borrow().iter() {
        report.metrics.insert(format!("map_load_ms.{m}"), *ms);
    }
    let st = s.game.stats;
    for (k, v) in [
        ("match.kills", st.kills),
        ("match.deaths", st.deaths),
        ("match.spawns", st.spawns),
        ("match.respawns", st.respawns),
        ("match.shots", st.shots),
        ("match.hits", st.hits),
        ("match.rounds_ended", st.matches_ended),
    ] {
        report.metrics.insert(k.into(), v as f64);
    }
    for (m, ms, nodes) in &s.game.nav_loads {
        report.metrics.insert(format!("nav_ms.{m}"), f64::from(*ms));
        report
            .metrics
            .insert(format!("nav_nodes.{m}"), f64::from(*nodes));
    }
    // Peak is the kernel's high-water mark, which includes the map-load spike the 1 Hz
    // samples can miss; steady is the median of the second half of the samples.
    let rss = rss.borrow();
    if let Some(p) = peak_rss() {
        report.metrics.insert("rss.peak_bytes".into(), p as f64);
        // The server budget is 512 MB; memory use does not depend on runner speed.
        if p > RSS_BUDGET {
            report.status = Status::Failed;
            report
                .notes
                .push(format!("rss: peak {p} B is over the {RSS_BUDGET} B budget"));
        }
    }
    if let Some(p) = cgroup_peak() {
        report
            .metrics
            .insert("rss.cgroup_peak_bytes".into(), p as f64);
    }
    match Percentiles::from_samples(&rss[rss.len() / 2..]) {
        Some(p) => {
            report.metrics.insert("rss.steady_bytes".into(), p.p50);
            if let Some(peak) = peak_rss()
                && (peak as f64) < p.p50
            {
                report.status = Status::Failed;
                report.notes.push(format!(
                    "rss: kernel peak {peak} B is below the steady sample {} B",
                    p.p50
                ));
            }
        }
        None if s.ticks > 0 => {
            report.status = Status::Failed;
            report
                .notes
                .push("rss: no samples over a running server".into());
        }
        None => {}
    }
    if let Some(r) = process_rss() {
        report.metrics.insert("rss.final_bytes".into(), r as f64);
    }
    if let Some(r) = recorder.borrow_mut().take() {
        match r.finish() {
            Ok(series) => {
                if let (Some(bot), Some(all)) = (series.get("tick.bot_ms"), series.get("tick.ms"))
                    && all.mean > 0.0
                {
                    report
                        .metrics
                        .insert("tick.bot_share".into(), bot.mean / all.mean);
                }
                report.series.extend(series);
            }
            Err(e) => report.notes.push(format!("ticks.csv: {e}")),
        }
        report.files.push("ticks.csv".into());
    }
    let stubs = s.stub_calls();
    if !stubs.is_empty() {
        let list: Vec<String> = stubs.iter().map(|(n, c)| format!("{n}x{c}")).collect();
        report.notes.push(format!(
            "builtins that ran as logged no-ops: {}",
            list.join(", ")
        ));
    }
    let missing = s.missing_builtins();
    if !missing.is_empty() {
        report.notes.push(format!(
            "{} bound builtins have no implementation",
            missing.len()
        ));
    }
    if !s.all_script_errors.is_empty() {
        report.notes.extend(
            s.all_script_errors
                .iter()
                .take(5)
                .map(|(m, e)| format!("{m}: {e}")),
        );
        if report.status == Status::Passed {
            report.status = Status::Failed;
            report.reason = Some(format!(
                "{} script runtime errors",
                s.all_script_errors.len()
            ));
        }
    }
    report.environment = Some(serde_json::json!({ "cvars": s.game.cvars.iter()
        .map(|c| (c.name.clone(), c.value.clone()))
        .collect::<std::collections::BTreeMap<_, _>>() }));
    report
}
