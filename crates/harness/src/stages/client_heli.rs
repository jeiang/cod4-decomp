// SPDX-License-Identifier: GPL-3.0-or-later
//! `client-heli`: the real client sees a helicopter. A bot is given the helicopter reward (`devhardpoint`) and
//! calls it in; once the server counts it, the player is put behind the helicopter looking at it
//! (`devheli view`) and a screenshot is saved. The client must have drawn at least one `VEHICLE` entity, and the
//! picture must differ from the same view with vehicles left out (`drawvehicles 0`). Needs a display and the
//! install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::{decode, no_display};
use super::client_session::run_client;
use crate::stage::{StageCtx, StageReport, Status};
use std::io;
use std::time::Duration;

const NAME: &str = "client-heli";
const LIMIT: Duration = Duration::from_secs(300);
const ARGS: &[&str] = &["--listen", "--bots", "3"];
// The bot selects its action slot 4 weapon some seconds after spawning when it is not fighting; the view is
// placed and both pictures taken in quick succession, as the helicopter flies on and the player falls.
const STEPS: &str = "ingame=120,wait=2,server=devhardpoint bot helicopter_mp,counter=helicopters:1:120,wait=2,\
server=devheli view human,shot=heli,set=drawvehicles 0,shot=bare,wait=1";
/// Share of the picture that must differ with the helicopter in it, and by how much per channel.
const MIN_CHANGED: f64 = 0.002;
const CHANNEL_DELTA: i32 = 24;

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
    let dir = ctx.dir.join("heli");
    let fail = |r: StageReport, why: String| r.with_status(Status::Failed).with_reason(why);
    let report = match run_client(
        &client,
        install,
        &dir,
        &dir.join("config"),
        ARGS,
        STEPS,
        LIMIT,
    ) {
        Ok(r) => r,
        Err(e) => return Ok(fail(StageReport::new(NAME, Status::Passed), e)),
    };
    let mut out = StageReport::new(NAME, Status::Passed);
    out.files
        .extend(["heli/ui-script.json", "heli/heli.png", "heli/bare.png"].map(String::from));
    let count = |v: &serde_json::Value| v.as_u64().unwrap_or(0);
    let helicopters = count(&report["objectives"]["helicopters"]);
    let shots = count(&report["objectives"]["heli_shots"]);
    let drawn = count(&report["net"]["vehicles_max_drawn"]);
    out.metrics.insert("helicopters".into(), helicopters as f64);
    out.metrics.insert("heli_shots".into(), shots as f64);
    out.metrics.insert("vehicles_drawn".into(), drawn as f64);
    let mut problems = Vec::new();
    if let Some(steps) = report["steps"].as_array() {
        for s in steps
            .iter()
            .filter(|s| s["ok"] != serde_json::Value::Bool(true))
        {
            problems.push(format!("step {} failed ({})", s["step"], s["note"]));
        }
    }
    if helicopters == 0 {
        problems.push("the helicopter was never called in".into());
    }
    if drawn == 0 {
        problems.push(format!(
            "the client drew no vehicle (unloaded models: {})",
            report["net"]["vehicles_unloaded"]
        ));
    }
    match (decode(&dir.join("heli.png")), decode(&dir.join("bare.png"))) {
        (Ok(a), Ok(b)) if (a.0, a.1) == (b.0, b.1) => {
            let changed =
                a.2.as_chunks::<3>().0.iter()
                    .zip(b.2.as_chunks::<3>().0.iter())
                    .filter(|(p, q)| {
                        p.iter()
                            .zip(q.iter())
                            .any(|(x, y)| (i32::from(*x) - i32::from(*y)).abs() > CHANNEL_DELTA)
                    })
                    .count();
            let share = changed as f64 / (a.0 as usize * a.1 as usize) as f64;
            out.metrics.insert("changed_pixel_share".into(), share);
            out.notes.push(format!(
                "the helicopter changes {:.2}% of the picture",
                share * 100.0
            ));
            if share < MIN_CHANGED {
                problems.push(format!(
                    "the picture changed by only {:.3}% without vehicles (need {:.1}%): is the helicopter in view?",
                    share * 100.0,
                    MIN_CHANGED * 100.0
                ));
            }
        }
        (Ok(_), Ok(_)) => problems.push("the two screenshots differ in size".into()),
        (Err(e), _) | (_, Err(e)) => problems.push(e),
    }
    if !problems.is_empty() {
        return Ok(fail(out, problems.join("; ")));
    }
    Ok(out)
}
