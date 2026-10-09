// SPDX-License-Identifier: GPL-3.0-only
//! Copies what the server told the UI into a [`LiveUi`] once per frame: the script hud elements with their strings
//! looked up, objectives, scoreboard lines, names and teams, and the replay state of a killcam.

use super::{KillIcon, LiveElem, LiveObjective, LiveUi, ScoreLine};
use crate::wire::Wire;
use net::client::NetClient;
use net::entity::etype;
use net::ui::{ClientUiState, HudElem, NO_ENTITY, UiEvent, cs, he};
use sim::pm::PmType;

/// How far above a player's feet a waypoint that follows them floats (`waypointPlayerOffsetStand`).
const PLAYER_WAYPOINT_HEIGHT: f32 = 74.0;

/// The text an element shows that is not computed per frame.
fn elem_text(ui: &ClientUiState, e: &HudElem) -> String {
    match e.kind {
        he::TEXT => ui.localized(e.text).to_owned(),
        he::PLAYERNAME => ui
            .client(e.value.round().max(0.0) as u16)
            .map_or_else(String::new, |c| c.name),
        he::MAPNAME => ui.config(cs::MAPNAME).to_owned(),
        he::GAMETYPE => ui.config(cs::GAMETYPE).to_owned(),
        _ => String::new(),
    }
}

/// Script elements ordered the way they are drawn: by `sort`, then by slot.
fn drawn_order(a: &LiveElem, b: &LiveElem) -> std::cmp::Ordering {
    a.e.sort.total_cmp(&b.e.sort).then(a.e.id.cmp(&b.e.id))
}

pub fn fill(net: &mut NetClient<Wire>, live: &mut LiveUi, time: i32, eye: Option<glam::Vec3>) {
    let Some(snap) = net.latest() else {
        live.active = false;
        return;
    };
    let (follow, pm, own, server_time) =
        (snap.follow, snap.ps.pm_type, snap.own(), snap.server_time);
    // Entities a waypoint can follow, by number, with their feet.
    let mut targets: Vec<(u16, [f32; 3], bool)> = Vec::new();
    let mut wanted: Vec<u16> = Vec::new();
    if let Some(ui) = net.ui() {
        wanted.extend(
            ui.hud()
                .iter()
                .filter(|e| e.kind == he::WAYPOINT && e.target_ent != NO_ENTITY)
                .map(|e| e.target_ent),
        );
    }
    if let Some(snap) = net.latest() {
        for n in wanted {
            if let Some(e) = snap.entity(n) {
                targets.push((n, e.origin, e.etype == etype::PLAYER));
            }
        }
    }
    let Some(ui) = net.ui() else {
        live.active = false;
        return;
    };
    let archive = follow.map_or(0, |f| f.archive_ms as i32);
    live.active = true;
    live.time = if time > 0 { time } else { server_time };
    live.killcam = archive > 0;
    live.hud_time = live.time - archive;
    live.dead = matches!(pm, PmType::Dead | PmType::DeadLinked);
    live.intermission = pm == PmType::Intermission;
    live.own = own;
    live.eye = eye.map_or(live.eye, |e| e.to_array());
    let n = usize::from(cs::CLIENTINFO_COUNT);
    live.names.resize(n, String::new());
    live.teams.resize(n, 0);
    live.ranks.resize(n, (0, 0));
    live.materials
        .resize(usize::from(cs::MATERIALS_COUNT), String::new());
    for (i, m) in live.materials.iter_mut().enumerate() {
        let name = ui.material(i as u16);
        if m != name {
            name.clone_into(m);
        }
    }
    for i in 0..cs::CLIENTINFO_COUNT {
        let info = ui.client(i);
        let slot = usize::from(i);
        match info {
            Some(c) => {
                if live.names[slot] != c.name {
                    live.names[slot] = c.name;
                }
                live.teams[slot] = c.team;
                live.ranks[slot] = (c.rank, c.prestige);
            }
            None => {
                live.names[slot].clear();
                live.teams[slot] = 0;
                live.ranks[slot] = (0, 0);
            }
        }
    }
    live.own_team = live.team(own);
    live.following = follow
        .filter(|f| f.archive_ms == 0 && f.followed != f.own)
        .map(|f| live.name(f.followed).to_owned());

    live.elems.clear();
    for e in ui.hud() {
        let world = (e.kind == he::WAYPOINT).then(|| {
            if e.target_ent == NO_ENTITY {
                Some([e.x, e.y, e.z])
            } else {
                targets.iter().find(|t| t.0 == e.target_ent).map(|t| {
                    [
                        t.1[0],
                        t.1[1],
                        t.1[2] + if t.2 { PLAYER_WAYPOINT_HEIGHT } else { 0.0 },
                    ]
                })
            }
        });
        live.elems.push(LiveElem {
            text: elem_text(ui, e),
            label: ui.localized(e.label).to_owned(),
            material: ui.material(e.material).to_owned(),
            offscreen: ui.material(e.offscreen_material).to_owned(),
            world: world.flatten(),
            e: e.clone(),
        });
    }
    live.elems.sort_by(drawn_order);

    live.objectives.clear();
    live.objectives.extend(
        ui.objectives()
            .iter()
            .filter(|o| o.visible())
            .map(|o| LiveObjective {
                pos: o.origin,
                icon: ui.material(o.icon).to_owned(),
                current: o.state == net::ui::obj::CURRENT,
            }),
    );

    let rows = &ui.scoreboard().rows;
    let lines: Vec<ScoreLine> = rows
        .iter()
        .filter_map(|r| {
            let c = ui.client(r.client)?;
            Some(ScoreLine {
                client: r.client,
                name: c.name,
                team: r.team,
                state: r.state,
                score: r.score,
                kills: r.kills,
                deaths: r.deaths,
                assists: r.assists,
                rank: c.rank,
                prestige: c.prestige,
                ping: r.ping,
                status_icon: ui.material(r.status_icon).to_owned(),
            })
        })
        .collect();
    if lines != live.scores {
        live.scores = lines;
    }
    (live.allies_score, live.axis_score) = ui.team_scores();
}

/// Looks up the kill icon of each weapon a kill in `events` was made with.
pub fn note_kill_icons(
    events: &[UiEvent],
    content: &server::content::Content,
    icons: &mut std::collections::HashMap<String, KillIcon>,
) {
    for ev in events {
        let UiEvent::Obituary(o) = ev else { continue };
        if o.weapon.is_empty() || icons.contains_key(&o.weapon) {
            continue;
        }
        let icon = content.weapon(&o.weapon).and_then(|d| {
            let m = d.kill_icon.as_ref()?.name.clone()?;
            Some(KillIcon {
                material: m.to_string(),
                ratio: d.kill_icon_ratio,
                flip: d.flip_kill_icon != 0,
            })
        });
        icons.insert(o.weapon.clone(), icon.unwrap_or_default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_draw_by_sort_then_slot() {
        let mk = |id, sort| LiveElem {
            e: HudElem {
                id,
                sort,
                ..HudElem::new(id)
            },
            ..LiveElem::default()
        };
        let mut v = [mk(5, 0.0), mk(2, 3.0), mk(1, 3.0), mk(9, -1.0)];
        v.sort_by(drawn_order);
        let ids: Vec<u16> = v.iter().map(|e| e.e.id).collect();
        assert_eq!(ids, [9, 5, 1, 2]);
    }
}
