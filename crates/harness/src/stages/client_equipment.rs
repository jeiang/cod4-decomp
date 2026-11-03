// SPDX-License-Identifier: GPL-3.0-only
//! `client-equipment`: the equipment a person reaches with their own keys works, driven through the control channel
//! (#376). Each run starts from the main menu and clicks with the pointer through Start New Server, the server's team
//! menu and the class menu. On the Assault class the rifle's action slot (key 5) raises its underbarrel launcher and a
//! shot puts a shell in the air; on Spec Ops the C4 (key 5, its action slot) goes down on the throw button (a pack
//! appears and one is spent) and comes up on the attack button (the pack goes off). The class is picked before the
//! spawn, so the change applies at once. Needs a display and the install; skips without.
use super::client_custom_class::session_report;
use super::client_flythrough::locate_client;
use super::client_hardpoint::join;
use super::client_models::no_display;
use super::client_session::verdict;
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

const NAME: &str = "client-equipment";
const ARGS: &[&str] = &["--bots", "1", "--duration", "240"];

fn count(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

/// One capture's numbers: the weapon in hand, the missiles in the world and the explosions received.
struct Shot {
    weapon: String,
    missiles: u64,
    explosions: u64,
    /// The grenade counter the HUD drew (the C4 packs for that weapon).
    stock: i64,
}

/// Saves the frame on screen as `<name>.png` and reads what it showed.
fn shot(c: &mut Driven, name: &str) -> Result<Shot, String> {
    c.capture(name)?;
    let weapon = c
        .lookup("player.weapon")?
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let entities = c.state("entities")?;
    let missiles = entities["list"].as_array().map_or(0, |l| {
        l.iter()
            .filter(|e| e["kind"] == "missile" && e["actor"] != true)
            .count()
    }) as u64;
    Ok(Shot {
        weapon,
        missiles,
        explosions: count(&c.lookup("events.world.explosion")?),
        stock: c.lookup("hud.report.weapon.stock")?.as_i64().unwrap_or(-1),
    })
}

/// Waits up to `limit` for the weapon in hand to be one whose name starts with `prefix` (`m16` is the rifle, `gl_m16`
/// its launcher raised: a part anywhere in the name would take the rifle for the launcher, #375).
fn until_weapon(c: &mut Driven, prefix: &str, limit: Duration, what: &str) -> Result<(), String> {
    let end = Instant::now() + limit;
    loop {
        let have = c.lookup("player.weapon")?;
        if have.as_str().is_some_and(|w| w.starts_with(prefix)) {
            return Ok(());
        }
        if Instant::now() > end {
            return Err(format!("{what}: {have} in hand"));
        }
        c.pause(Duration::from_millis(100))?;
    }
}

/// Holds `key` down for a second, as a hand presses it. A key the profile binds to nothing fails instead of reporting
/// a press that went nowhere.
fn hold(c: &mut Driven, key: &str) -> Result<(), String> {
    let bound = c.lookup(&format!("binds.{key}"))?;
    if bound.as_str().is_none_or(str::is_empty) {
        return Err(format!("{key} is not bound"));
    }
    c.key(key, "down")?;
    c.pause(Duration::from_secs(1))?;
    c.key(key, "up")
}

/// The stock menu path from the main menu to the spawn with `class` picked, each item clicked with the pointer.
fn start_server(c: &mut Driven, class: &str) -> Result<(), String> {
    let secs = Duration::from_secs;
    c.step("Start New Server", |c| {
        c.until(
            json!({"menu.open": {"contains": "main"}}),
            secs(90),
            "the main menu did not open",
        )?;
        c.click("Start New Server")?;
        c.until(
            json!({"menu.open": {"contains": "createserver"}}),
            secs(20),
            "Start New Server did not open the createserver menu",
        )?;
        c.click("Start")?;
        Ok(())
    })?;
    join(c, "auto_assign", class)
}

/// Assault: the rifle in hand, key 5 for the launcher, then a shot with the attack button.
fn launcher(c: &mut Driven) -> Result<[Shot; 3], String> {
    let secs = Duration::from_secs;
    c.step("the rifle in hand", |c| {
        until_weapon(c, "m16", secs(20), "the Assault class did not hold the m16")
    })?;
    c.pause(secs(2))?;
    let rifle = shot(c, "gl_rifle")?;
    c.step("key 5 raises the launcher", |c| {
        hold(c, "5")?;
        until_weapon(c, "gl_", secs(20), "key 5 did not raise the launcher")
    })?;
    c.pause(secs(2))?;
    let raised = shot(c, "gl_selected")?;
    c.step("the attack button fires the launcher", |c| {
        hold(c, "mouse1")
    })?;
    c.pause(secs(3))?;
    let fired = shot(c, "gl_fired")?;
    Ok([rifle, raised, fired])
}

/// Spec Ops: the C4 in hand (key 5, its action slot), planted with the throw button (mouse2, the stock ADS bind) and
/// set off with the attack button.
fn c4(c: &mut Driven) -> Result<[Shot; 3], String> {
    let secs = Duration::from_secs;
    c.step("key 5 raises the C4", |c| {
        hold(c, "5")?;
        until_weapon(c, "c4", secs(20), "key 5 did not raise the C4")
    })?;
    c.pause(secs(2))?;
    let hand = shot(c, "c4_hand")?;
    c.step("the throw button plants the C4", |c| hold(c, "mouse2"))?;
    c.pause(secs(2))?;
    let planted = shot(c, "c4_planted")?;
    c.step("the attack button sets the C4 off", |c| hold(c, "mouse1"))?;
    c.pause(secs(3))?;
    let blown = shot(c, "c4_detonated")?;
    Ok([hand, planted, blown])
}

/// How many missiles of `weapon` the client saw appear (`net.missiles_seen` of the run's state).
fn missiles_of(state: &Value, weapon: &str) -> u64 {
    count(&state["net"]["missiles_seen"][weapon])
}

/// One class's play after the spawn: its three captures, in order.
type Play = fn(&mut Driven) -> Result<[Shot; 3], String>;

/// One client run in `dir`: the stock menu path to `class`, then `play`. The shots it took, and the state it ended in
/// as [`verdict`] reads it (`session_report`, kept as `state.json`).
fn run_once(
    client: &Path,
    install: &Path,
    dir: &Path,
    class: &str,
    play: Play,
) -> Result<([Shot; 3], Value), String> {
    let mut c = Driven::start(client, install, dir, ARGS)?;
    let played = start_server(&mut c, class).and_then(|()| play(&mut c));
    let state = session_report(&mut c, "state.json");
    let status = c.finish(Duration::from_secs(30));
    let shots = played?;
    let state = state?;
    if !status.is_some_and(|s| s.success()) {
        return Err(format!("the client did not exit successfully: {status:?}"));
    }
    Ok((shots, state))
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
    for (label, class, play, files) in [
        (
            "gl",
            "Assault",
            launcher as Play,
            ["gl_rifle", "gl_selected", "gl_fired"],
        ),
        (
            "c4",
            "Spec Ops",
            c4 as Play,
            ["c4_hand", "c4_planted", "c4_detonated"],
        ),
    ] {
        out.files.push(format!("{label}/state.json"));
        for f in files {
            out.files.push(format!("{label}/{f}.png"));
        }
        let (shots, state) = match run_once(&client, install, &ctx.dir.join(label), class, play) {
            Ok(run) => run,
            Err(e) => {
                problems.push(format!("{label}: {e}"));
                continue;
            }
        };
        if let Some(w) = verdict(&state) {
            problems.push(format!("{label}: {w}"));
            continue;
        }
        if label == "gl" {
            let [rifle, raised, _] = &shots;
            let shells = missiles_of(&state, &raised.weapon);
            out.metrics.insert("launcher_shells".into(), shells as f64);
            if shells == 0 {
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
            let [hand, planted, blown] = &shots;
            out.metrics
                .insert("explosions".into(), blown.explosions as f64);
            if planted.missiles == 0 && missiles_of(&state, "c4_mp") == 0 {
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
                    "the C4 ({}) was planted with the throw button ({} packs left) and set off with the attack button",
                    hand.weapon, planted.stock
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
