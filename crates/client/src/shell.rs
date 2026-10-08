// SPDX-License-Identifier: GPL-3.0-or-later
//! The menu shell: owns the menu system and its drawing state, and answers what the menus ask of the client.
//!
//! [`Shell`] is the player-facing front end. The menus run scripts that read and write dvars, run console text, start
//! a server, join one and answer the server's script menus; [`HostCx`] implements those over the client's input layer
//! (dvars) and a [`ShellState`] (stats, map and gametype lists, queued [`Action`]s the app carries out).

use crate::hud::{self, Feed, LiveUi, ScoreView};
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
    /// A menu sound (`play` in a menu script) for the sound system to play.
    Sound(String),
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
    /// What the native HUD draws this frame, filled by `NetPlay::fill_live` before painting.
    pub live: LiveUi,
    /// Message windows, chat and the centre string, which keep their lines between frames.
    pub feed: Feed,
    pub score_view: ScoreView,
    /// First scoreboard line shown (1 = the top).
    pub scores_top: usize,
    /// The scoreboard held up by a script step instead of the key (the `--ui-script` `scores` step).
    pub scores_forced: bool,
    pub hud_stats: hud::Stats,
    was_active: bool,
    /// The join menu's server lists (LAN discovery, favorites).
    pub servers: crate::serverlist::ServerList,
    /// The install's files, for `exec <file>.cfg` from a menu script.
    vfs: Option<::assets::vfs::Vfs>,
    exec_depth: u32,
    /// The display's video modes as `r_mode` and `r_displayRefresh` list them.
    pub video_modes: Vec<(u32, u32)>,
    pub refresh_rates: Vec<u32>,
}

/// The `r_mode` list when the display's own is unknown.
const STOCK_MODES: &[(u32, u32)] = &[
    (640, 480),
    (800, 600),
    (1024, 768),
    (1152, 864),
    (1280, 720),
    (1280, 768),
    (1280, 800),
    (1280, 960),
    (1280, 1024),
    (1360, 768),
    (1440, 900),
    (1600, 900),
    (1600, 1200),
    (1680, 1050),
    (1920, 1080),
    (1920, 1200),
];

/// Facts of the running match that menu expressions read.
#[derive(Default)]
pub struct GameFacts {
    pub scoreboard: bool,
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
    /// What the HUD owner-draws read.
    pub hud: crate::hudstate::HudFacts,
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
                let title = cell(3);
                maps.push(MapEntry {
                    name,
                    title: assets.translate(&title).map_or(title, |t| t.to_string()),
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
                let title = cell(1);
                gametypes.push(GameTypeEntry {
                    id: cell(0),
                    title: assets.translate(&title).map_or(title, |t| t.to_string()),
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
            live: LiveUi::default(),
            feed: Feed::default(),
            score_view: ScoreView::default(),
            scores_top: 1,
            scores_forced: false,
            hud_stats: hud::Stats::default(),
            was_active: false,
            servers: Default::default(),
            vfs: ::assets::vfs::Vfs::open_stock(&install.root, 0).ok(),
            exec_depth: 0,
            video_modes: STOCK_MODES.to_vec(),
            refresh_rates: vec![60],
        }
    }

    /// The scoreboard rows are on screen: held by the player, or the match is over and its menu is up.
    pub fn scoreboard_shown(&self, ui: &Ui) -> bool {
        self.game.scoreboard
            || self.scores_forced
            || (self.live.intermission && ui.is_open("scoreboard"))
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
    // The stock message windows show only while this is 1 (the server's scripts turn it off for hardcore).
    ("ui_hud_obituaries", "1"),
];

/// The settings of the options menus with the stock engine's defaults: archived, so a change survives a restart.
/// (The graphics ones only the menus read yet; `r_mode` and `r_displayRefresh` are filled from the display at start.)
const OPTION_DEFAULTS: &[(&str, &str)] = &[
    ("r_aspectRatio", "auto"),
    ("r_gamma", "0.8"),
    ("r_vsync", "0"),
    ("r_aaSamples", "1"),
    ("r_picmip", "0"),
    ("r_picmip_bump", "0"),
    ("r_picmip_spec", "0"),
    ("r_picmip_manual", "0"),
    ("r_rendererPreference", "dx9"),
    ("r_multiGpu", "0"),
    ("r_dlightLimit", "4"),
    ("r_zfeather", "1"),
    ("r_depthPrepassModels", "0"),
    ("r_lodScaleRigid", "1"),
    ("r_lodScaleSkinned", "1"),
    ("r_lodBiasRigid", "0"),
    ("r_lodBiasSkinned", "0"),
    ("r_drawWater", "1"),
    ("r_specular", "1"),
    ("r_dof_enable", "1"),
    ("r_glow_allowed", "1"),
    ("r_texFilterAnisoMin", "1"),
    ("r_texFilterAnisoMax", "4"),
    ("sm_enable", "1"),
    ("sc_enable", "1"),
    ("ragdoll_enable", "1"),
    ("fx_marks", "1"),
    ("ai_corpseCount", "10"),
    ("cl_freelook", "1"),
    ("m_filter", "0"),
];

/// Gives every dvar the menus read its stock value unless the config set it.
fn register_defaults(input: &mut Input) {
    for (k, v) in UI_DEFAULTS {
        if input.cvars.get(k).is_none() {
            input.cvars.set(k, v, false);
        }
    }
    for (k, v) in OPTION_DEFAULTS {
        if input.cvars.get(k).is_none() {
            input.cvars.set(k, v, true);
        }
    }
    // The invert-mouse switch shows what `m_pitch` already says.
    let inverted = input.cvars.f32("m_pitch") < 0.0;
    input
        .cvars
        .set("ui_mousePitch", if inverted { "1" } else { "0" }, false);
}

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
        register_defaults(input);
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

    /// Tells the menus which video modes and refresh rates the display has, and the mode the window is in now
    /// (`r_mode` and `r_displayRefresh` start there unless the config chose one).
    pub fn set_display(
        &mut self,
        input: &mut Input,
        mut modes: Vec<(u32, u32)>,
        mut rates: Vec<u32>,
        current: (u32, u32),
    ) {
        modes.sort_unstable();
        modes.dedup();
        rates.sort_unstable();
        rates.dedup();
        if !modes.is_empty() {
            self.st.video_modes = modes;
        }
        if !rates.is_empty() {
            self.st.refresh_rates = rates;
        }
        if input.cvars.get("r_mode").is_none_or(str::is_empty) {
            input
                .cvars
                .set("r_mode", &format!("{}x{}", current.0, current.1), true);
        }
        if input
            .cvars
            .get("r_displayRefresh")
            .is_none_or(str::is_empty)
        {
            let hz = self.st.refresh_rates.last().copied().unwrap_or(60);
            input
                .cvars
                .set("r_displayRefresh", &format!("{hz} Hz"), true);
        }
    }

    /// The clock the menus and the HUD run on, in milliseconds.
    pub fn now_ms(&self) -> i32 {
        self.st.started.elapsed().as_millis() as i32
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

    /// Gives a pending bind item the key the player pressed.
    pub fn bind_key(&mut self, input: &mut Input, key: &str) {
        let mut h = Self::host(&mut self.st, input);
        self.ui.bind_capture(&mut h, key);
    }

    /// Like [`Shell::click`], through the pointer.
    pub fn mouse_click(&mut self, input: &mut Input, want: &str) -> bool {
        let mut h = Self::host(&mut self.st, input);
        self.ui.mouse_click(&mut h, want)
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
            UiEvent::Print { kind, text } => self.print(kind, &text),
            UiEvent::Announce { text } => self.print(net::ui::PrintKind::Bold, &text),
            UiEvent::Chat { team, client, text } => {
                let live = &self.st.live;
                let esc = hud::team_color_escape(live.own_team, live.team(client));
                let who = format!("{esc}{}^7", live.name(client));
                let line = format!("{}{who}: {text}", if team { "(Team) " } else { "" });
                self.st.feed.chat(line, live.time);
                self.st.hud_stats.chat += 1;
            }
            UiEvent::Obituary(o) => {
                let now = self.st.live.time;
                self.st
                    .feed
                    .obituary(&o, &self.st.live, &self.ui.assets, now);
                self.st.hud_stats.obituaries += 1;
            }
            // A new level: the old one's menus are gone; the server opens its own again.
            UiEvent::Map { .. } => self.close_all(input),
            UiEvent::Scores => {}
        }
    }

    /// An `iprintln` line or announcement: into the message window it belongs to, or only the console log.
    fn print(&mut self, kind: net::ui::PrintKind, text: &str) {
        use net::ui::PrintKind;
        let text = hud::localize(&self.ui.assets, text);
        let now = self.st.live.time;
        match kind {
            PrintKind::Console => {
                let log = &mut self.st.feed.console;
                log.push(text);
                if log.len() > 256 {
                    log.remove(0);
                }
            }
            PrintKind::Normal => {
                self.st.feed.text(hud::NOTIFY, &text, now);
                self.st.hud_stats.messages[hud::NOTIFY] += 1;
            }
            PrintKind::Bold => {
                self.st.feed.text(hud::BOLD, &text, now);
                self.st.hud_stats.messages[hud::BOLD] += 1;
            }
        }
    }

    /// Once a frame, after the network has been read: what follows from the live state. Opens the end-of-match
    /// menus, clears the feed between matches and says whether the scoreboard needs fresh rows.
    pub fn tick(&mut self, input: &mut Input) {
        let live_now = self.st.live.active;
        if live_now != self.st.was_active {
            self.st.was_active = live_now;
            self.st.feed.clear();
        }
        // `DrawIntermission`: at the end of a match the scoreboard (or the end-of-game menu when the scripts ask
        // for it) is up for as long as the server holds the players there.
        if self.st.live.intermission {
            let show_end = input
                .cvars
                .get("ui_showEndOfGame")
                .is_some_and(|v| v.trim() == "1");
            let want = if show_end { "endofgame" } else { "scoreboard" };
            if !self.ui.is_open(want) {
                self.open(input, want);
            }
        }
        self.st.live.scores_wanted = self.st.scoreboard_shown(&self.ui);
        if self.st.live.active {
            let st = &mut self.st;
            let rows = st
                .live
                .scores_wanted
                .then(|| hud::rows_shown(&st.live.scores, st.live.own_team, st.scores_top));
            st.hud_stats.frame(&st.live, rows);
        }
        if !self.st.live.scores_wanted {
            self.st.scores_top = 1;
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
        self.st.score_view.refresh(&input.cvars);
        self.ui2d.team_colors = hud::team_colors(&input.cvars);
        {
            let mut p = Painter {
                g: &mut self.ui2d,
                cache: &mut self.cache,
                images: &mut self.images,
            };
            let in_game = self.st.in_game;
            if in_game {
                hud::draw_under(&self.ui, &mut p, &self.st);
            }
            let mut h = Self::host(&mut self.st, input);
            self.ui.paint(&mut h, &mut p, in_game);
            if in_game {
                hud::draw_over(&self.ui, &mut p, &self.st);
            }
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
    /// Runs a config file of the install, one line at a time (`exec options_graphics.cfg`).
    fn exec_file(&mut self, ui: &Ui, name: &str) {
        const MAX_DEPTH: u32 = 8;
        if self.st.exec_depth >= MAX_DEPTH {
            return;
        }
        let bytes = self
            .st
            .vfs
            .as_ref()
            .and_then(|v| v.read(name).ok().flatten());
        let Some(bytes) = bytes else {
            eprintln!("ui: exec {name}: no such file");
            return;
        };
        self.st.exec_depth += 1;
        for line in String::from_utf8_lossy(&bytes).lines() {
            self.command(ui, line);
        }
        self.st.exec_depth -= 1;
    }

    /// The strings of an enumerated dvar, as the menus list them.
    fn enum_list(&self, name: &str) -> Vec<String> {
        match name.to_ascii_lowercase().as_str() {
            "r_mode" => self
                .st
                .video_modes
                .iter()
                .map(|(w, h)| format!("{w}x{h}"))
                .collect(),
            "r_displayrefresh" => self
                .st
                .refresh_rates
                .iter()
                .map(|hz| format!("{hz} Hz"))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Sets a dvar; an enumerated one given an index (what the menus store) keeps the string it names.
    fn store(&mut self, name: &str, value: &str) {
        let list = self.enum_list(name);
        let value = match value.trim().parse::<usize>().ok().and_then(|i| list.get(i)) {
            Some(s) => s.as_str(),
            None => value,
        };
        self.input.set_cvar(name, value);
    }

    fn dvar_get(&self, name: &str) -> String {
        self.input.cvars.get(name).unwrap_or("").to_owned()
    }

    /// The list the join menu shows (`ui_netSource`).
    fn net_source(&self) -> crate::serverlist::Source {
        crate::serverlist::Source::from_dvar(&self.dvar_get("ui_netSource"))
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
                    self.store(a(1), &v);
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
                | "writeconfig"
                | "setprofile" => {}
                "exec" => self.exec_file(ui, a(1)),
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
        self.st.live.killcam
    }
    fn game_message_window_active(&self, w: i32) -> bool {
        usize::try_from(w).is_ok_and(|w| self.st.feed.active(w, self.st.live.time))
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
        if self.st.game.gametype.is_empty() {
            self.dvar_get("g_gametype")
        } else {
            self.st.game.gametype.clone()
        }
    }
    fn key_binding(&self, cmd: &str) -> String {
        self.key_bindings(cmd)
            .into_iter()
            .next()
            .unwrap_or_else(|| "KEY_UNBOUND".to_owned())
    }
    fn key_bindings(&self, cmd: &str) -> Vec<String> {
        self.input
            .binding_keys(cmd)
            .into_iter()
            .map(crate::input::key_id)
            .collect()
    }
    fn hud_fade(&self, f: HudFade) -> f32 {
        crate::ownerdraw::menu_fade(&self.st.game.hud, &self.input.cvars, f)
    }
    fn selecting_location(&self) -> bool {
        self.st.game.hud.selecting_location
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
        self.store(name, value);
    }

    fn dvar_enum(&self, name: &str) -> Vec<String> {
        self.enum_list(name)
    }

    fn set_bind(&mut self, key: &str, command: &str) {
        let line = if command.is_empty() {
            format!("unbind {}", crate::input::config::quote(key))
        } else {
            format!(
                "bind {} {}",
                crate::input::config::quote(key),
                crate::input::config::quote(command)
            )
        };
        self.input.exec_line(&line);
    }

    fn exec(&mut self, ui: &Ui, text: &str) {
        self.command(ui, text);
    }

    fn play(&mut self, alias: &str) {
        self.st.actions.push(Action::Sound(alias.to_owned()));
    }

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
            "update" => {
                // The original's `UI_Update` knows one name that matters here: the invert-mouse switch.
                if a(0).eq_ignore_ascii_case("ui_mousePitch") {
                    let invert = atoi(&self.dvar_get("ui_mousePitch")) != 0;
                    self.input
                        .set_cvar("m_pitch", if invert { "-0.022" } else { "0.022" });
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
            "refreshservers" | "refreshfilter" | "updatefilter" => {
                self.st.servers.refresh();
                true
            }
            "serversort" => {
                self.st.servers.sort_by(atoi(a(0)).max(0) as usize);
                true
            }
            "joinserver" => {
                if let Some(addr) = self.st.servers.selected() {
                    self.st.actions.push(Action::Join(addr.to_string()));
                }
                true
            }
            "createfavorite" => {
                // The popup's typed address; the stock script adds it only while the favorites list is showing.
                let typed = self.dvar_get("ui_favoriteAddress");
                if let Ok(addr) = crate::serverlist::resolve(&typed) {
                    self.st.servers.add_favorite(addr);
                }
                true
            }
            "addfavorite" => {
                if let Some(addr) = self.st.servers.selected() {
                    self.st.servers.add_favorite(addr);
                }
                true
            }
            "deletefavorite" => {
                if let Some(addr) = self.st.servers.selected() {
                    self.st.servers.remove_favorite(addr);
                }
                true
            }
            "loadarenas"
            | "stoprefresh"
            | "addplayerprofiles"
            | "setpbclstatus"
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
            | "serverstatus" => true,
            "quit" => {
                self.st.actions.push(Action::Quit);
                true
            }
            "startsingleplayer" | "runmod" | "createplayerprofile" | "deleteplayerprofile" => true,
            // No single-player, mods, player list or language switch here; the menus still run these.
            "clearloaderrorssummary"
            | "playerstart"
            | "updatelanguage"
            | "muteplayer"
            | "votetempban"
            | "votekick" => true,
            "clearerror" => {
                self.input.cvars.set("com_errorMessage", "", false);
                self.input.cvars.set("com_isNotice", "0", false);
                true
            }
            "votemap" | "votetypemap" | "votegame" => {
                let map = self.st.maps.get(self.st.map_sel).map(|m| m.name.as_str());
                let gt = self
                    .st
                    .gametypes
                    .get(self.st.gametype_sel)
                    .map(|g| g.id.as_str());
                let line = match (name.to_ascii_lowercase().as_str(), map, gt) {
                    ("votemap", Some(m), _) => Some(format!("callvote map {m}")),
                    ("votetypemap", Some(m), Some(g)) => Some(format!("callvote typemap {g} {m}")),
                    ("votegame", _, Some(g)) => Some(format!("callvote g_gametype {g}")),
                    _ => None,
                };
                if let Some(l) = line {
                    self.st.actions.push(Action::Console(l));
                }
                true
            }
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
            2 => {
                self.st.servers.poll();
                let src = self.net_source();
                self.st.servers.rows(src).len()
            }
            4 => self.st.maps.len(),
            _ => 0,
        }
    }

    fn feeder_text(&mut self, feeder: i32, row: usize, col: usize) -> String {
        match (feeder, col) {
            (2, _) => {
                let src = self.net_source();
                self.st
                    .servers
                    .rows(src)
                    .get(row)
                    .map(|e| server_cell(e, col, &self.st.maps, &self.st.gametypes))
                    .unwrap_or_default()
            }
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
        if feeder == 2 {
            let src = self.net_source();
            self.st.servers.select(src, row);
        }
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

    fn game_message_window(
        &mut self,
        ui: &Ui,
        p: &mut Painter,
        d: &ItemDef,
        rect: &::assets::zone::menu::Rect,
        color: [f32; 4],
    ) {
        let now = self.st.live.time;
        let window = usize::try_from(d.game_msg_window_index).unwrap_or(usize::MAX);
        let lines = self.st.feed.draw_window(ui, p, window, rect, d, color, now);
        if let Some(n) = self.st.hud_stats.window_lines.get_mut(window) {
            *n += lines as u32;
        }
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
        if crate::ownerdraw::draw(self, ui, p, d, rect, color) {
            return;
        }
        match d.window.owner_draw {
            90 => {
                let now = self.st.live.time;
                self.st.feed.draw_center(ui, p, d, rect, color, now);
            }
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

/// One cell of the stock server list (the columns of the original's join menu).
fn server_cell(
    e: &crate::serverlist::Entry,
    col: usize,
    maps: &[MapEntry],
    gametypes: &[GameTypeEntry],
) -> String {
    match col {
        2 if e.ping > 0 => e.hostname.chars().take(38).collect(),
        2 => e.addr.to_string(),
        3 => maps
            .iter()
            .find(|m| m.name == e.map)
            .map_or_else(|| e.map.clone(), |m| m.title.clone()),
        4 => format!("{} ({})", e.clients, e.max_clients),
        5 if e.gametype.is_empty() => "?".into(),
        5 => gametypes
            .iter()
            .find(|g| g.id == e.gametype)
            .map_or_else(|| e.gametype.clone(), |g| g.title.clone()),
        10 if e.ping > 0 => e.ping.to_string(),
        10 => "...".into(),
        _ => String::new(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serverlist::Entry;

    #[test]
    fn server_list_cells_follow_the_stock_columns() {
        let maps = vec![MapEntry {
            name: "mp_crash".into(),
            title: "MPUI_CRASH".into(),
            image: String::new(),
        }];
        let types = vec![GameTypeEntry {
            id: "war".into(),
            title: "MPUI_WAR".into(),
        }];
        let mut e = Entry::unanswered("10.0.0.1:28960".parse().unwrap());
        // Unanswered: the address stands in for the name and the ping shows dots.
        assert_eq!(server_cell(&e, 2, &maps, &types), "10.0.0.1:28960");
        assert_eq!(server_cell(&e, 10, &maps, &types), "...");
        assert_eq!(server_cell(&e, 5, &maps, &types), "?");
        e.hostname = "Host".into();
        e.map = "mp_crash".into();
        e.gametype = "war".into();
        e.clients = 3;
        e.max_clients = 18;
        e.ping = 12;
        assert_eq!(server_cell(&e, 2, &maps, &types), "Host");
        assert_eq!(server_cell(&e, 3, &maps, &types), "MPUI_CRASH");
        assert_eq!(server_cell(&e, 4, &maps, &types), "3 (18)");
        assert_eq!(server_cell(&e, 5, &maps, &types), "MPUI_WAR");
        assert_eq!(server_cell(&e, 10, &maps, &types), "12");
        e.map = "mp_custom".into();
        assert_eq!(server_cell(&e, 3, &maps, &types), "mp_custom");
    }

    use ::assets::zone::menu::{ItemData, MenuDef};

    /// Every action script of a menu: its open/close/escape handlers, key handlers and its items' handlers.
    fn scripts(m: &MenuDef) -> Vec<&str> {
        let mut out: Vec<&str> = [&m.on_open, &m.on_close, &m.on_esc]
            .into_iter()
            .chain(m.on_key.iter().map(|k| &k.action))
            .flatten()
            .map(|s| &**s)
            .collect();
        for it in &m.items {
            let own = [
                &it.action,
                &it.on_accept,
                &it.on_focus,
                &it.leave_focus,
                &it.mouse_enter,
                &it.mouse_exit,
            ];
            let dbl = match &it.data {
                ItemData::ListBox(Some(l)) => Some(&l.on_double_click),
                _ => None,
            };
            out.extend(
                own.into_iter()
                    .chain(it.on_key.iter().map(|k| &k.action))
                    .chain(dbl)
                    .flatten()
                    .map(|s| &**s),
            );
        }
        out
    }

    /// No script of a stock menu names a command or `uiScript` that nothing implements.
    #[test]
    fn every_stock_menu_script_command_and_ui_script_is_known() {
        let root =
            std::env::var_os("COD4_PATH").map_or_else(|| "COD4".into(), std::path::PathBuf::from);
        let Some(install) = Install::open(&root)
            .ok()
            .filter(|i| i.zone_path("ui_mp").is_some())
        else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let assets = UiAssets::load(&install).expect("ui assets");
        let mut st = ShellState::new(&assets, &install);
        let mut ui = Ui::new(assets, (1280, 720));
        let mut input = Input::detached();
        let mut h = HostCx {
            st: &mut st,
            input: &mut input,
        };
        let defs: Vec<_> = ui.assets.menus.values().cloned().collect();
        for def in defs {
            let m = ui
                .menu_index(def.window.name.as_deref().unwrap_or(""))
                .unwrap();
            for src in scripts(&def) {
                ui.run_script(&mut h, Some(m), None, src);
            }
        }
        assert!(ui.unknown.is_empty(), "unimplemented: {:?}", ui.unknown);
    }

    /// Opening the graphics menu copies the engine's settings into the menu's own dvars, and every value the menu
    /// shows names an entry of its list.
    #[test]
    fn the_graphics_menu_shows_the_current_settings() {
        let root =
            std::env::var_os("COD4_PATH").map_or_else(|| "COD4".into(), std::path::PathBuf::from);
        let Some(install) = Install::open(&root)
            .ok()
            .filter(|i| i.zone_path("ui_mp").is_some())
        else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let assets = UiAssets::load(&install).expect("ui assets");
        let mut st = ShellState::new(&assets, &install);
        let mut ui = Ui::new(assets, (1280, 720));
        let mut input = Input::detached();
        register_defaults(&mut input);
        input.cvars.set("r_mode", "1280x720", true);
        let mut h = HostCx {
            st: &mut st,
            input: &mut input,
        };
        ui.open_by_name(&mut h, "options_graphics");
        let def = ui.assets.menus["options_graphics"].clone();
        let mut shown = 0;
        for d in &def.items {
            let dv = d.dvar.as_deref().unwrap_or("");
            match (&d.data, d.ty) {
                (ItemData::Multi(Some(mu)), crate::ui::ity::MULTI) => {
                    let cur = h.dvar(dv);
                    // The menu's own copies come from the exec'd config; the rest are engine dvars.
                    assert!(
                        crate::ui::multi_index(mu, &cur).is_some(),
                        "{dv} = {cur:?} is not one of the menu's choices"
                    );
                    shown += 1;
                }
                (ItemData::EnumDvarName(n), crate::ui::ity::DVARENUM) => {
                    let list = h.dvar_enum(n.as_deref().unwrap_or(""));
                    assert!(!list.is_empty(), "{n:?} has no list");
                    shown += 1;
                }
                _ => {}
            }
        }
        assert!(shown >= 9, "only {shown} value items checked");
        // The mode menu stores an index and the engine dvar keeps the name it stands for.
        h.set_dvar("r_mode", "2");
        assert_eq!(h.dvar("r_mode"), "1024x768");
        h.set_dvar("r_mode", "1920x1080");
        assert_eq!(h.dvar("r_mode"), "1920x1080");
        h.set_dvar("r_aspectRatio", "2");
        assert_eq!(h.dvar("r_aspectRatio"), "2", "not an enumerated dvar");
    }
}
