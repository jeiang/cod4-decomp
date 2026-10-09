// SPDX-License-Identifier: GPL-3.0-only
//! `net-turret`: two people (real UDP clients, no window) on mp_convoy, which places two stock `misc_turret`s. One
//! stands behind a turret and holds +activate until it is mounted, aims past its arc, fires and lets go; the other
//! watches. Asserts that the gunner's player state carries the turret flag, the drop hint and a view held inside the
//! arc, that the server's shot count rises while the attack button is down, that the other client is shown the
//! turret model and the gunner mounted on it, and that using again puts the gunner back on their feet.
use super::net_objective::Human;
use crate::stage::{StageCtx, StageReport, Status};
use net::entity::etype;
use server::client::Session;
use server::netsv::eflags;
use server::server::Server;
use sim::cm::{Collide, ENTITYNUM_NONE};
use sim::pm::{PLAYER_MAXS, PLAYER_MINS, ef};
use sim::weapon::pickup::WEAPON_HINT_OFFSET;
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const NAME: &str = "net-turret";
/// Waiting for a match to give both clients a body depends on the game, not the runner.
const LIMIT: Duration = Duration::from_secs(120);

/// A spot on the floor `back` units behind a turret at `origin` facing `yaw`, with room for a player.
fn behind(server: &Server, origin: [f32; 3], yaw: f32) -> Option<[f32; 3]> {
    let world = server.game.world.as_ref()?;
    let (s, c) = yaw.to_radians().sin_cos();
    [50.0f32, 60.0, 70.0, 40.0, 80.0]
        .into_iter()
        .find_map(|back| {
            let (x, y) = (origin[0] - back * c, origin[1] - back * s);
            let t = world.trace(
                [x, y, origin[2] + 40.0],
                [x, y, origin[2] - 120.0],
                PLAYER_MINS,
                PLAYER_MAXS,
                ENTITYNUM_NONE,
                sim::contents::MASK_PLAYERSOLID,
            );
            (!t.start_solid && t.fraction < 1.0 && t.walkable)
                .then(|| [x, y, origin[2] + 40.0 - 160.0 * t.fraction])
        })
}

fn yaw_delta(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    if d > 180.0 { d - 360.0 } else { d }
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(install) = ctx.install.as_deref() else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("install not found"));
    };
    let args: Vec<String> = ["+set", "net_port", "0"].map(String::from).to_vec();
    let mut server = match Server::boot(install, &args, false) {
        Ok(s) => s,
        Err(e) => return Ok(StageReport::new(NAME, Status::Failed).with_reason(e)),
    };
    let Some(addr) = server.net_addr() else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("cannot bind a UDP socket"));
    };
    let addr = SocketAddr::from(([127, 0, 0, 1], addr.port()));
    for line in [
        "set g_gametype war",
        "set scr_war_timelimit 0",
        "set scr_war_scorelimit 0",
        "map mp_convoy",
        "bots 2",
    ] {
        if let Err(e) = server.exec_line(line) {
            return Ok(StageReport::new(NAME, Status::Failed).with_reason(format!("{line}: {e}")));
        }
    }
    let mut people = [Human::new(addr, 0)?, Human::new(addr, 1)?];
    let fail = |why: String| Ok(StageReport::new(NAME, Status::Failed).with_reason(why));
    let deadline = Instant::now() + LIMIT;
    let frame = |server: &mut Server, people: &mut [Human; 2]| {
        server.run_for(Duration::from_millis(16));
        for p in people.iter_mut() {
            p.step();
        }
    };
    let playing = |server: &Server, people: &[Human; 2]| {
        people.iter().all(|p| {
            p.own.and_then(|n| server.game.client(n)).is_some_and(|c| {
                c.session == Session::Playing
                    && !c.frozen
                    && c.ps.weapon_flags & sim::pm::wf::DISABLED == 0
            })
        })
    };
    while !playing(&server, &people) {
        if Instant::now() > deadline {
            return fail("the two clients never got a playable body".into());
        }
        frame(&mut server, &mut people);
    }
    let (a, b) = (
        people[0].own.unwrap_or_default(),
        people[1].own.unwrap_or_default(),
    );
    let Some((turret, origin, yaw, spot)) = server
        .game
        .turrets()
        .map(|(t, e, _)| (t, e.origin, e.angles[1]))
        .collect::<Vec<_>>()
        .into_iter()
        .find_map(|(t, o, y)| behind(&server, o, y).map(|p| (t, o, y, p)))
    else {
        return fail("mp_convoy has no turret with room to stand behind it".into());
    };
    let (weapon, arc) = {
        let tu = server.game.ent(turret).and_then(|e| e.turret.as_deref());
        match tu {
            Some(tu) => (tu.weapon, tu.arc_max[1].max(-tu.arc_min[1])),
            None => return fail("the turret entity lost its state".into()),
        }
    };
    let model = server
        .game
        .ent(turret)
        .map(|e| e.model.to_string())
        .unwrap_or_default();

    // B watches from beside the gun, A stands behind it facing along its line, and holds +activate.
    let (s_, c_) = yaw.to_radians().sin_cos();
    let beside = [origin[0] + 60.0 * c_, origin[1] + 60.0 * s_, spot[2]];
    server.game.teleport(b, beside);
    server
        .game
        .set_client_view_angle(b, [0.0, yaw + 180.0, 0.0]);
    server.game.teleport(a, spot);
    server.game.set_client_view_angle(a, [0.0, yaw, 0.0]);
    people[0].hold_use = true;
    let mounted = |server: &Server| {
        server
            .game
            .client(a)
            .is_some_and(|c| c.turret == Some(turret))
    };
    let mut guard = 0;
    while !mounted(&server) {
        if Instant::now() > deadline {
            let c = server.game.client(a);
            return fail(format!(
                "the turret was never mounted (A at {:?}, hint {:?}, ground {:?})",
                c.map(|c| c.ps.origin),
                c.map(|c| (c.ps.cursor_hint, c.ps.cursor_hint_ent_index)),
                c.map(|c| c.ps.ground_entity_num)
            ));
        }
        // Keep the player where the turret can be used: a hold lasts a few frames and a fall would end it.
        guard += 1;
        if guard % 20 == 0 {
            server.game.teleport(a, spot);
        }
        frame(&mut server, &mut people);
    }
    people[0].hold_use = false;

    // A is mounted: the flag, the drop hint, the view held in the arc, the shot count rising.
    people[0].yaw = Some(yaw + 120.0);
    people[0].attack = true;
    let shots = server.game.stats.shots;
    let mut seen_by_b = (false, false);
    let (mut flagged, mut dropped, mut held) = (false, false, false);
    for _ in 0..200 {
        frame(&mut server, &mut people);
        if let Some(s) = people[0].c.latest() {
            flagged |= s.ps.e_flags & ef::TURRET_ACTIVE == ef::TURRET_ACTIVE;
            dropped |= u16::from(s.ps.cursor_hint) == u16::from(WEAPON_HINT_OFFSET) + weapon
                && s.ps.cursor_hint_string >= 0;
            held |= (yaw_delta(s.ps.viewangles[1], yaw).abs() - arc).abs() < 1.0;
        }
        let (models, gunner) = people[1].c.latest().map_or((Vec::new(), false), |s| {
            let models = s
                .entities
                .iter()
                .filter(|e| e.number == turret && e.etype == etype::SCRIPT_MODEL)
                .map(|e| e.model)
                .collect();
            let gunner = s
                .entity(a)
                .is_some_and(|e| e.etype == etype::PLAYER && e.eflags & eflags::TURRET != 0);
            (models, gunner)
        });
        seen_by_b.1 |= gunner;
        if let Some(ui) = people[1].c.ui() {
            seen_by_b.0 |= models.iter().any(|&m| ui.model(m) == model);
        }
        if server.game.stats.shots >= shots + 5
            && flagged
            && dropped
            && held
            && seen_by_b.0
            && seen_by_b.1
        {
            break;
        }
    }
    let fired = server.game.stats.shots - shots;
    people[0].attack = false;
    people[0].yaw = None;

    // Using again lets go.
    for _ in 0..20 {
        frame(&mut server, &mut people);
    }
    people[0].hold_use = true;
    while mounted(&server) {
        if Instant::now() > deadline {
            return fail("using again never let the gunner go".into());
        }
        frame(&mut server, &mut people);
    }
    people[0].hold_use = false;
    for _ in 0..10 {
        frame(&mut server, &mut people);
    }
    let off = people[0]
        .c
        .latest()
        .is_some_and(|s| s.ps.e_flags & ef::TURRET_ACTIVE == 0);
    for p in &mut people {
        p.c.disconnect();
    }
    let checks = [
        (
            flagged,
            "the gunner's player state never carried the turret flag",
        ),
        (dropped, "the gunner was never shown the drop hint"),
        (
            held,
            "the gunner's view was never held at the edge of the arc",
        ),
        (
            fired >= 5,
            "the turret fired fewer than five shots with the attack button down",
        ),
        (
            seen_by_b.0,
            "the other client was never shown the turret model",
        ),
        (
            seen_by_b.1,
            "the other client was never shown the gunner on the turret",
        ),
        (
            off,
            "the gunner's player state kept the turret flag after letting go",
        ),
    ];
    if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
        return fail((*why).to_string());
    }
    let mut report = StageReport::new(NAME, Status::Passed);
    report.notes.push(format!(
        "{} mounted, {fired} shots, view held at {arc} degrees",
        server.game.weapons.name(weapon)
    ));
    if !server.script_errors.is_empty() {
        report.status = Status::Failed;
        report.reason = Some(format!("{:?}", server.script_errors));
    }
    Ok(report)
}
