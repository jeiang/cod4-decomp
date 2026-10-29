// SPDX-License-Identifier: GPL-3.0-only
//! The live state the control channel reads: the player, the entities the newest snapshot carried, the scoreboard,
//! the sound voices and the connection. One section at a time, so a `wait` on `player.health` does not build the
//! entity list sixty times a second.

use super::{NetPlay, team_of};
use crate::models::Team;
use net::entity::etype;
use serde_json::{Value, json};

/// A player's numbers as the snapshot has them.
fn player(st: &NetPlay) -> Value {
    let Some(snap) = st.net.latest() else {
        return Value::Null;
    };
    let ps = &snap.ps;
    let own = snap.own();
    let team = snap
        .entity(own)
        .and_then(|e| team_of(e.eflags))
        .map(|t| if t == Team::Axis { 1 } else { 2 })
        .or_else(|| {
            st.net
                .ui_ref()
                .and_then(|ui| ui.client(own))
                .map(|c| c.team)
        })
        .unwrap_or(0);
    let name = st
        .net
        .ui_ref()
        .and_then(|ui| ui.client(own))
        .map(|c| c.name)
        .unwrap_or_default();
    json!({
        "client": own,
        "name": name,
        "team": team,
        "health": ps.health,
        "max_health": ps.max_health,
        "origin": ps.origin,
        "velocity": ps.velocity,
        "angles": ps.viewangles,
        "weapon": st.weapon(),
        "weapon_index": ps.weapon,
        "ads": ps.weapon_pos_frac,
        "alive": ps.health > 0 && ps.pm_type < sim::pm::PmType::Dead,
        "dead": ps.pm_type >= sim::pm::PmType::Dead && ps.pm_type != sim::pm::PmType::Spectator,
        "spectator": ps.pm_type == sim::pm::PmType::Spectator,
        "prone": ps.pm_flags & sim::pm::pmf::PRONE != 0,
        "crouched": ps.pm_flags & sim::pm::pmf::DUCKED != 0,
        "spawn_count": ps.spawn_count,
        "server_time": snap.server_time,
        "snapshot": snap.num,
        "killcam": snap.killcam(),
    })
}

/// What kind of thing an entity is, as a word.
fn kind(e: &net::entity::EntityState) -> &'static str {
    match e.etype {
        etype::PLAYER => "player",
        etype::CORPSE => "corpse",
        etype::SCRIPT_MODEL => "script_model",
        etype::MISSILE => "missile",
        etype::ITEM => "item",
        etype::EVENT => "event",
        etype::VEHICLE => "vehicle",
        _ => "general",
    }
}

/// The entities the newest snapshot carried, and the actors the compass knows, one object each.
fn entities(st: &NetPlay) -> Value {
    let Some(snap) = st.net.latest() else {
        return json!([]);
    };
    let visible: std::collections::HashSet<u16> = snap.entities.iter().map(|e| e.number).collect();
    let mut out = Vec::with_capacity(snap.entities.len() + snap.actors.len());
    for e in snap.entities.iter().chain(snap.actors.iter()) {
        out.push(json!({
            "number": e.number,
            "kind": kind(e),
            "origin": e.origin,
            "angles": e.angles,
            "client": e.client,
            "weapon": e.weapon,
            "model": e.model,
            "team": team_of(e.eflags).map(|t| if t == Team::Axis { 1 } else { 2 }),
            "loops": e.loop_sound != 0,
            "actor": !visible.contains(&e.number),
        }));
    }
    Value::Array(out)
}

/// The scoreboard the server last sent, with each row's numbers.
fn scoreboard(st: &NetPlay) -> Value {
    let Some(ui) = st.net.ui_ref() else {
        return Value::Null;
    };
    let sb = ui.scoreboard();
    let rows: Vec<Value> = sb
        .rows
        .iter()
        .map(|r| {
            let info = ui.client(r.client);
            json!({
                "client": r.client,
                "ping": r.ping,
                "name": info.as_ref().map(|c| c.name.clone()).unwrap_or_default(),
                "team": info.as_ref().map(|c| c.team).unwrap_or(0),
                "score": info.as_ref().map(|c| c.score).unwrap_or(0),
                "kills": info.as_ref().map(|c| c.kills).unwrap_or(0),
                "deaths": info.as_ref().map(|c| c.deaths).unwrap_or(0),
            })
        })
        .collect();
    let (allies, axis) = ui.team_scores();
    json!({"rows": rows, "axis": axis, "allies": allies, "limit": sb.limit})
}

/// The connection and what the network side counted.
fn connection(st: &NetPlay) -> Value {
    let s = st.net.stats();
    json!({
        "connected": st.net.connected(),
        "spawned": st.c.spawned,
        "refused": st.refused(),
        "dropped": st.dropped(),
        "packets_in": s.map(|s| s.packets_in),
        "bytes_in": s.map(|s| s.bytes_in),
        "bytes_out": s.map(|s| s.bytes_out),
    })
}

impl NetPlay {
    /// One section of the state document. A section this level does not know is `null`.
    pub fn control_state(&mut self, part: &str) -> Value {
        match part {
            "player" => player(self),
            "entities" => entities(self),
            "scoreboard" => scoreboard(self),
            "net" => connection(self),
            "sound" => self.sound.report(),
            "fx" => {
                let full = self.report();
                full.get("fx").cloned().unwrap_or(Value::Null)
            }
            _ => Value::Null,
        }
    }
}
