// SPDX-License-Identifier: GPL-3.0-only
//! The minimap's scale and its objective icons, checked inside `client-hud` (#415, #417). Reference: KisakCOD MP 1.8
//! (GPL-3.0), `cgame/cg_compass.cpp`: `CG_CompassRegisterDvars` (`compassMaxRange` 2500 in multiplayer, 3500 in single
//! player; `compassObjectiveWidth`/`Height` 20, `compassObjectiveMaxRange` 2048, `compassObjectiveMinAlpha` 1),
//! `CG_WorldPosToCompass` (the minimap's height spans `compassMaxRange`, an icon off the map sits on its rim) and
//! `CG_CompassDrawPlayerPointers_MP` (every current or active objective drawn, a 2-unit line across the map at the
//! current one's row and one down its column). 1.7 is not verified against `iw3mp.exe`.
//!
//! The server's `devobjective` hook adds a current objective a quarter of `compassMaxRange` from the player (inside
//! the minimap) and an active one well past both ranges, as `objective_add` would; the client reports what its
//! minimap drew (`hud.report.objective_draws`).
use crate::control::Driven;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// The multiplayer `compassMaxRange`.
const MAX_RANGE: f64 = 2500.0;
/// An icon the stock scripts precache in every gametype (the airstrike's), so the test adds no missing material.
const ICON: &str = "compass_objpoint_airstrike_friendly";
/// World inches from the player: inside the minimap (half way from the centre to the top edge when straight ahead)
/// and past both `compassMaxRange` and `compassObjectiveMaxRange`.
const NEAR: f64 = MAX_RANGE / 4.0;
const FAR: f64 = 4000.0;

/// What is wrong with the minimap's objective draws, given `compassSize` (`k`); `Ok` names what was seen.
pub(crate) fn check(draws: &Value, k: f64) -> Result<String, String> {
    let list = draws
        .as_array()
        .ok_or("no objective_draws in the HUD report")?;
    let f = |d: &Value, key: &str| d[key].as_f64().unwrap_or(f64::NAN);
    let find = |current: bool| list.iter().find(|d| d["current"] == json!(current));
    let (Some(near), Some(far)) = (find(true), find(false)) else {
        return Err(format!(
            "the minimap did not draw both objectives (a current one {NEAR} away, an active one {FAR} away): {draws}"
        ));
    };
    let mut bad = Vec::new();
    for (what, d, dist) in [("current", near, NEAR), ("active", far, FAR)] {
        if (f(d, "dist") - dist).abs() > dist * 0.1 {
            bad.push(format!("{what}: {} away, want about {dist}", f(d, "dist")));
        }
        // The drawn offset over the distance gives the range the minimap's height spans.
        if (f(d, "range") - MAX_RANGE).abs() > MAX_RANGE * 0.01 {
            bad.push(format!(
                "{what}: the minimap's height spans {} world inches, want compassMaxRange {MAX_RANGE}",
                f(d, "range")
            ));
        }
        let size = [&d["size"][0], &d["size"][1]].map(|v| v.as_f64().unwrap_or(f64::NAN));
        if size.iter().any(|s| (s - 20.0 * k).abs() > 1e-3) {
            bad.push(format!(
                "{what}: icon {size:?}, want compassObjectiveWidth/Height 20 x compassSize {k}"
            ));
        }
        if (f(d, "alpha") - 1.0).abs() > 1e-3 {
            bad.push(format!("{what}: alpha {}, want 1", f(d, "alpha")));
        }
    }
    if near["clipped"] != json!(false) {
        bad.push(format!("current: clipped {NEAR} away, inside the minimap"));
    }
    if near["lines"].as_u64() != Some(2) || f(near, "line_width") != 2.0 {
        bad.push(format!(
            "current: {} lines {} wide across the minimap, want 2 of width 2",
            near["lines"], near["line_width"]
        ));
    }
    if far["clipped"] != json!(true) || (f(far, "edge") - 1.0).abs() > 1e-3 {
        bad.push(format!(
            "active: {FAR} away must sit on the rim, clipped {} at {} of the way out",
            far["clipped"], far["edge"]
        ));
    }
    if far["lines"].as_u64() != Some(0) {
        bad.push(format!(
            "active: {} lines, only the current objective has them",
            far["lines"]
        ));
    }
    if bad.is_empty() {
        Ok(format!("minimap objectives: {draws}"))
    } else {
        Err(format!("{}; drawn {draws}", bad.join("; ")))
    }
}

fn cvar(c: &mut Driven, name: &str) -> Result<Value, String> {
    Ok(c.ask(json!({"cmd": "cvar", "name": name}))?["value"].clone())
}

fn exec(c: &mut Driven, line: &str) -> Result<(), String> {
    c.ask(json!({"cmd": "server", "op": "exec", "line": line}))?;
    Ok(())
}

/// The step: the registered `compassMaxRange`, then the two objectives as the minimap drew them. Clears them after.
pub(crate) fn step(c: &mut Driven, notes: &mut Vec<String>) -> Result<(), String> {
    let range = cvar(c, "compassMaxRange")?;
    if range != json!("2500") {
        return Err(format!(
            "compassMaxRange: expected the multiplayer default \"2500\", got {range}"
        ));
    }
    let k = cvar(c, "compassSize")?
        .as_str()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1.0);
    let origin = c.lookup("player.origin")?;
    let o = |i: usize| origin[i].as_f64().unwrap_or(0.0);
    let (x, y, z) = (o(0), o(1), o(2));
    exec(
        c,
        &format!("devobjective 0 current {} {y} {z} {ICON}", x + NEAR),
    )?;
    exec(
        c,
        &format!("devobjective 1 active {x} {} {z} {ICON}", y - FAR),
    )?;
    let end = Instant::now() + Duration::from_secs(15);
    let result = loop {
        c.pause(Duration::from_millis(250))?;
        let draws = c.lookup("hud.report.objective_draws")?;
        match check(&draws, k) {
            Ok(seen) => break Ok(seen),
            Err(e) if Instant::now() > end => break Err(e),
            Err(_) => {}
        }
    };
    exec(c, "devobjective 0 empty")?;
    exec(c, "devobjective 1 empty")?;
    notes.push(result?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draws(range: f64, size: f64, lines: u64, far_clipped: bool) -> Value {
        json!([
            {"dist": NEAR, "range": range, "edge": 0.5, "clipped": false, "size": [size, size], "alpha": 1.0,
             "current": true, "lines": lines, "line_width": 2.0},
            {"dist": FAR, "range": range, "edge": if far_clipped { 1.0 } else { 0.9 }, "clipped": far_clipped,
             "size": [size, size], "alpha": 1.0, "current": false, "lines": 0, "line_width": 2.0},
        ])
    }

    #[test]
    fn the_stock_minimap_passes() {
        check(&draws(2500.0, 20.0, 2, true), 1.0).unwrap();
        check(&draws(2500.0, 30.0, 2, true), 1.5).unwrap();
    }

    #[test]
    fn each_departure_from_the_stock_minimap_is_named() {
        let e = check(&draws(3500.0, 20.0, 2, true), 1.0).unwrap_err();
        assert!(e.contains("spans 3500"), "{e}");
        let e = check(&draws(2500.0, 18.75, 2, true), 1.0).unwrap_err();
        assert!(e.contains("icon [18.75, 18.75]"), "{e}");
        let e = check(&draws(2500.0, 20.0, 0, true), 1.0).unwrap_err();
        assert!(e.contains("current: 0 lines"), "{e}");
        let e = check(&draws(2500.0, 20.0, 2, false), 1.0).unwrap_err();
        assert!(e.contains("must sit on the rim"), "{e}");
        let e = check(&json!([]), 1.0).unwrap_err();
        assert!(e.contains("did not draw both"), "{e}");
    }
}
