// SPDX-License-Identifier: GPL-3.0-only
//! The live state the control channel reads: the player, the entities the newest snapshot carried, the scoreboard,
//! the sound voices and the connection. One section at a time, so a `wait` on `player.health` does not build the
//! entity list sixty times a second.

use super::{NetPlay, team_of};
use crate::models::Team;
use net::entity::etype;
use serde_json::{Value, json};
use server::netsv::eflags;

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
        // The current local aim before view kick/sway, not the last server snapshot's acknowledged angles.
        "aim_angles": st.angles,
        "weapon": st.weapon(),
        "weapon_index": ps.weapon,
        "weapons_enabled": ps.weapon_flags & sim::pm::wf::DISABLED == 0,
        "weapon_state": ps.weapon_state,
        "offhand_secondary": ps.offhand_secondary,
        "smoke_ammo": sim::weapon::PlayerWeapons::from_words(&snap.inv)
            .clip(&st.weapons, st.weapons.index("smoke_grenade_mp")),
        "weapon_time": ps.weapon_time,
        "ads": ps.weapon_pos_frac,
        // The same predicted frame that supplied the rendered reticle, not an older snapshot.
        "visual_weapon_state": st.hud_view.as_ref().map(|(ps, _)| ps.weapon_state),
        "crosshair_alpha": st.reticle.as_ref().map_or(0.0, crate::crosshair::alpha),
        // The crosshair's centre size this frame (a cooked grenade grows it), and the crosshair bits the client put
        // on its predicted `weapFlags` (8 a teammate, 0x10 an enemy in range).
        "crosshair_center": st.reticle.as_ref().and_then(crate::crosshair::center_size),
        "crosshair_flags": st.hud_view.as_ref().map_or(0, |(ps, _)| ps.weapon_flags
            & (sim::pm::wf::FRIEND_IN_CROSSHAIR | sim::pm::wf::ENEMY_IN_CROSSHAIR)),
        "grenade_time_left": st.hud_view.as_ref().map(|(ps, _)| ps.grenade_time_left),
        // Per weapon state, `[frames, frames with a visible crosshair]` drawn so far: a check diffs two reads.
        "crosshair_state_frames": st.crosshair_state_frames,
        "alive": ps.health > 0 && ps.pm_type < sim::pm::PmType::Dead,
        "dead": ps.pm_type >= sim::pm::PmType::Dead && ps.pm_type != sim::pm::PmType::Spectator,
        "spectator": ps.pm_type == sim::pm::PmType::Spectator,
        "prone": ps.pm_flags & sim::pm::pmf::PRONE != 0,
        "crouched": ps.pm_flags & sim::pm::pmf::DUCKED != 0,
        "spawn_count": ps.spawn_count,
        "server_time": snap.server_time,
        "command_time": ps.command_time,
        "client_command_time": st.cmd_time,
        "snapshot": snap.num,
        // The newest teleport the snapshots show (`net::view::SnapshotBuffer::teleport`): `after` is the first snapshot
        // built since the server last moved the player (a spawn, `setviewpos`). `null` until the teleport bit flips.
        "teleport": st.net.snaps.teleport.map(|[before, after]| json!({"before": before, "after": after})),
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
        return json!({"count": 0, "list": []});
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
            "model_name": st.net.ui_ref().map(|ui| ui.model(e.model)),
            "team": team_of(e.eflags).map(|t| if t == Team::Axis { 1 } else { 2 }),
            "dead": e.eflags & eflags::DEAD != 0,
            "loops": e.loop_sound != 0,
            "persistent_smoke": crate::events::persistent_smoke(e),
            "custom_explode_start_time": e.custom_explode_start_time,
            "time2": e.time2,
            // A player's stance from the snapshot's `eFlags`, for aiming at the body rather than over it.
            "stance": if e.eflags & sim::pm::ef::PRONE != 0 {
                "prone"
            } else if e.eflags & sim::pm::ef::CROUCH != 0 {
                "crouch"
            } else {
                "stand"
            },
            "actor": !visible.contains(&e.number),
        }));
    }
    // The count first: a `wait` on `entities.count` is what an agent asks for, not the list itself. The snapshot's
    // number and server time name the snapshot the list came from, so one request ties the two together.
    json!({"count": out.len(), "snapshot": snap.num, "server_time": snap.server_time, "list": out})
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

/// The connection, what the network side counted, and what the view did over the run (the dead view behind the body,
/// the killcam, the frames depth of field blurred).
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
        "view": {
            "death_view_frames": st.c.death_view_frames,
            "death_view_range_max": st.c.death_view_range_max,
            "kill_cam_frames": st.c.kill_cam_frames,
            "dof_frames": st.c.dof_frames,
        },
    })
}

/// The entity a harness follows through every snapshot the client accepts (`net::view::SnapshotWatch`), as counted so
/// far: the snapshot numbers it covers, how many carried the entity with the model asked for, and the first gaps,
/// absences and wrong models by snapshot number. `null` while nothing is followed.
fn watch(st: &NetPlay) -> Value {
    let Some(w) = st.net.snaps.watch.as_ref() else {
        return Value::Null;
    };
    json!({
        "number": w.number,
        "model": w.model,
        "model_name": w.model.and_then(|m| st.net.ui_ref().map(|ui| ui.model(m))),
        "first": w.first,
        "last": w.last,
        "received": w.received,
        "present": w.present,
        "matched": w.matched,
        "missed": w.missed,
        "breaks": w.breaks,
        "gaps": w.gaps,
        "absent": w.absent,
        "wrong_model": w.wrong_model,
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
            // `world`: the entity events the snapshots carried, by kind; `own`: the own player's events the prediction
            // delivered, by event number (`sim::pm::ev`), as the object `{"26": 3}` so a path can name one.
            "events" => json!({
                "world": self.c.events,
                "own": self.c.own_events.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<_, _>>(),
            }),
            "fx" => {
                let full = self.report();
                full.get("fx").cloned().unwrap_or(Value::Null)
            }
            "watch" => watch(self),
            // The vision channels, the blend under way and the last goggles switch (`look::Look::report`).
            "look" => self.look.report(),
            // What the run saw so far, the `net` object of `client.json` (`NetPlay::report`): the
            // draw counters a check compares (`vehicles_max_drawn`, `projectiles_max_drawn`, `brush_models_max`, ...).
            "report" => self.report(),
            _ => Value::Null,
        }
    }

    /// Follows one entity through every snapshot accepted from now on (`{"number": N, "model": M, "from": S}`, the
    /// model index and `from` optional), counting afresh, or stops following (`{"clear": true}`). With `from`, the
    /// count first takes the held snapshots numbered S or later, so it can begin at one that arrived before this
    /// request. Answers with the `watch` section.
    pub fn control_watch(&mut self, req: &serde_json::Map<String, Value>) -> Result<Value, String> {
        if req.get("clear").and_then(Value::as_bool) == Some(true) {
            self.net.snaps.watch = None;
            return Ok(json!({"cleared": true}));
        }
        let number = req
            .get("number")
            .and_then(Value::as_u64)
            .and_then(|n| u16::try_from(n).ok())
            .ok_or("watch needs the entity's number (or clear)")?;
        let model = match req.get("model") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_u64()
                    .and_then(|n| u16::try_from(n).ok())
                    .ok_or("watch's model is a model index")?,
            ),
        };
        let from = match req.get("from") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or("watch's from is a snapshot number")?,
            ),
        };
        self.net
            .snaps
            .follow(net::view::SnapshotWatch::new(number, model), from);
        Ok(watch(self))
    }
}
