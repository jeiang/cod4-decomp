// SPDX-License-Identifier: GPL-3.0-only
//! #357: the mp_crash garage is a match scene, not a serverless flythrough. The car is a script_model and the
//! loose tyre is a client-side dynamic prop. The client counts the car in every snapshot it accepts after the
//! teleport to the issue pose (the control `watch`), and each one must carry it. Compare each class against the
//! same live camera with it hidden.
//! Reference: KisakCOD (GPL-3.0), game/g_scr_mover.cpp:201-207 (SP_script_model), cgame/cg_ents.cpp:847-883
//! (CG_ScriptMover); DynEntity/DynEntity_client.cpp:82-119 (DynEntCl_InitEntities), gfx_d3d/r_dpvs.cpp:1451-1457
//! (R_AddDynEntToCell). CoD4 MP 1.8, used as a guide for 1.7.
use super::client_flythrough::locate_client;
use super::client_models::{decode, no_display};
use super::client_session::render_problem;
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::json;
use std::io;
use std::path::Path;
use std::time::Duration;

const NAME: &str = "client-garage";
const CAR: &str = "vehicle_80s_sedan1_green_destructible_mp";

fn changed(a: &Path, b: &Path, region: [f64; 4]) -> Result<f64, String> {
    let (w, h, a) = decode(a)?;
    let (bw, bh, b) = decode(b)?;
    if (w, h) != (bw, bh) {
        return Err("garage captures differ in size".into());
    }
    let [x0, y0, x1, y1] = region;
    let (x0, y0, x1, y1) = (
        (x0 * f64::from(w)) as usize,
        (y0 * f64::from(h)) as usize,
        (x1 * f64::from(w)) as usize,
        (y1 * f64::from(h)) as usize,
    );
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * w as usize + x) * 3;
            if (0..3).any(|c| a[i + c].abs_diff(b[i + c]) > 24) {
                n += 1;
            }
        }
    }
    Ok(n as f64 / ((x1 - x0) * (y1 - y0)) as f64)
}

/// Writes `v` to `path` as pretty-printed JSON.
fn write_json(path: &Path, v: &serde_json::Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(v).map_err(|e| e.to_string())?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

fn drive(c: &mut Driven, report: &mut StageReport) -> Result<(), String> {
    let secs = Duration::from_secs;
    // `devmap` is the player's own path that enables cheats (sv_cheats is init/write protected).
    c.ask(json!({"cmd": "exec", "line": "devmap mp_crash"}))?;
    c.until(
        json!({"menu.open.0": "team_marinesopfor"}),
        secs(120),
        "team menu did not open",
    )?;
    c.click("Marines")?;
    c.until(
        json!({"menu.open.0": "changeclass"}),
        secs(30),
        "class menu did not open",
    )?;
    c.click("Assault")?;
    c.until(
        json!({"player.health": {"gt": 0}}),
        secs(120),
        "player did not spawn",
    )?;
    for line in ["noclip", "set cg_draw2D 0", "set cg_drawGun 0"] {
        c.ask(json!({"cmd": "exec", "line": line}))?;
    }
    c.ask(json!({"cmd": "step", "frames": 30}))?;
    // A script_model at the garage car's spot (within `r` units in the plane), by kind and origin alone.
    let near = |e: &serde_json::Value, r: f64| {
        e["kind"] == "script_model"
            && e["origin"][0]
                .as_f64()
                .zip(e["origin"][1].as_f64())
                .is_some_and(|(x, y)| (x - 58.0).hypot(y - 2112.5) < r)
    };
    // The newest snapshot's garage script_models. The list and the number of the snapshot it came from are read in
    // one request, so a sample never mixes two snapshots.
    let sample = |c: &mut Driven| -> Result<serde_json::Value, String> {
        let entities = c.state("entities")?;
        let list = entities["list"].as_array().cloned().unwrap_or_default();
        let garage: Vec<_> = list
            .iter()
            .filter(|e| {
                near(e, 400.0)
                    || e["model_name"]
                        .as_str()
                        .is_some_and(|m| m.starts_with("vehicle_"))
            })
            .map(|e| json!({"n": e["number"], "k": e["kind"], "m": e["model_name"], "o": e["origin"]}))
            .collect();
        Ok(json!({
            "snapshot": entities["snapshot"],
            "server_time": entities["server_time"],
            "count": entities["count"],
            "numbers": list.iter().map(|e| e["number"].clone()).collect::<Vec<_>>(),
            "car": list.iter().any(|e| near(e, 1.0)),
            "car_model": list.iter().any(|e| near(e, 1.0) && e["model_name"] == CAR),
            "garage": garage,
        }))
    };
    let mut spawn = sample(c)?;
    spawn["origin"] = c.state("player")?["origin"].clone();
    // TeleportPlayer adds one unit after subtracting view height; ask for 323.5 to put the render eye at 324.5.
    c.ask(json!({"cmd": "exec", "line": "setviewpos 200.8 2267.5 323.5 175.3 0"}))?;
    // Render frames are not server ticks: judge what the server sends from the garage only once a snapshot carries
    // the teleported player; an earlier reading describes the spawn, not the issue pose. The wait names the snapshot
    // it saw, the newest one when the position matched.
    let arrived = c.until(
        json!({
            "player.origin.0": {"gt": 199.8, "lt": 201.8},
            "player.origin.1": {"gt": 2266.5, "lt": 2268.5},
            "player.origin.2": {"gt": 263.5, "lt": 265.5},
            "player.snapshot": {"gt": 0},
        }),
        secs(5),
        "no snapshot arrived from the teleported position",
    )?;
    let teleported = arrived["observed"]["player.snapshot"]
        .as_u64()
        .ok_or_else(|| format!("the teleport wait did not say which snapshot it saw: {arrived}"))?;
    // Every snapshot sent from the teleported eye must carry the car, not only the ones a reading happens to catch.
    // Find the car in the newest snapshot, then let the client count it in each snapshot it accepts from the one the
    // wait saw onward: a snapshot that drops it between two readings is counted too.
    let seen = c.state("entities")?;
    let timeline = c.dir.join("teleport-timeline.json");
    let list = seen["list"].as_array().cloned().unwrap_or_default();
    let Some(car) = list.iter().find(|e| e["actor"] != true && near(e, 1.0)) else {
        write_json(
            &timeline,
            &json!({"spawn": spawn, "teleported": teleported, "seen": seen}),
        )?;
        return Err(format!(
            "garage car script_model missing from snapshot {} after the teleport (control/teleport-timeline.json)",
            seen["snapshot"]
        ));
    };
    if car["model_name"] != CAR {
        write_json(
            &timeline,
            &json!({"spawn": spawn, "teleported": teleported, "seen": seen}),
        )?;
        return Err(format!(
            "garage car at (58, 2112.5) is {} in snapshot {}, not {CAR} (control/teleport-timeline.json)",
            car["model_name"], seen["snapshot"]
        ));
    }
    c.ask(json!({
        "cmd": "watch",
        "number": car["number"],
        "model": car["model"],
        "from": teleported,
    }))?;
    // A wait that runs out still leaves counts to judge; the checks below name what fell short.
    c.wait(&json!({"watch.received": {"ge": 40}}), secs(20))?;
    let watch = c.state("watch")?;
    c.ask(json!({"cmd": "watch", "clear": true}))?;
    write_json(
        &timeline,
        &json!({
            "spawn": spawn,
            "teleported": teleported,
            "seen": {"snapshot": seen["snapshot"], "server_time": seen["server_time"], "car": car},
            "watch": watch,
        }),
    )?;
    let n = |k: &str| watch[k].as_u64().unwrap_or(0);
    let (first, last) = (&watch["first"], &watch["last"]);
    if first.as_u64() != Some(teleported) {
        return Err(format!(
            "the car's count began at snapshot {first}, not at {teleported}, the one the teleport wait saw (control/teleport-timeline.json)"
        ));
    }
    if n("breaks") != 0 {
        return Err(format!(
            "snapshots {first}..{last} after the teleport were not consecutive: {} missed, first gap {} (control/teleport-timeline.json)",
            n("missed"),
            watch["gaps"][0]
        ));
    }
    if n("present") != n("received") {
        return Err(format!(
            "garage car script_model missing from {} of {} post-teleport snapshots, first {} (control/teleport-timeline.json)",
            n("received").saturating_sub(n("present")),
            n("received"),
            watch["absent"][0]
        ));
    }
    if n("matched") != n("present") {
        return Err(format!(
            "garage car is not {CAR} in {} post-teleport snapshots, first [snapshot, model] {} (control/teleport-timeline.json)",
            n("present").saturating_sub(n("matched")),
            watch["wrong_model"][0]
        ));
    }
    if n("received") < 40 {
        return Err(format!(
            "only {} post-teleport snapshots arrived in 20 s (control/teleport-timeline.json)",
            n("received")
        ));
    }
    c.ask(json!({"cmd": "step", "frames": 60}))?;
    let view = c.state("view")?;
    let origin = view["origin"].as_array().ok_or("missing render eye")?;
    for (i, want) in [200.8, 2267.5, 324.5].into_iter().enumerate() {
        if origin
            .get(i)
            .and_then(|v| v.as_f64())
            .is_none_or(|v| (v - want).abs() > 2.0)
        {
            return Err(format!("setviewpos did not reach the issue pose: {view}"));
        }
    }
    let entities = c.state("entities")?;
    let car = entities["list"].as_array().is_some_and(|es| {
        es.iter().any(|e| {
            e["kind"] == "script_model"
                && e["model_name"] == CAR
                && e["origin"][0]
                    .as_f64()
                    .is_some_and(|x| (x - 58.0).abs() < 1.0)
                && e["origin"][1]
                    .as_f64()
                    .is_some_and(|y| (y - 2112.5).abs() < 1.0)
        })
    });
    let net = c.state("net")?;
    write_json(
        &c.dir.join("models.json"),
        &json!({"entities": entities, "net": net, "view": view}),
    )?;
    if !car {
        return Err("garage car script_model was not published at (58, 2112.5, 309.125)".into());
    }
    let live = c.capture("garage")?;
    if let Some(problem) = render_problem(&json!({"render": c.state("render")?})) {
        return Err(problem);
    }
    c.ask(json!({"cmd": "exec", "line": "drawscriptmodels 0"}))?;
    c.ask(json!({"cmd": "step", "frames": 10}))?;
    let bare_car = c.capture("no-car")?;
    c.ask(json!({"cmd": "exec", "line": "drawscriptmodels 1"}))?;
    c.ask(json!({"cmd": "exec", "line": "drawprops 0"}))?;
    c.ask(json!({"cmd": "step", "frames": 10}))?;
    let bare_tyre = c.capture("no-tyre")?;
    // Ignore HUD and weapon regions. Each object's absence must change its own part of the garage image.
    for (label, bare, region) in [
        ("car", bare_car, [0.0, 0.25, 0.2, 0.5]),
        ("tyre", bare_tyre, [0.29, 0.70, 0.35, 0.80]),
    ] {
        let share = changed(&live, &bare, region)?;
        report
            .metrics
            .insert(format!("{label}.changed_pixel_share"), share);
        if share < 0.005 {
            return Err(format!(
                "garage {label} changes only {:.3}% of its image region (need 0.5%): not drawn",
                share * 100.0
            ));
        }
    }
    report.notes.push("garage car and tyre change separate image regions at the issue #357 pose; serverless flythroughs intentionally contain neither entity class".into());
    Ok(())
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(install) = &ctx.install else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("no install"));
    };
    let Some(client) = locate_client() else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("no client binary"));
    };
    if let Some(r) = no_display(&client, NAME)? {
        return Ok(r);
    }
    let mut report = StageReport::new(NAME, Status::Passed);
    let mut c = match Driven::start(
        &client,
        install,
        &ctx.dir.join("control"),
        &["--bots", "0", "--duration", "300"],
    ) {
        Ok(c) => c,
        Err(e) => return Ok(StageReport::new(NAME, Status::Failed).with_reason(e)),
    };
    let result = drive(&mut c, &mut report);
    let status = c.finish(Duration::from_secs(30));
    report.files.extend(
        [
            "garage.png",
            "no-car.png",
            "no-tyre.png",
            "models.json",
            "teleport-timeline.json",
        ]
        .map(|f| format!("control/{f}")),
    );
    if let Err(e) = result {
        report.status = Status::Failed;
        report.reason = Some(e);
    } else if !status.is_some_and(|s| s.success()) {
        report.status = Status::Failed;
        report.reason = Some("garage client did not exit successfully".into());
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(path: &Path, white_left: bool) {
        let mut bytes = vec![0; 10 * 10 * 3];
        if white_left {
            for y in 0..10 {
                for x in 0..5 {
                    bytes[(y * 10 + x) * 3..(y * 10 + x + 1) * 3].fill(255);
                }
            }
        }
        let mut encoder = png::Encoder::new(std::fs::File::create(path).unwrap(), 10, 10);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&bytes)
            .unwrap();
    }

    #[test]
    fn missing_models_fail_and_changes_outside_the_object_region_do_not_count() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.png"), dir.path().join("b.png"));
        image(&a, false);
        image(&b, true);
        assert_eq!(changed(&a, &a, [0.0, 0.0, 0.5, 1.0]).unwrap(), 0.0);
        assert_eq!(changed(&a, &b, [0.0, 0.0, 0.5, 1.0]).unwrap(), 1.0);
        assert_eq!(changed(&a, &b, [0.5, 0.0, 1.0, 1.0]).unwrap(), 0.0);
    }
}
