// SPDX-License-Identifier: GPL-3.0-only
//! `client-smoke`: a real off-hand throw, an on-time observer and a late-joining observer.
//! KisakCOD (GPL-3.0), game/g_missile.cpp:326-404, cgame_mp/cg_snapshot_mp.cpp:419-422
//! and cgame_mp/cg_main_mp.cpp:1588-1600 (MP 1.8; native 1.7 unverified).
use super::client_dedicated::{Dedicated, locate_server};
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use crate::control::Driven;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::fs;
use std::io;
use std::time::Duration;

const NAME: &str = "client-smoke";

fn join(c: &mut Driven, address: &str, class: &str) -> Result<(), String> {
    c.console(&format!("connect {address}"))?;
    c.until(
        json!({"menu.open": {"contains": "team_marinesopfor"}}),
        Duration::from_secs(90),
        "the connected player's team menu did not open",
    )?;
    c.click("auto_assign")?;
    c.until(
        json!({"menu.open": {"contains": "changeclass"}}),
        Duration::from_secs(30),
        "the class menu did not open",
    )?;
    c.click(class)?;
    c.until(
        json!({"net.spawned": true, "player.alive": true}),
        Duration::from_secs(120),
        "the player did not spawn",
    )?;
    Ok(())
}

/// Stock classes in menu order. Their special grenades come from the install's class table, so join each in turn
/// until the spawned player carries smoke as the off-hand secondary that `+smoke` throws.
const CLASSES: [&str; 5] = [
    "Assault",
    "Spec Ops",
    "Heavy Gunner",
    "Demolitions",
    "Sniper",
];

fn join_with_smoke(c: &mut Driven, address: &str) -> Result<&'static str, String> {
    let mut seen = Vec::new();
    for (i, class) in CLASSES.into_iter().enumerate() {
        if i > 0 {
            c.console("disconnect")?;
            // The next join waits for the new connection's team menu.
            c.pause(Duration::from_secs(3))?;
        }
        join(c, address, class)?;
        let carries = c.until(
            json!({"player.offhand_secondary": 0, "player.smoke_ammo": {"gt": 0}}),
            Duration::from_secs(5),
            "no smoke off-hand",
        );
        if carries.is_ok() {
            return Ok(class);
        }
        let player = c.state("player")?;
        seen.push(format!(
            "{class}: offhand_secondary={} smoke_ammo={}",
            player["offhand_secondary"], player["smoke_ammo"]
        ));
    }
    Err(format!(
        "no stock class spawned with a smoke off-hand: {}",
        seen.join("; ")
    ))
}

fn integer(v: &Value, key: &str) -> Result<i64, String> {
    v[key]
        .as_i64()
        .ok_or_else(|| format!("missing integer {key}: {v}"))
}

fn cloud(c: &mut Driven) -> Result<Value, String> {
    let fx = c.state("fx")?;
    let clouds = fx["persistent_smoke"]
        .as_array()
        .ok_or_else(|| format!("persistent smoke FX diagnostics missing: {fx}"))?;
    if clouds.len() != 1 {
        return Err(format!(
            "expected the single thrown smoke cloud, got {clouds:?}"
        ));
    }
    Ok(clouds[0].clone())
}

fn burst(c: &mut Driven) -> Result<(u64, u64), String> {
    let events = c.state("events")?;
    let sound = c.state("sound")?;
    if sound["ready"] != true {
        return Err(format!("sound bank not ready: {sound}"));
    }
    let world = events["world"]
        .as_object()
        .ok_or_else(|| format!("missing world event counters: {events}"))?;
    // Event kinds are inserted only when delivered; an absent explosion key means no burst.
    let explosions = match world.get("explosion") {
        None => 0,
        Some(v) => v
            .as_u64()
            .ok_or_else(|| format!("invalid explosion counter: {events}"))?,
    };
    let cues = sound["explosion_cues"]
        .as_u64()
        .ok_or_else(|| format!("missing explosion cue counter: {sound}"))?;
    Ok((explosions, cues))
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(client) = locate_client() else {
        return Ok(
            StageReport::new(NAME, Status::Skipped).with_reason("cod4e client binary missing")
        );
    };
    let Some(server) = locate_server() else {
        return Ok(
            StageReport::new(NAME, Status::Skipped).with_reason("cod4e-server binary missing")
        );
    };
    let Some(install) = &ctx.install else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("no install"));
    };
    if let Some(r) = no_display(&client, NAME)? {
        return Ok(r);
    }
    let work = ctx.dir.join("server");
    fs::create_dir_all(&work)?;
    let mut out = StageReport::new(NAME, Status::Passed);
    let result = (|| -> Result<Value, String> {
        let (mut server, port) =
            Dedicated::boot(&server, install, &work, 0, &ctx.dir.join("server.log"))?;
        // The map command owns sv_cheats (an init-protected dvar); wait for its synchronous reload and echo.
        server
            .command("devmap mp_crash", Duration::from_secs(120))
            .ok_or("server did not finish devmap mp_crash")?;
        let args = ["--no-autojoin", "--duration", "300"];
        let mut early = Driven::start(&client, install, &ctx.dir.join("early"), &args)?;
        let address = format!("127.0.0.1:{port}");
        let class = early.step("join before smoke", |c| join_with_smoke(c, &address))?;
        // A known walkable garage viewpoint (also used by client-garage), aimed down so the cloud stays nearby.
        early.console("setviewpos 200.8 2267.5 323.5 175.3 60")?;
        early.pause(Duration::from_secs(1))?;
        early.report(
            "before-throw.json",
            &["player", "hud", "fx", "events", "sound"],
        )?;
        let thrown = early.step("throw smoke with the off-hand bind", |c| {
            c.until(
                json!({"player.weapons_enabled": true, "player.weapon_state": 0,
                    "player.weapon_time": {"le": 0}, "player.offhand_secondary": 0,
                    "player.smoke_ammo": {"gt": 0}}),
                Duration::from_secs(30),
                "the smoke class did not become ready with enabled weapons and smoke ammunition",
            )?;
            c.report(
                "ready-to-throw.json",
                &["player", "hud", "fx", "events", "sound"],
            )?;
            c.bind("+smoke", "down")?;
            // Wait until the off-hand has actually been primed, not a fixed key pulse during weapon raising.
            let primed = c.until(
                json!({"player.weapon_state": {"ge": 17}}),
                Duration::from_secs(5),
                "the smoke bind never primed an off-hand grenade",
            );
            c.bind("+smoke", "up")?;
            primed?;
            c.until(
                json!({"fx.persistent_smoke": {"len": 1}}),
                Duration::from_secs(20),
                "the player's smoke throw never produced a persistent cloud",
            )?;
            Ok(())
        });
        if let Err(reason) = thrown {
            let _ = early.report(
                "throw-failure.json",
                &[
                    "player", "hud", "fx", "events", "sound", "entities", "console",
                ],
            );
            let _ = early.capture("throw-failure");
            return Err(reason);
        }
        let first = cloud(&mut early)?;
        if !(0..=200).contains(&integer(&first, "age_ms")?) || first["live"] != true {
            return Err(format!(
                "the on-time observer did not exercise the <=200 ms burst window: {first}"
            ));
        }
        let (explosions, cues) = burst(&mut early)?;
        if explosions != 1 || cues != 1 {
            return Err(format!(
                "on-time smoke must burst exactly once: events={explosions}, cues={cues}"
            ));
        }
        let start = integer(&first, "start_time")?;
        let end = integer(&first, "end_time")?;
        if end - start != 61_000 {
            return Err(format!("smoke must retain its 61000 ms deadline: {first}"));
        }
        early.pause(Duration::from_secs(2))?;
        if early.lookup("fx.played.persistent_smoke")? != 1 || burst(&mut early)? != (1, 1) {
            return Err(
                "on-time observer restarted smoke or replayed its burst across snapshots".into(),
            );
        }
        early.capture("on-time-smoke")?;
        let early_report = early.report("state.json", &["fx", "events", "sound", "player"])?;
        // Only one window at a time: an occluded client can stop processing its control requests.
        // The dedicated server must retain the timed cloud even after its thrower disconnects.
        if !early
            .finish(Duration::from_secs(15))
            .is_some_and(|s| s.success())
        {
            return Err("on-time smoke observer did not exit cleanly".into());
        }
        let mut late = Driven::start(&client, install, &ctx.dir.join("late"), &args)?;
        late.step("join after the smoke burst", |c| {
            join(c, &address, "Assault")
        })?;
        late.console("setviewpos 220.8 2267.5 323.5 175.3 60")?;
        late.until(
            json!({"fx.persistent_smoke": {"len": 1}, "fx.persistent_smoke_quads": {"gt": 0}}),
            Duration::from_secs(10),
            "the late joiner never drew the already-developed smoke cloud",
        )?;
        let restored = cloud(&mut late)?;
        for key in ["number", "start_time", "end_time"] {
            if restored[key].is_null() || restored[key] != first[key] {
                return Err(format!(
                    "late smoke changed {key}: first={first}, late={restored}"
                ));
            }
        }
        let age = integer(&restored, "age_ms")?;
        if !(2_000..61_000).contains(&age) || restored["live"] != true {
            return Err(format!(
                "late smoke did not reconstruct visible FX at original age: {restored}"
            ));
        }
        let late_burst = burst(&mut late)?;
        if late_burst != (0, 0) {
            return Err(format!("late smoke replayed a burst: {late_burst:?}"));
        }
        // Repeated snapshots must neither recreate the late cloud nor replay its burst.
        late.pause(Duration::from_secs(2))?;
        let advanced = cloud(&mut late)?;
        if late.lookup("fx.played.persistent_smoke")? != 1 {
            return Err(
                "repeated snapshots must start exactly one smoke effect per observer".into(),
            );
        }
        if advanced["start_time"] != restored["start_time"]
            || integer(&advanced, "age_ms")? < age + 1_000
            || burst(&mut late)? != (0, 0)
        {
            return Err(format!(
                "smoke restarted or replayed across snapshots: {restored} -> {advanced}"
            ));
        }
        late.capture("late-smoke")?;
        let late_report = late.report("state.json", &["fx", "events", "sound", "player"])?;
        let evidence = json!({"class": class, "first": first, "restored": restored, "advanced": advanced,
            "early": early_report, "late": late_report});
        if !late
            .finish(Duration::from_secs(15))
            .is_some_and(|s| s.success())
        {
            return Err("smoke observer did not exit cleanly".into());
        }
        Ok(evidence)
    })();
    match result {
        Ok(evidence) => {
            out.reason = evidence["class"]
                .as_str()
                .map(|class| format!("thrown with the stock {class} class"));
            fs::write(
                ctx.dir.join("smoke.json"),
                serde_json::to_vec_pretty(&evidence)?,
            )?;
            out.metrics.insert(
                "late_smoke_age_ms".into(),
                evidence["restored"]["age_ms"].as_f64().unwrap_or(0.0),
            );
            out.files.extend([
                "smoke.json".into(),
                "early/on-time-smoke.png".into(),
                "late/late-smoke.png".into(),
            ]);
        }
        Err(reason) => {
            out.status = Status::Failed;
            out.reason = Some(reason);
        }
    }
    Ok(out)
}
