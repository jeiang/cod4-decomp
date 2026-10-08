// SPDX-License-Identifier: GPL-3.0-only
//! The expression environment: what menu expressions read, assembled from the UI's own state and the client's.

use super::expr::{
    Compiled, ExprEnv, HudFade, LocPart, LocalVar, PlayerField, TeamField, TeamSel, Value,
};
use super::{Host, Ui};

/// The game-side facts expressions ask about; the client implements them over the live match.
pub trait World {
    fn flashbanged(&self) -> bool {
        false
    }
    fn scoped(&self) -> bool {
        false
    }
    fn scoreboard_visible(&self) -> bool {
        false
    }
    fn in_killcam(&self) -> bool {
        false
    }
    fn player_field(&self, _: PlayerField) -> Value {
        Value::Int(0)
    }
    fn selecting_location(&self) -> bool {
        false
    }
    fn team_field(&self, _: TeamSel, _: TeamField) -> Value {
        Value::Int(0)
    }
    fn stat(&self, _: i32) -> i32 {
        0
    }
    fn time_left(&self) -> i32 {
        0
    }
    fn game_message_window_active(&self, _: i32) -> bool {
        false
    }
    fn gametype_name(&self) -> String {
        String::new()
    }
    fn gametype(&self) -> String {
        String::new()
    }
    fn gametype_description(&self) -> String {
        String::new()
    }
    fn score_at_rank(&self, _: i32) -> i32 {
        0
    }
    fn following(&self) -> bool {
        false
    }
    fn key_binding(&self, _: &str) -> String {
        String::new()
    }
    /// The key ids (`KEY_Q`, `KEY_MOUSE1`) bound to a command, in name order.
    fn key_bindings(&self, _: &str) -> Vec<String> {
        Vec::new()
    }
    fn action_slot_usable(&self, _: i32) -> bool {
        false
    }
    fn hud_fade(&self, _: HudFade) -> f32 {
        1.0
    }
    fn is_intermission(&self) -> bool {
        false
    }
    fn ads_javelin(&self) -> bool {
        false
    }
}

pub struct Env<'a> {
    pub ui: &'a Ui,
    pub host: &'a dyn Host,
}

impl ExprEnv for Env<'_> {
    fn dvar_string(&self, name: &str) -> String {
        self.host.dvar(name)
    }
    fn local_var(&self, name: &str) -> Option<LocalVar> {
        self.ui.locals.get(&name.to_ascii_lowercase()).cloned()
    }
    fn milliseconds(&self) -> i32 {
        self.host.time_ms()
    }
    fn ui_active(&self) -> bool {
        self.ui.captures_input()
    }
    fn flashbanged(&self) -> bool {
        self.host.flashbanged()
    }
    fn scoped(&self) -> bool {
        self.host.scoped()
    }
    fn scoreboard_visible(&self) -> bool {
        self.host.scoreboard_visible()
    }
    fn in_killcam(&self) -> bool {
        self.host.in_killcam()
    }
    fn player_field(&self, f: PlayerField) -> Value {
        self.host.player_field(f)
    }
    fn selecting_location(&self) -> bool {
        self.host.selecting_location()
    }
    fn team_field(&self, t: TeamSel, f: TeamField) -> Value {
        self.host.team_field(t, f)
    }
    fn menu_is_open(&self, name: &str) -> bool {
        self.ui.is_open(name)
    }
    fn stat(&self, i: i32) -> i32 {
        self.host.stat(i)
    }
    fn localize(&self, parts: &[LocPart]) -> String {
        let Some(first) = parts.first() else {
            return String::new();
        };
        let mut s = if first.is_ref {
            self.ui
                .assets
                .translate(&first.text)
                .map_or_else(|| first.text.clone(), |t| t.to_string())
        } else {
            first.text.clone()
        };
        for (i, p) in parts.iter().enumerate().skip(1) {
            let v = if p.is_ref {
                self.ui
                    .assets
                    .translate(&p.text)
                    .map_or_else(|| p.text.clone(), |t| t.to_string())
            } else {
                p.text.clone()
            };
            s = s
                .replace(&format!("&&{i}"), &v)
                .replace(&format!("&{i}"), &v);
        }
        s
    }
    fn table_lookup(
        &self,
        file: &str,
        search_column: i32,
        search_value: &str,
        return_column: i32,
    ) -> String {
        self.ui
            .table_lookup(file, search_column, search_value, return_column)
    }
    fn time_left(&self) -> i32 {
        self.host.time_left()
    }
    fn game_message_window_active(&self, w: i32) -> bool {
        self.host.game_message_window_active(w)
    }
    fn gametype_name(&self) -> String {
        self.host.gametype_name()
    }
    fn gametype(&self) -> String {
        self.host.gametype()
    }
    fn gametype_description(&self) -> String {
        self.host.gametype_description()
    }
    fn score_at_rank(&self, r: i32) -> i32 {
        self.host.score_at_rank(r)
    }
    fn following(&self) -> bool {
        self.host.following()
    }
    fn key_binding(&self, c: &str) -> String {
        self.ui.localize_key(&self.host.key_binding(c))
    }
    fn action_slot_usable(&self, s: i32) -> bool {
        self.host.action_slot_usable(s)
    }
    fn hud_fade(&self, e: HudFade) -> f32 {
        self.host.hud_fade(e)
    }
    fn is_intermission(&self) -> bool {
        self.host.is_intermission()
    }
    fn ads_javelin(&self) -> bool {
        self.host.ads_javelin()
    }
}

impl Ui {
    pub fn table_lookup(
        &self,
        file: &str,
        search_column: i32,
        value: &str,
        return_column: i32,
    ) -> String {
        let Some(t) = self.assets.table(file) else {
            return String::new();
        };
        let (cols, rows) = (t.column_count as i32, t.row_count as i32);
        if search_column < 0 || search_column >= cols || return_column < 0 || return_column >= cols
        {
            return String::new();
        }
        for r in 0..rows {
            let cell = |c: i32| t.values[(r * cols + c) as usize].as_deref().unwrap_or("");
            if cell(search_column).eq_ignore_ascii_case(value) {
                return cell(return_column).to_owned();
            }
        }
        String::new()
    }

    pub fn eval_bool(&self, host: &dyn Host, e: &Compiled) -> bool {
        e.eval_bool(&Env { ui: self, host })
    }

    pub fn eval_float(&self, host: &dyn Host, e: &Compiled) -> f32 {
        e.eval_float(&Env { ui: self, host })
    }

    pub fn eval_string(&self, host: &dyn Host, e: &Compiled) -> String {
        e.eval_string(&Env { ui: self, host })
    }
}
