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
        server
            .command("set sv_cheats 1", Duration::from_secs(10))
            .ok_or("server did not answer sv_cheats")?;
        let args = ["--no-autojoin", "--duration", "300"];
        let mut early = Driven::start(&client, install, &ctx.dir.join("early"), &args)?;
        // Load the second process before throwing: zone/shader startup must not eat the cloud's 61 s lifetime.
        let mut late = Driven::start(&client, install, &ctx.dir.join("late"), &args)?;
        let address = format!("127.0.0.1:{port}");
        early.step("join before smoke", |c| join(c, &address, "Sniper"))?;
        // A known walkable garage viewpoint (also used by client-garage), aimed down so the cloud stays nearby.
        early.console("setviewpos 200.8 2267.5 323.5 175.3 60")?;
        early.pause(Duration::from_secs(1))?;
        early.step("throw smoke with the off-hand bind", |c| {
            c.bind("+smoke", "down")?;
            c.pause(Duration::from_millis(200))?;
            c.bind("+smoke", "up")?;
            c.until(
                json!({"fx.persistent_smoke": {"len": 1}}),
                Duration::from_secs(20),
                "the player's smoke throw never produced a persistent cloud",
            )?;
            Ok(())
        })?;
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
        // Repeated snapshots must neither recreate the cloud nor replay either observer's burst.
        late.pause(Duration::from_secs(2))?;
        let advanced = cloud(&mut late)?;
        if early.lookup("fx.played.persistent_smoke")? != 1
            || late.lookup("fx.played.persistent_smoke")? != 1
        {
            return Err(
                "repeated snapshots must start exactly one smoke effect per observer".into(),
            );
        }
        if advanced["start_time"] != restored["start_time"]
            || integer(&advanced, "age_ms")? < age + 1_000
            || burst(&mut late)? != (0, 0)
            || burst(&mut early)? != (1, 1)
        {
            return Err(format!(
                "smoke restarted or replayed across snapshots: {restored} -> {advanced}"
            ));
        }
        early.capture("on-time-smoke")?;
        late.capture("late-smoke")?;
        let early_report = early.report("state.json", &["fx", "events", "sound", "player"])?;
        let late_report = late.report("state.json", &["fx", "events", "sound", "player"])?;
        let evidence = json!({"first": first, "restored": restored, "advanced": advanced,
            "early": early_report, "late": late_report});
        if !early
            .finish(Duration::from_secs(15))
            .is_some_and(|s| s.success())
            || !late
                .finish(Duration::from_secs(15))
                .is_some_and(|s| s.success())
        {
            return Err("smoke observer did not exit cleanly".into());
        }
        Ok(evidence)
    })();
    match result {
        Ok(evidence) => {
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
