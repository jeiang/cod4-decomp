// SPDX-License-Identifier: GPL-3.0-only
//! `client-equipment`: the equipment a person reaches with their own keys works. On the Assault class the rifle's
//! action slot raises its underbarrel launcher and a shot puts a shell in the air; on Spec Ops the C4 goes down on the
//! throw button (a pack appears and one is spent) and comes up on the attack button (the pack goes off). The class is
//! picked before the spawn, so the change applies at once. Needs a display and the install; skips without.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::{run_client, verdict};
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::Value;
use std::io;
use std::path::Path;
use std::time::Duration;

const NAME: &str = "client-equipment";
const ARGS: &[&str] = &["--bots", "1"];
/// The stock menu path up to the spawn with the class of the run picked.
const MENU: &str = "click=Start New Server,menu=createserver:20,click=Start,menu=team_marinesopfor:90,\
click=auto_assign,menu=changeclass:30,wait=1,click=";
/// Assault: the rifle in hand, key 5 for the launcher, then a shot.
const ASSAULT: &str = "Assault,ingame=120,weapon=m16:20,wait=2,shot=gl_rifle,\
holdkey=5,wait=1,releasekey=5,weapon=gl_:20,wait=2,shot=gl_selected,\
holdkey=mouse1,wait=1,releasekey=mouse1,wait=3,shot=gl_fired";
/// Spec Ops: the C4 in hand (key 5, its action slot), planted with the throw button (mouse2, the stock ADS bind) and
/// set off with the attack button.
const SPECOPS: &str = "Spec Ops,ingame=120,holdkey=5,wait=1,releasekey=5,weapon=c4:20,wait=2,shot=c4_hand,\
holdkey=mouse2,wait=1,releasekey=mouse2,wait=2,shot=c4_planted,\
holdkey=mouse1,wait=1,releasekey=mouse1,wait=3,shot=c4_detonated";

fn count(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

/// One `shot=` step's numbers: the weapon in hand, the missiles in the world and the explosions received.
struct Shot {
    weapon: String,
    missiles: u64,
    explosions: u64,
    /// The grenade counter the HUD drew (the C4 packs for that weapon).
    stock: i64,
}

/// The `shot=` steps of one run, by name.
fn shots(report: &Value) -> Vec<(String, Shot)> {
    report["steps"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    let name = s["step"].as_str()?.strip_prefix("shot=")?.to_owned();
                    Some((
                        name,
                        Shot {
                            weapon: s["weapon"].as_str().unwrap_or_default().to_owned(),
                            missiles: count(&s["missiles"]),
                            explosions: count(&s["explosions"]),
                            stock: s
                                .get("hud")
                                .and_then(|h| h.get("weapon"))
                                .and_then(|w| w.get("stock"))
                                .and_then(Value::as_i64)
                                .unwrap_or(-1),
                        },
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn shot<'a>(shots: &'a [(String, Shot)], n: &str) -> Option<&'a Shot> {
    shots.iter().find(|(k, _)| k == n).map(|(_, s)| s)
}

/// How many missiles of `weapon` the client saw appear (`net.missiles_seen`).
fn missiles_of(report: &Value, weapon: &str) -> u64 {
    report["net"]["missiles_seen"]
        .get(weapon)
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

/// One client run: the stock menu path to the class, then `steps`.
fn run_once(
    client: &Path,
    install: &Path,
    base: &Path,
    label: &str,
    steps: &str,
) -> Result<Value, String> {
    let dir = base.join(label);
    run_client(
        client,
        install,
        &dir,
        &dir.join("config"),
        ARGS,
        steps,
        Duration::from_secs(240),
    )
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
    let mut out = StageReport::new(NAME, Status::Passed);
    let mut problems = Vec::new();
    let mut notes = Vec::new();
    for (label, steps, files) in [
        (
            "gl",
            ASSAULT,
            ["gl_rifle", "gl_selected", "gl_fired"].as_slice(),
        ),
        (
            "c4",
            SPECOPS,
            ["c4_hand", "c4_planted", "c4_detonated"].as_slice(),
        ),
    ] {
        out.files.push(format!("{label}/ui-script.json"));
        for f in files {
            out.files.push(format!("{label}/{f}.png"));
        }
        let report = match run_once(&client, install, &ctx.dir, label, &format!("{MENU}{steps}")) {
            Ok(report) => report,
            Err(e) => {
                return Ok(out
                    .with_status(Status::Failed)
                    .with_reason(format!("{label}: {e}")));
            }
        };
        if let Some(w) = verdict(&report) {
            problems.push(format!("{label}: {w}"));
            continue;
        }
        let steps = shots(&report);
        if label == "gl" {
            let (Some(rifle), Some(raised)) =
                (shot(&steps, "gl_rifle"), shot(&steps, "gl_selected"))
            else {
                problems.push("gl: the raise and the shot were not reached".into());
                continue;
            };
            out.metrics.insert(
                "launcher_shells".into(),
                missiles_of(&report, &raised.weapon) as f64,
            );
            if raised.weapon == rifle.weapon {
                problems.push(format!(
                    "key 5 left {:?} in hand instead of raising the launcher",
                    raised.weapon
                ));
            } else if !raised.weapon.contains("gl") {
                problems.push(format!(
                    "key 5 raised {:?}, not the launcher",
                    raised.weapon
                ));
            } else if missiles_of(&report, &raised.weapon) == 0 {
                problems.push(format!(
                    "no {} shell reached the client after the shot",
                    raised.weapon
                ));
            } else {
                notes.push(format!(
                    "key 5 raised {} from {}, and its shell reached the client",
                    raised.weapon, rifle.weapon
                ));
            }
        } else {
            let (Some(hand), Some(planted), Some(blown)) = (
                shot(&steps, "c4_hand"),
                shot(&steps, "c4_planted"),
                shot(&steps, "c4_detonated"),
            ) else {
                problems.push("c4: the plant and the detonation were not reached".into());
                continue;
            };
            out.metrics
                .insert("explosions".into(), blown.explosions as f64);
            if !hand.weapon.contains("c4") {
                problems.push(format!("the C4 was not in hand but {:?}", hand.weapon));
            } else if planted.missiles == 0 && missiles_of(&report, "c4_mp") == 0 {
                problems.push("the C4 was never planted: no pack in the world".into());
            } else if planted.stock >= hand.stock {
                problems.push(format!(
                    "the plant spent no C4 ({} packs before, {} after)",
                    hand.stock, planted.stock
                ));
            } else if blown.explosions <= planted.explosions {
                problems.push(format!(
                    "detonating the C4 set nothing off ({} explosions before, {} after)",
                    planted.explosions, blown.explosions
                ));
            } else {
                notes.push(format!(
                    "the C4 was planted with the throw button ({} packs left) and set off with the attack button",
                    planted.stock
                ));
                if blown.missiles > 0 {
                    notes.push(format!("{} missile(s) still in the world", blown.missiles));
                }
            }
        }
    }
    if problems.is_empty() {
        out.notes.extend(notes);
        Ok(out)
    } else {
        Ok(out
            .with_status(Status::Failed)
            .with_reason(problems.join("; ")))
    }
}
