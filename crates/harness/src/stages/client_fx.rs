// SPDX-License-Identifier: GPL-3.0-only
//! `client-fx`: runs `cod4e --fx-selftest` on the real effects. With no window and no GPU it plays stock effects
//! against a stock map and asserts what a match shows: an explosion draws sprites and then ends, a bullet into the
//! floor leaves a decal clipped to it, a shot plays its muzzle flash and ejects a shell, a burst of tracer rounds draws
//! one beam each in the stock tracer material, every effect material that names another zone's is the one that zone
//! defines (#453), a real GSC playfxontag event keeps existing elements bolted to a model moved after 500 ms and
//! rejects unknown tags (#421), an effect bolted to a projectile follows the projectile model the match draws and a
//! bolt on an entity never drawn is reported, and vision and shock files change the picture. Needs the install;
//! skips without it.
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::Value;
use std::io;
use std::process::Command;

const NAME: &str = "client-fx";

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(client) = super::client_flythrough::locate_client() else {
        return Ok(StageReport::new(NAME, Status::Skipped)
            .with_reason("cod4e client binary not found next to the harness"));
    };
    let Some(install) = &ctx.install else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("no install"));
    };
    let out = Command::new(client)
        .arg("--install")
        .arg(install)
        .args(["--fx-selftest", "--map", "mp_crash", "--out"])
        .arg(&ctx.dir)
        .output()?;
    let mut r = StageReport::new(
        NAME,
        if out.status.success() {
            Status::Passed
        } else {
            Status::Failed
        },
    );
    r.exit_code = out.status.code();
    r.files.push("fx.json".into());
    if out.status.success() {
        if let Ok(Value::Object(m)) = serde_json::from_slice(&out.stdout) {
            for (k, v) in m {
                if let Some(n) = v.as_f64() {
                    r.metrics.insert(format!("fx.{k}"), n);
                }
            }
        }
        // The burst of five tracer rounds is five beams in flight.
        if r.metrics.get("fx.tracers_in_flight") != Some(&5.0) {
            r.status = Status::Failed;
            r.reason = Some(format!(
                "a burst of five tracer rounds should draw five beams, the report says {:?}",
                r.metrics.get("fx.tracers_in_flight")
            ));
        }
        // A sprite or model material still named behind a comma is one no zone linked: its sprites never draw.
        if r.metrics.get("fx.unresolved_materials") != Some(&0.0) {
            r.status = Status::Failed;
            r.reason = Some(format!(
                "every effect material should be defined in full, the report says {:?} are not (see fx.json unresolved_material_names)",
                r.metrics.get("fx.unresolved_materials")
            ));
        }
        // Missing metrics fail too: an origin/main client cannot silently pass an unimplemented bolt path.
        for (metric, minimum, maximum) in [
            ("bolted_events", 1.0, 1.0),
            ("bolted_tag_displacement", 3.0, f64::INFINITY),
            ("bolted_element_displacement", 3.0, f64::INFINITY),
            ("bolted_follow_error", 0.0, 1.0),
            ("bolted_unknown_tag_error", 1.0, 1.0),
            ("bolted_missile_tag_displacement", 3.0, f64::INFINITY),
            ("bolted_missile_follow_error", 0.0, 1.0),
            ("bolted_unposed_reported", 1.0, 1.0),
        ] {
            let key = format!("fx.{metric}");
            if !r
                .metrics
                .get(&key)
                .is_some_and(|n| n.is_finite() && *n >= minimum && *n <= maximum)
            {
                r.status = Status::Failed;
                r.reason = Some(format!(
                    "{key} must be in {minimum}..={maximum}, observed {:?}",
                    r.metrics.get(&key)
                ));
            }
        }
        // Require the counters, not only process success: zero-visual model elements are simulation-only,
        // while authored model visuals must resolve and the stock debris effect must actually draw models.
        for (key, positive) in [
            ("fx.unresolved_models", false),
            ("fx.model_elements_drawn_max", true),
            ("fx.simulation_only_model_elements", true),
        ] {
            let value = r.metrics.get(key).copied();
            let valid = value.is_some_and(|n| if positive { n > 0.0 } else { n == 0.0 });
            if !valid {
                r.status = Status::Failed;
                r.reason = Some(format!(
                    "{key} must be {}, report says {value:?} (see fx.json model names)",
                    if positive { "positive" } else { "zero" }
                ));
            }
        }
    } else {
        r.reason = Some(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    Ok(r)
}
