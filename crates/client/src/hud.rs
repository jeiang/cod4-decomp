// SPDX-License-Identifier: GPL-3.0-only
//! What the original's cgame draws itself on top of the stock HUD menus: the script hud elements, the message
//! windows behind the `gamemessages` menus, chat, the centre string, the kill feed and the scoreboard rows.
//!
//! The data flows one way. [`crate::netplay::NetPlay::fill_live`] copies what the server sent into a [`LiveUi`]
//! (plain owned data, no lifetimes) once per frame; [`crate::shell::Shell::paint`] draws it. The scrolling feed
//! windows are the one thing that keeps state between frames ([`Feed`]); the pure parts (placement, timers, fades,
//! row layout) are plain functions with unit tests.
//!
//! Draw order follows `CG_Draw2D`: chat, script elements under the menus, the HUD menus and open menus (which draw
//! the feed windows and the centre string through their own items), the foreground elements, then the scoreboard
//! rows.

mod elems;
mod feed;
pub mod fill;
pub mod names;
mod prompt;
mod scores;

use crate::input::Cvars;
use crate::ui::assets::UiAssets;
use net::ui::HudElem;
use std::collections::HashMap;

pub use elems::{SPECTATE_PROMPTS, draw_over, draw_under};
pub use feed::{BOLD, Feed, NOTIFY, WINDOWS};
pub use prompt::{LiveVote, draw_typing, draw_vote};
pub use scores::{ScoreView, draw_scoreboard, rows_shown, scoreboard_lines};

use serde_json::{Value, json};

/// One script hud element as the screen needs it, with every configstring index already looked up. Strings are
/// still the server's keys: translation uses the menu assets at draw time.
#[derive(Clone, Debug, Default)]
pub struct LiveElem {
    pub e: HudElem,
    /// `settext` (or the value, player name, map name, game type).
    pub text: String,
    /// `label`.
    pub label: String,
    pub material: String,
    pub offscreen: String,
    /// Where a waypoint is in the world (its target entity when it follows one).
    pub world: Option<[f32; 3]>,
}

/// An objective with a position in the world; the compass draws (`152`, `182`) mark it.
#[derive(Clone, Debug, Default)]
pub struct LiveObjective {
    pub pos: [f32; 3],
    pub icon: String,
    pub current: bool,
}

/// Another player the view could put a name or an icon over, as the client sees them this frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NearPlayer {
    pub client: u16,
    /// The `j_head` bone in the world.
    pub head: [f32; 3],
    /// The head is in front of the eye, not behind the viewer.
    pub ahead: bool,
    /// The head is in front and nothing solid is between it and the eye (`CG_CanSeeFriendlyHead`'s trace). Only
    /// worked out for players whose name or markers can be drawn.
    pub clear: bool,
    /// The material index the scripts' `headicon` names and who is meant to see it (0 everyone, 1 axis, 2 allies, 3
    /// spectators).
    pub icon: Option<(u16, u8)>,
    pub talking: bool,
    pub interrupted: bool,
    /// The viewer's own body, seen in a killcam.
    pub you: bool,
}

/// Other players as one frame of the view sees them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NameScan {
    pub near: Vec<NearPlayer>,
    /// The player the crosshair is on, when a name should be shown for them (`CG_ScanForCrosshairEntity`).
    pub crosshair: Option<u16>,
    /// A flashbang blinds the view: no names (`CG_Flashbanged`).
    pub flashed: bool,
}

/// One scoreboard line with the client's name looked up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScoreLine {
    pub client: u16,
    pub name: String,
    /// 0 free, 1 axis, 2 allies, 3 spectator.
    pub team: u8,
    pub state: u8,
    pub score: i32,
    pub kills: i32,
    pub deaths: i32,
    pub assists: i32,
    pub rank: u8,
    pub prestige: u8,
    pub ping: i32,
    pub status_icon: String,
}

/// The weapon icon of a kill and how wide it is drawn (`killIconRatio`: 0 square, 1 2:1, 2 4:1).
#[derive(Clone, Debug, Default)]
pub struct KillIcon {
    pub material: String,
    pub ratio: i32,
    pub flip: bool,
}

/// Per-frame facts for the native HUD, filled by `NetPlay::fill_live`; empty (`active` false) outside a match.
#[derive(Default)]
pub struct LiveUi {
    pub active: bool,
    /// The scope picture over the screen while a sniper aims.
    pub scope: Option<crate::viewmodel::Overlay>,
    /// The weapon's crosshair over the world, its field of view filled in by the app.
    pub reticle: Option<crate::crosshair::Reticle>,
    /// The red flash and the wedges of the hits the player took.
    pub damage: crate::damage::DamageHud,
    /// `cg_hudDamageIconInScope`: the wedges stay while a scope is up.
    pub damage_in_scope: bool,
    /// The client's estimate of the server clock, ms.
    pub time: i32,
    /// The clock script hud element times are read on: `time`, minus the replay offset in a killcam.
    pub hud_time: i32,
    /// A killcam or final killcam replay is on screen (what the stock `killcam` menu keys on).
    pub killcam: bool,
    /// A flashbang is blinding the view (`CG_Flashbanged`).
    pub flashed: bool,
    /// The view is through night vision goggles.
    pub night_vision: bool,
    /// Name of the player being followed live as a spectator.
    pub following: Option<String>,
    /// The view's player is dead (hud elements marked hide-when-dead stay away).
    pub dead: bool,
    /// The match is over and the server holds the players at the scoreboard.
    pub intermission: bool,
    pub own: u16,
    /// 0 free, 1 axis, 2 allies, 3 spectator.
    pub own_team: u8,
    /// Client names and teams by slot.
    pub names: Vec<String>,
    pub teams: Vec<u8>,
    /// Rank and prestige by slot (`setrank`).
    pub ranks: Vec<(u8, u8)>,
    /// Material names by index (`cs::MATERIALS`).
    pub materials: Vec<String>,
    /// What the world says about the other players this frame, for overhead names and head icons.
    pub scan: NameScan,
    /// The keys the commands named in `[{+cmd}]` marks of the elements' text are bound to, as the player reads them.
    pub keys: HashMap<String, String>,
    pub elems: Vec<LiveElem>,
    pub objectives: Vec<LiveObjective>,
    pub scores: Vec<ScoreLine>,
    pub allies_score: i32,
    pub axis_score: i32,
    pub kill_icons: HashMap<String, KillIcon>,
    pub server_addr: String,
    /// The server has been silent long enough to warn of it ("Connection interrupted" and the net icon).
    pub interrupted: bool,
    /// The vote the server is running, for the yellow lines (`CG_DrawVote`).
    pub vote: Option<LiveVote>,
    /// Set by the shell: the scoreboard is up, so ask the server for fresh rows every couple of seconds.
    pub scores_wanted: bool,
    /// Eye position and clip matrix of the frame the world is drawn with, set by the app before painting.
    pub eye: [f32; 3],
    pub clip: Option<glam::Mat4>,
}

impl LiveUi {
    pub fn name(&self, client: u16) -> &str {
        self.names
            .get(usize::from(client))
            .map_or("", String::as_str)
    }

    pub fn team(&self, client: u16) -> u8 {
        self.teams.get(usize::from(client)).copied().unwrap_or(0)
    }
}

/// The colour escape the original puts before a name on the kill feed and scoreboard: `^8` for the viewer's team,
/// `^9` for the other team, `^7` for everyone when the viewer is not on a team (`CG_DrawScoreboard_GetTeamColorIndex`).
pub fn team_color_escape(viewer_team: u8, team: u8) -> &'static str {
    let on_team = |t: u8| t == 1 || t == 2;
    if !on_team(team) || !on_team(viewer_team) {
        "^7"
    } else if team == viewer_team {
        "^8"
    } else {
        "^9"
    }
}

/// A server string as the player reads it (`SEH_LocalizeTextMessage`). A script reference to a localized string
/// arrives as `&KEY` (or a bare `&` for none). Within a message the first piece is a localized key, `\x14` starts
/// another key and `\x15` a literal argument (a player's name, a number); each piece after the first fills the
/// next `&&1`, `&&2`.. left in the text before it, and `\x16` keeps the pieces after it from being filled in.
/// Text without any of those is shown as it is.
pub fn localize(assets: &UiAssets, raw: &str) -> String {
    let reference = raw.strip_prefix('&').filter(|k| !k.starts_with('&'));
    let raw = reference.unwrap_or(raw);
    if reference.is_none() && !raw.bytes().any(|c| matches!(c, 20..=22)) {
        return raw.to_owned();
    }
    let digit =
        |s: &[u8], j: usize| s.get(j + 2).is_some_and(u8::is_ascii_digit) && s[j..j + 2] == *b"&&";
    let b = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let (mut loc_on, mut insert_enabled, mut skipped) = (true, true, false);
    let (mut level, mut index) = (0i32, 1u8);
    let (mut start, mut i) = (0, 0);
    while start < b.len() {
        if i < b.len() && !matches!(b[i], 20..=22) {
            i += 1;
            continue;
        }
        if i > start {
            let mut tok = b[start..i].to_vec();
            if loc_on {
                let key = String::from_utf8_lossy(&tok);
                if let Some(t) = assets.translate(&key).filter(|_| key.len() > 1) {
                    tok = t.as_bytes().to_vec();
                }
            }
            for j in 0..tok.len().saturating_sub(2) {
                if digit(&tok, j) {
                    if insert_enabled {
                        level += 1;
                    } else {
                        tok[j] = 22;
                        skipped = true;
                    }
                }
            }
            let slot = (level > 0 && !out.is_empty())
                .then(|| (0..out.len()).find(|&j| digit(&out, j) && out[j + 2] - b'0' == index))
                .flatten();
            match slot {
                Some(j) => {
                    let tail = out.split_off(j + 3);
                    out.truncate(j);
                    out.extend_from_slice(&tok);
                    out.extend_from_slice(&tail);
                    index += 1;
                    level -= 1;
                }
                None => out.extend_from_slice(&tok),
            }
        }
        insert_enabled = true;
        if i < b.len() {
            match b[i] {
                20 => (loc_on, i) = (true, i + 1),
                21 => (loc_on, i) = (false, i + 1),
                _ => {}
            }
        }
        if i < b.len() && b[i] == 22 {
            (insert_enabled, i) = (false, i + 1);
        }
        start = i;
    }
    if skipped {
        out.iter_mut().filter(|c| **c == 22).for_each(|c| *c = b'%');
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The `{+command}` marks of `text`, without the braces.
pub fn key_marks(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(a) = rest.find("[{") {
        let Some(b) = rest[a..].find("}]") else { break };
        out.push(&rest[a + 2..a + b]);
        rest = &rest[a + b + 2..];
    }
    out
}

/// `text` with each `[{+command}]` replaced by `[key]`, the key `command` is bound to in `keys` (a mark with no
/// entry is left as it is).
pub fn expand_keys(text: &str, keys: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(a) = rest.find("[{") {
        let Some(b) = rest[a..].find("}]") else { break };
        let cmd = &rest[a + 2..a + b];
        out.push_str(&rest[..a]);
        match keys.get(cmd) {
            Some(k) => {
                out.push('[');
                out.push_str(k);
                out.push(']');
            }
            None => out.push_str(&rest[a..a + b + 2]),
        }
        rest = &rest[a + b + 2..];
    }
    out.push_str(rest);
    out
}

/// What the HUD got to show during a run, for the harness to check (`hud` of `ui-script.json`).
#[derive(Default)]
pub struct Stats {
    pub elems_max: usize,
    pub waypoints_max: usize,
    pub timer_elems_max: usize,
    /// Messages added per window, obituaries among window 0's.
    pub messages: [u32; WINDOWS],
    pub obituaries: u32,
    /// Lines the message windows drew, summed over frames.
    pub window_lines: [u32; WINDOWS],
    pub chat: u32,
    pub scoreboard_rows_max: usize,
    pub scoreboard_frames: u32,
    pub killcam_frames: u32,
    pub intermission_frames: u32,
    pub following_frames: u32,
    /// Most names drawn over heads in one frame, frames with the crosshair's player named, and the head icons seen.
    pub names_max: usize,
    pub crosshair_name_frames: u32,
    pub head_icons_max: usize,
    pub head_icon_materials: std::collections::BTreeSet<String>,
    /// Frames a teammate's name could be read, and frames the crosshair was on a player a name was due for: what the
    /// two name counts above can be held to.
    pub friends_in_sight_frames: u32,
    pub crosshair_due_frames: u32,
    /// Frames a vote was on screen, and the most yes and no answers its lines showed.
    pub vote_frames: u32,
    pub vote_yes_max: i32,
    pub vote_no_max: i32,
    /// The elements of the busiest frame, as the screen got them.
    sample: Vec<Value>,
}

impl Stats {
    /// One frame of the live state; `rows_shown` is how many player rows the scoreboard drew, `None` when it is not up.
    pub fn frame(&mut self, live: &LiveUi, rows_shown: Option<usize>) {
        use net::ui::he;
        if live.elems.len() > self.elems_max {
            self.sample = live
                .elems
                .iter()
                .map(|e| {
                    json!({"id": e.e.id, "kind": e.e.kind, "text": e.text, "label": e.label, "material": e.material,
                        "offscreen": e.offscreen, "x": e.e.x, "y": e.e.y, "font": e.e.font, "scale": e.e.font_scale,
                        "flags": e.e.flags, "align_screen": e.e.align_screen, "align_org": e.e.align_org})
                })
                .collect();
        }
        self.elems_max = self.elems_max.max(live.elems.len());
        let count = |f: fn(u8) -> bool| live.elems.iter().filter(|e| f(e.e.kind)).count();
        self.waypoints_max = self.waypoints_max.max(count(|k| k == he::WAYPOINT));
        self.timer_elems_max = self.timer_elems_max.max(count(|k| {
            matches!(
                k,
                he::TIMER_DOWN | he::TIMER_UP | he::TENTHS_TIMER_DOWN | he::TENTHS_TIMER_UP
            )
        }));
        if let Some(rows) = rows_shown {
            self.scoreboard_frames += 1;
            self.scoreboard_rows_max = self.scoreboard_rows_max.max(rows);
        }
        self.killcam_frames += u32::from(live.killcam);
        self.intermission_frames += u32::from(live.intermission);
        self.following_frames += u32::from(live.following.is_some());
        if let Some(v) = &live.vote {
            self.vote_frames += 1;
            self.vote_yes_max = self.vote_yes_max.max(v.yes);
            self.vote_no_max = self.vote_no_max.max(v.no);
        }
    }

    /// One frame of the names and icons over players' heads: how many reached the screen, and whether one was the
    /// crosshair's.
    pub fn names_drawn(&mut self, names: usize, crosshair: bool, icons: usize) {
        self.names_max = self.names_max.max(names);
        self.crosshair_name_frames += u32::from(crosshair);
        self.head_icons_max = self.head_icons_max.max(icons);
    }

    /// A head icon was drawn.
    pub fn head_icon(&mut self, material: &str) {
        if !self.head_icon_materials.contains(material) {
            self.head_icon_materials.insert(material.to_owned());
        }
    }

    /// What the view offered the names this frame: teammates whose name could be read, and a player under the
    /// crosshair a name was due for.
    pub fn names_offered(&mut self, names: &names::Names, live: &LiveUi) {
        self.friends_in_sight_frames += u32::from(names.friends_in_sight > 0);
        self.crosshair_due_frames += u32::from(live.scan.crosshair.is_some());
    }

    pub fn report(&self) -> Value {
        json!({
            "elems_max": self.elems_max,
            "waypoints_max": self.waypoints_max,
            "timer_elems_max": self.timer_elems_max,
            "messages": self.messages,
            "obituaries": self.obituaries,
            "window_lines": self.window_lines,
            "chat": self.chat,
            "scoreboard_rows_max": self.scoreboard_rows_max,
            "scoreboard_frames": self.scoreboard_frames,
            "killcam_frames": self.killcam_frames,
            "intermission_frames": self.intermission_frames,
            "following_frames": self.following_frames,
            "names_max": self.names_max,
            "crosshair_name_frames": self.crosshair_name_frames,
            "head_icons_max": self.head_icons_max,
            "head_icon_materials": self.head_icon_materials,
            "friends_in_sight_frames": self.friends_in_sight_frames,
            "crosshair_due_frames": self.crosshair_due_frames,
            "vote_frames": self.vote_frames,
            "vote_yes_max": self.vote_yes_max,
            "vote_no_max": self.vote_no_max,
            "sample": self.sample,
        })
    }
}

/// Colours of `^8` (the viewer's team) and `^9` (the other team): `g_TeamColor_MyTeam` and `_EnemyTeam`.
pub fn team_colors(cvars: &Cvars) -> [[f32; 4]; 2] {
    let get = |name: &str, default: [f32; 3]| {
        let c = cvars
            .get(name)
            .and_then(scores::parse_color)
            .unwrap_or(default);
        [c[0], c[1], c[2], 1.0]
    };
    [
        get("g_TeamColor_MyTeam", [0.4, 0.6, 0.85]),
        get("g_TeamColor_EnemyTeam", [0.75, 0.25, 0.25]),
    ]
}

/// `m:ss` (or `h:mm:ss`) of a countdown or count-up, the way `HudElemTimerString` rounds: down timers round up.
pub fn timer_text(ms: i32) -> String {
    let s = ms.max(0) / 1000;
    let (h, m, s) = (s / 3600, s % 3600 / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// `m:ss.t` of a tenths timer.
pub fn tenths_timer_text(ms: i32) -> String {
    let t = ms.max(0) / 100;
    let (h, m, s, tenths) = (t / 36000, t % 36000 / 600, t % 600 / 10, t % 10);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{tenths}")
    } else {
        format!("{m}:{s:02}.{tenths}")
    }
}

/// The milliseconds a timer or clock element shows at `now` (`GetHudElemTime`).
pub fn elem_time_ms(e: &HudElem, now: i32) -> i32 {
    use net::ui::he;
    let t = match e.kind {
        he::TIMER_DOWN => e.time.wrapping_sub(now).saturating_add(999),
        he::TENTHS_TIMER_DOWN => e.time.wrapping_sub(now).saturating_add(99),
        he::CLOCK_DOWN => e.time.wrapping_sub(now),
        _ => now.wrapping_sub(e.time),
    };
    t.max(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn key_marks_are_found_and_replaced_by_the_bound_key() {
        let text =
            "Press [{+actionslot 4}] to use, [{+activate}] to open, [{+nothing}] and [{+open";
        assert_eq!(key_marks(text), ["+actionslot 4", "+activate", "+nothing"]);
        let keys: HashMap<String, String> = [
            ("+actionslot 4".to_owned(), "4".to_owned()),
            ("+activate".to_owned(), "F".to_owned()),
        ]
        .into();
        assert_eq!(
            expand_keys(text, &keys),
            "Press [4] to use, [F] to open, [{+nothing}] and [{+open"
        );
        assert_eq!(expand_keys("plain", &keys), "plain");
    }

    use super::*;
    use net::ui::he;

    #[test]
    fn team_colour_follows_the_viewers_side() {
        assert_eq!(team_color_escape(2, 2), "^8");
        assert_eq!(team_color_escape(2, 1), "^9");
        assert_eq!(team_color_escape(3, 1), "^7", "a spectator sees no sides");
        assert_eq!(
            team_color_escape(2, 0),
            "^7",
            "free-for-all players have no side"
        );
    }

    #[test]
    fn script_string_references_resolve_to_their_text() {
        let mut a = UiAssets::default();
        a.localize.insert("MP_OPFOR_NAME".into(), "OpFor".into());
        a.localize.insert("MP_TIME".into(), "Time: &&1".into());
        a.localize
            .insert("MP_CONNECTED".into(), "&&1 connected".into());
        assert_eq!(localize(&a, "&MP_OPFOR_NAME"), "OpFor");
        assert_eq!(localize(&a, "&"), "", "a reference to nothing is nothing");
        assert_eq!(
            localize(&a, "&MP_TIME"),
            "Time: &&1",
            "the marker is for the label to fill in"
        );
        assert_eq!(localize(&a, "&MP_CONNECTED\x15Bob"), "Bob connected");
        assert_eq!(localize(&a, "plain text"), "plain text");
        assert_eq!(localize(&a, "&NO_SUCH_KEY"), "NO_SUCH_KEY");
    }

    #[test]
    fn messages_fill_their_parameters_in_order() {
        let mut a = UiAssets::default();
        for (k, v) in [
            ("MP_CONNECTED", "&&1 connected"),
            ("MP_WAR_RADAR", "Radar for &&1 seconds"),
            ("MP_TWO", "&&1 killed &&2"),
            ("CGAME_TEAMMATE", "(teammate)"),
            ("CGAME_YOUKILLED", "You killed &&1&&2"),
        ] {
            a.localize.insert(k.into(), v.into());
        }
        // The first piece is a key; each literal after it fills the next marker; a name is literal text.
        assert_eq!(localize(&a, "MP_CONNECTED\x15Bob^7"), "Bob^7 connected");
        assert_eq!(localize(&a, "MP_WAR_RADAR\x1530"), "Radar for 30 seconds");
        assert_eq!(localize(&a, "MP_TWO\x15Al\x15Bo"), "Al killed Bo");
        // `\x14` makes the next piece a key again, and a key's own marker is filled by what follows it.
        assert_eq!(
            localize(&a, "CGAME_YOUKILLED\x15Bob\x14CGAME_TEAMMATE"),
            "You killed Bob(teammate)"
        );
    }

    #[test]
    fn timers_round_the_way_the_original_does() {
        let mut e = HudElem::new(1);
        e.kind = he::TIMER_DOWN;
        e.time = 10_000;
        // 5.0 s left shows 5, 4.001 s left shows 5, 4.0 s left shows 4.
        assert_eq!(timer_text(elem_time_ms(&e, 5_000)), "0:05");
        assert_eq!(timer_text(elem_time_ms(&e, 5_999)), "0:05");
        assert_eq!(timer_text(elem_time_ms(&e, 6_001)), "0:04");
        assert_eq!(
            timer_text(elem_time_ms(&e, 20_000)),
            "0:00",
            "never negative"
        );
        e.kind = he::TIMER_UP;
        e.time = 1_000;
        assert_eq!(timer_text(elem_time_ms(&e, 62_000)), "1:01");
        assert_eq!(timer_text(3_600_000 + 61_000), "1:01:01");
    }

    #[test]
    fn tenths_timers_show_a_tenth() {
        let mut e = HudElem::new(1);
        e.kind = he::TENTHS_TIMER_DOWN;
        e.time = 10_000;
        assert_eq!(tenths_timer_text(elem_time_ms(&e, 7_650)), "0:02.4");
        assert_eq!(tenths_timer_text(elem_time_ms(&e, 7_700)), "0:02.3");
    }
}
