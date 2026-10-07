// SPDX-License-Identifier: GPL-3.0-or-later
//! The menu shell: owns the menu system and its drawing state, and answers what the menus ask of the client.
//!
//! [`Shell`] is the player-facing front end. The menus run scripts that read and write dvars, run console text, start
//! a server, join one and answer the server's script menus; [`HostCx`] implements those over the client's input layer
//! (dvars) and a [`ShellState`] (stats, map and gametype lists, queued [`Action`]s the app carries out).

use crate::input::Input;
use crate::ui::assets::UiAssets;
use crate::ui::env::World;
use crate::ui::expr::{HudFade, PlayerField, TeamField, TeamSel, Value};
use crate::ui::paint::Painter;
use crate::ui::place::Px;
use crate::ui::{Host, Ui, UiKey};
use ::assets::zone::menu::ItemDef;
use render::ui2d::{Ui2d, UiImage};
use render::{Gpu, TextureCache};
use server::content::Install;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// What the app does on the menus' behalf after a UI call.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// Start a listen server on `map` with `gametype` and join it.
    StartServer {
        map: String,
        gametype: String,
    },
    /// Connect to `host:port`.
    Join(String),
    Disconnect,
    Quit,
    /// Tell the server's scripts a script menu answer.
    MenuResponse {
        menu: String,
        response: String,
    },
    /// A console line the shell does not handle itself (binds, `+commands`, ...).
    Console(String),
}

pub struct MapEntry {
    pub name: String,
    pub title: String,
    pub image: String,
}

pub struct GameTypeEntry {
    pub id: String,
    pub title: String,
}

/// Everything the shell remembers between frames.
pub struct ShellState {
    pub stats: Vec<i32>,
    pub actions: Vec<Action>,
    pub in_game: bool,
    pub started: Instant,
    pub maps: Vec<MapEntry>,
    pub gametypes: Vec<GameTypeEntry>,
    pub gametype_sel: usize,
    pub map_sel: usize,
    /// The live match as the expressions and HUD see it; empty in the menus.
    pub game: GameFacts,
    /// Printed lines (feed, centre messages, chat) as the server sent them.
    pub messages: Vec<(net::ui::PrintKind, String)>,
}

/// Facts of the running match that menu expressions read.
#[derive(Default)]
pub struct GameFacts {
    pub scoreboard: bool,
    pub killcam: bool,
    pub dead: bool,
    pub team: String,
    pub gametype: String,
    pub time_left: i32,
    pub allies_score: i32,
    pub axis_score: i32,
    pub score: i32,
    pub kills: i32,
    pub deaths: i32,
    pub ping: i32,
    pub clip_ammo: i32,
    pub intermission: bool,
}

/// Number of persistent stats the original keeps.
const STAT_COUNT: usize = 4000;

impl ShellState {
    pub fn new(assets: &UiAssets, install: &Install) -> Self {
        let mut maps = Vec::new();
        if let Some(t) = assets.table("mp/mapstable.csv") {
            for r in 0..t.row_count as usize {
                let cell = |c: usize| {
                    t.values[r * t.column_count as usize + c]
                        .as_deref()
                        .unwrap_or("")
                        .to_owned()
                };
                let name = cell(0);
                if name.ends_with("_locked") || name == "dlc" || !name.starts_with("mp_") {
                    continue;
                }
                if !install.map_exists(&name) {
                    continue;
                }
                maps.push(MapEntry {
                    name,
                    title: cell(3),
                    image: cell(4),
                });
            }
        }
        let mut gametypes = Vec::new();
        if let Some(t) = assets.table("mp/gametypestable.csv") {
            for r in 0..t.row_count as usize {
                let cell = |c: usize| {
                    t.values[r * t.column_count as usize + c]
                        .as_deref()
                        .unwrap_or("")
                        .to_owned()
                };
                gametypes.push(GameTypeEntry {
                    id: cell(0),
                    title: cell(1),
                });
            }
        }
        let gametype_sel = gametypes.iter().position(|g| g.id == "war").unwrap_or(0);
        ShellState {
            stats: vec![0; STAT_COUNT],
            actions: Vec::new(),
            in_game: false,
            started: Instant::now(),
            maps,
            gametypes,
            gametype_sel,
            map_sel: 0,
            game: GameFacts::default(),
            messages: Vec::new(),
        }
    }
}

/// The dvars the menus expect to exist before the first frame.
const UI_DEFAULTS: &[(&str, &str)] = &[
    ("com_playerProfile", "player"),
    ("ui_playerProfileCount", "1"),
    ("ui_playerProfileSelected", "player"),
    ("fs_game", ""),
    ("ui_hideBack", "0"),
    ("ui_showEndOfGame", "0"),
    ("ui_netGametype", "war"),
    ("ui_dedicated", "0"),
    ("onlinegame", "1"),
    ("ui_scorelimit", "0"),
    ("splitscreen", "0"),
    ("scr_allies", "usmc"),
    ("scr_axis", "arab"),
    ("g_TeamIcon_Allies", "faction_128_usmc"),
    ("g_TeamIcon_Axis", "faction_128_arab"),
    ("g_TeamName_Allies", "MPUI_MARINES_SHORT"),
    ("g_TeamName_Axis", "MPUI_OPFOR_SHORT"),
    ("cl_updateAvailable", "0"),
    ("ui_mod_logo", ""),
    ("sv_voice", "0"),
    ("scr_teambalance", "0"),
    ("g_allowvote", "1"),
    ("sv_punkbuster", "0"),
];

pub struct Shell {
    pub ui: Ui,
    pub st: ShellState,
    ui2d: Ui2d,
    cache: TextureCache,
    images: HashMap<String, UiImage>,
}

impl Shell {
    pub fn new(
        gpu: Arc<Gpu>,
        format: wgpu::TextureFormat,
        size: (u32, u32),
        install: &Install,
        input: &mut Input,
    ) -> Result<Self, String> {
        let mut assets = UiAssets::load(install)?;
        assets.note_menu_materials();
        let st = ShellState::new(&assets, install);
        for (k, v) in UI_DEFAULTS {
            if input.cvars.get(k).is_none() {
                input.cvars.set(k, v, false);
            }
        }
        let vfs = ::assets::vfs::Vfs::open_stock(&install.root, 0)
            .map_err(|e| format!("cannot open the install: {e}"))?;
        let mut shell = Shell {
            ui: Ui::new(assets, size),
            st,
            ui2d: Ui2d::new(gpu, format),
            cache: TextureCache::new(Some(vfs), 0),
            images: HashMap::new(),
        };
        shell.ui.cursor_visible = true;
        Ok(shell)
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        self.ui.resize(w, h);
    }

    fn host<'a>(st: &'a mut ShellState, input: &'a mut Input) -> HostCx<'a> {
        HostCx { st, input }
    }

    /// Opens a menu by name (the app's `togglemenu`, the server's `openmenu`).
    pub fn open(&mut self, input: &mut Input, name: &str) {
        let mut h = Self::host(&mut self.st, input);
        self.ui.open_by_name(&mut h, name);
    }

    pub fn close_by_name(&mut self, input: &mut Input, name: &str) {
        let mut h = Self::host(&mut self.st, input);
        self.ui.close_by_name(&mut h, name);
    }

    pub fn close_all(&mut self, input: &mut Input) {
        let mut h = Self::host(&mut self.st, input);
        self.ui.close_all(&mut h);
    }

    pub fn key(&mut self, input: &mut Input, key: UiKey) -> bool {
        let mut h = Self::host(&mut self.st, input);
        self.ui.key(&mut h, key)
    }

    /// Clicks the item of the top menu named or labelled `want`.
    pub fn click(&mut self, input: &mut Input, want: &str) -> bool {
        let mut h = Self::host(&mut self.st, input);
        self.ui.click(&mut h, want)
    }

    /// Carries out one thing the server asked of the UI.
    pub fn apply(&mut self, input: &mut Input, ev: net::ui::UiEvent) {
        use net::ui::UiEvent;
        match ev {
            UiEvent::SetDvar { name, value } => input.cvars.set(&name, &value, false),
            UiEvent::OpenMenu { name, mouse } => {
                self.open(input, &name);
                self.ui.cursor_visible = mouse;
            }
            UiEvent::CloseMenu { name } => {
                let target = if name.is_empty() {
                    self.ui
                        .open_menus()
                        .last()
                        .map(|s| (*s).to_owned())
                        .unwrap_or_default()
                } else {
                    name
                };
                self.close_by_name(input, &target);
            }
            UiEvent::CloseIngameMenu => self.close_all(input),
            UiEvent::Print { kind, text } => self.st.messages.push((kind, text)),
            UiEvent::Announce { text } => self.st.messages.push((net::ui::PrintKind::Bold, text)),
            UiEvent::Chat { client, text, .. } => self
                .st
                .messages
                .push((net::ui::PrintKind::Console, format!("{client}: {text}"))),
            UiEvent::Map { .. } | UiEvent::Scores | UiEvent::Obituary(_) => {}
        }
    }

    pub fn mouse_move(&mut self, input: &mut Input, x: f32, y: f32) {
        let mut h = Self::host(&mut self.st, input);
        self.ui.mouse_move(&mut h, x, y);
    }

    /// Draws the UI over `target`; the world, if any, is already there (`clear` for the menu-only case).
    pub fn paint(
        &mut self,
        input: &mut Input,
        target: &wgpu::TextureView,
        size: (u32, u32),
        clear: Option<wgpu::Color>,
    ) {
        self.ui2d.begin(size);
        {
            let mut p = Painter {
                g: &mut self.ui2d,
                cache: &mut self.cache,
                images: &mut self.images,
            };
            let mut h = Self::host(&mut self.st, input);
            let in_game = h.st.in_game;
            self.ui.paint(&mut h, &mut p, in_game);
        }
        self.ui2d.flush(target, clear);
    }

    /// Images the UI could not find, for the log.
    pub fn missing(&self) -> &[String] {
        &self.ui2d.missing
    }

    pub fn drain_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.st.actions)
    }
}

pub struct HostCx<'a> {
    pub st: &'a mut ShellState,
    pub input: &'a mut Input,
}

fn atoi(s: &str) -> i32 {
    s.trim().parse::<f64>().map_or(0, |v| v as i32)
}

impl HostCx<'_> {
    fn dvar_get(&self, name: &str) -> String {
        self.input.cvars.get(name).unwrap_or("").to_owned()
    }

    /// One console command the shell understands; `false` if it is not one of its own.
    fn command(&mut self, ui: &Ui, line: &str) {
        let toks: Vec<String> = crate::input::config::split_commands(line)
            .into_iter()
            .flatten()
            .filter(|t| t != "(" && t != ")" && t != ",")
            .collect();
        for cmd in crate::input::config::split_commands(line) {
            let Some(name) = cmd.first().map(|s| s.to_ascii_lowercase()) else {
                continue;
            };
            let a = |i: usize| cmd.get(i).map_or("", String::as_str);
            match name.as_str() {
                "setfromdvar" => {
                    let v = self.dvar_get(a(2));
                    self.input.cvars.set(a(1), &v, false);
                }
                "setdvartotime" => {
                    let t = self.st.started.elapsed().as_secs();
                    self.input.cvars.set(a(1), &t.to_string(), false);
                }
                "selectstringtableentryindvar" => {
                    // `<table> <column> <dvar>`: a random row's cell.
                    if let Some(t) = ui.assets.table(a(1)) {
                        let rows = t.row_count.max(1) as usize;
                        let r = (self.st.started.elapsed().as_millis() as usize / 1000) % rows;
                        let c =
                            (atoi(a(2)) as usize).min(t.column_count.saturating_sub(1) as usize);
                        let v = t.values[r * t.column_count as usize + c]
                            .as_deref()
                            .unwrap_or("")
                            .to_owned();
                        self.input.cvars.set(a(3), &v, false);
                    }
                }
                "statset" => {
                    if let Some(s) = self.st.stats.get_mut(atoi(a(1)) as usize) {
                        *s = atoi(a(2));
                    }
                }
                "statgetindvar" => {
                    let v = self.st.stats.get(atoi(a(1)) as usize).copied().unwrap_or(0);
                    self.input.cvars.set(a(2), &v.to_string(), false);
                }
                "statsetusingtable" => {
                    // statsetusingtable ( stat , tablelookup ( file , searchColumn , value , returnColumn ) )
                    let flat: Vec<&String> = toks.iter().collect();
                    if flat.len() >= 7 {
                        let v = ui.table_lookup(flat[3], atoi(flat[4]), flat[5], atoi(flat[6]));
                        if let Some(s) = self.st.stats.get_mut(atoi(flat[1]) as usize) {
                            *s = atoi(&v);
                        }
                    }
                }
                "statclearbitmask"
                | "statclearperknew"
                | "wait"
                | "snd_restart"
                | "vid_restart"
                | "updatedvarsfromprofile"
                | "loc_warnings"
                | "r_applypicmip"
                | "exec"
                | "writeconfig"
                | "setprofile" => {}
                "quit" => self.st.actions.push(Action::Quit),
                "disconnect" => self.st.actions.push(Action::Disconnect),
                "connect" => self.st.actions.push(Action::Join(a(1).to_owned())),
                "map" | "devmap" => self.st.actions.push(Action::StartServer {
                    map: a(1).to_owned(),
                    gametype: self.dvar_get("g_gametype"),
                }),
                "say" | "say_team" | "togglemenu" => self
                    .st
                    .actions
                    .push(Action::Console(crate::input::config::join(&cmd))),
                _ => self.input.exec_line(&crate::input::config::join(&cmd)),
            }
        }
    }
}

impl World for HostCx<'_> {
    fn scoreboard_visible(&self) -> bool {
        self.st.game.scoreboard
    }
    fn in_killcam(&self) -> bool {
        self.st.game.killcam
    }
    fn player_field(&self, f: PlayerField) -> Value {
        let g = &self.st.game;
        match f {
            PlayerField::TeamName => Value::Str(
                match g.team.as_str() {
                    "allies" => "TEAM_ALLIES",
                    "axis" => "TEAM_AXIS",
                    "spectator" => "TEAM_SPECTATOR",
                    _ => "TEAM_FREE",
                }
                .into(),
            ),
            PlayerField::OtherTeamName => Value::Str(
                match g.team.as_str() {
                    "allies" => "TEAM_AXIS",
                    "axis" => "TEAM_ALLIES",
                    _ => "TEAM_FREE",
                }
                .into(),
            ),
            PlayerField::Dead => Value::Int(i32::from(g.dead)),
            PlayerField::ClipAmmo => Value::Int(g.clip_ammo),
            PlayerField::NightVision => Value::Int(0),
            PlayerField::Score => Value::Int(g.score),
            PlayerField::Deaths => Value::Int(g.deaths),
            PlayerField::Kills => Value::Int(g.kills),
            PlayerField::Ping => Value::Int(g.ping),
        }
    }
    fn team_field(&self, t: TeamSel, f: TeamField) -> Value {
        let g = &self.st.game;
        let allies = match t {
            TeamSel::Marines => true,
            TeamSel::Opfor => false,
            TeamSel::Own => g.team == "allies",
            TeamSel::Other => g.team == "axis",
        };
        match f {
            TeamField::Score => Value::Int(if allies { g.allies_score } else { g.axis_score }),
            TeamField::Name => Value::Str(if allies { "TEAM_ALLIES" } else { "TEAM_AXIS" }.into()),
        }
    }
    fn stat(&self, i: i32) -> i32 {
        self.st.stats.get(i as usize).copied().unwrap_or(0)
    }
    fn time_left(&self) -> i32 {
        self.st.game.time_left
    }
    fn gametype(&self) -> String {
        self.st.game.gametype.clone()
    }
    fn key_binding(&self, cmd: &str) -> String {
        self.input
            .binding_keys(cmd)
            .first()
            .map_or_else(|| "KEY_UNBOUND".to_owned(), |k| k.to_ascii_uppercase())
    }
    fn hud_fade(&self, _: HudFade) -> f32 {
        1.0
    }
    fn is_intermission(&self) -> bool {
        self.st.game.intermission
    }
}

impl Host for HostCx<'_> {
    fn dvar(&self, name: &str) -> String {
        self.dvar_get(name)
    }

    fn set_dvar(&mut self, name: &str, value: &str) {
        self.input.cvars.set(name, value, false);
    }

    fn exec(&mut self, ui: &Ui, text: &str) {
        self.command(ui, text);
    }

    fn play(&mut self, _alias: &str) {}

    fn menu_response(&mut self, menu: &str, response: &str) {
        self.st.actions.push(Action::MenuResponse {
            menu: menu.to_owned(),
            response: response.to_owned(),
        });
    }

    fn ui_script(&mut self, ui: &mut Ui, name: &str, args: &[String]) -> bool {
        let a = |i: usize| args.get(i).map_or("", String::as_str);
        match name.to_ascii_lowercase().as_str() {
            "openmenuondvar" | "openmenuondvarnot" | "closemenuondvar" | "closemenuondvarnot" => {
                let n = name.to_ascii_lowercase();
                let eq = self.dvar_get(a(0)).eq_ignore_ascii_case(a(1));
                let want = !n.ends_with("not");
                if eq == want {
                    let m = a(2).to_owned();
                    // Re-enter through a console-less path: open/close need the host, which is `self`.
                    if n.starts_with("open") {
                        ui.open_by_name(self, &m);
                    } else {
                        ui.close_by_name(self, &m);
                    }
                }
                true
            }
            "startserver" => {
                let map = self
                    .st
                    .maps
                    .get(self.st.map_sel)
                    .map(|m| m.name.clone())
                    .unwrap_or_else(|| "mp_crash".into());
                let gametype = self
                    .st
                    .gametypes
                    .get(self.st.gametype_sel)
                    .map_or_else(|| "war".into(), |g| g.id.clone());
                self.st.actions.push(Action::StartServer { map, gametype });
                true
            }
            "loadarenas"
            | "stoprefresh"
            | "addplayerprofiles"
            | "updatefilter"
            | "setpbclstatus"
            | "update"
            | "getlanguage"
            | "verifylanguage"
            | "setrecommended"
            | "closejoin"
            | "sortplayerprofiles"
            | "selectactiveplayerprofile"
            | "loadplayerprofile"
            | "getcdkey"
            | "verifycdkey"
            | "clearmods"
            | "loadmods"
            | "refreshfilter"
            | "refreshservers"
            | "serversort"
            | "serverstatus" => true,
            "joinserver" => true,
            "createfavorite" | "deletefavorite" | "addfavorite" => true,
            "startsingleplayer" | "runmod" | "createplayerprofile" | "deleteplayerprofile" => true,
            _ => false,
        }
    }

    fn in_game(&self) -> bool {
        self.st.in_game
    }

    fn time_ms(&self) -> i32 {
        self.st.started.elapsed().as_millis() as i32
    }

    fn feeder_count(&mut self, feeder: i32) -> usize {
        match feeder {
            4 => self.st.maps.len(),
            _ => 0,
        }
    }

    fn feeder_text(&mut self, feeder: i32, row: usize, col: usize) -> String {
        match (feeder, col) {
            (4, _) => self
                .st
                .maps
                .get(row)
                .map(|m| m.title.clone())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    fn feeder_select(&mut self, feeder: i32, row: usize) {
        if feeder == 4 {
            self.st.map_sel = row;
            if let Some(m) = self.st.maps.get(row) {
                let n = m.name.clone();
                self.input.cvars.set("ui_mapname", &n, false);
            }
        }
    }

    fn feeder_image(&mut self, _feeder: i32, _row: usize, _col: usize) -> String {
        String::new()
    }

    fn owner_key(&mut self, _ui: &Ui, id: i32, key: &UiKey) -> bool {
        // 245: the gametype chooser of the server settings.
        if id == 245 {
            let n = self.st.gametypes.len();
            if n > 0 {
                let d = if *key == UiKey::Left { n - 1 } else { 1 };
                self.st.gametype_sel = (self.st.gametype_sel + d) % n;
                let id = self.st.gametypes[self.st.gametype_sel].id.clone();
                self.input.cvars.set("ui_netGametype", &id, false);
                self.input.cvars.set("g_gametype", &id, false);
            }
            return true;
        }
        false
    }

    fn owner_draw(
        &mut self,
        ui: &Ui,
        p: &mut Painter,
        d: &ItemDef,
        rect: Px,
        color: [f32; 4],
        text: &str,
    ) {
        match d.window.owner_draw {
            245 => {
                let t = self
                    .st
                    .gametypes
                    .get(self.st.gametype_sel)
                    .map(|g| g.title.clone())
                    .unwrap_or_default();
                let t = ui.assets.translate(&t).map_or(t.clone(), |s| s.to_string());
                ui_text(ui, p, d, rect, color, &t);
            }
            254 => {
                // Map preview: the loading-screen image of the selected map.
                if let Some(m) = self.st.maps.get(self.st.map_sel) {
                    let img = p.named(&ui.assets, &m.image);
                    p.pic(&img, rect, [1.0; 4]);
                }
            }
            _ => {
                let _ = text;
            }
        }
    }
}

/// Draws text inside a pixel rect for an owner-draw item, left aligned with the item's text offset.
fn ui_text(ui: &Ui, p: &mut Painter, d: &ItemDef, rect: Px, color: [f32; 4], text: &str) {
    use crate::ui::paint::TextDraw;
    let h = ui.text_height(d.font_enum, d.text_scale);
    ui.draw_text(
        p,
        &TextDraw {
            text,
            font_enum: d.font_enum,
            scale: d.text_scale,
            style: d.text_style,
            color,
            x: rect.x + d.text_align_x * ui.place.scale.0,
            y: rect.y + (d.text_align_y + h * 0.5) * ui.place.scale.1 + rect.h * 0.5,
            horz: 5,
            vert: 5,
        },
    );
}
