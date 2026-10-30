// SPDX-License-Identifier: GPL-3.0-only
//! #357: the mp_crash garage is a match scene, not a serverless flythrough. The car is a script_model and the
//! loose tyre is a client-side dynamic prop. Compare each class against the same live camera with it hidden.
//! Reference: KisakCOD (GPL-3.0), game/g_scr_mover.cpp:201-207 (SP_script_model), cgame/cg_ents.cpp:847-883
//! (CG_ScriptMover); DynEntity/DynEntity_client.cpp:82-119 (DynEntCl_InitEntities), gfx_d3d/r_dpvs.cpp:1451-1457
//! (R_AddDynEntToCell). CoD4 MP 1.8, used as a guide for 1.7.
use super::client_flythrough::locate_client;
use super::client_models::{decode, no_display};
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
    // TeleportPlayer adds one unit after subtracting view height; ask for 323.5 to put the render eye at 324.5.
    c.ask(json!({"cmd": "exec", "line": "setviewpos 200.8 2267.5 323.5 175.3 0"}))?;
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
    if !car {
        return Err("garage car script_model was not published at (58, 2112.5, 309.125)".into());
    }
    let net = c.state("net")?;
    std::fs::write(
        c.dir.join("models.json"),
        serde_json::to_vec_pretty(&json!({"entities": entities, "net": net, "view": view}))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let live = c.capture("garage")?;
    c.ask(json!({"cmd": "exec", "line": "drawscriptmodels 0"}))?;
    c.ask(json!({"cmd": "step", "frames": 10}))?;
    let bare_car = c.capture("no-car")?;
    c.ask(json!({"cmd": "exec", "line": "drawscriptmodels 1"}))?;
    c.ask(json!({"cmd": "exec", "line": "drawprops 0"}))?;
    c.ask(json!({"cmd": "step", "frames": 10}))?;
    let bare_tyre = c.capture("no-tyre")?;
    // Ignore HUD and weapon regions. Each object's absence must change its own part of the garage image.
    for (label, bare, region) in [
        ("car", bare_car, [0.0, 0.15, 0.3, 0.65]),
        ("tyre", bare_tyre, [0.2, 0.55, 0.6, 0.95]),
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
        ["garage.png", "no-car.png", "no-tyre.png", "models.json"].map(|f| format!("control/{f}")),
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
