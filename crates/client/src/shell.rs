// SPDX-License-Identifier: GPL-3.0-only
// The server browser's owner-draws and keys, the map list's game-mode filter and the Server Info list are translated in
// part from KisakCOD (ui_mp/ui_main_mp.cpp: UI_OwnerDraw, UI_OwnerDrawHandleKey, UI_BuildServerStatus,
// UI_MapCountByGameType; GPL-3.0, copyright the KisakCOD contributors and Activision).
//! The menu shell: owns the menu system and its drawing state, and answers what the menus ask of the client.
//!
//! [`Shell`] is the player-facing front end. The menus run scripts that read and write dvars, run console text, start
//! a server, join one and answer the server's script menus; [`HostCx`] implements those over the client's input layer
//! (dvars) and a [`ShellState`] (stats, map and gametype lists, queued [`Action`]s the app carries out).

use crate::hud::{self, Feed, LiveUi, ScoreView};
use crate::input::config::atoi;
use crate::input::{Input, Item};
use crate::profile::{CreateError, Profiles};
use crate::ui::assets::UiAssets;
use crate::ui::env::World;
use crate::ui::expr::{HudFade, PlayerField, TeamField, TeamSel, Value};
use crate::ui::loading::LoadingView;
use crate::ui::paint::Painter;
use crate::ui::place::Px;
use crate::ui::{Host, Ui, UiKey};
use ::assets::zone::menu::ItemDef;
use render::ui2d::{Ui2d, UiImage};
use render::{Gpu, TextureCache};
use server::content::Install;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use web_time::Instant;

/// What the app does on the menus' behalf after a UI call.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// Start a listen server on `map` with `gametype` and join it; `cheats` is `devmap` (`SV_Map_f` sets
    /// `sv_cheats` on for it, off for `map`).
    StartServer {
        map: String,
        gametype: String,
        cheats: bool,
    },
    /// Connect to `host:port`.
    Join(String),
    /// `playdemo <name>`: play a recorded demo.
    PlayDemo(String),
    /// A menu's `soundLoop` starts or stops.
    SoundLoop(String),
    SoundLoopStop(String),
    /// Join the server last joined again (`reconnect`).
    Reconnect,
    Disconnect,
    Quit,
    /// Tell the server's scripts a script menu answer.
    MenuResponse {
        menu: String,
        response: String,
    },
    /// A console line the shell does not handle itself (binds, `+commands`, ...).
    Console(String),
    /// `vid_restart`: take the graphics settings.
    VidRestart,
    /// `screenshot` or `screenshotJpeg`: write the next frame to the screenshots folder.
    Screenshot(crate::screenshot::Request),
    /// A menu sound (`play` in a menu script) for the sound system to play.
    Sound(String),
    /// Start the install's Windows Single Player executable.
    StartSingleplayer,
}

pub struct MapEntry {
    pub name: String,
    pub title: String,
    pub image: String,
    /// The game modes its `.arena` file allows; empty when it names none, which allows every mode.
    pub modes: Vec<String>,
}

pub struct GameTypeEntry {
    pub id: String,
    pub title: String,
    /// The in-game description (`OBJECTIVES_<ID>`), and its form with a score limit (`&&1` is the limit).
    pub objective: String,
    pub objective_score: String,
}

/// Everything the shell remembers between frames.
pub struct ShellState {
    pub stats: Vec<i32>,
    /// The player profiles, and the names the profile menu lists (feeder 24) in its order.
    pub profiles: Profiles,
    pub profile_names: Vec<String>,
    pub actions: Vec<Action>,
    /// The menu `soundLoop`s playing now.
    pub menu_loops: Vec<String>,
    pub in_game: bool,
    /// The GPU's name, for `setRecommended`'s match against `configure_mp.csv`.
    pub gpu_description: String,
    /// The languages the install has files for (`g_languages[].bPresent`), for the language menu.
    pub languages_present: Vec<usize>,
    pub started: Instant,
    pub maps: Vec<MapEntry>,
    pub gametypes: Vec<GameTypeEntry>,
    pub map_sel: usize,
    /// The rows picked in the kick/vote list (feeder 7) and the mute list (feeder 20), as player numbers.
    pub player_sel: usize,
    pub mute_sel: usize,
    /// Players the user muted (the toggle the mute menu shows; there is no voice chat to silence).
    pub muted: std::collections::HashSet<u16>,
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
    /// `cg_draw2D`: off, the HUD, the crosshair, the chat and the scoreboard are not drawn (`CG_Draw2D`); the menus
    /// and the console still are. A cheat dvar, so it only goes off with `sv_cheats` on.
    pub draw_2d: bool,
    /// The frame rate counter (`cg_drawFPS`): its frame times, what it asks for, and what it drew last frame.
    pub fps: hud::fps::Meter,
    pub fps_cfg: hud::fps::Cfg,
    /// What the renderer drew last frame, for the counter's `-Scene-` lines (set by the app after each render).
    pub fps_scene: hud::fps::Scene,
    pub fps_drawn: Option<String>,
    /// How many ownerdraws the HUD menus painted last frame (what `cg_draw2D` switches off).
    pub ownerdraws_painted: u64,
    pub hud_stats: hud::Stats,
    /// The names and icons over other players, and the dvars that shape them.
    pub names: hud::names::Names,
    pub names_cfg: hud::names::Cfg,
    /// The chat field and the console.
    pub console: crate::console::Console,
    was_active: bool,
    /// The join menu's server lists (LAN discovery, favorites).
    pub servers: crate::serverlist::ServerList,
    /// The Server Info popup's lines (feeder 13).
    pub status_rows: Vec<[String; 4]>,
    /// A map list row the lists are to scroll to on the next frame (the game mode changed under them).
    pub reselect_map: Option<usize>,
    /// The install's files, for completing `exec <file>`.
    vfs: Option<::assets::vfs::Vfs>,
    /// The install, and the mod (`fs_game`) the menus and files above were loaded for.
    root: std::path::PathBuf,
    pub fs_game: String,
    /// The mods folder's mods (name, description) as `loadmods` listed them (feeder 9), and the selected row.
    mods: Vec<(String, String)>,
    mod_sel: usize,
    /// The `.cfg` names of the install, listed on the first Tab that needs them.
    cfg_names: std::cell::OnceCell<Vec<String>>,
    /// The display's video modes as `r_mode` and `r_displayRefresh` list them.
    pub video_modes: Vec<(u32, u32)>,
    pub refresh_rates: Vec<u32>,
}

/// The `r_mode` list when the display's own is unknown.
pub(crate) const STOCK_MODES: &[(u32, u32)] = &[
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
    /// A line for the console's output; it keeps the latest 256.
    fn log(&mut self, text: &str) {
        let log = &mut self.feed.console;
        log.extend(text.lines().map(str::to_owned));
        let extra = log.len().saturating_sub(256);
        log.drain(..extra);
    }

    /// The maps the install can run and the game modes the menus offer, from the UI tables, and the selected map.
    fn catalog(assets: &UiAssets, install: &Install) -> (Vec<MapEntry>, usize, Vec<GameTypeEntry>) {
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
                    modes: assets
                        .arenas
                        .get(&name.to_ascii_lowercase())
                        .cloned()
                        .unwrap_or_default(),
                    name,
                    title: assets.translate(&title).map_or(title, |t| t.to_string()),
                    image: cell(4),
                });
            }
        } else {
            tracing::warn!("mp/mapstable.csv not found: no stock maps to offer");
        }
        // Custom maps (`usermaps/`, a mod's own) the tables do not list are offered under their file name.
        for name in ::assets::paths::custom_maps(&install.root, &install.game) {
            if !maps
                .iter()
                .any(|m: &MapEntry| m.name.eq_ignore_ascii_case(&name))
            {
                maps.push(MapEntry {
                    modes: Vec::new(),
                    title: name.clone(),
                    image: String::new(),
                    name,
                });
            }
        }
        // The server settings list the maps alphabetically by title; the table's first map stays the selected one.
        let first = maps.first().map(|m: &MapEntry| m.name.clone());
        maps.sort_by_cached_key(|m| m.title.to_lowercase());
        let map_sel = maps
            .iter()
            .position(|m| Some(&m.name) == first.as_ref())
            .unwrap_or(0);
        let mut gametypes = Vec::new();
        if let Some(t) = assets.table("mp/gametypestable.csv") {
            // Row 0 is the column header (`a0,b1,c2,d3`).
            for r in 1..t.row_count as usize {
                let cell = |c: usize| {
                    t.values[r * t.column_count as usize + c]
                        .as_deref()
                        .unwrap_or("")
                        .to_owned()
                };
                let title = cell(1);
                let id = cell(0);
                let text = |key: String| {
                    assets
                        .translate(&key)
                        .map(|t| t.to_string())
                        .unwrap_or_default()
                };
                gametypes.push(GameTypeEntry {
                    objective: text(format!("OBJECTIVES_{}", id.to_ascii_uppercase())),
                    objective_score: text(format!("OBJECTIVES_{}_SCORE", id.to_ascii_uppercase())),
                    id,
                    title: assets.translate(&title).map_or(title, |t| t.to_string()),
                });
            }
        } else {
            tracing::warn!("mp/gametypestable.csv not found: no game modes to offer");
        }
        // The original's chooser steps through the modes in this order, not the table's.
        const MODE_ORDER: [&str; 6] = ["dm", "dom", "sd", "sab", "war", "koth"];
        gametypes.sort_by_key(|g| {
            MODE_ORDER
                .iter()
                .position(|m| *m == g.id)
                .unwrap_or(MODE_ORDER.len())
        });
        (maps, map_sel, gametypes)
    }

    pub fn new(assets: &UiAssets, install: &Install) -> Self {
        let (maps, map_sel, gametypes) = Self::catalog(assets, install);
        ShellState {
            stats: vec![0; STAT_COUNT],
            profiles: Profiles::none(),
            profile_names: Vec::new(),
            actions: Vec::new(),
            menu_loops: Vec::new(),
            in_game: false,
            gpu_description: String::new(),
            languages_present: vec![0],
            started: Instant::now(),
            maps,
            gametypes,
            map_sel,
            player_sel: 0,
            mute_sel: 0,
            muted: Default::default(),
            game: GameFacts::default(),
            live: LiveUi::default(),
            feed: Feed::default(),
            score_view: ScoreView::default(),
            scores_top: 1,
            scores_forced: false,
            draw_2d: true,
            fps: hud::fps::Meter::default(),
            fps_cfg: hud::fps::Cfg::read(&crate::input::Cvars::default()),
            fps_scene: hud::fps::Scene::default(),
            fps_drawn: None,
            ownerdraws_painted: 0,
            hud_stats: hud::Stats::default(),
            names: hud::names::Names::default(),
            names_cfg: hud::names::Cfg::read(&crate::input::Cvars::default()),
            console: crate::console::Console::default(),
            was_active: false,
            // Unit tests never scan the real network.
            servers: crate::serverlist::ServerList::for_shell(),
            status_rows: Vec::new(),
            reselect_map: None,
            vfs: ::assets::vfs::Vfs::open(&install.root, 0, &install.game, None)
                .map_err(|e| {
                    tracing::warn!(
                        root = %install.root.display(),
                        "install files unavailable to the shell: {e}"
                    )
                })
                .ok(),
            root: install.root.clone(),
            fs_game: install.game.clone(),
            mods: Vec::new(),
            mod_sel: 0,
            cfg_names: std::cell::OnceCell::new(),
            video_modes: STOCK_MODES.to_vec(),
            refresh_rates: vec![60],
        }
    }

    /// The scoreboard rows are on screen: held by the player, or the match is over (`DrawIntermission` keeps drawing
    /// them behind a script popup; only the end-of-game menu replaces them).
    pub fn scoreboard_shown(&self, ui: &Ui) -> bool {
        self.game.scoreboard
            || self.scores_forced
            || (self.live.intermission && !ui.is_open("endofgame"))
    }

    /// The wheel, the arrows and the page keys move the scoreboard's list while it is up and has more rows than fit.
    pub fn scroll_scores(&mut self, ui: &Ui, key: &UiKey) -> bool {
        if !self.scoreboard_shown(ui) || self.live.scores.is_empty() {
            return false;
        }
        let live = &self.live;
        let m = &self.score_view.metrics;
        let page = hud::rows_shown(&live.scores, live.own_team, self.scores_top, m).max(1);
        let total = hud::scoreboard_lines(&live.scores, live.own_team, m);
        let top = self.scores_top;
        let to = match key {
            UiKey::WheelUp | UiKey::Up => top.saturating_sub(1),
            UiKey::WheelDown | UiKey::Down => top + 1,
            UiKey::PageUp => top.saturating_sub(page),
            UiKey::PageDown => top + page,
            _ => return false,
        };
        self.scores_top = to.clamp(1, total.max(1));
        true
    }
}

/// What the Internet list says when `cl_masters` names no master server.
const NO_MASTER: &str = "No master server set (cl_masters)";

/// What it says when none of the names in `cl_masters` could be looked up.
const UNRESOLVED_MASTER: &str = "Cannot find the master server (cl_masters)";

/// The dvars the menus expect to exist before the first frame.
const UI_DEFAULTS: &[(&str, &str)] = &[
    ("com_playerProfile", ""),
    ("ui_playerProfileCount", "0"),
    ("ui_playerProfileSelected", ""),
    ("ui_playerProfileNameNew", ""),
    ("ui_playerProfileAlreadyChosen", "0"),
    ("fs_game", ""),
    ("ui_hideBack", "0"),
    ("ui_showEndOfGame", "0"),
    ("ui_netGametype", "4"),
    ("ui_netGametypeName", "war"),
    ("ui_netSource", "0"),
    ("ui_joinGameType", "0"),
    // The userinfo dvar a join sends as its password (`password_popmenu` edits it); never saved.
    ("password", ""),
    ("ui_gametype", "0"),
    ("ui_currentMap", "0"),
    ("ui_currentNetMap", "0"),
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
    // The objective icons on the minimap draw only while this is not 0 (the stock config sets 1; hardcore turns it off).
    ("ui_hud_showobjicons", "1"),
    // The scoreboard's size and ping display (stock registers these without the archive flag).
    ("cg_scoreboardWidth", "500"),
    ("cg_scoreboardHeight", "435"),
    ("cg_scoreboardItemHeight", "18"),
    ("cg_scoreboardBannerHeight", "35"),
    ("cg_scoreboardPingWidth", "0.036"),
    ("cg_scoreboardPingHeight", "0.7"),
    ("cg_scoreboardPingText", "1"),
    ("cg_scoreboardPingGraph", "0"),
    // Cheat protected: only acts with `sv_cheats` on.
    ("snd_enableReverb", "1"),
];

/// The settings of the options menus with the stock engine's defaults: archived, so a change survives a restart.
/// (`r_mode` and `r_displayRefresh` are filled from the display at start.)
const OPTION_DEFAULTS: &[(&str, &str)] = &[
    // The frame cap in frames per second, 0 for none (the stock multiplayer default). Unlike stock it applies only
    // with `r_vsync` 0: with vsync on the display paces the frames, and a cap below the refresh rate would make the
    // cadence uneven.
    ("com_maxfps", "85"),
    ("cg_drawCrosshair", "1"),
    ("r_aspectRatio", "auto"),
    ("r_gamma", "0.8"),
    ("r_ignorehwgamma", "0"),
    // On, unlike the stock default: the window has always waited for the display, and this keeps it so.
    ("r_vsync", "1"),
    ("r_aaSamples", "1"),
    ("r_picmip", "0"),
    ("r_picmip_bump", "0"),
    ("r_picmip_spec", "0"),
    ("r_picmip_manual", "0"),
    // Kept for the menus only: a Direct3D 9 code path and an SLI/Crossfire mode have nothing to act on here.
    ("r_rendererPreference", "dx9"),
    ("r_multiGpu", "0"),
    ("r_dlightLimit", "4"),
    ("r_drawSun", "1"),
    ("r_zfeather", "1"),
    // The frame rate counter (`cg_drawFPS`: Off, Simple, SimpleRanges, Verbose; a stock 1.7 profile saves `Off`),
    // its labels and its corner offset, and the texture filtering the graphics menu writes.
    ("cg_drawFPS", "Off"),
    ("cg_drawFPSLabels", "1"),
    ("cg_debugInfoCornerOffset", "5 -5"),
    ("r_texFilterMipMode", "Unchanged"),
    // The options the Game and Sound menus write: blood effects, subtitles, brass, and the sound options this
    // build accepts but cannot act on (`snd_khz`: the mixer runs at the device's rate; `snd_enableEq`: no
    // equalizer pass).
    ("cg_blood", "1"),
    ("cg_brass", "1"),
    ("cg_subtitles", "1"),
    ("snd_khz", "44"),
    ("snd_enableEq", "1"),
    ("r_distortion", "1"),
    ("r_depthPrepassModels", "0"),
    ("r_lodScaleRigid", "1"),
    ("r_lodScaleSkinned", "1"),
    ("r_lodBiasRigid", "0"),
    ("r_lodBiasSkinned", "0"),
    ("r_drawWater", "1"),
    ("r_specular", "1"),
    ("r_dof_enable", "1"),
    ("r_dof_bias", "0.5"),
    ("r_dof_tweak", "0"),
    ("r_glow_allowed", "1"),
    ("r_texFilterAnisoMin", "1"),
    ("r_texFilterAnisoMax", "4"),
    ("snd_volume", "0.8"),
    // Stock: archived and cheat protected (they only act with `sv_cheats` on): how long slaves take to duck.
    ("snd_slaveFadeTime", "500"),
    // The shortest time a subtitle stays up, seconds (stock: archived).
    ("cg_subtitleMinTime", "3"),
    ("sm_enable", "1"),
    ("sc_enable", "0"),
    ("ragdoll_enable", "1"),
    ("fx_marks", "1"),
    // Kept for the menus only: it caps single-player actor corpses, which a multiplayer match has none of.
    ("ai_corpseCount", "10"),
    // Where the HUD puts the vote, the chat field and the chat lines (also while the scoreboard is up), and how
    // chat behaves.
    ("cg_hudVotePosition", "5 220"),
    ("cg_hudSayPosition", "5 180"),
    ("cg_hudChatPosition", "5 204"),
    ("cg_hudChatIntermissionPosition", "5 110"),
    ("cg_chatTime", "12000"),
    ("cg_chatHeight", "8"),
    // The damage wedge's size, distance from the screen centre and time on screen.
    ("cg_hudDamageIconWidth", "128"),
    ("cg_hudDamageIconHeight", "64"),
    ("cg_hudDamageIconOffset", "128"),
    ("cg_hudDamageIconTime", "2000"),
    // The scoreboard's ping graph.
    ("cg_ScoresPing_MaxBars", "4"),
    ("cg_ScoresPing_Interval", "100"),
    ("cg_ScoresPing_HighColor", "0.8 0 0 1"),
    ("cg_ScoresPing_MedColor", "0.8 0.8 0 1"),
    ("cg_ScoresPing_LowColor", "0 0.75 0 1"),
    ("cg_ScoresPing_BgColor", "0.25 0.25 0.25 0.5"),
    // The four game message windows: seconds on screen, lines, scroll, fade in and fade out.
    ("con_gameMsgWindow0MsgTime", "5"),
    ("con_gameMsgWindow0LineCount", "4"),
    ("con_gameMsgWindow0ScrollTime", "0.25"),
    ("con_gameMsgWindow0FadeInTime", "0.25"),
    ("con_gameMsgWindow0FadeOutTime", "0.5"),
    ("con_gameMsgWindow1MsgTime", "8"),
    ("con_gameMsgWindow1LineCount", "5"),
    ("con_gameMsgWindow1ScrollTime", "0.25"),
    ("con_gameMsgWindow1FadeInTime", "0.25"),
    ("con_gameMsgWindow1FadeOutTime", "0.01"),
    ("con_gameMsgWindow2MsgTime", "5"),
    ("con_gameMsgWindow2LineCount", "7"),
    ("con_gameMsgWindow2ScrollTime", "0.25"),
    ("con_gameMsgWindow2FadeInTime", "0.75"),
    ("con_gameMsgWindow2FadeOutTime", "0.5"),
    ("con_gameMsgWindow3MsgTime", "5"),
    ("con_gameMsgWindow3LineCount", "5"),
    ("con_gameMsgWindow3ScrollTime", "0.25"),
    ("con_gameMsgWindow3FadeInTime", "0.25"),
    ("con_gameMsgWindow3FadeOutTime", "0.5"),
    ("cl_freelook", "1"),
    ("m_filter", "0"),
    // The join menu's filters (`filter_popmenu`), the stock values. `ui_browserShowPure` (stock 1) has no control in
    // the popup: a profile carries it, and it lists pure servers only (`sv_pure`).
    ("ui_browserShowFull", "1"),
    ("ui_browserShowEmpty", "1"),
    ("ui_browserShowPassword", "-1"),
    ("ui_browserShowPure", "1"),
    ("ui_browserMod", "0"),
    ("ui_browserShowDedicated", "0"),
    ("ui_browserFriendlyfire", "-1"),
    ("ui_browserKillcam", "-1"),
    ("ui_browserShowPunkBuster", "-1"),
    // Not a stock dvar: the bots a server started from the menus plays with, on every map.
    ("bot_count", "0"),
    // Not a stock dvar: the master servers (`host[:port]`, space separated, up to five) the Internet list asks.
    // Empty until the user names one.
    ("cl_masters", ""),
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
        let mut st = ShellState::new(&assets, install);
        // Warnings and errors reach the console as the stock engine's `Com_PrintWarning`/`Com_Error` text does.
        logging::forward_to_console();
        st.servers.load_favorites(input.config_dir());
        register_defaults(input);
        let vfs = ::assets::vfs::Vfs::open(&install.root, 0, &install.game, None)
            .map_err(|e| format!("cannot open the install: {e}"))?;
        st.gpu_description = gpu.adapter.get_info().name;
        st.languages_present = vfs.languages_present();
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

    /// The device was replaced: the menus' pipelines and every texture uploaded so far belonged to the old one. They
    /// are made again, the images as the menus ask for them.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub fn replace_gpu(
        &mut self,
        gpu: Arc<Gpu>,
        format: wgpu::TextureFormat,
        install: &std::path::Path,
    ) -> Result<(), String> {
        let vfs = ::assets::vfs::Vfs::open(install, 0, &self.st.fs_game, None)
            .map_err(|e| format!("cannot open the install: {e}"))?;
        self.ui2d = Ui2d::new(gpu, format);
        self.cache = TextureCache::new(Some(vfs), 0);
        self.images.clear();
        Ok(())
    }

    /// `fs_game` was changed (`runmod`, `clearmods`, a server's mod): the mod the menus run is not the one in the
    /// cvar. The original restarts its file system at the next `vid_restart` for this.
    pub fn game_changed(&self, input: &Input) -> bool {
        crate::app::game_of(input) != self.st.fs_game
    }

    /// The file system restart for a new `fs_game` (`FS_Restart`): the menus, fonts, strings and textures are loaded
    /// again from the stock zones and the mod's `mod.ff` and IWDs, and the main menu opens.
    pub fn restart_files(
        &mut self,
        install: &Install,
        size: (u32, u32),
        input: &mut Input,
    ) -> Result<(), String> {
        let mut assets = UiAssets::load(install)?;
        assets.note_menu_materials();
        let (maps, map_sel, gametypes) = ShellState::catalog(&assets, install);
        let open = || {
            ::assets::vfs::Vfs::open(&install.root, 0, &install.game, None)
                .map_err(|e| format!("cannot open the install: {e}"))
        };
        self.cache = TextureCache::new(Some(open()?), 0);
        self.images.clear();
        self.ui = Ui::new(assets, size);
        self.ui.cursor_visible = true;
        self.st.vfs = Some(open()?);
        self.st.fs_game = install.game.clone();
        self.st.maps = maps;
        self.st.map_sel = map_sel;
        self.st.gametypes = gametypes;
        self.st.mods.clear();
        self.open(input, "main");
        Ok(())
    }

    /// Installs the player profiles and the active one's stats, and tells the menus.
    pub fn set_profiles(&mut self, input: &mut Input, profiles: Profiles, stats: Vec<i32>) {
        self.st.stats = stats;
        self.st.profiles = profiles;
        self.st.profile_names = self.st.profiles.list();
        let active = self.st.profiles.active().to_owned();
        input.cvars.set("com_playerProfile", &active, false);
        input.cvars.set(
            "ui_playerProfileCount",
            &self.st.profile_names.len().to_string(),
            false,
        );
        input.cvars.set("ui_playerProfileSelected", &active, false);
    }

    /// Tells the menus which video modes and refresh rates the display has, and the mode the window is in now
    /// (`r_mode` and `r_displayRefresh` start there unless the config chose one).
    pub fn set_display(
        &mut self,
        input: &mut Input,
        mut modes: Vec<(u32, u32)>,
        mut rates: Vec<u32>,
        current: (u32, u32),
        fullscreen: bool,
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
        // A page's size and fullscreen are the browser's to give (fullscreen needs a click, the canvas follows the
        // page), so there they are not kept in the config: the next visit starts from the page again.
        let keep = cfg!(not(target_arch = "wasm32"));
        if input.cvars.get("r_fullscreen").is_none() {
            input
                .cvars
                .set("r_fullscreen", if fullscreen { "1" } else { "0" }, keep);
        }
        if input.cvars.get("r_mode").is_none_or(str::is_empty) {
            input
                .cvars
                .set("r_mode", &format!("{}x{}", current.0, current.1), keep);
        }
        if input
            .cvars
            .get("r_displayRefresh")
            .is_none_or(str::is_empty)
        {
            let hz = self.st.refresh_rates.last().copied().unwrap_or(60);
            input
                .cvars
                .set("r_displayRefresh", &format!("{hz} Hz"), keep);
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

    /// The server refused a join (a wrong password, a full server, a ban): the stock error popup with the reason, over
    /// the menus. A wrong password is forgotten and asked for again.
    pub fn join_refused(&mut self, input: &mut Input, why: &str) {
        let mut h = Self::host(&mut self.st, input);
        show_refusal(&mut self.ui, &mut h, why);
    }

    /// Opens a menu by name (the app's `togglemenu`, the server's `openmenu`).
    pub fn open(&mut self, input: &mut Input, name: &str) {
        Self::open_menu(&mut self.st, &mut self.ui, input, name);
    }

    fn open_menu(st: &mut ShellState, ui: &mut Ui, input: &mut Input, name: &str) {
        let kind = if name == "scoreboard" {
            crate::ui::MenuKind::Scoreboard
        } else if st.in_game {
            crate::ui::MenuKind::Ingame
        } else {
            crate::ui::MenuKind::Other
        };
        let mut h = Self::host(st, input);
        ui.open_as(&mut h, name, kind);
    }

    pub fn close_by_name(&mut self, input: &mut Input, name: &str) {
        let mut h = Self::host(&mut self.st, input);
        self.ui.close_by_name(&mut h, name);
    }

    pub fn close_all(&mut self, input: &mut Input) {
        let mut h = Self::host(&mut self.st, input);
        self.ui.close_all(&mut h);
    }

    /// Runs a console line as a bind or a menu does (no echo; a cvar's name alone shows it).
    pub fn exec_line(&mut self, input: &mut Input, line: &str) {
        Self::host(&mut self.st, input).command(&self.ui, line);
    }

    /// The pointer to the middle of the screen (`UI_Popup`'s quick message menu starts it there).
    pub fn center_cursor(&mut self, input: &mut Input) {
        let (w, h) = self.ui.screen_size();
        self.mouse_move(input, w / 2.0, h / 2.0);
    }

    pub fn key(&mut self, input: &mut Input, key: UiKey) -> bool {
        if self.st.scroll_scores(&self.ui, &key) {
            return true;
        }
        let mut h = Self::host(&mut self.st, input);
        self.ui.key(&mut h, key)
    }

    /// Runs a menu script (the harness's colour probe).
    pub fn run_menu_script(&mut self, input: &mut Input, menu: &str, src: &str) {
        let mut h = Self::host(&mut self.st, input);
        self.ui.run_menu_script(&mut h, menu, src);
    }

    /// A mouse button came up: a dragged scroll bar or slider is let go.
    pub fn mouse_up(&mut self) {
        self.ui.key_up(&UiKey::Mouse1);
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

    /// Whether the top menu shows an item named or labelled `want`.
    pub fn has_item(&mut self, input: &mut Input, want: &str) -> bool {
        let mut h = Self::host(&mut self.st, input);
        self.ui.has_item(&mut h, want)
    }

    /// Clicks the item of the top menu named or labelled `want`.
    pub fn click(&mut self, input: &mut Input, want: &str) -> bool {
        let mut h = Self::host(&mut self.st, input);
        self.ui.click(&mut h, want)
    }

    /// Carries out one thing the server asked of the UI.
    pub fn apply(&mut self, input: &mut Input, ev: net::ui::UiEvent) {
        use net::ui::UiEvent;
        self.sync_layout(input);
        match ev {
            UiEvent::SetDvar { name, value } => input.cvars.set(&name, &value, false),
            UiEvent::OpenMenu { name, mouse } => {
                let mut h = Self::host(&mut self.st, input);
                self.ui.open_script_menu(&mut h, &name, mouse);
            }
            UiEvent::CloseMenu { name } => {
                self.ui.clear_waiting_menu();
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
            UiEvent::CloseIngameMenu => {
                let mut h = Self::host(&mut self.st, input);
                self.ui.close_ingame_menu(&mut h);
            }
            UiEvent::Print { kind, text } => self.print(input, kind, &text),
            UiEvent::Announce { text } => self.print(input, net::ui::PrintKind::Bold, &text),
            UiEvent::Chat {
                team,
                client,
                tag,
                text,
            } => {
                let text = hud::chat_text(&self.ui.assets, &text);
                self.st.hud_stats.note_text(&text);
                let line = self.chat_line(team, client, tag, &text);
                self.log(&line);
                self.st.feed.chat(line, self.st.live.time);
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
            // Nothing the closing menus would answer reaches a server that has moved on (`CG_CloseScriptMenu`).
            UiEvent::Map { .. } => {
                self.ui.clear_waiting_menu();
                self.ui.allow_menu_response = false;
                self.close_all(input);
                self.ui.allow_menu_response = true;
            }
            UiEvent::Scores => {}
        }
    }

    /// A line for the console's output; it keeps the latest 256.
    fn log(&mut self, text: &str) {
        self.st.log(text);
    }

    /// A line for the console's output only.
    pub fn print_console(&mut self, text: &str) {
        self.log(text);
    }

    /// A chat line as the player reads it: the speaker's name in the colour of their team, `(Team)` for a team line
    /// and `(Dead)` or `(Spectator)` for one from a player not in the match.
    fn chat_line(&self, team: bool, client: u16, tag: u8, text: &str) -> String {
        use net::ui::chat_tag;
        let live = &self.st.live;
        let esc = hud::team_color_escape(live.own_team, live.team(client));
        let state = match tag {
            chat_tag::DEAD => Some("GAME_DEAD"),
            chat_tag::SPECTATOR => Some("GAME_SPECTATOR"),
            _ => None,
        }
        .map_or_else(String::new, |k| {
            format!(
                "{esc}({}) ",
                hud::localize(&self.ui.assets, &format!("&{k}"))
            )
        });
        format!(
            "{state}{}{esc}{}^7: {text}",
            if team { "(Team) " } else { "" },
            live.name(client)
        )
    }

    /// A key typed into the chat field or the console.
    pub fn console_key(&mut self, input: &mut Input, key: UiKey) {
        use crate::console::{Arg, Entered, Want};
        let (maps, vfs, names) = (&self.st.maps, self.st.vfs.as_ref(), &self.st.cfg_names);
        let complete = |w: Want| -> Vec<String> {
            let starts = |n: &String| n.starts_with(w.prefix);
            match w.kind {
                Arg::Cvar => input
                    .cvars
                    .with_prefix(w.prefix)
                    .into_iter()
                    .map(|(n, _)| n)
                    .collect(),
                Arg::Map => maps
                    .iter()
                    .map(|m| m.name.to_ascii_lowercase())
                    .filter(starts)
                    .collect(),
                Arg::Cfg => vfs
                    .map(|v| names.get_or_init(|| v.names_with_ext(".cfg")))
                    .into_iter()
                    .flatten()
                    .filter(|n| starts(n))
                    .cloned()
                    .collect(),
                Arg::Demo => input
                    .config_dir()
                    .and_then(|d| std::fs::read_dir(d.join("demos")).ok())
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|e| {
                        let n = e.file_name().to_string_lossy().to_ascii_lowercase();
                        n.strip_suffix(".c4edemo").map(str::to_owned)
                    })
                    .filter(starts)
                    .collect(),
            }
        };
        match self.st.console.key(key, complete) {
            Some(Entered::Say { team, text }) => {
                let verb = if team { "say_team" } else { "say" };
                self.st
                    .actions
                    .push(Action::Console(crate::input::config::join(&[
                        verb.to_owned(),
                        text,
                    ])));
            }
            Some(Entered::Line(line)) => self.console_line(input, &line),
            None => {}
        }
    }

    /// A line typed at the console: echoed, then run as one buffer. A cvar's name alone says its value and with a
    /// value sets it; a `wait` holds the rest of the line.
    pub fn console_line(&mut self, input: &mut Input, line: &str) {
        self.log(&format!("] {line}"));
        let cmds = crate::input::config::split_commands(line)
            .into_iter()
            .map(|toks| Item {
                console: true,
                ..Item::new(toks, 0, None)
            })
            .collect();
        Self::host(&mut self.st, input).submit(&self.ui, cmds);
    }

    /// Runs the buffers whose `wait` has run out, once a frame.
    pub fn run_due(&mut self, input: &mut Input) {
        for (level, line) in logging::drain_console() {
            let (colour, word) = if level == tracing::Level::ERROR {
                ("^1", "ERROR")
            } else {
                ("^3", "WARNING")
            };
            self.st.log(&format!("{colour}{word}: {line}"));
        }
        if let Some(cmds) = input.take_due() {
            Self::host(&mut self.st, input).run_buffer(&self.ui, cmds);
        }
    }

    /// Reads the HUD placement and timing dvars, which take effect at once: the feed keeps what it last read.
    fn sync_layout(&mut self, input: &Input) {
        self.st.feed.layout = hud::Layout::read(&input.cvars);
    }

    /// An `iprintln` line or announcement: into the message window it belongs to, or only the console log.
    fn print(&mut self, input: &Input, kind: net::ui::PrintKind, text: &str) {
        self.sync_layout(input);
        use net::ui::PrintKind;
        let text = hud::localize(&self.ui.assets, text);
        let marks: Vec<String> = hud::key_marks(&text)
            .into_iter()
            .map(str::to_owned)
            .collect();
        let text = if marks.is_empty() {
            text
        } else {
            hud::expand_keys(&text, &self.key_texts(input, marks))
        };
        self.st.hud_stats.note_text(&text);
        let now = self.st.live.time;
        self.log(&text);
        match kind {
            PrintKind::Console => {}
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

    /// `DrawIntermission` / `CG_DrawActiveFrame`: at the end of a match the scoreboard (or the end-of-game menu when
    /// the scripts ask for it) is up for as long as the server holds the players there, unless a menu the server
    /// opened is up (the unlock and summary popups `_globallogic.gsc::endGame` opens): that one is left alone, and
    /// the scoreboard rows stay drawn behind it. Once the match is on again the end-of-game request is forgotten.
    fn intermission_menus(st: &mut ShellState, ui: &mut Ui, input: &mut Input) {
        if st.live.intermission {
            if !ui.script_popup_up() {
                let show_end = input
                    .cvars
                    .get("ui_showEndOfGame")
                    .is_some_and(|v| v.trim() == "1");
                let want = if show_end { "endofgame" } else { "scoreboard" };
                if !ui.is_open(want) {
                    Self::open_menu(st, ui, input, want);
                }
            }
        } else if st.live.active
            && input
                .cvars
                .get("ui_showEndOfGame")
                .is_some_and(|v| v.trim() != "0")
        {
            input.set_cvar("ui_showEndOfGame", "0");
        }
    }

    /// A sound's subtitle (`CG_SubtitlePrint`): `key` localized, in the subtitle window for as long as the sound
    /// lasts but at least `cg_subtitleMinTime`.
    pub fn subtitle(&mut self, input: &Input, key: &str, sound_ms: u32) {
        self.sync_layout(input);
        let min = (input.cvars.f32("cg_subtitleMinTime") * 1000.0).round() as i32;
        let ms = i32::try_from(sound_ms).unwrap_or(i32::MAX).max(min);
        let text = hud::localize(&self.ui.assets, key);
        let now = self.st.live.time;
        self.st.feed.text_for(hud::SUBTITLE, &text, now, ms);
    }

    /// Once a frame, after the network has been read: what follows from the live state. Opens the end-of-match
    /// menus, clears the feed between matches and says whether the scoreboard needs fresh rows.
    pub fn tick(&mut self, input: &mut Input) {
        self.sync_layout(input);
        self.st.score_view.metrics = hud::ScoreMetrics::read(&input.cvars);
        {
            let mut h = Self::host(&mut self.st, input);
            self.ui.check_waiting_menu(&mut h);
        }
        for (old, new) in std::mem::take(&mut self.st.live.renamed) {
            let verb = hud::localize(&self.ui.assets, "&CGAME_PLAYERRENAMES");
            let verb = if verb == "CGAME_PLAYERRENAMES" {
                "renamed to".to_owned()
            } else {
                verb
            };
            self.print(
                input,
                net::ui::PrintKind::Normal,
                &format!("{old}^7 {verb} {new}"),
            );
        }
        let live_now = self.st.live.active;
        if live_now != self.st.was_active {
            self.st.was_active = live_now;
            self.st.feed.clear();
        }
        Self::intermission_menus(&mut self.st, &mut self.ui, input);
        self.st.live.scores_wanted = self.st.scoreboard_shown(&self.ui);
        if self.st.live.active {
            let st = &mut self.st;
            let rows = st.live.scores_wanted.then(|| {
                hud::rows_shown(
                    &st.live.scores,
                    st.live.own_team,
                    st.scores_top,
                    &st.score_view.metrics,
                )
            });
            st.hud_stats.frame(&st.live, rows);
            st.names_cfg = hud::names::Cfg::read(&input.cvars);
            st.names
                .update(&st.live, &st.names_cfg, hud::team_colors(&input.cvars));
            st.hud_stats.names_offered(&st.names, &st.live);
        } else {
            let st = &mut self.st;
            st.names.update(&st.live, &st.names_cfg, [[1.0; 4]; 2]);
        }
        if !self.st.live.scores_wanted {
            self.st.scores_top = 1;
        }
        // The join list's highlight follows the selected server as rows come, go and re-sort.
        if self.ui.is_open("pc_join_unranked") {
            let row = Self::host(&mut self.st, input).selected_server_row();
            self.ui.sync_feeder_cursor(2, row);
        }
        if let Some(row) = self.st.reselect_map.take() {
            let mut h = Self::host(&mut self.st, input);
            self.ui.select_feeder_row(&mut h, 4, row);
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
        self.sync_layout(input);
        self.resolve_keys(input);
        self.ui2d.team_colors = hud::team_colors(&input.cvars);
        self.st.draw_2d = !input.cvars.bool("sv_cheats")
            || input.cvars.get("cg_draw2D").is_none_or(|v| v.trim() != "0");
        self.st.fps_cfg = hud::fps::Cfg::read(&input.cvars);
        self.st.fps.tick();
        let ownerdraws_before: u64 = self.st.game.hud.drawn.values().map(|&n| u64::from(n)).sum();
        {
            let mut p = Painter {
                g: &mut self.ui2d,
                cache: &mut self.cache,
                images: &mut self.images,
            };
            // `CG_Draw2D`: with `cg_draw2D` off none of the match's 2D is drawn; the open menus still are.
            let hud_on = self.st.in_game && self.st.draw_2d;
            if hud_on {
                hud::draw_under(&self.ui, &mut p, &self.st);
                hud::names::draw(
                    &self.ui,
                    &mut p,
                    &self.st.live,
                    &self.st.names,
                    &self.st.names_cfg,
                    &mut self.st.hud_stats,
                );
            }
            let mut h = Self::host(&mut self.st, input);
            self.ui.paint(&mut h, &mut p, hud_on);
            if hud_on {
                hud::draw_over(&self.ui, &mut p, &self.st);
            }
            // `CL_DrawScreen`: the frame rate counter, over everything but the console, while a match is on.
            self.st.fps_drawn = if self.st.in_game {
                hud::fps::draw(
                    &self.ui,
                    &mut p,
                    &self.st.fps,
                    &self.st.fps_cfg,
                    self.st.fps_scene,
                )
            } else {
                None
            };
            hud::draw_typing(
                &self.ui,
                &mut p,
                &self.st.console,
                &self.st.feed.console,
                &self.st.feed.layout,
            );
        }
        let ownerdraws_after: u64 = self.st.game.hud.drawn.values().map(|&n| u64::from(n)).sum();
        self.st.ownerdraws_painted = ownerdraws_after - ownerdraws_before;
        self.ui2d.flush(target, clear);
    }

    /// The keys of the `[{+command}]` marks in the HUD elements' text this frame.
    fn resolve_keys(&mut self, input: &Input) {
        let mut marks = Vec::new();
        for le in &self.st.live.elems {
            for raw in [&le.text, &le.label] {
                let text = hud::localize(&self.ui.assets, raw);
                self.st.hud_stats.note_text(&text);
                marks.extend(hud::key_marks(&text).into_iter().map(str::to_owned));
            }
        }
        if self.st.live.vote.is_some() {
            marks.extend(["vote yes".to_owned(), "vote no".to_owned()]);
        }
        // The spectator's help names the first of several commands that has a key.
        let mut keys = HashMap::new();
        for cmd in hud::spectator_commands(self.st.live.spectator_flags) {
            if let Some(key) = input
                .binding_keys(cmd)
                .into_iter()
                .map(crate::input::key_id)
                .next()
            {
                keys.insert(cmd.to_owned(), self.ui.localize_key(&key));
            }
        }
        keys.extend(self.key_texts(input, marks));
        self.st.live.keys = keys;
    }

    /// What each `[{+command}]` mark reads as: the keys the command is bound to, or the unbound text.
    fn key_texts(&self, input: &Input, cmds: Vec<String>) -> HashMap<String, String> {
        cmds.into_iter()
            .map(|cmd| {
                let bound: Vec<String> = input
                    .binding_keys(&cmd)
                    .into_iter()
                    .map(crate::input::key_id)
                    .collect();
                let text = self
                    .ui
                    .keys_text(&bound)
                    .unwrap_or_else(|| format!("{}({cmd})", self.ui.localize_key("KEY_UNBOUND")));
                (cmd, text)
            })
            .collect()
    }

    /// Draws the loading screen over `target`.
    pub fn paint_loading(
        &mut self,
        target: &wgpu::TextureView,
        size: (u32, u32),
        view: &LoadingView,
    ) {
        self.ui2d.begin(size);
        {
            let mut p = Painter {
                g: &mut self.ui2d,
                cache: &mut self.cache,
                images: &mut self.images,
            };
            // The view names the game mode by id; the screen shows its title.
            let title = self
                .st
                .gametypes
                .iter()
                .find(|g| g.id.eq_ignore_ascii_case(view.gametype))
                .map_or("", |g| g.title.as_str());
            self.ui.paint_loading(
                &mut p,
                &LoadingView {
                    gametype: title,
                    ..*view
                },
            );
        }
        self.ui2d.flush(target, Some(wgpu::Color::BLACK));
    }

    /// Images the UI could not find, for the log.
    pub fn missing(&self) -> &[String] {
        &self.ui2d.missing
    }

    /// How many rows the list of `feeder` shows now.
    pub fn feeder_rows(&mut self, input: &mut Input, feeder: i32) -> usize {
        Self::host(&mut self.st, input).feeder_count(feeder)
    }

    pub fn drain_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.st.actions)
    }
}

pub struct HostCx<'a> {
    pub st: &'a mut ShellState,
    pub input: &'a mut Input,
}

/// First and last stat of the perks, weapons and attachments that carry an unlock flag.
const UNLOCK_STATS: std::ops::RangeInclusive<i32> = 150..=298;

/// `Script_StatClearPerkNew`: an unlocked item (bit 0, or bit 1 for "new") stays unlocked and loses its "new" flag.
fn clear_new_flag(stats: &mut [i32], index: i32) -> Result<(), String> {
    if !UNLOCK_STATS.contains(&index) {
        return Err(format!("invalid stat index {index}"));
    }
    let stat = stats
        .get_mut(index as usize)
        .ok_or_else(|| format!("no stat {index}"))?;
    if *stat & 3 == 0 {
        return Err(format!("stat {index} isn't unlocked"));
    }
    *stat = (*stat & !2) | 1;
    Ok(())
}

/// `Script_StatClearBitMask`.
fn clear_stat_bits(stats: &mut [i32], stat: i32, mask: i32) {
    if let Some(s) = usize::try_from(stat).ok().and_then(|i| stats.get_mut(i)) {
        *s &= !mask;
    } else {
        logging::dedup!(WARN, "statclearbitmask", stat, "stat index out of range");
    }
}

impl HostCx<'_> {
    /// The other players of the match: client number and name, in slot order.
    fn players(&self) -> Vec<(u16, String)> {
        let live = &self.st.live;
        live.names
            .iter()
            .enumerate()
            .filter(|(n, name)| !name.is_empty() && *n != usize::from(live.own))
            .map(|(n, name)| (n as u16, clean_str(name)))
            .collect()
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

    /// The game mode the server settings show: the one `ui_netGametypeName` holds (a saved profile may have set it).
    fn gametype_sel(&self) -> usize {
        let want = self.dvar_get("ui_netGametypeName");
        let at = |id: &str| self.st.gametypes.iter().position(|g| g.id == id);
        at(&want).or_else(|| at("war")).unwrap_or(0)
    }

    fn dvar_get(&self, name: &str) -> String {
        self.input
            .cvars
            .get(name)
            .unwrap_or_else(|| {
                logging::dedup!(DEBUG, name, dvar = name, "dvar not set, read as empty");
                ""
            })
            .to_owned()
    }

    /// `UI_VerifyLanguage`: `ui_language` moves to the next language the install has files for
    /// (`SEH_VerifyLanguageSelection`), and `ui_languagechanged` says whether it differs from `loc_language`.
    fn verify_language(&mut self) {
        let old = atoi(&self.dvar_get("loc_language"));
        let new = atoi(&self.dvar_get("ui_language"));
        let n = ::assets::vfs::LANGUAGES.len() as i32;
        let present = &self.st.languages_present;
        let verified = (0..n)
            .map(|i| (i + new).rem_euclid(n))
            .find(|l| present.contains(&(*l as usize)))
            .unwrap_or(0);
        if verified != new {
            self.input
                .cvars
                .set("ui_language", &verified.to_string(), false);
        }
        self.input.cvars.set(
            "ui_languagechanged",
            if new == old { "0" } else { "1" },
            false,
        );
    }

    /// Re-reads the profile list into the menu's order and publishes the count.
    fn refresh_profile_names(&mut self) {
        self.st.profile_names = self.st.profiles.list();
        let n = self.st.profile_names.len().to_string();
        self.input.cvars.set("ui_playerProfileCount", &n, false);
    }

    /// The profile menu's scripts (`UI_AddPlayerProfiles` and friends).
    fn profile_script(&mut self, ui: &mut Ui, name: &str) {
        let select = |h: &mut Self, ui: &mut Ui, row: usize| {
            if row < h.st.profile_names.len() {
                ui.select_feeder_row(h, 24, row);
            }
        };
        let position = |h: &Self, name: &str| {
            h.st.profile_names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(name))
        };
        match name {
            "addplayerprofiles" | "sortplayerprofiles" => {
                let flip = name == "sortplayerprofiles";
                let up = if flip {
                    !self.st.profiles.ascending()
                } else {
                    true
                };
                self.st.profiles.set_ascending(up);
                self.refresh_profile_names();
                select(self, ui, 0);
            }
            "selectactiveplayerprofile" => {
                let active = self.dvar_get("com_playerProfile");
                if let Some(row) = position(self, &active) {
                    select(self, ui, row);
                }
            }
            "loadplayerprofile" => {
                let want = self.dvar_get("ui_playerProfileSelected");
                if want.is_empty() || want.eq_ignore_ascii_case(&self.dvar_get("com_playerProfile"))
                {
                    return;
                }
                if let Err(e) = self.input.save() {
                    tracing::error!("cannot save the settings: {e}");
                }
                if let Some(stats) = self.st.profiles.switch(&self.st.stats, &want) {
                    self.st.stats = stats;
                    let (read, write) = self.st.profiles.config_paths();
                    self.input.use_profile(read, write);
                    let active = self.st.profiles.active().to_owned();
                    self.input.cvars.set("com_playerProfile", &active, false);
                }
            }
            "createplayerprofile" => {
                let new = self.dvar_get("ui_playerProfileNameNew");
                if new.trim().is_empty() {
                    return;
                }
                self.input.cvars.set("ui_playerProfileNameNew", "", false);
                match self.st.profiles.create(&new) {
                    Ok(made) => {
                        self.refresh_profile_names();
                        if let Some(row) = position(self, &made) {
                            select(self, ui, row);
                        }
                    }
                    Err(e) => {
                        let popup = match e {
                            CreateError::TooMany => "profile_create_too_many_popmenu",
                            CreateError::Exists => "profile_exists_popmenu",
                            CreateError::Failed => "profile_create_fail_popmenu",
                        };
                        ui.open_by_name(self, popup);
                    }
                }
            }
            "deleteplayerprofile" => {
                let gone = self.dvar_get("ui_playerProfileSelected");
                if self.st.profile_names.is_empty() {
                    return;
                }
                if !self.st.profiles.delete(&gone) {
                    ui.open_by_name(self, "profile_delete_fail_popmenu");
                    return;
                }
                let row = position(self, &gone).unwrap_or(0);
                if self.st.profiles.active().is_empty() {
                    self.input.cvars.set("com_playerProfile", "", false);
                    self.st.stats = crate::profile::default_stats();
                }
                self.refresh_profile_names();
                if self.st.profile_names.is_empty() {
                    self.input.cvars.set("ui_playerProfileSelected", "", false);
                } else {
                    select(self, ui, row.min(self.st.profile_names.len() - 1));
                }
            }
            _ => {}
        }
    }

    /// The list the join menu shows (`ui_netSource`).
    fn net_source(&self) -> crate::serverlist::Source {
        crate::serverlist::Source::from_dvar(&self.dvar_get("ui_netSource"))
    }

    /// The game mode the join list is cut to (`ui_joinGameType`: 0 is every mode, `n` the `n`th mode of the table).
    fn join_gametype(&self) -> Option<&GameTypeEntry> {
        let n = usize::try_from(atoi(&self.dvar_get("ui_joinGameType"))).ok()?;
        self.st.gametypes.get(n.checked_sub(1)?)
    }

    /// The filter popup's settings (`ui_browser*`).
    fn server_filter(&self) -> crate::serverlist::Filter {
        let on = |name: &str| atoi(&self.dvar_get(name)) != 0;
        let n = |name: &str| atoi(&self.dvar_get(name));
        crate::serverlist::Filter {
            show_full: on("ui_browserShowFull"),
            show_empty: on("ui_browserShowEmpty"),
            password: n("ui_browserShowPassword"),
            pure_only: on("ui_browserShowPure"),
            dedicated_only: on("ui_browserShowDedicated"),
            modded: n("ui_browserMod"),
            friendly_fire: n("ui_browserFriendlyfire"),
            killcam: n("ui_browserKillcam"),
            punkbuster: n("ui_browserShowPunkBuster"),
        }
    }

    /// The join list as shown: the servers of the chosen source that run the chosen game mode (a server that has not
    /// answered does not say, so a chosen mode hides it) and pass the filter popup's settings.
    fn server_rows(&self) -> Vec<&crate::serverlist::Entry> {
        let mode = self.join_gametype().map(|g| g.id.as_str());
        let filter = self.server_filter();
        self.st
            .servers
            .rows(self.net_source())
            .iter()
            .filter(|e| mode.is_none_or(|m| e.gametype == m) && filter.passes(e))
            .collect()
    }

    /// Starts the list the join menu shows again (`UI_StartServerRefresh`): `full` forgets what a LAN scan found,
    /// otherwise the servers are asked again in place. The time is kept for the "Refresh time" line of the source.
    /// A list that is already being refreshed is left to finish.
    fn start_refresh(&mut self, ui: &Ui, full: bool) {
        let source = self.net_source();
        let masters = self.dvar_get("cl_masters");
        if !self.st.servers.refresh(source, full, &masters) {
            return;
        }
        self.st.servers.select(None);
        let n = atoi(&self.dvar_get("ui_netSource")).clamp(0, 2);
        let stamp = refresh_stamp(ui);
        self.input
            .cvars
            .set(&format!("ui_lastServerRefresh_{n}"), &stamp, false);
    }

    /// The row of the selected server in the join list, or -1: where the list's highlight belongs.
    fn selected_server_row(&self) -> i32 {
        let Some(sel) = self.st.servers.selected() else {
            return -1;
        };
        self.server_rows()
            .iter()
            .position(|e| e.addr == sel)
            .map_or(-1, |i| i as i32)
    }

    /// Takes in what the network brought: server answers and a status answer, which becomes the Server Info lines.
    fn poll_servers(&mut self) {
        self.st.servers.poll();
        if let Some(status) = self.st.servers.take_status() {
            let st = &*self.st;
            let mode = |id: &str| {
                st.gametypes
                    .iter()
                    .find(|g| g.id == id)
                    .map_or_else(|| id.to_owned(), |g| g.title.clone())
            };
            let map = |id: &str| {
                st.maps
                    .iter()
                    .find(|m| m.name == id)
                    .map_or_else(|| id.to_owned(), |m| m.title.clone())
            };
            self.st.status_rows = crate::serverlist::status_rows(&status, &mode, &map);
        }
    }

    /// The maps the chosen game mode (`ui_netGametypeName`) can be played on, as indices of the map table: the rows of
    /// the map list (`UI_MapCountByGameType`).
    fn mode_maps(&self) -> Vec<usize> {
        let mode = self
            .st
            .gametypes
            .get(self.gametype_sel())
            .map_or("", |g| g.id.as_str());
        (0..self.st.maps.len())
            .filter(|&i| {
                let m = &self.st.maps[i].modes;
                m.is_empty() || m.iter().any(|x| x == mode)
            })
            .collect()
    }

    /// After the game mode changed: keeps the selected map if the new mode has it, else takes the first, and has the
    /// lists show the choice.
    fn reselect_map(&mut self) {
        let maps = self.mode_maps();
        let row = maps.iter().position(|&i| i == self.st.map_sel).unwrap_or(0);
        if let Some(&i) = maps.get(row) {
            self.st.map_sel = i;
            self.set_current_map(i);
        }
        self.st.reselect_map = Some(row);
    }

    /// The map index the menus and the preview read (`ui_currentMap`, `ui_currentNetMap`).
    fn set_current_map(&mut self, index: usize) {
        let i = index.to_string();
        self.input.cvars.set("ui_currentMap", &i, false);
        self.input.cvars.set("ui_currentNetMap", &i, false);
    }

    /// One console line, as a menu script or a bind runs it.
    fn command(&mut self, ui: &Ui, line: &str) {
        let cmds = crate::input::config::split_commands(line)
            .into_iter()
            .map(|t| Item::new(t, 0, None))
            .collect();
        self.submit(ui, cmds);
    }

    /// New commands queue behind a pending `wait` instead of running ahead of it (`Cbuf_AddText`).
    fn submit(&mut self, ui: &Ui, cmds: VecDeque<Item>) {
        if self.input.busy() {
            self.input.enqueue(cmds);
        } else {
            self.run_buffer(ui, cmds);
        }
    }

    /// `Cbuf_Execute` over one buffer of commands the shell understands. An item typed at the console (`console`)
    /// shows or sets a cvar by its name alone, and an unknown word is said.
    fn run_buffer(&mut self, ui: &Ui, mut rest: VecDeque<Item>) {
        const MAX_INSERTS: u32 = 64;
        let mut inserted = 0;
        let toks: Vec<String> = rest
            .iter()
            .flat_map(|i| i.toks.iter())
            .filter(|t| *t != "(" && *t != ")" && *t != ",")
            .cloned()
            .collect();
        while let Some(Item {
            toks: cmd,
            depth,
            dir,
            console,
        }) = rest.pop_front()
        {
            let Some(name) = cmd.first().map(|s| s.to_ascii_lowercase()) else {
                continue;
            };
            let _span = tracing::debug_span!("command", name = name.as_str()).entered();
            let _ = console;
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
                    let Some(t) = ui.assets.table(a(1)) else {
                        logging::dedup!(
                            WARN,
                            a(1),
                            table = a(1),
                            "selectStringTableEntryInDvar: table not found"
                        );
                        continue;
                    };
                    let rows = t.row_count.max(1) as usize;
                    let r = (self.st.started.elapsed().as_millis() as usize / 1000) % rows;
                    let c = (atoi(a(2)) as usize).min(t.column_count.saturating_sub(1) as usize);
                    let v = t.values[r * t.column_count as usize + c]
                        .as_deref()
                        .unwrap_or("")
                        .to_owned();
                    self.input.cvars.set(a(3), &v, false);
                }
                "statset" => {
                    if let Some(s) = self.st.stats.get_mut(atoi(a(1)) as usize) {
                        *s = atoi(a(2));
                    } else {
                        logging::dedup!(WARN, "statset", stat = a(1), "stat index out of range");
                    }
                }
                "statgetindvar" => {
                    let v = self.st.stats.get(atoi(a(1)) as usize).copied();
                    if v.is_none() {
                        logging::dedup!(
                            WARN,
                            "statgetindvar",
                            stat = a(1),
                            "stat index out of range, read as 0"
                        );
                    }
                    self.input
                        .cvars
                        .set(a(2), &v.unwrap_or(0).to_string(), false);
                }
                "statsetusingtable" => {
                    // statsetusingtable ( stat , tablelookup ( file , searchColumn , value , returnColumn ) )
                    let flat: Vec<&String> = toks.iter().collect();
                    if flat.len() >= 7 {
                        let v = ui.table_lookup(flat[3], atoi(flat[4]), flat[5], atoi(flat[6]));
                        if let Some(s) = self.st.stats.get_mut(atoi(flat[1]) as usize) {
                            *s = atoi(&v);
                        } else {
                            logging::dedup!(
                                WARN,
                                "statsetusingtable",
                                stat = flat[1].as_str(),
                                "stat index out of range"
                            );
                        }
                    } else {
                        logging::dedup!(
                            WARN,
                            "statsetusingtable",
                            args = flat.len(),
                            "statsetusingtable: too few arguments"
                        );
                    }
                }
                // statClearBitMask <stat dvar> <mask dvar>: the dvars hold the stat number and the bits to clear.
                "statclearbitmask" => {
                    let from = |n: &str| {
                        let v = self.dvar_get(n);
                        atoi(if v.is_empty() { n } else { &v })
                    };
                    let (stat, mask) = (from(a(1)), from(a(2)));
                    clear_stat_bits(&mut self.st.stats, stat, mask);
                }
                // statClearPerkNew ( ref ): the perk, weapon or attachment has been looked at, so it is not "new".
                "statclearperknew" => {
                    let name = cmd
                        .iter()
                        .skip(1)
                        .find(|t| !matches!(t.as_str(), "(" | ")" | "\""))
                        .map_or("", String::as_str);
                    let index = atoi(&ui.table_lookup("mp/statstable.csv", 4, name, 1));
                    if let Err(e) = clear_new_flag(&mut self.st.stats, index) {
                        tracing::warn!(perk = name, "statClearPerkNew: {e}");
                    }
                }
                // The rest of the buffer waits for later frames (`Cmd_Wait_f`).
                "wait" => {
                    let n = if cmd.len() > 1 { atoi(a(1)) } else { 1 };
                    if n > 0 {
                        self.input
                            .defer(n.unsigned_abs(), std::mem::take(&mut rest));
                        return;
                    }
                }
                // `snd_volume` is read live every frame, and no other snd_ dvar needs a restart: these stay no-ops.
                "snd_restart"
                | "updatedvarsfromprofile"
                | "loc_warnings"
                | "r_applypicmip"
                | "writeconfig"
                | "setprofile" => {}
                // `vstr` and `exec` run their commands in place, in this buffer.
                "vstr" | "exec" => {
                    inserted += 1;
                    if inserted > MAX_INSERTS || (name == "exec" && depth >= 8) {
                        self.input.say(format!("{name}: too many nested commands"));
                    } else if let Some((next, from)) = self.input.expand(&cmd, dir.as_deref(), true)
                    {
                        let dir = from.map(Arc::from).or(dir);
                        let depth = depth + u32::from(name == "exec");
                        for toks in next.into_iter().rev() {
                            rest.push_front(Item::new(toks, depth, dir.clone()));
                        }
                    }
                }
                "vid_restart" => self.st.actions.push(Action::VidRestart),
                // The stock videos are Bink; there is no decoder, so the player is told rather than left to a
                // "Unknown command".
                "cinematic" | "unskippablecinematic" | "logo" => self
                    .st
                    .feed
                    .console
                    .push(format!("{name}: not supported (Bink video)")),
                "screenshot" | "screenshotjpeg" => {
                    let format = if name == "screenshot" {
                        crate::screenshot::Format::Tga
                    } else {
                        crate::screenshot::Format::Jpeg
                    };
                    match crate::screenshot::Request::parse(format, &cmd[1..]) {
                        Some(r) => self.st.actions.push(Action::Screenshot(r)),
                        None => tracing::warn!(command = name.as_str(), "bad screenshot arguments"),
                    }
                }
                "quit" => self.st.actions.push(Action::Quit),
                "disconnect" => self.st.actions.push(Action::Disconnect),
                "connect" => self.st.actions.push(Action::Join(a(1).to_owned())),
                "playdemo" | "demo" => self.st.actions.push(Action::PlayDemo(a(1).to_owned())),
                "reconnect" => self.st.actions.push(Action::Reconnect),
                // The server's `g_gametype` defaults to `war` (`SV_Init` registers it so); a client that has not
                // played yet has none set, and `set g_gametype` with nothing would fail the server's start.
                // A bare `map` does nothing, as `SV_Map_f` checks the zone only `if (*map)`.
                "map" | "devmap" if a(1).is_empty() => {}
                "map" | "devmap" => self.st.actions.push(Action::StartServer {
                    map: a(1).to_owned(),
                    gametype: match self.dvar_get("g_gametype") {
                        g if g.is_empty() => "war".to_owned(),
                        g => g,
                    },
                    cheats: name == "devmap",
                }),
                // `setFromLocString <dvar> <string>` (`Dvar_SetFromLocalizedStr_f`): an `@` reference is localized.
                "setfromlocstring" => {
                    if cmd.len() < 3 {
                        self.st
                            .log("USAGE: setFromLocalizedString <variable> <string>");
                    } else if !a(1).chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                        self.st.log(&format!("invalid variable name: {}", a(1)));
                    } else {
                        let combined = cmd[2..].join(" ");
                        let value = combined
                            .strip_prefix('@')
                            .and_then(|k| ui.assets.translate_ref(k))
                            .map(|t| t.to_string())
                            .filter(|t| !t.is_empty())
                            .unwrap_or(combined);
                        self.input.exec_line(&format!(
                            "set {} {}",
                            a(1),
                            crate::input::config::quote(&value)
                        ));
                    }
                }
                // The server's, or the app's own: not for the input layer, whose `pending_commands` a menu drops.
                n if crate::console::is_app_verb(n) => self
                    .st
                    .actions
                    .push(Action::Console(crate::input::config::join(&cmd))),
                // The input layer's own (`bind`, `set`, `toggle`, the `+` commands, what it hands on).
                n if crate::console::is_command(n) => {
                    self.input.exec_line(&crate::input::config::join(&cmd))
                }
                // `Cmd_ExecuteSingleCommand` past the commands: a cvar's name shows or sets it (`Dvar_Command`);
                // anything else is the app's to forward to the server or call unknown
                // (`CL_ForwardCommandToServer`), not the input layer's, whose pending list a menu drops.
                _ => {
                    if let Some(value) = self.input.cvars.get(&name).map(str::to_owned) {
                        if cmd.len() > 1 {
                            self.input.exec_line(&format!(
                                "set {name} {}",
                                crate::input::config::quote(&cmd[1..].join(" "))
                            ));
                        } else {
                            self.st.log(&format!("\"{name}\" is \"{value}\""));
                        }
                    } else {
                        self.st
                            .actions
                            .push(Action::Console(crate::input::config::join(&cmd)));
                    }
                }
            }
        }
    }
}

impl World for HostCx<'_> {
    fn flashbanged(&self) -> bool {
        self.st.live.flashed
    }
    fn scoped(&self) -> bool {
        self.st.live.scope.is_some()
    }
    fn following(&self) -> bool {
        self.st.live.following.is_some()
    }
    /// `UI_GetScoreAtRank`: the score of the n-th row of the scoreboard as the server ordered it, 0 past the end.
    fn score_at_rank(&self, rank: i32) -> i32 {
        usize::try_from(rank - 1)
            .ok()
            .and_then(|i| self.st.live.scores.get(i))
            .map_or(0, |r| r.score)
    }
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
            PlayerField::NightVision => Value::Int(i32::from(self.st.live.night_vision)),
            PlayerField::Score => Value::Int(g.score),
            PlayerField::Deaths => Value::Int(g.deaths),
            PlayerField::Kills => Value::Int(g.kills),
            PlayerField::Ping => Value::Int(g.ping),
        }
    }
    fn team_field(&self, t: TeamSel, f: TeamField) -> Value {
        let g = &self.st.game;
        // `GetTeamField`: the player's own team, as `TEAM_FREE` before the server has said (or on no team), and
        // `GetOtherTeamField`: the opposing team, the same spectator and unassigned names otherwise.
        let team = match t {
            TeamSel::Marines => "allies",
            TeamSel::Opfor => "axis",
            TeamSel::Own => g.team.as_str(),
            TeamSel::Other => match g.team.as_str() {
                "allies" => "axis",
                "axis" => "allies",
                other => other,
            },
        };
        match f {
            TeamField::Score => Value::Int(match team {
                "allies" => g.allies_score,
                "axis" => g.axis_score,
                _ => 0,
            }),
            TeamField::Name => Value::Str(
                match team {
                    "allies" => "TEAM_ALLIES",
                    "axis" => "TEAM_AXIS",
                    "spectator" => "TEAM_SPECTATOR",
                    _ => "TEAM_FREE",
                }
                .into(),
            ),
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
    fn gametype_name(&self) -> String {
        let gt = self.gametype();
        let t = self.st.gametypes.iter().find(|g| g.id == gt);
        t.map(|g| g.title.clone()).unwrap_or_default()
    }
    /// What the in-game menus say under the mode's name: the objective, with the score limit when there is one.
    fn gametype_description(&self) -> String {
        let gt = self.gametype();
        let Some(g) = self.st.gametypes.iter().find(|g| g.id == gt) else {
            return String::new();
        };
        let limit = self.dvar_get("ui_scorelimit");
        if limit.parse::<i32>().unwrap_or(0) > 0 && !g.objective_score.is_empty() {
            g.objective_score.replace("&&1", &limit)
        } else {
            g.objective.clone()
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
    fn action_slot_usable(&self, slot: i32) -> bool {
        self.st.game.hud.slots[(slot - 1) as usize].usable
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

    fn menu_loops(&mut self, aliases: &[String]) {
        for a in aliases.iter().filter(|a| !self.st.menu_loops.contains(a)) {
            self.st.actions.push(Action::SoundLoop(a.clone()));
        }
        for a in self.st.menu_loops.iter().filter(|a| !aliases.contains(a)) {
            self.st.actions.push(Action::SoundLoopStop(a.clone()));
        }
        self.st.menu_loops = aliases.to_vec();
    }

    fn menu_response(&mut self, menu: &str, response: &str) {
        self.st.actions.push(Action::MenuResponse {
            menu: menu.to_owned(),
            response: response.to_owned(),
        });
    }

    fn ui_script(&mut self, ui: &mut Ui, name: &str, args: &[String]) -> bool {
        let _span = tracing::debug_span!("menu_action", name).entered();
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
                let map = match self.st.maps.get(self.st.map_sel) {
                    Some(m) => m.name.clone(),
                    None => {
                        tracing::warn!(
                            sel = self.st.map_sel,
                            "startserver: no map selected, using mp_crash"
                        );
                        "mp_crash".into()
                    }
                };
                let gametype = self
                    .st
                    .gametypes
                    .get(self.gametype_sel())
                    .map_or_else(|| "war".into(), |g| g.id.clone());
                self.st.actions.push(Action::StartServer {
                    map,
                    gametype,
                    cheats: false,
                });
                true
            }
            // The list is made again; a quick refresh asks the servers it has again without forgetting them. Opening the
            // join menu scans the LAN afresh and only asks the other sources' servers.
            "refreshservers" => {
                self.start_refresh(ui, true);
                true
            }
            "refreshfilter" => {
                self.start_refresh(ui, false);
                true
            }
            "updatefilter" => {
                let lan = self.net_source() == crate::serverlist::Source::Lan;
                self.start_refresh(ui, lan);
                true
            }
            "stoprefresh" => {
                self.st.servers.stop_refresh();
                true
            }
            // Back out of a refresh before leaving the menu: the list stays as far as it got.
            "closejoin" => {
                if self.st.servers.refreshing(self.net_source()) {
                    self.st.servers.stop_refresh();
                    self.st.servers.select(None);
                }
                true
            }
            // The Server Info popup: asks the selected server for its status; the lines fill in when it answers.
            "serverstatus" => {
                self.st.status_rows.clear();
                if let Some(addr) = self.st.servers.selected() {
                    self.st.servers.request_status(addr);
                }
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
                match crate::serverlist::resolve(&typed) {
                    Ok(addr) => self.st.servers.add_favorite(addr),
                    Err(e) => tracing::warn!(address = typed, "createfavorite: bad address: {e}"),
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
            // The server settings open on the selected map, the list scrolled to it.
            "loadarenas" => {
                let row = self
                    .mode_maps()
                    .iter()
                    .position(|&i| i == self.st.map_sel)
                    .unwrap_or(0);
                ui.select_feeder_row(self, 4, row);
                true
            }
            // No PunkBuster and no CD key check here: the menus' buttons say so instead of doing nothing.
            "setpbclstatus" => {
                self.st.log("setPbClStatus: not supported (no PunkBuster)");
                true
            }
            "getcdkey" | "verifycdkey" => {
                self.st
                    .log(&format!("{name}: not supported (no CD key check)"));
                true
            }
            // The language menu (`UI_RunMenuScript`): the selection starts at the current language, is checked
            // against the languages present (`SEH_VerifyLanguageSelection`), and `updateLanguage` makes it current
            // and restarts the renderer.
            "getlanguage" => {
                let cur = self.dvar_get("loc_language");
                self.input.cvars.set("ui_language", &cur, false);
                self.verify_language();
                true
            }
            "verifylanguage" => {
                self.verify_language();
                true
            }
            "updatelanguage" => {
                let sel = self.dvar_get("ui_language");
                self.input.cvars.set("loc_language", &sel, true);
                self.verify_language();
                self.st.actions.push(Action::VidRestart);
                true
            }
            // `Com_SetRecommended`: the settings `configure_mp.csv` names for this CPU class and GPU.
            "setrecommended" => {
                let gpu = self.st.gpu_description.clone();
                let csv = self
                    .input
                    .vfs()
                    .and_then(|v| v.read("configure_mp.csv").ok().flatten());
                self.st.log("========= autoconfigure");
                match csv {
                    Some(bytes) => {
                        match crate::configure::recommended(&String::from_utf8_lossy(&bytes), &gpu)
                        {
                            Ok(r) => {
                                self.st.log(&r.cpu_line);
                                self.st.log(&r.gpu_line);
                                self.command(ui, "exec configure_mp.cfg");
                                for (n, v) in r.dvars {
                                    self.input.exec_line(&format!(
                                        "seta {n} {}",
                                        crate::input::config::quote(&v)
                                    ));
                                }
                            }
                            Err(e) => {
                                tracing::warn!(gpu, "setRecommended: {e}");
                                self.st.log(&format!("configure_mp.csv: {e}"));
                            }
                        }
                    }
                    None => self.st.log("configure_mp.csv: not found"),
                }
                true
            }
            // `UI_LoadMods`: the folders of `mods/` with their descriptions (the mods menu's list, feeder 9).
            "loadmods" => {
                self.st.mods = ::assets::paths::list_mods(&self.st.root);
                self.st.mod_sel = 0;
                true
            }
            // Back to the stock game: `fs_game` is reset and the file system restarts.
            "clearmods" => {
                self.input.cvars.set("fs_game", "", false);
                self.st.actions.push(Action::VidRestart);
                true
            }
            // Runs the selected mod: `fs_game` is `mods/<name>` and the file system restarts.
            "runmod" => {
                if let Some((name, _)) = self.st.mods.get(self.st.mod_sel) {
                    let game = format!("mods/{name}");
                    if ::assets::paths::valid_game(&game) {
                        self.input.cvars.set("fs_game", &game, false);
                        self.st.actions.push(Action::VidRestart);
                    } else {
                        tracing::warn!(game, "runmod: not a valid mod folder name");
                    }
                }
                true
            }
            "quit" => {
                self.st.actions.push(Action::Quit);
                true
            }
            "startsingleplayer" => {
                self.st.actions.push(Action::StartSingleplayer);
                true
            }
            "addplayerprofiles"
            | "sortplayerprofiles"
            | "selectactiveplayerprofile"
            | "loadplayerprofile"
            | "createplayerprofile"
            | "deleteplayerprofile" => {
                self.profile_script(ui, &name.to_ascii_lowercase());
                true
            }
            // `clearLoadErrorsSummary` closes every menu (`Menus_CloseAll`); `playerstart` is single-player's, and
            // the multiplayer `UI_RunMenuScript` says so on the console.
            "clearloaderrorssummary" => {
                ui.close_all(self);
                true
            }
            "playerstart" => {
                self.st.log("unknown UI script playerstart in block");
                true
            }
            "muteplayer" => {
                if let Some((n, _)) = self.players().get(self.st.mute_sel).cloned()
                    && !self.st.muted.remove(&n)
                {
                    self.st.muted.insert(n);
                }
                true
            }
            "votetempban" | "votekick" => {
                if let Some((n, _)) = self.players().get(self.st.player_sel) {
                    // The list is by slot: the by-name `kick` would need the name spelled the server's way.
                    let how = if name.eq_ignore_ascii_case("votekick") {
                        "clientkick"
                    } else {
                        "tempBanClient"
                    };
                    self.st
                        .actions
                        .push(Action::Console(format!("callvote {how} {n}")));
                }
                true
            }
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
                    .get(self.gametype_sel())
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
                self.poll_servers();
                self.server_rows().len()
            }
            4 => self.mode_maps().len(),
            9 => self.st.mods.len(),
            13 => {
                self.poll_servers();
                self.st.status_rows.len()
            }
            24 => self.st.profile_names.len(),
            7 | 20 => self.players().len(),
            _ => {
                logging::dedup!(
                    DEBUG,
                    &feeder.to_string(),
                    feeder,
                    "unknown list feeder, no rows"
                );
                0
            }
        }
    }

    fn feeder_text(&mut self, feeder: i32, row: usize, col: usize) -> String {
        match (feeder, col) {
            (2, _) => self
                .server_rows()
                .get(row)
                .map(|e| server_cell(e, col, &self.st.maps, &self.st.gametypes))
                .unwrap_or_default(),
            (13, _) => self
                .st
                .status_rows
                .get(row)
                .and_then(|r| r.get(col))
                .cloned()
                .unwrap_or_default(),
            (7, _) => self
                .players()
                .get(row)
                .map(|(_, n)| n.clone())
                .unwrap_or_default(),
            (20, 0) => self
                .players()
                .get(row)
                .filter(|(n, _)| self.st.muted.contains(n))
                .map(|_| "@MP_MUTED".to_owned())
                .unwrap_or_default(),
            (20, _) => self
                .players()
                .get(row)
                .map(|(_, n)| n.clone())
                .unwrap_or_default(),
            (24, _) => self.st.profile_names.get(row).cloned().unwrap_or_default(),
            (9, _) => self
                .st
                .mods
                .get(row)
                .map(|(_, desc)| desc.clone())
                .unwrap_or_default(),
            (4, _) => self
                .mode_maps()
                .get(row)
                .map(|&i| self.st.maps[i].title.clone())
                .unwrap_or_default(),
            _ => {
                logging::dedup!(
                    DEBUG,
                    &feeder.to_string(),
                    feeder,
                    col,
                    "no text for this list feeder"
                );
                String::new()
            }
        }
    }

    fn feeder_select(&mut self, feeder: i32, row: usize) {
        if feeder == 2 {
            let addr = self.server_rows().get(row).map(|e| e.addr);
            self.st.servers.select(addr);
        }
        if feeder == 7 {
            self.st.player_sel = row;
        }
        if feeder == 9 {
            self.st.mod_sel = row;
        }
        if feeder == 20 {
            self.st.mute_sel = row;
        }
        if feeder == 24
            && let Some(n) = self.st.profile_names.get(row).cloned()
        {
            self.input.cvars.set("ui_playerProfileSelected", &n, false);
        }
        if feeder == 4
            && let Some(&i) = self.mode_maps().get(row)
        {
            self.st.map_sel = i;
            self.set_current_map(i);
        }
    }

    /// The level shot of a map list row, for a list that shows pictures.
    fn feeder_image(&mut self, feeder: i32, row: usize, col: usize) -> String {
        match (feeder, col) {
            (4, 0) => self
                .mode_maps()
                .get(row)
                .map(|&i| self.st.maps[i].image.clone())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    fn owner_key(&mut self, ui: &Ui, id: i32, key: &UiKey) -> bool {
        // The pickers below step on a click or Enter, the right button back.
        let pick = matches!(key, UiKey::Mouse1 | UiKey::Mouse2 | UiKey::Enter);
        let back = *key == UiKey::Mouse2;
        // `i` stepped through `n` entries, wrapping.
        let next = |i: usize, n: usize| if back { (i + n - 1) % n } else { (i + 1) % n };
        match id {
            // 245: the gametype chooser of the server settings. A click or Enter steps on, the right button back (the
            // arrow keys are not its: they move the focus on, as in the original); the map list and the settings menu follow `ui_netGametypeName`.
            245 => {
                let n = self.st.gametypes.len();
                let step = pick;
                if n > 0 && step {
                    let d = if back { n - 1 } else { 1 };
                    let sel = (self.gametype_sel() + d) % n;
                    let id = self.st.gametypes[sel].id.clone();
                    self.input
                        .cvars
                        .set("ui_netGametype", &sel.to_string(), false);
                    self.input.cvars.set("ui_netGametypeName", &id, false);
                    self.input.cvars.set("g_gametype", &id, false);
                    self.reselect_map();
                }
                step
            }
            // 205: the game mode of a list of modes (`ui_gametype`).
            205 if pick && !self.st.gametypes.is_empty() => {
                let at = usize::try_from(atoi(&self.dvar_get("ui_gametype"))).unwrap_or(0);
                let to = next(at, self.st.gametypes.len());
                self.input.cvars.set("ui_gametype", &to.to_string(), false);
                true
            }
            // 220: the list to browse; each list scans again.
            220 if pick => {
                let at =
                    usize::try_from(atoi(&self.dvar_get("ui_netSource")).clamp(0, 2)).unwrap_or(0);
                let to = next(at, 3);
                self.input.cvars.set("ui_netSource", &to.to_string(), false);
                self.start_refresh(ui, true);
                self.st.servers.select(None);
                true
            }
            // 222: the server filter button; the filter popup's settings apply as the list is built.
            222 if pick => {
                self.st.servers.select(None);
                true
            }
            // 253: the game mode the join list is cut to; 0 is every mode.
            253 if pick => {
                let at = usize::try_from(atoi(&self.dvar_get("ui_joinGameType"))).unwrap_or(0);
                let to = next(at, self.st.gametypes.len() + 1);
                self.input
                    .cvars
                    .set("ui_joinGameType", &to.to_string(), false);
                self.st.servers.select(None);
                true
            }
            _ => false,
        }
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
                    .get(self.gametype_sel())
                    .map(|g| g.title.clone())
                    .unwrap_or_default();
                let t = ui.assets.translate(&t).map_or(t.clone(), |s| s.to_string());
                ui_text(ui, p, d, rect, color, &t);
            }
            // 250: what a bind item waits for (`UI_DrawKeyBindStatus`).
            250 => {
                let key = if ui.bind_pending() {
                    "EXE_KEYWAIT"
                } else {
                    "EXE_KEYCHANGE"
                };
                ui_text(ui, p, d, rect, color, &translate(ui, key));
            }
            205 => {
                let at = usize::try_from(atoi(&self.dvar_get("ui_gametype"))).unwrap_or(0);
                let t = self
                    .st
                    .gametypes
                    .get(at)
                    .map_or_else(|| translate(ui, "EXE_ALL"), |g| g.title.clone());
                ui_text(ui, p, d, rect, color, &t);
            }
            220 => {
                let at =
                    usize::try_from(atoi(&self.dvar_get("ui_netSource")).clamp(0, 2)).unwrap_or(0);
                let src = translate(ui, ["EXE_LOCAL", "EXE_INTERNET", "EXE_FAVORITES"][at]);
                ui_text(ui, p, d, rect, color, &loc_with(ui, "EXE_NETSOURCE", &src));
            }
            222 => {
                let all = translate(ui, "EXE_ALL");
                ui_text(
                    ui,
                    p,
                    d,
                    rect,
                    color,
                    &loc_with(ui, "EXE_SERVERFILTER", &all),
                );
            }
            253 => {
                let t = self
                    .join_gametype()
                    .map_or_else(|| translate(ui, "EXE_ALL"), |g| g.title.clone());
                ui_text(ui, p, d, rect, color, &t);
            }
            247 => {
                // While the list is being collected the line says how far it got, pulsing; after, when it was made.
                let source = self.net_source();
                if source == crate::serverlist::Source::Internet
                    && self.dvar_get("cl_masters").trim().is_empty()
                {
                    // Not a stock string: the stock game always had a master to ask.
                    ui_text(ui, p, d, rect, color, NO_MASTER);
                } else if source == crate::serverlist::Source::Internet
                    && self.st.servers.masters_unresolved()
                {
                    ui_text(ui, p, d, rect, color, UNRESOLVED_MASTER);
                } else if self.st.servers.refreshing(source) {
                    let n = self.st.servers.rows(source).len();
                    let text = if source == crate::serverlist::Source::Internet {
                        translate(ui, "EXE_WAITINGFORMASTERSERVERRESPONSE")
                    } else {
                        loc_with(ui, "EXE_GETTINGINFOFORSERVERS", &n.to_string())
                    };
                    let t = ((self.time_ms() / 75) as f32).sin() * 0.5 + 0.5;
                    let pulse = color.map(|c| c + (c * 0.8 - c) * t);
                    ui_text(ui, p, d, rect, pulse, &text);
                } else {
                    let at = atoi(&self.dvar_get("ui_netSource")).clamp(0, 2);
                    let when = self.dvar_get(&format!("ui_lastServerRefresh_{at}"));
                    ui_text(
                        ui,
                        p,
                        d,
                        rect,
                        color,
                        &loc_with(ui, "EXE_REFRESHTIME", &when),
                    );
                }
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

/// A name without its `^n` colour codes (`I_CleanStr`): the lists show names plain.
fn clean_str(name: &str) -> String {
    let mut out = String::new();
    let mut it = name.chars().peekable();
    while let Some(c) = it.next() {
        if c == '^' && it.peek().is_some_and(char::is_ascii_digit) {
            it.next();
        } else {
            out.push(c);
        }
    }
    out
}

/// The text of a localize key, or the key itself when it has none.
fn translate(ui: &Ui, key: &str) -> String {
    ui.assets
        .translate(key)
        .map_or_else(|| key.to_owned(), |t| t.to_string())
}

/// A localize string with its `&&1` filled in.
fn loc_with(ui: &Ui, key: &str, arg: &str) -> String {
    translate(ui, key).replace("&&1", arg)
}

/// The date and time as the server list shows when it was made: `Oct 9, 2026   14:05`. UTC, as the client has no
/// source for the zone.
fn refresh_stamp(ui: &Ui) -> String {
    let secs = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (year, month, day, hour, minute) = civil(i64::try_from(secs).unwrap_or(0));
    const MONTHS: [&str; 12] = [
        "EXE_MONTH_ABV_JANUARY",
        "EXE_MONTH_ABV_FEBRUARY",
        "EXE_MONTH_ABV_MARCH",
        "EXE_MONTH_ABV_APRIL",
        "EXE_MONTH_ABV_MAY",
        "EXE_MONTH_ABV_JUN",
        "EXE_MONTH_ABV_JULY",
        "EXE_MONTH_ABV_AUGUST",
        "EXE_MONTH_ABV_SEPTEMBER",
        "EXE_MONTH_ABV_OCTOBER",
        "EXE_MONTH_ABV_NOVEMBER",
        "EXE_MONTH_ABV_DECEMBER",
    ];
    let name = translate(ui, MONTHS[(month - 1) as usize]);
    format!("{name} {day}, {year}   {hour}:{minute:02}")
}

/// Year, month (1-12), day, hour and minute of `secs` since the Unix epoch.
fn civil(secs: i64) -> (i64, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Days to a calendar date (Howard Hinnant's `civil_from_days`).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        year,
        month,
        day,
        (rem / 3600) as u32,
        (rem % 3600 / 60) as u32,
    )
}

/// [`Shell::join_refused`] on the parts that hold the menus and the dvars.
fn show_refusal(ui: &mut Ui, h: &mut HostCx, why: &str) {
    use crate::ui::MenuKind::Other;
    h.input.set_cvar("com_errorMessage", why);
    ui.open_as(h, "error_popmenu", Other);
    if why
        .trim_end_matches('.')
        .eq_ignore_ascii_case("Invalid password")
    {
        h.input.set_cvar("password", "");
        ui.open_as(h, "password_popmenu", Other);
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
        0 => mark(e.password),
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
        6 => mark(e.voice),
        7 => mark(e.pure),
        // The original marks the servers that are not modified.
        8 => mark(!e.modded),
        9 => mark(e.punkbuster),
        10 if e.ping > 0 => e.ping.to_string(),
        10 => "...".into(),
        _ => String::new(),
    }
}

/// A yes/no column of the server list.
fn mark(on: bool) -> String {
    if on { "X" } else { "" }.into()
}

/// Draws text for an owner-draw item with no text of its own: the baseline is the item's text offset below the top of its
/// rect, as the original places it (`UI_OwnerDraw` adds the offsets to the rect).
fn ui_text(ui: &Ui, p: &mut Painter, d: &ItemDef, rect: Px, color: [f32; 4], text: &str) {
    use crate::ui::paint::TextDraw;
    ui.draw_text(
        p,
        &TextDraw {
            text,
            font_enum: d.font_enum,
            scale: d.text_scale,
            style: d.text_style,
            color,
            x: rect.x + d.text_align_x * ui.place.scale.0,
            y: rect.y + d.text_align_y * ui.place.scale.1,
            horz: 5,
            vert: 5,
        },
    );
}

/// Select the stock Single Player executable or explain why it cannot be started here.
/// The shipped CoD4 client names `iw3sp.exe`; KisakCOD's `cod3sp.exe` is a CoD3 leftover.
pub(crate) fn singleplayer_executable(install: &Path, windows: bool) -> Result<PathBuf, String> {
    if !windows {
        return Err("Single Player can only be launched on Windows.".into());
    }

    let executable = install.join("iw3sp.exe");
    if executable.is_file() {
        Ok(executable)
    } else {
        Err(
            "Single Player is unavailable because iw3sp.exe was not found in the game install."
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serverlist::{Entry, Source};

    /// The registered HUD dvars say what the layout reads when they are unset, and a changed one reaches the layout.
    #[test]
    fn the_registered_hud_dvars_match_what_the_layout_defaults_to() {
        let mut input = Input::detached();
        register_defaults(&mut input);
        assert_eq!(hud::Layout::read(&input.cvars), hud::Layout::default());
        assert_eq!(
            hud::ScoreMetrics::read(&input.cvars),
            hud::ScoreMetrics::default()
        );
        assert_eq!(input.cvars.get("cg_hudVotePosition"), Some("5 220"));
        input.cvars.set("cg_hudVotePosition", "40 100", true);
        assert_eq!(hud::Layout::read(&input.cvars).vote, [40.0, 100.0]);
        let archived: Vec<_> = input.cvars.archived().into_iter().map(|(k, _)| k).collect();
        assert!(archived.contains(&"cg_hudsayposition"));
        assert!(!archived.contains(&"cg_scoreboardwidth"));
    }

    #[test]
    fn singleplayer_selects_install_executable_and_reports_unavailable_cases() {
        let install =
            std::env::temp_dir().join(format!("cod4e-singleplayer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&install);
        std::fs::create_dir_all(&install).unwrap();

        let executable = install.join("iw3sp.exe");
        assert!(matches!(
            singleplayer_executable(&install, true),
            Err(message) if message.contains("iw3sp.exe")
        ));
        std::fs::write(&executable, []).unwrap();
        assert_eq!(singleplayer_executable(&install, true).unwrap(), executable);
        assert!(matches!(
            singleplayer_executable(&install, false),
            Err(message) if message.contains("Windows")
        ));

        let _ = std::fs::remove_dir_all(&install);
    }

    #[test]
    fn server_list_cells_follow_the_stock_columns() {
        let maps = vec![MapEntry {
            name: "mp_crash".into(),
            title: "MPUI_CRASH".into(),
            image: String::new(),
            modes: Vec::new(),
        }];
        let types = vec![GameTypeEntry {
            id: "war".into(),
            title: "MPUI_WAR".into(),
            objective: String::new(),
            objective_score: String::new(),
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
        // Nor does any `exec` of theirs end as a word nobody runs: the app would forward it to the server as
        // unknown (`CL_ForwardCommandToServer`), and the input layer's pending list holds only its own commands.
        // Two words are the console's (Xbox) commands, which the stock PC build does not register either, so it
        // forwards them or says `Unknown command` just the same: `updategamerprofile` (`CL_UpdateGamerProfile`,
        // `cl_main_xenon`) and `uploadstats` (`LiveStorage_UploadStatsCmd`).
        const STOCK_UNKNOWN: [&str; 2] = ["updategamerprofile", "uploadstats"];
        let mut forwarded: Vec<&str> =
            h.st.actions
                .iter()
                .filter_map(|a| match a {
                    Action::Console(l) => Some(l.split_whitespace().next().unwrap_or("")),
                    _ => None,
                })
                .filter(|w| !crate::console::is_app_verb(w) && !crate::console::is_command(w))
                .collect();
        forwarded.sort_unstable();
        forwarded.dedup();
        let unexpected: Vec<&str> = forwarded
            .iter()
            .copied()
            .filter(|w| !STOCK_UNKNOWN.contains(w))
            .collect();
        assert!(
            unexpected.is_empty(),
            "exec of unknown words: {unexpected:?}"
        );
        let pending: Vec<String> = h
            .input
            .frame(0.016)
            .pending_commands
            .into_iter()
            .filter(|l| !crate::console::is_command(l.split_whitespace().next().unwrap_or("")))
            .collect();
        assert!(
            pending.is_empty(),
            "dropped in the input layer: {pending:?}"
        );
    }

    /// A menu script's `wait` holds the rest of its buffer, which then runs through the shell's own commands (a
    /// `setfromdvar` after it is the shell's, not the input layer's).
    #[test]
    fn a_menu_wait_keeps_the_shell_commands_after_it() {
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
        let ui = Ui::new(assets, (1280, 720));
        let mut input = Input::detached();
        input.cvars.set("wait_src", "7", false);
        let mut h = HostCx {
            st: &mut st,
            input: &mut input,
        };
        h.command(&ui, "set wait_a 1; wait 2; setfromdvar wait_dst wait_src");
        assert_eq!(h.input.cvar("wait_a"), Some("1"));
        assert!(h.input.take_due().is_none(), "held for two frames");
        let due = h.input.take_due().expect("due on the second frame");
        h.run_buffer(&ui, due);
        assert_eq!(h.input.cvar("wait_dst"), Some("7"));
    }

    /// The team menu lists both teams to a player on none (and on the spectators), and the other team to a player on
    /// one: `team(name)` is `TEAM_FREE` / `TEAM_SPECTATOR` there, not an axis stand-in.
    #[test]
    fn the_team_menu_lists_the_teams_a_player_can_join() {
        let root =
            std::env::var_os("COD4_PATH").map_or_else(|| "COD4".into(), std::path::PathBuf::from);
        let Some(install) = Install::open(&root)
            .ok()
            .filter(|i| i.zone_path("ui_mp").is_some())
        else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        // What the server hears when the player clicks `want` (the banner texts at the left answer no click).
        let join = |team: &str, want: &str| -> Option<String> {
            let assets = UiAssets::load(&install).expect("ui assets");
            let mut st = ShellState::new(&assets, &install);
            st.game.team = team.into();
            let mut ui = Ui::new(assets, (1280, 720));
            let mut input = Input::detached();
            register_defaults(&mut input);
            let mut h = HostCx {
                st: &mut st,
                input: &mut input,
            };
            ui.open_by_name(&mut h, "team_marinesopfor");
            ui.click(&mut h, want);
            st.actions.iter().find_map(|a| match a {
                Action::MenuResponse { response, .. } => Some(response.clone()),
                _ => None,
            })
        };
        let some = |s: &str| Some(s.to_owned());
        for team in ["free", "spectator"] {
            assert_eq!(join(team, "OpFor"), some("axis"), "{team}: OpFor");
            assert_eq!(join(team, "Marines"), some("allies"), "{team}: Marines");
        }
        // On a team, the other team stays listed.
        assert_eq!(join("allies", "OpFor"), some("axis"));
        assert_eq!(join("axis", "Marines"), some("allies"));
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
        let vfs = ::assets::vfs::Vfs::open_stock(&root, 0).expect("install files");
        let mut input = Input::detached_with(vfs);
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

    /// The kick/vote list (feeder 7) and the mute list (feeder 20) show the other players, and the picks reach the
    /// vote command and the mute toggle.
    #[test]
    fn the_player_lists_feed_the_kick_vote_and_mute_menus() {
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
        st.live.own = 1;
        st.live.names = ["Ann", "Me", "", "^2Bo^7x"].map(String::from).to_vec();
        let mut h = HostCx {
            st: &mut st,
            input: &mut input,
        };
        for feeder in [7, 20] {
            assert_eq!(h.feeder_count(feeder), 2, "not me, not the empty slot");
        }
        assert_eq!(
            h.feeder_text(7, 1, 0),
            "Box",
            "names are listed without colour codes"
        );
        assert_eq!(h.feeder_text(20, 0, 1), "Ann");
        h.feeder_select(7, 1);
        assert!(h.ui_script(&mut ui, "voteKick", &[]));
        h.feeder_select(20, 0);
        assert_eq!(h.feeder_text(20, 0, 0), "");
        assert!(h.ui_script(&mut ui, "mutePlayer", &[]));
        assert_eq!(h.feeder_text(20, 0, 0), "@MP_MUTED");
        assert!(h.ui_script(&mut ui, "mutePlayer", &[]));
        assert_eq!(h.feeder_text(20, 0, 0), "");
        let lines: Vec<_> = st
            .actions
            .iter()
            .filter_map(|a| match a {
                Action::Console(l) => Some(l.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(lines, ["callvote clientkick 3"]);
    }

    /// The profile menu lists the install's profiles, picks one, makes one and deletes it; the active profile's stats
    /// are the ones the menus read (so a profile with everything unlocked unlocks Create a Class).
    #[test]
    fn the_profile_menu_lists_picks_creates_and_deletes_profiles() {
        let root =
            std::env::var_os("COD4_PATH").map_or_else(|| "COD4".into(), std::path::PathBuf::from);
        let Some(install) = Install::open(&root)
            .ok()
            .filter(|i| i.zone_path("ui_mp").is_some())
        else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let tmp = std::env::temp_dir().join(format!("cod4e-shell-profiles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = tmp.join("install/players/profiles");
        for (name, xp) in [("Lagahoo", 125_490), ("Ann", 0)] {
            std::fs::create_dir_all(store.join(name)).unwrap();
            let mut s = crate::profile::default_stats();
            s[2301] = xp;
            std::fs::write(store.join(name).join("mpdata"), crate::profile::encode(&s)).unwrap();
        }
        std::fs::write(store.join("active.txt"), "Lagahoo").unwrap();
        let assets = UiAssets::load(&install).expect("ui assets");
        let mut st = ShellState::new(&assets, &install);
        let mut ui = Ui::new(assets, (1280, 720));
        let mut input = Input::detached();
        register_defaults(&mut input);
        let (profiles, stats) =
            Profiles::open(&tmp.join("install"), Some(tmp.join("cfg")), "default");
        st.stats = stats;
        st.profiles = profiles;
        let mut h = HostCx {
            st: &mut st,
            input: &mut input,
        };
        h.input.cvars.set("com_playerProfile", "Lagahoo", false);
        assert_eq!(
            h.st.stats[2301], 125_490,
            "the install's profile is the active one"
        );
        ui.open_by_name(&mut h, "player_profile");
        assert_eq!(h.feeder_count(24), 2);
        assert_eq!(
            (h.feeder_text(24, 0, 0), h.feeder_text(24, 1, 0)),
            ("Ann".into(), "Lagahoo".into())
        );
        assert_eq!(h.dvar("ui_playerProfileCount"), "2");
        assert_eq!(
            h.dvar("ui_playerProfileSelected"),
            "Lagahoo",
            "the active profile is picked"
        );
        // Pick Ann and load her: her stats replace the active ones.
        h.feeder_select(24, 0);
        assert!(h.ui_script(&mut ui, "loadPlayerProfile", &[]));
        assert_eq!(
            (h.dvar("com_playerProfile"), h.st.stats[2301]),
            ("Ann".into(), 0)
        );
        // A new profile is listed, picked and starts with the default classes.
        h.set_dvar("ui_playerProfileNameNew", "Zed");
        assert!(h.ui_script(&mut ui, "createPlayerProfile", &[]));
        assert_eq!(h.feeder_count(24), 3);
        assert_eq!(h.dvar("ui_playerProfileSelected"), "Zed");
        assert_eq!(h.dvar("ui_playerProfileNameNew"), "");
        assert!(h.ui_script(&mut ui, "loadPlayerProfile", &[]));
        assert_eq!(h.st.stats, crate::profile::default_stats());
        // Deleting the active one leaves none chosen; the install's profiles cannot be deleted.
        assert!(h.ui_script(&mut ui, "deletePlayerProfile", &[]));
        assert_eq!(
            (h.feeder_count(24), h.dvar("com_playerProfile")),
            (2, String::new())
        );
        h.feeder_select(24, 1);
        assert!(h.ui_script(&mut ui, "deletePlayerProfile", &[]));
        assert_eq!(h.feeder_count(24), 2);
        assert!(ui.open_menus().contains(&"profile_delete_fail_popmenu"));
        // The column header flips the order.
        assert!(h.ui_script(&mut ui, "sortPlayerProfiles", &["0".into()]));
        assert_eq!(h.feeder_text(24, 0, 0), "Lagahoo");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The game mode chooser steps through the modes on a click and back on the right button, and the settings menus
    /// follow `ui_netGametypeName`.
    #[test]
    fn the_game_mode_chooser_steps_on_click() {
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
        let ui = Ui::new(assets, (1280, 720));
        let mut input = Input::detached();
        register_defaults(&mut input);
        let mut h = HostCx {
            st: &mut st,
            input: &mut input,
        };
        assert_eq!(h.dvar("ui_netGametypeName"), "war");
        // Left click goes on through Free-for-all, Domination, Search and Destroy, Sabotage, Team Deathmatch and
        // Headquarters, round again; right click goes back.
        let mut seen = Vec::new();
        for _ in 0..6 {
            assert!(h.owner_key(&ui, 245, &UiKey::Mouse1));
            seen.push(h.dvar("ui_netGametypeName"));
        }
        assert_eq!(seen, ["koth", "dm", "dom", "sd", "sab", "war"]);
        assert_eq!(h.dvar("g_gametype"), "war");
        assert!(h.owner_key(&ui, 245, &UiKey::Mouse2));
        assert_eq!(h.dvar("ui_netGametypeName"), "sab");
        assert!(!h.owner_key(&ui, 245, &UiKey::Up));
        let before = h.gametype_sel();
        assert!(
            !h.owner_key(&ui, 245, &UiKey::Left),
            "the arrows are not the chooser's"
        );
        assert!(!h.owner_key(&ui, 245, &UiKey::Right));
        assert_eq!(h.gametype_sel(), before);
        assert!(h.owner_key(&ui, 245, &UiKey::Enter));
        assert_eq!(h.dvar("ui_netGametypeName"), "war");
    }

    /// The in-game menus name the mode and describe its objective, with the score limit when there is one.
    #[test]
    fn the_in_game_menus_name_the_mode_and_say_its_objective() {
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
        let mut input = Input::detached();
        register_defaults(&mut input);
        input.cvars.set("g_gametype", "war", false);
        input.cvars.set("ui_scorelimit", "750", false);
        let h = HostCx {
            st: &mut st,
            input: &mut input,
        };
        assert_eq!(h.gametype_name(), "Team Deathmatch");
        assert_eq!(
            h.gametype_description(),
            "Gain points by eliminating enemy players.  First team to 750 wins."
        );
        h.input.cvars.set("ui_scorelimit", "0", false);
        assert_eq!(
            h.gametype_description(),
            "Gain points by eliminating enemy players."
        );
    }

    /// The wheel, arrows and page keys scroll a scoreboard that has more rows than fit, and stay in range.
    #[test]
    fn the_scoreboard_scrolls_with_the_wheel_and_page_keys() {
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
        let ui = Ui::new(assets, (1280, 720));
        assert!(
            !st.scroll_scores(&ui, &UiKey::WheelDown),
            "no scoreboard up"
        );
        st.scores_forced = true;
        st.live.own_team = 2;
        st.live.scores = (0..30)
            .map(|i| crate::hud::ScoreLine {
                client: i,
                team: 2,
                ..Default::default()
            })
            .collect();
        assert!(st.scroll_scores(&ui, &UiKey::WheelUp));
        assert_eq!(st.scores_top, 1, "not above the top");
        st.scroll_scores(&ui, &UiKey::WheelDown);
        assert_eq!(st.scores_top, 2);
        st.scroll_scores(&ui, &UiKey::PageDown);
        assert!(st.scores_top > 4);
        for _ in 0..100 {
            st.scroll_scores(&ui, &UiKey::Down);
        }
        assert_eq!(st.scores_top, 32, "the last of 30 rows and the banners");
        st.scroll_scores(&ui, &UiKey::PageUp);
        assert!(st.scores_top < 32);
        assert!(
            !st.scroll_scores(&ui, &UiKey::Tab),
            "other keys are not its own"
        );
    }

    /// A shell with the install's menus and no match, or `None` when there is no install.
    fn shell_env() -> Option<(ShellState, Ui, Input)> {
        let root =
            std::env::var_os("COD4_PATH").map_or_else(|| "COD4".into(), std::path::PathBuf::from);
        let Some(install) = Install::open(&root)
            .ok()
            .filter(|i| i.zone_path("ui_mp").is_some())
        else {
            eprintln!("COD4_PATH not set; skipping");
            return None;
        };
        let assets = UiAssets::load(&install).expect("ui assets");
        let st = ShellState::new(&assets, &install);
        let mut input = Input::detached();
        register_defaults(&mut input);
        Some((st, Ui::new(assets, (1280, 720)), input))
    }

    /// The unlock popup the server opens at the end of a match stays up through the intermission, with the
    /// scoreboard rows still wanted behind it.
    #[test]
    fn the_intermission_leaves_a_server_popup_up_and_keeps_the_rows() {
        let Some((mut st, mut ui, mut input)) = shell_env() else {
            return;
        };
        st.live.active = true;
        st.live.intermission = true;
        Shell::intermission_menus(&mut st, &mut ui, &mut input);
        assert!(ui.is_open("scoreboard"));
        let mut h = Shell::host(&mut st, &mut input);
        ui.open_script_menu(&mut h, "popup_unlock", true);
        for _ in 0..3 {
            Shell::intermission_menus(&mut st, &mut ui, &mut input);
        }
        // With nothing unlocked the unlock popup hands over to the summary popup; either way a popup is up.
        assert!(ui.script_popup_up(), "the popup is not closed again");
        assert!(!ui.is_open("scoreboard"));
        assert!(st.scoreboard_shown(&ui), "the rows stay drawn behind it");
    }

    fn lan_server(addr: &str, mode: &str) -> Entry {
        Entry {
            addr: addr.parse().unwrap(),
            hostname: addr.into(),
            map: "mp_crash".into(),
            gametype: mode.into(),
            clients: 1,
            max_clients: 18,
            ping: 5,
            // The stock `ui_browserShowPure 1` lists pure servers only.
            pure: true,
            ..Entry::unanswered(addr.parse().unwrap())
        }
    }

    /// The join menu's source picker steps local, internet, favorites on a click and back on the right button, the
    /// game mode picker cuts the list to one mode (and its first stop is every mode), and each list shows its own rows.
    #[test]
    fn the_join_menu_cycles_its_source_and_cuts_the_list_to_a_game_mode() {
        let Some((mut st, ui, mut input)) = shell_env() else {
            return;
        };
        let modes = st.gametypes.len();
        let mut h = HostCx {
            st: &mut st,
            input: &mut input,
        };
        h.st.servers.take(lan_server("192.168.1.5:28960", "dm"));
        h.st.servers.take(lan_server("192.168.1.6:28960", "war"));
        h.st.servers.add_favorite("10.9.8.7:28960".parse().unwrap());
        assert_eq!(h.feeder_count(2), 2, "the local network is the first list");
        // The mode picker: every mode, then each of the table's modes in turn; the right button goes back round.
        assert!(h.owner_key(&ui, 253, &UiKey::Mouse1));
        assert_eq!(h.dvar("ui_joinGameType"), "1");
        let first = h.st.gametypes[0].id.clone();
        assert_eq!(first, "dm");
        assert_eq!(h.feeder_count(2), 1);
        assert_eq!(h.feeder_text(2, 0, 2), "192.168.1.5:28960");
        assert!(h.owner_key(&ui, 253, &UiKey::Mouse2));
        assert!(h.owner_key(&ui, 253, &UiKey::Mouse2));
        assert_eq!(h.dvar("ui_joinGameType"), modes.to_string());
        assert!(h.owner_key(&ui, 253, &UiKey::Enter));
        assert_eq!(h.dvar("ui_joinGameType"), "0");
        assert!(
            !h.owner_key(&ui, 253, &UiKey::Left),
            "arrows are not its keys"
        );
        // The source picker: local, internet (nothing to list), favorites (the one added, unanswered) and round.
        assert!(h.owner_key(&ui, 220, &UiKey::Mouse1));
        assert_eq!((h.dvar("ui_netSource"), h.feeder_count(2)), ("1".into(), 0));
        assert!(h.owner_key(&ui, 220, &UiKey::Mouse1));
        assert_eq!((h.dvar("ui_netSource"), h.feeder_count(2)), ("2".into(), 1));
        assert_eq!(h.feeder_text(2, 0, 2), "10.9.8.7:28960");
        // A mode hides a server that has not said which it runs.
        h.set_dvar("ui_joinGameType", "1");
        assert_eq!(h.feeder_count(2), 0);
        h.set_dvar("ui_joinGameType", "0");
        assert!(h.owner_key(&ui, 220, &UiKey::Mouse1));
        assert_eq!(h.dvar("ui_netSource"), "0");
        assert!(h.owner_key(&ui, 220, &UiKey::Mouse2));
        assert_eq!(h.dvar("ui_netSource"), "2");
        assert!(!h.owner_key(&ui, 220, &UiKey::Up));
    }

    /// The filter popup's dvars cut the join list, and its sort buttons order every column.
    #[test]
    fn the_filter_popup_dvars_cut_the_join_list() {
        let Some((mut st, mut ui, mut input)) = shell_env() else {
            return;
        };
        let mut h = HostCx {
            st: &mut st,
            input: &mut input,
        };
        let mut locked = lan_server("192.168.1.5:28960", "war");
        locked.password = true;
        h.st.servers.take(locked);
        h.st.servers.take(lan_server("192.168.1.6:28960", "war"));
        for (k, v) in [
            ("ui_browserShowPassword", "-1"),
            ("ui_browserShowFull", "1"),
        ] {
            h.set_dvar(k, v);
        }
        assert_eq!(h.feeder_count(2), 2);
        h.set_dvar("ui_browserShowPassword", "1");
        assert_eq!(h.feeder_count(2), 1);
        assert_eq!(h.feeder_text(2, 0, 2), "192.168.1.5:28960");
        h.set_dvar("ui_browserShowPassword", "0");
        assert_eq!(h.feeder_text(2, 0, 2), "192.168.1.6:28960");
        h.set_dvar("ui_browserShowPassword", "-1");
        // Sorting by the password column puts the open server first.
        assert!(h.ui_script(&mut ui, "ServerSort", &["0".into()]));
        assert_eq!(h.feeder_text(2, 0, 2), "192.168.1.6:28960");
        assert!(h.ui_script(&mut ui, "ServerSort", &["0".into()]));
        assert_eq!(h.feeder_text(2, 0, 2), "192.168.1.5:28960");
        // `ui_browserShowPure 1` (the stock default) lists pure servers only.
        h.st.servers.take(Entry {
            pure: false,
            ..lan_server("192.168.1.7:28960", "war")
        });
        h.set_dvar("ui_browserShowPure", "1");
        assert_eq!(h.feeder_count(2), 2);
        h.set_dvar("ui_browserShowPure", "0");
        assert_eq!(h.feeder_count(2), 3);
    }

    /// The browser's dvars start at their stock values, and `password` (the userinfo dvar a join sends) is not saved.
    #[test]
    fn browser_dvars_start_at_stock_values_and_the_password_is_not_archived() {
        let mut input = Input::detached();
        register_defaults(&mut input);
        for (k, v) in [
            ("ui_browserShowFull", "1"),
            ("ui_browserShowEmpty", "1"),
            ("ui_browserShowPassword", "-1"),
            ("ui_browserShowPure", "1"),
            ("ui_browserMod", "0"),
            ("ui_browserShowDedicated", "0"),
            ("ui_browserFriendlyfire", "-1"),
            ("ui_browserKillcam", "-1"),
            ("ui_browserShowPunkBuster", "-1"),
            ("password", ""),
        ] {
            assert_eq!(input.cvar(k), Some(v), "{k}");
        }
        input.set_cvar("password", "secret");
        assert!(
            input
                .cvars
                .archived()
                .iter()
                .all(|(k, _)| !k.eq_ignore_ascii_case("password"))
        );
        assert!(
            input
                .cvars
                .archived()
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("ui_browserKillcam"))
        );
    }

    /// A refused join shows the reason in the error popup; a wrong password is forgotten and asked for again.
    #[test]
    fn a_refused_join_shows_the_error_and_a_wrong_password_asks_again() {
        let Some((mut st, mut ui, mut input)) = shell_env() else {
            return;
        };
        input.set_cvar("password", "wrong");
        let mut h = HostCx {
            st: &mut st,
            input: &mut input,
        };
        show_refusal(&mut ui, &mut h, "Server is full.");
        assert!(ui.is_open("error_popmenu"));
        assert!(!ui.is_open("password_popmenu"));
        assert_eq!(
            h.dvar("password"),
            "wrong",
            "another refusal keeps the password"
        );
        show_refusal(&mut ui, &mut h, "Invalid password.");
        assert_eq!(h.dvar("com_errorMessage"), "Invalid password.");
        assert!(ui.is_open("password_popmenu"));
        assert_eq!(h.dvar("password"), "");
    }

    /// A refresh runs until it is stopped, and leaving the join menu while one runs stops it first (the list stays).
    #[test]
    fn closing_the_join_menu_during_a_refresh_stops_it_and_a_quick_refresh_keeps_the_list() {
        let Some((mut st, mut ui, mut input)) = shell_env() else {
            return;
        };
        let mut h = HostCx {
            st: &mut st,
            input: &mut input,
        };
        // The refresh window is wall-clock: under load the test could outlast the real 1.5 s.
        h.st.servers.window_override = Some(std::time::Duration::from_secs(3600));
        h.st.servers.take(lan_server("192.168.1.5:28960", "dm"));
        assert!(h.ui_script(&mut ui, "RefreshFilter", &[]));
        assert!(h.st.servers.refreshing(Source::Lan));
        assert_eq!(h.feeder_count(2), 1, "a quick refresh keeps what it found");
        assert!(h.ui_script(&mut ui, "closeJoin", &[]));
        assert!(!h.st.servers.refreshing(Source::Lan));
        assert!(h.ui_script(&mut ui, "RefreshServers", &[]));
        assert!(h.st.servers.refreshing(Source::Lan));
        assert_eq!(h.feeder_count(2), 0, "a full refresh starts the list over");
        assert!(h.ui_script(&mut ui, "StopRefresh", &[]));
        assert!(!h.st.servers.refreshing(Source::Lan));
        // The time of the refresh is kept for the "Refresh time" line of its source.
        assert!(!h.dvar("ui_lastServerRefresh_0").is_empty());
    }

    /// The map list shows the maps the game mode can be played on, with the level shot of each row, and a pick sets
    /// the map index the menus and the preview read.
    #[test]
    fn the_map_list_is_cut_to_the_game_mode() {
        let Some((mut st, ui, mut input)) = shell_env() else {
            return;
        };
        let all = st.maps.len();
        assert!(all >= 2, "the install has maps");
        // The first map is for Deathmatch only.
        st.maps[0].modes = vec!["dm".into()];
        st.map_sel = 0;
        let mut h = HostCx {
            st: &mut st,
            input: &mut input,
        };
        h.set_dvar("ui_netGametypeName", "dm");
        assert_eq!(h.feeder_count(4), all);
        h.set_dvar("ui_netGametypeName", "war");
        assert_eq!(h.feeder_count(4), all - 1);
        let second = h.st.maps[1].title.clone();
        assert_eq!(h.feeder_text(4, 0, 0), second);
        assert_eq!(h.feeder_image(4, 0, 0), h.st.maps[1].image);
        assert_eq!(
            h.feeder_image(4, 0, 1),
            "",
            "only the first column is a picture"
        );
        h.feeder_select(4, 0);
        assert_eq!(h.st.map_sel, 1, "the pick is a place in the whole table");
        assert_eq!(
            (h.dvar("ui_currentMap"), h.dvar("ui_currentNetMap")),
            ("1".into(), "1".into())
        );
        // Going to a mode the chosen map is not for moves the choice to the first map that is.
        h.st.map_sel = 0;
        h.set_dvar("ui_netGametypeName", "dm");
        assert!(h.owner_key(&ui, 245, &UiKey::Mouse1));
        assert_eq!(h.dvar("ui_netGametypeName"), "dom");
        assert_eq!(h.st.map_sel, 1);
        assert_eq!(h.st.reselect_map, Some(0));
        assert_eq!(h.dvar("ui_currentNetMap"), "1");
    }

    /// Server Info asks the selected server for its status and lists the answer.
    #[test]
    fn server_info_lists_what_the_selected_server_answers() {
        let Some((mut st, mut ui, mut input)) = shell_env() else {
            return;
        };
        let srv = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = srv.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            let (n, from) = srv.recv_from(&mut buf).unwrap();
            let Some(net::Oob::GetStatus(c)) = net::Oob::parse(&buf[..n]) else {
                panic!("not a getstatus")
            };
            let reply = net::Oob::StatusResponse {
                info: vec![
                    ("sv_hostname".into(), "Fun".into()),
                    ("g_gametype".into(), "war".into()),
                    ("challenge".into(), c.to_string()),
                ],
                players: vec![net::oob::StatusPlayer {
                    score: 4,
                    ping: 20,
                    name: "Ann".into(),
                }],
            };
            srv.send_to(&reply.encode(), from).unwrap();
        });
        // This one talks to a server it starts itself, on a real socket.
        st.servers.offline = false;
        let mut h = HostCx {
            st: &mut st,
            input: &mut input,
        };
        assert_eq!(h.feeder_count(13), 0, "nothing before a server is asked");
        h.st.servers.select(Some(addr));
        assert!(h.ui_script(&mut ui, "ServerStatus", &[]));
        let end = Instant::now() + std::time::Duration::from_secs(10);
        while h.feeder_count(13) == 0 {
            assert!(Instant::now() < end, "the server did not answer");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        t.join().unwrap();
        let rows: Vec<[String; 4]> = (0..h.feeder_count(13))
            .map(|r| std::array::from_fn(|c| h.feeder_text(13, r, c)))
            .collect();
        assert_eq!(rows[0][0], "@EXE_SV_INFO_SERVERNAME");
        assert_eq!(rows[0][3], "Fun");
        let war =
            h.st.gametypes
                .iter()
                .find(|g| g.id == "war")
                .unwrap()
                .title
                .clone();
        assert!(
            rows.iter()
                .any(|r| r[0] == "@EXE_SV_INFO_GAMETYPE" && r[3] == war)
        );
        assert_eq!(
            rows.last().unwrap(),
            &["0", "4", "20", "Ann"].map(str::to_owned)
        );
        // Asking again empties the list until the answer comes.
        assert!(h.ui_script(&mut ui, "ServerStatus", &[]));
        assert_eq!(h.feeder_count(13), 0);
    }

    #[test]
    fn the_refresh_time_is_a_calendar_date_in_utc() {
        assert_eq!(civil(0), (1970, 1, 1, 0, 0));
        assert_eq!(civil(951_868_740), (2000, 2, 29, 23, 59));
        assert_eq!(civil(1_791_554_700), (2026, 10, 9, 14, 5));
    }

    #[test]
    fn stat_clear_commands_clear_the_new_flag_and_the_masked_bits() {
        let mut stats = vec![0; 300];
        stats[200] = 3;
        stats[201] = 1;
        stats[202] = 0;
        clear_new_flag(&mut stats, 200).unwrap();
        assert_eq!(stats[200], 1, "unlocked, no longer new");
        clear_new_flag(&mut stats, 201).unwrap();
        assert_eq!(stats[201], 1);
        assert!(clear_new_flag(&mut stats, 202).is_err(), "locked");
        assert!(clear_new_flag(&mut stats, 5).is_err(), "not an unlock stat");
        assert!(clear_new_flag(&mut stats, 299).is_err());
        stats[10] = 0b1111;
        clear_stat_bits(&mut stats, 10, 0b0110);
        assert_eq!(stats[10], 0b1001);
        clear_stat_bits(&mut stats, -1, 1);
        clear_stat_bits(&mut stats, 9999, 1);
    }
    /// A scratch install: the real `main` and `zone` linked in, a `mods/` and a `usermaps/` of its own.
    #[cfg(unix)]
    struct Scratch(std::path::PathBuf);

    #[cfg(unix)]
    impl Scratch {
        fn new(root: &std::path::Path) -> Self {
            let dir = std::env::temp_dir().join(format!("cod4e-shell-mods-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            for d in ["main", "zone", "main_shared", "localization.txt"] {
                if root.join(d).exists() {
                    let _ = std::os::unix::fs::symlink(root.join(d), dir.join(d));
                }
            }
            Scratch(dir)
        }

        fn file(&self, rel: &str, bytes: &[u8]) {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, bytes).unwrap();
        }
    }

    #[cfg(unix)]
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The mods menu lists `mods/`, `RunMod` sets `fs_game` and asks for the restart that loads the mod's menu assets,
    /// `ClearMods` goes back, and a custom map is offered in the map list.
    #[cfg(unix)]
    #[test]
    fn the_mods_menu_lists_runs_and_clears_mods() {
        let root =
            std::env::var_os("COD4_PATH").map_or_else(|| "COD4".into(), std::path::PathBuf::from);
        if Install::open(&root)
            .ok()
            .filter(|i| i.zone_path("ui_mp").is_some())
            .is_none()
        {
            eprintln!("COD4_PATH not set; skipping");
            return;
        }
        let scratch = Scratch::new(&root);
        scratch.file("mods/alpha/description.txt", b"Alpha mod");
        scratch.file(
            "mods/alpha/mod.ff",
            &::assets::zone::testing::rawfile_zone(&[(
                "mp/modmap.arena",
                "{\nmap \"mp_modmap\"\ngametype \"dm\"\n}\n",
            )]),
        );
        scratch.file("mods/beta/mod.ff", b"");
        scratch.file("usermaps/mp_user/mp_user.ff", b"");

        // Stock first: nothing of the mod is there.
        let stock = Install::open(&scratch.0).unwrap();
        assert!(
            UiAssets::load(&stock)
                .unwrap()
                .arenas
                .get("mp_modmap")
                .is_none()
        );

        let install = Install::open_game(&scratch.0, "mods/alpha").unwrap();
        let assets = UiAssets::load(&install).expect("ui assets");
        assert!(
            assets.arenas.contains_key("mp_modmap"),
            "the mod's fastfile is part of the UI assets"
        );
        let mut st = ShellState::new(&assets, &install);
        assert!(
            st.maps.iter().any(|m| m.name == "mp_user"),
            "a usermaps map is offered"
        );
        let mut ui = Ui::new(assets, (1280, 720));
        let mut input = Input::detached();
        let mut h = HostCx {
            st: &mut st,
            input: &mut input,
        };
        assert_eq!(h.feeder_count(9), 0, "nothing listed before loadmods");
        assert!(h.ui_script(&mut ui, "loadMods", &[]));
        assert_eq!(h.feeder_count(9), 2);
        assert_eq!(h.feeder_text(9, 0, 0), "Alpha mod");
        assert_eq!(
            h.feeder_text(9, 1, 0),
            "beta",
            "no description: the folder name"
        );
        h.feeder_select(9, 1);
        assert!(h.ui_script(&mut ui, "RunMod", &[]));
        assert_eq!(h.dvar("fs_game"), "mods/beta");
        assert!(h.st.actions.iter().any(|a| matches!(a, Action::VidRestart)));
        assert!(h.ui_script(&mut ui, "ClearMods", &[]));
        assert_eq!(h.dvar("fs_game"), "");
    }
}
