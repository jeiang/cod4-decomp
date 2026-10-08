// SPDX-License-Identifier: GPL-3.0-or-later
//! What the game tells players' screens: hud elements, objectives, configstrings and the
//! one-shot commands (`openmenu`, `iprintln`, `setclientdvar`, ...). Scripts write into
//! [`ServerUi`]; [`crate::netsv::NetSv`] turns it into reliable commands and snapshot state
//! (the wire side is [`net::ui`]).

use crate::client::Team;
use crate::game::Game;
use net::ui::{self, ClientInfo, ServerCmd, cs};

/// Who a command is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dest {
    All,
    Client(u16),
    /// Everyone on this team.
    Team(Team),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outgoing {
    pub to: Dest,
    pub cmd: ServerCmd,
}

/// One slot of the hud element table (`game_hudelem_s`).
#[derive(Debug, Clone, Default)]
pub struct HudSlot {
    pub inuse: bool,
    /// Only this client sees it (`newclienthudelem`).
    pub client: Option<u16>,
    /// Only this team sees it (`newteamhudelem`); [`Team::Free`] means everybody.
    pub team: Team,
    pub e: ui::HudElem,
}

/// One objective slot (`objective_t`): the client-visible part plus who may see it.
#[derive(Debug, Clone, Copy, Default)]
pub struct ObjSlot {
    pub o: ui::Objective,
    /// [`Team::Free`] = everybody.
    pub team: Team,
}

impl ObjSlot {
    pub fn cleared() -> Self {
        Self {
            o: ui::Objective {
                entity: ui::NO_ENTITY,
                ..ui::Objective::default()
            },
            team: Team::Free,
        }
    }
}

pub struct ServerUi {
    pub hud: Vec<HudSlot>,
    pub objectives: [ObjSlot; ui::MAX_OBJECTIVES],
    /// Commands scripts issued since the network layer last took them, in order.
    pub out: Vec<Outgoing>,
    /// Configstrings that changed since the network layer last took them.
    pub dirty_cs: Vec<u16>,
    /// Clients owed a scoreboard (`showscoreboard`).
    pub score_requests: Vec<u16>,
}

impl Default for ServerUi {
    fn default() -> Self {
        Self {
            hud: Vec::new(),
            objectives: [ObjSlot::cleared(); ui::MAX_OBJECTIVES],
            out: Vec::new(),
            dirty_cs: Vec::new(),
            score_requests: Vec::new(),
        }
    }
}

/// The name tables scripts precache into; each is a configstring range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Table {
    Model,
    Material,
    Menu,
    /// Localized string references and `settext` text; matched case-sensitively.
    Text,
}

impl Table {
    fn range(self) -> (u16, u16, &'static str) {
        match self {
            Table::Model => (cs::MODELS, cs::MODELS_COUNT, "models"),
            Table::Material => (cs::MATERIALS, cs::MATERIALS_COUNT, "materials"),
            Table::Menu => (cs::SCRIPT_MENUS, cs::SCRIPT_MENUS_COUNT, "script menus"),
            Table::Text => (cs::LOCALIZED, cs::LOCALIZED_COUNT, "localized strings"),
        }
    }
}

/// Longest text one command carries; longer is cut so a command always fits one reliable
/// message.
pub const MAX_TEXT: usize = 400;

/// `text` cut to [`MAX_TEXT`] bytes on a character boundary.
pub fn clip(text: &str) -> String {
    let mut end = text.len().min(MAX_TEXT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

impl Game {
    /// Sets a configstring; clients are told at the end of the frame when it changed.
    pub fn set_configstring(&mut self, index: u16, text: &str) {
        if self
            .configstrings
            .get(&u32::from(index))
            .map(String::as_str)
            == Some(text)
            || (text.is_empty() && !self.configstrings.contains_key(&u32::from(index)))
        {
            return;
        }
        if text.is_empty() {
            self.configstrings.remove(&u32::from(index));
        } else {
            self.configstrings.insert(u32::from(index), text.to_owned());
        }
        if !self.ui.dirty_cs.contains(&index) {
            self.ui.dirty_cs.push(index);
        }
    }

    /// The index of `name` in a precache table, registering it (and its configstring) on first
    /// use. 0 is "none", so the first name is 1.
    pub fn precache(&mut self, table: Table, name: &str) -> Result<u16, String> {
        if name.is_empty() {
            return Ok(0);
        }
        let (base, count, what) = table.range();
        let list = match table {
            Table::Model => &mut self.models,
            Table::Material => &mut self.shaders,
            Table::Menu => &mut self.menus,
            Table::Text => &mut self.strings,
        };
        let known = list.len();
        let i = if table == Table::Text {
            list.index_exact(name)
        } else {
            list.index(name)
        };
        if i >= usize::from(count) {
            list.truncate(known);
            return Err(format!("too many {what} (max {})", count - 1));
        }
        if i > known {
            self.set_configstring(base + i as u16, name);
        }
        Ok(i as u16)
    }

    /// `G_ModelIndex`: an entity's model gets a precache index (and its configstring) when it is set, so clients can
    /// name it. Inline models (`*N`) are map geometry and need none; a full table leaves the entity undrawn.
    pub fn note_model(&mut self, name: &str) {
        if !name.starts_with('*')
            && let Err(e) = self.precache(Table::Model, name)
        {
            self.print(format!("setmodel: {e}\n"));
        }
    }

    /// `G_LocalizedStringIndex`: the index of a localized reference (`&NAME`) or literal text.
    pub fn localized_index(&mut self, text: &str) -> Result<u16, String> {
        self.precache(Table::Text, &clip(text))
    }

    /// Drops the localized strings after index `keep` (`clearalltextafterhudelem`).
    pub fn clear_text_after(&mut self, keep: u16) {
        let had = self.strings.len();
        self.strings.truncate(usize::from(keep));
        for i in usize::from(keep) + 1..=had {
            self.set_configstring(cs::LOCALIZED + i as u16, "");
        }
    }

    /// Queues a command for the network layer.
    pub fn send(&mut self, to: Dest, cmd: ServerCmd) {
        self.ui.out.push(Outgoing { to, cmd });
    }

    /// Keeps the per-client `n\name\t\team` configstrings current.
    pub fn refresh_client_info(&mut self) {
        let (allies, axis) = (self.team_score[2], self.team_score[1]);
        self.set_configstring(cs::SCORES_ALLIES, &allies.to_string());
        self.set_configstring(cs::SCORES_AXIS, &axis.to_string());
        for n in 0..cs::CLIENTINFO_COUNT {
            let text = match self.client(n).filter(|c| c.connected()) {
                Some(c) => ui::client_info_string(&ClientInfo {
                    name: c.name.clone(),
                    team: c.team as u8,
                }),
                None => String::new(),
            };
            self.set_configstring(cs::CLIENTINFO + n, &text);
        }
    }

    /// The scoreboard rows, best first, without pings (the network layer knows those).
    pub fn score_rows(&self) -> Vec<ui::ScoreRow> {
        let mut rows: Vec<ui::ScoreRow> = self
            .connected_clients()
            .map(|(n, c)| ui::ScoreRow {
                client: n,
                score: c.score,
                ping: if c.bot { -1 } else { 0 },
                deaths: c.deaths,
                kills: c.kills,
                assists: c.assists,
                status_icon: self.shaders.find(&c.status_icon) as u16,
                team: c.team as u8,
                state: match c.session {
                    crate::client::Session::Playing => ui::pstate::PLAYING,
                    crate::client::Session::Dead => ui::pstate::DEAD,
                    _ => ui::pstate::SPECTATING,
                },
            })
            .collect();
        rows.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(b.kills.cmp(&a.kills))
                .then(a.deaths.cmp(&b.deaths))
                .then(a.client.cmp(&b.client))
        });
        rows
    }

    /// `scr_<gametype>_scorelimit`, else the round limit (what the scoreboard header shows).
    pub fn score_limit(&self) -> i32 {
        let gt = self.cvars.string("g_gametype").to_ascii_lowercase();
        let n = self.cvars.int(&format!("scr_{gt}_scorelimit"));
        if n != 0 {
            n
        } else {
            self.cvars.int(&format!("scr_{gt}_roundlimit"))
        }
    }

    /// The hud elements `client` sees: global ones, its own and its team's, at most
    /// [`ui::MAX_HUD_PER_GROUP`] archived and as many not (`HudElem_UpdateClient`).
    pub fn visible_hud(&self, client: u16) -> Vec<ui::HudElem> {
        let team = self.client(client).map_or(Team::Free, |c| c.team);
        let (mut archived, mut live) = (0, 0);
        let mut out = Vec::new();
        for h in &self.ui.hud {
            if !h.inuse
                || (h.team != Team::Free && h.team != team)
                || h.client.is_some_and(|c| c != client)
            {
                continue;
            }
            let n = if h.e.archived() {
                &mut archived
            } else {
                &mut live
            };
            if *n < ui::MAX_HUD_PER_GROUP {
                *n += 1;
                out.push(h.e.clone());
            }
        }
        out
    }

    /// The objectives `client` sees, entity-attached ones at their entity's position.
    pub fn visible_objectives(&self, client: u16) -> [ui::Objective; ui::MAX_OBJECTIVES] {
        let team = self.client(client).map_or(Team::Free, |c| c.team);
        self.ui.objectives.map(|s| {
            if s.team != Team::Free && s.team != team {
                return ui::Objective::default();
            }
            let mut o = s.o;
            if o.entity != ui::NO_ENTITY {
                if let Some(c) = self.client(o.entity) {
                    o.origin = c.ps.origin;
                } else if let Some(e) = self.ent(o.entity) {
                    o.origin = e.origin;
                }
            }
            o
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_cuts_on_a_character_boundary() {
        let s = "é".repeat(300);
        let c = clip(&s);
        assert!(c.len() <= MAX_TEXT && c.chars().all(|c| c == 'é'));
        assert_eq!(clip("abc"), "abc");
    }
}
