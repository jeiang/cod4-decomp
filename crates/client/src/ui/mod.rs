// SPDX-License-Identifier: GPL-3.0-only
// Menu key, mouse-focus and script-menu behaviour follows KisakCOD (GPL-3.0; `ui/ui_shared.cpp`, `ui_mp/ui_main_mp.cpp`,
// `cgame_mp/cg_servercmds_mp.cpp`; copyright holders of KisakCOD and the original Call of Duty 4 authors).
//! The stock menu system: menu and item state, the open-menu stack, focus, the action language and input.
//!
//! Menus are the decoded `menuDef` assets (immutable, shared); [`Ui`] keeps the per-menu and per-item runtime state
//! (dynamic flags, colours, focus, list cursors). Everything the menus need from the rest of the client (dvars, the
//! console, the server, sound, the game state) comes through [`Host`].

pub mod assets;
pub mod env;
pub mod expr;
pub mod loading;
pub mod paint;
pub mod place;
pub mod script;

use ::assets::zone::menu::{ItemDef, MenuDef, Rect};
use assets::UiAssets;
#[allow(unused_imports)]
use env::World;
use expr::Compiled;
use std::collections::HashMap;
use std::sync::Arc;

/// What opened the current menu (`uiInfo.currentMenuType`): decides whether a server menu may replace it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MenuKind {
    #[default]
    Other,
    /// The Escape menu of a match.
    Ingame,
    /// A menu the server opened (`openmenu`).
    ScriptPopup,
    Scoreboard,
}

/// Window dynamic flags.
pub mod dynf {
    /// The pointer is over the item (`WINDOW_MOUSEOVER`): `mouseEnter` has run and `mouseExit` is owed.
    pub const MOUSEOVER: u32 = 0x1;
    pub const HASFOCUS: u32 = 0x2;
    pub const VISIBLE: u32 = 0x4;
    pub const FADINGOUT: u32 = 0x10;
    pub const FADINGIN: u32 = 0x20;
    /// The pointer is over the item's text: `mouseEnterText` has run and `mouseExitText` is owed.
    pub const MOUSEOVER_TEXT: u32 = 0x40;
    pub const FORECOLOR_SET: u32 = 0x10000;
}

/// Window static flags.
pub mod statf {
    pub const DECORATION: u32 = 0x10_0000;
    pub const AUTOWRAPPED: u32 = 0x80_0000;
    pub const POPUP: u32 = 0x100_0000;
    pub const OUT_OF_BOUNDS_CLICK: u32 = 0x200_0000;
    /// `hiddenDuringFlashbang`.
    pub const HIDDEN_DURING_FLASHBANG: u32 = 0x1000_0000;
    /// `hiddenDuringScope`.
    pub const HIDDEN_DURING_SCOPE: u32 = 0x2000_0000;
    /// `hiddenDuringUI`: hidden while any menu is open.
    pub const HIDDEN_DURING_UI: u32 = 0x4000_0000;
}

/// Item types (`itemDef.type`).
pub mod ity {
    pub const TEXT: i32 = 0;
    pub const BUTTON: i32 = 1;
    pub const EDITFIELD: i32 = 4;
    pub const LISTBOX: i32 = 6;
    pub const OWNERDRAW: i32 = 8;
    pub const NUMERICFIELD: i32 = 9;
    pub const SLIDER: i32 = 10;
    pub const YESNO: i32 = 11;
    pub const MULTI: i32 = 12;
    pub const DVARENUM: i32 = 13;
    pub const BIND: i32 = 14;
    pub const VALIDFILEFIELD: i32 = 16;
    pub const DECIMALFIELD: i32 = 17;
    pub const UPREDITFIELD: i32 = 18;
    pub const GAME_MSG_WINDOW: i32 = 19;
}

/// The slider bar picture, in virtual units; the thumb travels 84 of it, 6 in from the left.
pub(crate) const SLIDER_W: f32 = 96.0;
pub(crate) const SLIDER_H: f32 = 16.0;
const SLIDER_TRAVEL: f32 = 84.0;
const SLIDER_INSET: f32 = 6.0;

/// Left edge of a slider's bar: the bar is placed in the item's rect by its text alignment (`Item_Slider_Paint`).
pub(crate) fn slider_bar_x(r: &Rect, d: &ItemDef) -> f32 {
    let x0 = r.x + d.text_align_x;
    match d.text_align_mode & 3 {
        1 => (r.w - SLIDER_W) * 0.5 + x0,
        2 => r.w - SLIDER_W + x0,
        _ => x0,
    }
}

/// Where the thumb's centre sits for value `v`, given the bar's left edge.
pub(crate) fn slider_thumb_x(bar_x: f32, e: &::assets::zone::menu::EditFieldDef, v: f32) -> f32 {
    let t = if e.max_val > e.min_val {
        ((v.clamp(e.min_val, e.max_val) - e.min_val) / (e.max_val - e.min_val)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    bar_x + SLIDER_INSET + t * SLIDER_TRAVEL
}

/// The value a click at fraction `t` (0..1) of the usable travel selects.
pub(crate) fn slider_value_at(e: &::assets::zone::menu::EditFieldDef, t: f32) -> f32 {
    e.min_val + t.clamp(0.0, 1.0) * (e.max_val - e.min_val)
}

/// The entry of a multi item whose value the dvar currently holds.
pub(crate) fn multi_index(mu: &::assets::zone::menu::MultiDef, cur: &str) -> Option<usize> {
    let num = cur.trim().parse::<f32>().unwrap_or(f32::NAN);
    (0..mu.count.clamp(0, 32) as usize).find(|&k| {
        if mu.str_def != 0 {
            mu.dvar_str
                .get(k)
                .and_then(|s| s.as_deref())
                .is_some_and(|s| s.eq_ignore_ascii_case(cur))
        } else {
            mu.dvar_value.get(k).is_some_and(|v| (num - v).abs() < 1e-4)
        }
    })
}

/// The entry of a dvar-enum item: the dvar holds an index, or one of the strings; anything else is entry 0.
pub(crate) fn enum_index(list: &[String], cur: &str) -> usize {
    let cur = cur.trim();
    if let Ok(i) = cur.parse::<usize>()
        && i < list.len()
    {
        return i;
    }
    list.iter()
        .position(|s| s.eq_ignore_ascii_case(cur))
        .unwrap_or(0)
}

/// Keys the menus react to.
#[derive(Clone, Debug, PartialEq)]
pub enum UiKey {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Mouse1,
    Mouse2,
    WheelUp,
    WheelDown,
    Char(char),
}

/// The original's key number of a key press, for `execKey` handlers (`Menu_HandleKey` offers keys 1..=255 to them):
/// letters in lower case, Tab 9, Enter 13, Escape 27, Backspace 127.
fn key_code(key: &UiKey) -> Option<i32> {
    match key {
        UiKey::Char(c) => {
            let c = c.to_ascii_lowercase() as u32;
            (1..=255).contains(&c).then_some(c as i32)
        }
        UiKey::Tab => Some(9),
        UiKey::Enter => Some(13),
        UiKey::Escape => Some(27),
        UiKey::Backspace => Some(127),
        _ => None,
    }
}

/// `UI_OwnerDrawVisible`: the join-menu pieces that show for LAN (flag 4) or for every other source (0x1000).
fn owner_draw_visible(flags: u32, host: &dyn Host) -> bool {
    let lan = host.dvar("ui_netSource").trim().parse::<i32>().ok() == Some(2);
    (flags & 4 == 0 || lan) && !(flags & 0x1000 != 0 && lan)
}

/// What the menus need of the client around them.
pub trait Host: env::World {
    /// A dvar as text; empty when unset.
    fn dvar(&self, name: &str) -> String;
    fn set_dvar(&mut self, name: &str, value: &str);
    /// Runs console text (`;`-separated commands) now.
    fn exec(&mut self, ui: &Ui, text: &str);
    /// Plays a UI sound alias.
    fn play(&mut self, alias: &str);
    /// `scriptMenuResponse`: tells the server's scripts what was chosen in a script menu.
    fn menu_response(&mut self, menu: &str, response: &str);
    /// Runs one `uiScript` (server list, start server, profiles, ...). `false` if unknown.
    fn ui_script(&mut self, ui: &mut Ui, name: &str, args: &[String]) -> bool;
    /// Whether a match is running (menus opened with `ingameopen`).
    /// The strings of an enumerated dvar (`r_mode`, `r_displayRefresh`); empty if it is not one.
    fn dvar_enum(&self, _name: &str) -> Vec<String> {
        Vec::new()
    }
    /// Binds `key` to `command` (an empty command unbinds the key).
    fn set_bind(&mut self, _key: &str, _command: &str) {}
    fn in_game(&self) -> bool;
    fn time_ms(&self) -> i32;
    /// The number of rows of a feeder list and its text.
    fn feeder_count(&mut self, feeder: i32) -> usize;
    fn feeder_text(&mut self, feeder: i32, row: usize, col: usize) -> String;
    fn feeder_select(&mut self, feeder: i32, row: usize);
    /// The material of a list row's image column, or empty.
    fn feeder_image(&mut self, feeder: i32, row: usize, col: usize) -> String;
    /// A key on an owner-draw item (`ownerdraw` id); `true` if it was used.
    fn owner_key(&mut self, ui: &Ui, id: i32, key: &UiKey) -> bool;
    /// Draws the game message window an item of type 19 stands for (`d.game_msg_window_index`), anchored at `rect`
    /// (virtual units and alignments, size unused).
    fn game_message_window(
        &mut self,
        _ui: &Ui,
        _p: &mut paint::Painter,
        _d: &ItemDef,
        _rect: &Rect,
        _color: [f32; 4],
    ) {
    }
    /// Draws an owner-draw item (`ownerdraw` id in `d.window.owner_draw`) inside `rect` (pixels).
    fn owner_draw(
        &mut self,
        ui: &Ui,
        p: &mut paint::Painter,
        d: &ItemDef,
        rect: place::Px,
        color: [f32; 4],
        text: &str,
    );
}

struct ItemRt {
    dyn_flags: u32,
    fore: [f32; 4],
    rect: Rect,
    visible: Option<Compiled>,
    text: Option<Compiled>,
    material: Option<Compiled>,
    rect_x: Option<Compiled>,
    rect_y: Option<Compiled>,
    rect_w: Option<Compiled>,
    rect_h: Option<Compiled>,
    fore_a: Option<Compiled>,
    /// Selected row and first visible row of a list box.
    list_cursor: i32,
    list_start: i32,
    /// Edit field text and caret (characters).
    edit: String,
    editing: bool,
}

struct MenuRt {
    def: Arc<MenuDef>,
    name: String,
    dyn_flags: u32,
    rect: Rect,
    visible: Option<Compiled>,
    rect_x: Option<Compiled>,
    rect_y: Option<Compiled>,
    items: Vec<ItemRt>,
    /// The focused item.
    cursor: Option<usize>,
}

fn compile(s: &::assets::zone::menu::Statement) -> Option<Compiled> {
    if s.entries.is_empty() {
        None
    } else {
        Compiled::new(s).ok()
    }
}

impl ItemRt {
    fn new(d: &ItemDef) -> Self {
        ItemRt {
            dyn_flags: d.window.dynamic_flags,
            fore: d.window.fore_color,
            rect: d.window.rect,
            visible: compile(&d.visible_exp),
            text: compile(&d.text_exp),
            material: compile(&d.material_exp),
            rect_x: compile(&d.rect_x_exp),
            rect_y: compile(&d.rect_y_exp),
            rect_w: compile(&d.rect_w_exp),
            rect_h: compile(&d.rect_h_exp),
            fore_a: compile(&d.forecolor_a_exp),
            list_cursor: 0,
            list_start: 0,
            edit: String::new(),
            editing: false,
        }
    }
}

impl MenuRt {
    fn new(def: &Arc<MenuDef>) -> Self {
        MenuRt {
            name: def
                .window
                .name
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase(),
            dyn_flags: def.window.dynamic_flags,
            rect: def.window.rect,
            visible: compile(&def.visible_exp),
            rect_x: compile(&def.rect_x_exp),
            rect_y: compile(&def.rect_y_exp),
            items: def.items.iter().map(ItemRt::new).collect(),
            cursor: None,
            def: def.clone(),
        }
    }
}

/// The menu system.
pub struct Ui {
    pub assets: UiAssets,
    menus: Vec<MenuRt>,
    index: HashMap<String, usize>,
    /// Open menus, bottom first.
    stack: Vec<usize>,
    /// Menus that are always on while a match runs (the HUD set).
    hud: Vec<usize>,
    /// `setLocalVar*` values.
    pub locals: HashMap<String, expr::LocalVar>,
    pub place: place::Place,
    /// Cursor position in pixels.
    pub cursor: (f32, f32),
    pub cursor_visible: bool,
    /// Clipboard of closed menu names for `closeForGameType` etc. is not needed; kept lean.
    pub now_ms: i32,
    /// Script commands and `uiScript`s nothing implements, each once (also logged once); the stock-menu test
    /// requires this to stay empty.
    pub unknown: Vec<String>,
    /// The bind item waiting for a key (menu, item).
    bind_pending: Option<(usize, usize)>,
    /// Whether `scriptMenuResponse` reaches the server; off while menus close because the level changed.
    pub allow_menu_response: bool,
    /// What opened the menu that is up.
    pub menu_kind: MenuKind,
    /// A server menu that came while another had focus: it opens when that one closes (`cg_waitingScriptMenu`).
    waiting_menu: Option<(String, bool)>,
}

impl Ui {
    fn note_unknown(&mut self, what: String) {
        if !self.unknown.contains(&what) {
            eprintln!("ui: unknown {what}");
            self.unknown.push(what);
        }
    }

    pub fn new(assets: UiAssets, size: (u32, u32)) -> Self {
        let mut menus = Vec::new();
        let mut index = HashMap::new();
        let mut hud = Vec::new();
        for name in &assets.menu_order {
            if let Some(def) = assets.menus.get(name) {
                let rt = MenuRt::new(def);
                index.insert(name.clone(), menus.len());
                menus.push(rt);
            }
        }
        for name in &assets.hud_order {
            if let Some(&i) = index.get(name) {
                menus[i].dyn_flags |= dynf::VISIBLE;
                hud.push(i);
            }
        }
        Ui {
            assets,
            menus,
            index,
            stack: Vec::new(),
            hud,
            locals: HashMap::new(),
            place: place::Place::new(size.0, size.1),
            cursor: (size.0 as f32 * 0.5, size.1 as f32 * 0.5),
            cursor_visible: false,
            now_ms: 0,
            unknown: Vec::new(),
            bind_pending: None,
            allow_menu_response: true,
            menu_kind: MenuKind::Other,
            waiting_menu: None,
        }
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        self.place = place::Place::new(w, h);
    }

    pub fn menu_index(&self, name: &str) -> Option<usize> {
        self.index.get(&name.to_ascii_lowercase()).copied()
    }

    pub fn is_open(&self, name: &str) -> bool {
        self.menu_index(name)
            .is_some_and(|i| self.stack.contains(&i))
    }

    /// True when any menu on the stack wants the mouse and keyboard (the game does not get them).
    pub fn captures_input(&self) -> bool {
        !self.stack.is_empty()
    }

    pub fn open_menus(&self) -> Vec<&str> {
        self.stack
            .iter()
            .map(|&i| self.menus[i].name.as_str())
            .collect()
    }

    // ---- script execution ------------------------------------------------------------------------------------

    /// Runs an action string for an item of menu `menu` (an index, or `None` for a menu-level script).
    pub fn run_script(
        &mut self,
        host: &mut dyn Host,
        menu: Option<usize>,
        item: Option<usize>,
        src: &str,
    ) {
        for cmd in script::parse(src) {
            self.run_command(host, menu, item, &cmd);
        }
    }

    fn translate(&self, w: &str) -> String {
        match w.strip_prefix('@') {
            Some(k) => self
                .assets
                .translate(k)
                .map_or_else(|| w.to_owned(), |s| s.to_string()),
            None => w.to_owned(),
        }
    }

    fn run_command(
        &mut self,
        host: &mut dyn Host,
        menu: Option<usize>,
        _item: Option<usize>,
        cmd: &[String],
    ) {
        let Some(name) = cmd.first() else { return };
        let arg = |i: usize| {
            cmd.get(i + 1)
                .map(|w| self.translate(w))
                .unwrap_or_default()
        };
        match name.to_ascii_lowercase().as_str() {
            "open" => self.open_by_name(host, &arg(0)),
            "close" => {
                let a = arg(0);
                if a == "self" {
                    if let Some(m) = menu {
                        self.close(host, m);
                    }
                } else if let Some(m) = self.menu_index(&a) {
                    self.close(host, m);
                }
            }
            "closemenu" => self.close_by_name(host, &arg(0)),
            "ingameopen" if host.in_game() => self.open_by_name(host, &arg(0)),
            "ingameclose" if host.in_game() => {
                if let Some(m) = self.menu_index(&arg(0)) {
                    self.close(host, m);
                }
            }
            "showmenu" => {
                if let Some(m) = self.menu_index(&arg(0)) {
                    self.menus[m].dyn_flags |= dynf::VISIBLE;
                }
            }
            "hidemenu" => {
                if let Some(m) = self.menu_index(&arg(0)) {
                    self.menus[m].dyn_flags &= !dynf::VISIBLE;
                }
            }
            "show" | "hide" => {
                let show = name.eq_ignore_ascii_case("show");
                if let Some(m) = menu {
                    for i in self.items_in_group(m, &arg(0)) {
                        let it = &mut self.menus[m].items[i];
                        if show {
                            it.dyn_flags |= dynf::VISIBLE;
                        } else {
                            it.dyn_flags &= !dynf::VISIBLE;
                        }
                    }
                }
            }
            "fadein" | "fadeout" => {
                let out = name.eq_ignore_ascii_case("fadeout");
                if let Some(m) = menu {
                    for i in self.items_in_group(m, &arg(0)) {
                        let it = &mut self.menus[m].items[i];
                        if out {
                            it.dyn_flags =
                                (it.dyn_flags | dynf::VISIBLE | dynf::FADINGOUT) & !dynf::FADINGIN;
                        } else {
                            it.dyn_flags =
                                (it.dyn_flags | dynf::VISIBLE | dynf::FADINGIN) & !dynf::FADINGOUT;
                        }
                    }
                }
            }
            "setcolor" | "setitemcolor" => {
                // setitemcolor <group> <forecolor|backcolor|bordercolor> r g b a
                if let Some(m) = menu {
                    let which = arg(1);
                    let rgba: Vec<f32> = (2..6).map(|i| arg(i).parse().unwrap_or(0.0)).collect();
                    if which.eq_ignore_ascii_case("forecolor") && rgba.len() == 4 {
                        for i in self.items_in_group(m, &arg(0)) {
                            let it = &mut self.menus[m].items[i];
                            it.fore = [rgba[0], rgba[1], rgba[2], rgba[3]];
                            it.dyn_flags |= dynf::FORECOLOR_SET;
                        }
                    }
                }
            }
            "focusfirst" => {
                if let Some(m) = menu {
                    self.focus_first(host, m);
                }
            }
            "setfocus" => {
                if let Some(m) = menu {
                    let want = arg(0).to_ascii_lowercase();
                    let found = self.menus[m].def.items.iter().position(|d| {
                        d.window
                            .name
                            .as_deref()
                            .is_some_and(|n| n.eq_ignore_ascii_case(&want))
                    });
                    if let Some(i) = found {
                        self.set_focus(host, m, i);
                    }
                }
            }
            "setfocusbydvar" => {
                if let Some(m) = menu {
                    let dv = arg(0);
                    let found = self.menus[m].def.items.iter().position(|d| {
                        d.dvar_test
                            .as_deref()
                            .is_some_and(|t| t.eq_ignore_ascii_case(&dv))
                            && self.enable_dvar_matches(host, d, 0x10)
                    });
                    if let Some(i) = found {
                        self.set_focus(host, m, i);
                    }
                }
            }
            "setdvar" | "set" => {
                let (n, v) = (arg(0), arg(1));
                host.set_dvar(&n, &v);
            }
            "exec" | "execnow" => host.exec(&*self, &arg(0)),
            "execondvarstringvalue" | "execnowondvarstringvalue" => {
                if host.dvar(&arg(0)).eq_ignore_ascii_case(&arg(1)) {
                    host.exec(&*self, &arg(2));
                }
            }
            "execondvarintvalue" | "execnowondvarintvalue" => {
                let atoi = |s: &str| s.trim().parse::<f64>().map_or(0, |v| v as i32);
                if atoi(&host.dvar(&arg(0))) == atoi(&arg(1)) {
                    host.exec(&*self, &arg(2));
                }
            }
            "execondvarfloatvalue" | "execnowondvarfloatvalue" => {
                let atof = |s: &str| s.trim().parse::<f64>().unwrap_or(0.0);
                if (atof(&host.dvar(&arg(0))) - atof(&arg(1))).abs() < 1e-5 {
                    host.exec(&*self, &arg(2));
                }
            }
            "play" => host.play(&arg(0)),
            "scriptmenuresponse" => {
                if let Some(m) = menu.filter(|_| self.allow_menu_response) {
                    host.menu_response(&self.menus[m].name.clone(), &arg(0));
                }
            }
            "scriptmenurespondondvarstringvalue"
            | "scriptmenurespondondvarintvalue"
            | "scriptmenurespondondvarfloatvalue" => {
                let kind = name.to_ascii_lowercase();
                let (cur, want) = (host.dvar(&arg(0)), arg(1));
                let hit = if kind.ends_with("stringvalue") {
                    cur.eq_ignore_ascii_case(&want)
                } else {
                    let f = |s: &str| s.trim().parse::<f64>().unwrap_or(0.0);
                    (f(&cur) - f(&want)).abs() < 1e-5
                };
                if hit
                    && self.allow_menu_response
                    && let Some(m) = menu
                {
                    host.menu_response(&self.menus[m].name.clone(), &arg(2));
                }
            }
            "setlocalvarbool" | "setlocalvarint" | "setlocalvarfloat" | "setlocalvarstring" => {
                use expr::LocalVar;
                let v = arg(1);
                let num = v.trim().parse::<f64>().unwrap_or(0.0);
                let var = match name.to_ascii_lowercase().as_str() {
                    "setlocalvarbool" => LocalVar::Int(i32::from(num != 0.0)),
                    "setlocalvarint" => LocalVar::Int(num as i32),
                    "setlocalvarfloat" => LocalVar::Float(num as f32),
                    _ => LocalVar::Str(v),
                };
                self.locals.insert(arg(0).to_ascii_lowercase(), var);
            }
            "feedertop" | "feederbottom" => {
                if let Some(m) = menu {
                    let top = name.eq_ignore_ascii_case("feedertop");
                    let want = arg(0).to_ascii_lowercase();
                    let found = self.menus[m].def.items.iter().position(|d| {
                        d.window
                            .name
                            .as_deref()
                            .is_some_and(|n| n.eq_ignore_ascii_case(&want))
                    });
                    if let Some(i) = found {
                        let n = host.feeder_count(self.menus[m].def.items[i].special as i32) as i32;
                        let it = &mut self.menus[m].items[i];
                        it.list_cursor = if top { 0 } else { (n - 1).max(0) };
                        it.list_start = if top { 0 } else { (n - 1).max(0) };
                    }
                }
            }
            "statsetusingtable" | "statset" | "statclearbitmask" | "statclearperknew"
            | "getautoupdate" | "wait" | "setbackground" | "openforgametype"
            | "closeforgametype" => {
                host.exec(
                    &*self,
                    &cmd.iter().map(String::as_str).collect::<Vec<_>>().join(" "),
                );
            }
            "uiscript" => {
                let args: Vec<String> = cmd.iter().skip(2).map(|w| self.translate(w)).collect();
                let n = arg(0);
                if !host.ui_script(self, &n, &args) {
                    self.note_unknown(format!("uiScript '{n}'"));
                }
            }
            other => self.note_unknown(format!("menu command '{other}'")),
        }
    }

    fn items_in_group(&self, menu: usize, group: &str) -> Vec<usize> {
        let g = group.to_ascii_lowercase();
        self.menus[menu]
            .def
            .items
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                d.window
                    .name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(&g))
                    || d.window
                        .group
                        .as_deref()
                        .is_some_and(|n| n.eq_ignore_ascii_case(&g))
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// `enableDvar`/`dvarTest` rule of an item: does the dvar's value (given by `dvarTest`) appear in the
    /// space-separated `enableDvar` list, for the flag kinds in `mask` (1 enable, 2 disable, 4 show, 8 hide, 16 focus).
    fn enable_dvar_matches(&self, host: &dyn Host, d: &ItemDef, mask: i32) -> bool {
        let (Some(list), Some(test)) = (d.enable_dvar.as_deref(), d.dvar_test.as_deref()) else {
            return true;
        };
        if list.is_empty() || test.is_empty() {
            return true;
        }
        let cur = host.dvar(test);
        let hit = list.split(';').any(|v| v.trim().eq_ignore_ascii_case(&cur));
        let flags = d.dvar_flags;
        let mut ok = true;
        if flags & mask & 0x1 != 0 {
            ok &= hit;
        }
        if flags & mask & 0x2 != 0 {
            ok &= !hit;
        }
        if flags & mask & 0x4 != 0 {
            ok &= hit;
        }
        if flags & mask & 0x8 != 0 {
            ok &= !hit;
        }
        if flags & mask & 0x10 != 0 {
            ok &= hit;
        }
        ok
    }

    // ---- open / close / focus -------------------------------------------------------------------------------

    pub fn open_by_name(&mut self, host: &mut dyn Host, name: &str) {
        match self.menu_index(name) {
            Some(m) => self.open(host, m),
            None => eprintln!("ui: could not find menu '{name}'"),
        }
    }

    pub fn open(&mut self, host: &mut dyn Host, m: usize) {
        // A menu is mouse-driven unless the server opened it with `openmenunomouse` (which says so after this call).
        // Leaving the flag as the last menu set it hid the pointer of every later menu, the Escape menu included.
        self.cursor_visible = true;
        for &o in &self.stack.clone() {
            self.lose_focus(host, o);
        }
        self.stack.retain(|&s| s != m);
        self.stack.push(m);
        self.menus[m].dyn_flags |= dynf::VISIBLE | dynf::HASFOCUS;
        self.gain_focus(host, m);
        if let Some(src) = self.menus[m].def.on_open.clone() {
            self.run_script(host, Some(m), None, &src);
        }
        if let Some(s) = self.menus[m].def.sound_name.clone() {
            host.play(&s);
        }
    }

    /// `UI_PopupScriptMenu`: opens a menu the server asked for, replacing the menus up, unless another menu has the
    /// focus that is not a script menu or the scoreboard. `false` if refused.
    fn popup_script_menu(&mut self, host: &mut dyn Host, name: &str, mouse: bool) -> bool {
        let top = self.stack.last().map(|&m| self.menus[m].name.clone());
        let name = name.to_ascii_lowercase();
        if top.is_some() && !matches!(self.menu_kind, MenuKind::ScriptPopup | MenuKind::Scoreboard)
        {
            return false;
        }
        if top.as_ref() != Some(&name) {
            self.close_all(host);
            self.open_by_name(host, &name);
        }
        self.menu_kind = MenuKind::ScriptPopup;
        self.cursor_visible = mouse;
        true
    }

    /// `CG_OpenScriptMenu`: a menu the player does not have is answered `bad`; one that cannot open yet waits, and the
    /// menu that waited before is answered `noop`.
    pub fn open_script_menu(&mut self, host: &mut dyn Host, name: &str, mouse: bool) {
        if self.menu_index(name).is_none() {
            host.menu_response(name, "bad");
        } else if !self.popup_script_menu(host, name, mouse) {
            if let Some((old, _)) = self.waiting_menu.take() {
                if old.eq_ignore_ascii_case(name) {
                    self.waiting_menu = Some((old, mouse));
                    return;
                }
                host.menu_response(&old, "noop");
            }
            self.waiting_menu = Some((name.to_owned(), mouse));
        }
    }

    /// `CG_CheckOpenWaitingScriptMenu`: once a frame, opens the waiting menu if nothing blocks it any more.
    pub fn check_waiting_menu(&mut self, host: &mut dyn Host) {
        if let Some((name, mouse)) = self.waiting_menu.clone()
            && self.popup_script_menu(host, &name, mouse)
        {
            self.waiting_menu = None;
        }
    }

    pub fn clear_waiting_menu(&mut self) {
        self.waiting_menu = None;
    }

    /// Opens a menu on behalf of the client (not a menu script), recording what kind it is.
    pub fn open_as(&mut self, host: &mut dyn Host, name: &str, kind: MenuKind) {
        self.menu_kind = kind;
        self.open_by_name(host, name);
    }

    /// `UI_CloseInGameMenu`: closes the Escape menu, unless a full-screen menu is up or something else is.
    pub fn close_ingame_menu(&mut self, host: &mut dyn Host) {
        if self.menu_kind == MenuKind::Ingame && !self.full_screen_visible(host) {
            self.close_all(host);
        }
    }

    pub fn close_by_name(&mut self, host: &mut dyn Host, name: &str) {
        if let Some(m) = self.menu_index(name) {
            self.close(host, m);
        }
    }

    pub fn close(&mut self, host: &mut dyn Host, m: usize) {
        if !self.stack.contains(&m) {
            return;
        }
        if let Some(src) = self.menus[m].def.on_close.clone() {
            self.run_script(host, Some(m), None, &src);
        }
        let had_focus = self.menus[m].dyn_flags & dynf::HASFOCUS != 0;
        self.stack.retain(|&s| s != m);
        self.menus[m].dyn_flags &= !(dynf::VISIBLE | dynf::HASFOCUS);
        self.lose_item_focus(host, m);
        for i in 0..self.menus[m].items.len() {
            if self.menus[m].items[i].dyn_flags & dynf::MOUSEOVER != 0 {
                self.mouse_leave(host, m, i);
            }
        }
        if had_focus && let Some(&top) = self.stack.last() {
            self.menus[top].dyn_flags |= dynf::HASFOCUS;
            self.gain_focus(host, top);
        }
    }

    /// Whether an open menu that covers the screen is showing (`Menus_AnyFullScreenVisible`).
    pub fn full_screen_visible(&mut self, host: &mut dyn Host) -> bool {
        self.stack
            .clone()
            .into_iter()
            .any(|o| self.menus[o].def.full_screen != 0 && self.menu_visible(host, o))
    }

    pub fn close_all(&mut self, host: &mut dyn Host) {
        for m in self.stack.clone().into_iter().rev() {
            self.close(host, m);
        }
    }

    fn lose_focus(&mut self, host: &mut dyn Host, m: usize) {
        self.menus[m].dyn_flags &= !dynf::HASFOCUS;
        self.lose_item_focus(host, m);
    }

    fn lose_item_focus(&mut self, host: &mut dyn Host, m: usize) {
        if let Some(i) = self.menus[m].cursor.take() {
            self.menus[m].items[i].dyn_flags &= !dynf::HASFOCUS;
            if let Some(s) = self.menus[m].def.items[i].leave_focus.clone() {
                self.run_script(host, Some(m), Some(i), &s);
            }
        }
    }

    fn gain_focus(&mut self, host: &mut dyn Host, m: usize) {
        if self.menus[m].cursor.is_none() && !self.cursor_visible {
            self.focus_first(host, m);
        }
    }

    /// Whether the mouse/keyboard can focus the item: visible, not decoration, an interactive type.
    fn selectable(&mut self, host: &mut dyn Host, m: usize, i: usize) -> bool {
        let d = &self.menus[m].def.items[i];
        if d.window.static_flags & statf::DECORATION != 0 {
            return false;
        }
        use ity::*;
        let t = d.ty;
        if !matches!(
            t,
            BUTTON
                | EDITFIELD
                | LISTBOX
                | NUMERICFIELD
                | SLIDER
                | YESNO
                | MULTI
                | DVARENUM
                | BIND
                | VALIDFILEFIELD
                | DECIMALFIELD
                | UPREDITFIELD
        ) && !(t == OWNERDRAW && d.action.as_deref().is_some_and(|a| !a.trim().is_empty()))
            && !(t == TEXT && d.action.as_deref().is_some_and(|a| !a.trim().is_empty()))
        {
            return false;
        }
        self.item_visible(host, m, i)
    }

    pub fn focus_first(&mut self, host: &mut dyn Host, m: usize) {
        for i in 0..self.menus[m].items.len() {
            if self.selectable(host, m, i) {
                self.set_focus(host, m, i);
                return;
            }
        }
    }

    pub fn set_focus(&mut self, host: &mut dyn Host, m: usize, i: usize) {
        if self.menus[m].cursor == Some(i) {
            return;
        }
        self.lose_item_focus(host, m);
        // Typing in a field ends when another item takes focus.
        for it in &mut self.menus[m].items {
            it.editing = false;
        }
        self.menus[m].cursor = Some(i);
        self.menus[m].items[i].dyn_flags |= dynf::HASFOCUS;
        let d = self.menus[m].def.clone();
        // A static text item takes focus without running `onFocus` (`Item_SetFocus`).
        if d.items[i].ty != ity::TEXT
            && let Some(s) = &d.items[i].on_focus
        {
            self.run_script(host, Some(m), Some(i), s);
        }
        if d.items[i].ty == ity::EDITFIELD {
            self.begin_edit(host, m, i);
        }
    }

    fn move_focus(&mut self, host: &mut dyn Host, m: usize, dir: i32) {
        let n = self.menus[m].items.len() as i32;
        if n == 0 {
            return;
        }
        let start = self.menus[m]
            .cursor
            .map_or(if dir > 0 { -1 } else { n }, |c| c as i32);
        let mut i = start;
        for _ in 0..n {
            i = (i + dir).rem_euclid(n);
            if self.selectable(host, m, i as usize) {
                self.set_focus(host, m, i as usize);
                return;
            }
        }
    }

    fn begin_edit(&mut self, host: &mut dyn Host, m: usize, i: usize) {
        let dv = self.menus[m].def.items[i].dvar.clone().unwrap_or_default();
        let v = host.dvar(&dv);
        let it = &mut self.menus[m].items[i];
        it.edit = v;
        it.editing = true;
    }

    // ---- visibility and layout ------------------------------------------------------------------------------

    pub fn menu_visible(&mut self, host: &mut dyn Host, m: usize) -> bool {
        let mrt = &self.menus[m];
        if mrt.dyn_flags & dynf::VISIBLE == 0 {
            return false;
        }
        let w = &mrt.def.window;
        if w.owner_draw_flags != 0 && !owner_draw_visible(w.owner_draw_flags, &*host) {
            return false;
        }
        let hidden_by = |flag: u32, state: bool| w.static_flags & flag != 0 && state;
        if hidden_by(statf::HIDDEN_DURING_SCOPE, host.scoped())
            || hidden_by(statf::HIDDEN_DURING_FLASHBANG, host.flashbanged())
            || hidden_by(statf::HIDDEN_DURING_UI, self.captures_input())
        {
            return false;
        }
        match &mrt.visible {
            Some(e) => self.eval_bool(&*host, e),
            None => true,
        }
    }

    fn item_visible(&mut self, host: &mut dyn Host, m: usize, i: usize) -> bool {
        let it = &self.menus[m].items[i];
        if it.dyn_flags & dynf::VISIBLE == 0 {
            return false;
        }
        let d = &self.menus[m].def.items[i];
        if d.dvar_flags & 0xC != 0 && !self.enable_dvar_matches(host, d, 4 | 8) {
            return false;
        }
        match &it.visible {
            Some(e) => self.eval_bool(&*host, e),
            None => true,
        }
    }

    // ---- input -----------------------------------------------------------------------------------------------

    /// The mouse moved to pixel `(x, y)` (`Display_MouseMove`): a focused popup alone follows it; otherwise the menus
    /// are walked from the top down until one takes focus or is full screen.
    pub fn mouse_move(&mut self, host: &mut dyn Host, x: f32, y: f32) {
        self.cursor = (x, y);
        let Some(&top) = self.stack.last() else {
            return;
        };
        if self.menus[top].def.window.static_flags & statf::POPUP != 0 {
            self.menu_mouse_move(host, top);
            return;
        }
        for m in self.stack.clone().into_iter().rev() {
            if self.menu_mouse_move(host, m) || self.menus[m].def.full_screen != 0 {
                break;
            }
        }
    }

    /// `Menu_HandleMouseMove`: runs the enter and exit scripts of the items the pointer reached or left, focuses
    /// the topmost item under it, and clears the focus when it left the focused item. `true` if an item took focus.
    fn menu_mouse_move(&mut self, host: &mut dyn Host, m: usize) -> bool {
        if !self.cursor_visible
            || self.bind_pending.is_some()
            || self.menus[m].dyn_flags & dynf::VISIBLE == 0
        {
            return false;
        }
        let (x, y) = self.cursor;
        let (mut focused, mut focus_set) = (None, false);
        for pass in 0..2 {
            for i in (0..self.menus[m].items.len()).rev() {
                if !self.item_live(host, m, i) {
                    // An item that went away with the pointer on it owes its exit script.
                    if self.menus[m].items[i].dyn_flags & dynf::MOUSEOVER != 0 {
                        self.mouse_leave(host, m, i);
                    }
                    continue;
                }
                if self.menus[m].items[i].dyn_flags & dynf::HASFOCUS != 0 && focused.is_none() {
                    focused = Some(i);
                }
                if self.item_contains(m, i, x, y) {
                    if pass == 1 && self.over_text(&*host, m, i, x, y) {
                        self.mouse_enter(host, m, i, x, y);
                        if !focus_set && self.try_focus(host, m, i, x, y) {
                            focus_set = true;
                            focused = Some(i);
                        }
                    }
                } else if self.menus[m].items[i].dyn_flags & dynf::MOUSEOVER != 0 {
                    self.mouse_leave(host, m, i);
                }
            }
        }
        if !focus_set
            && let Some(f) = focused
            && !self.item_contains(m, f, x, y)
        {
            self.lose_item_focus(host, m);
        }
        focus_set
    }

    /// The item shows and takes the pointer: visible, and not switched off by its `enableDvar` rule.
    fn item_live(&mut self, host: &mut dyn Host, m: usize, i: usize) -> bool {
        let d = &self.menus[m].def.items[i];
        (d.dvar_flags & 3 == 0 || self.enable_dvar_matches(host, d, 1))
            && self.item_visible(host, m, i)
    }

    /// Where the text of an item is, in pixels (`Item_CorrectedTextRect`): `None` for an item with no text.
    fn text_rect_px(&self, host: &dyn Host, m: usize, i: usize) -> Option<place::Px> {
        let d = &self.menus[m].def.items[i];
        let text = self.item_text(host, m, i, d);
        if text.is_empty() {
            return None;
        }
        let r = self.menus[m].items[i].rect;
        let h = self.text_height(d.font_enum, d.text_scale);
        let w = self.text_width(&text, d.font_enum, d.text_scale);
        let mut x = d.text_align_x;
        match d.text_align_mode & 3 {
            1 => x += (r.w - w) * 0.5,
            2 => x += r.w - w,
            _ => {}
        }
        let y = d.text_align_y + self.text_y(d.text_align_mode & 0xC, r.h, h);
        let bsz = if d.window.border != 0 {
            d.window.border_size
        } else {
            0.0
        };
        // `y` is the baseline: the rect spans the text height above it.
        Some(self.place.rect(
            x + bsz + r.x,
            y + bsz + r.y - h,
            w,
            h,
            r.horz_align,
            r.vert_align,
        ))
    }

    /// Type-0 text items answer the pointer over their text only; every other item over its whole rect.
    fn over_text(&self, host: &dyn Host, m: usize, i: usize, x: f32, y: f32) -> bool {
        self.menus[m].def.items[i].ty != ity::TEXT
            || self
                .text_rect_px(host, m, i)
                .is_none_or(|r| r.contains(x, y))
    }

    /// `Item_MouseEnter`: the first move onto the item's text runs `mouseEnterText`, the first onto the item
    /// `mouseEnter`; moving off the text but staying on the item runs `mouseExitText`.
    fn mouse_enter(&mut self, host: &mut dyn Host, m: usize, i: usize, x: f32, y: f32) {
        let in_text = self
            .text_rect_px(&*host, m, i)
            .is_some_and(|r| r.contains(x, y));
        let fl = self.menus[m].items[i].dyn_flags;
        if in_text {
            if fl & dynf::MOUSEOVER_TEXT == 0 {
                self.run_item_script(host, m, i, |d| &d.mouse_enter_text);
                self.menus[m].items[i].dyn_flags |= dynf::MOUSEOVER_TEXT;
            }
        } else if fl & dynf::MOUSEOVER_TEXT != 0 {
            self.run_item_script(host, m, i, |d| &d.mouse_exit_text);
            self.menus[m].items[i].dyn_flags &= !dynf::MOUSEOVER_TEXT;
        }
        if fl & dynf::MOUSEOVER == 0 {
            self.run_item_script(host, m, i, |d| &d.mouse_enter);
            self.menus[m].items[i].dyn_flags |= dynf::MOUSEOVER;
        }
    }

    /// `Item_MouseLeave`: the pointer left (or the item or its menu went away): `mouseExitText` if the pointer was on
    /// the text, then `mouseExit`.
    fn mouse_leave(&mut self, host: &mut dyn Host, m: usize, i: usize) {
        if self.menus[m].items[i].dyn_flags & dynf::MOUSEOVER_TEXT != 0 {
            self.run_item_script(host, m, i, |d| &d.mouse_exit_text);
        }
        self.run_item_script(host, m, i, |d| &d.mouse_exit);
        self.menus[m].items[i].dyn_flags &= !(dynf::MOUSEOVER | dynf::MOUSEOVER_TEXT);
    }

    fn run_item_script(
        &mut self,
        host: &mut dyn Host,
        m: usize,
        i: usize,
        pick: fn(&ItemDef) -> &Option<Arc<str>>,
    ) {
        let def = self.menus[m].def.clone();
        if let Some(s) = pick(&def.items[i]) {
            self.run_script(host, Some(m), Some(i), s);
        }
    }

    /// `Item_SetFocus` for the pointer: any visible item that is not a decoration takes focus, unless the pointer is
    /// also over the focused menu above this one. `true` if the item has focus now.
    fn try_focus(&mut self, host: &mut dyn Host, m: usize, i: usize, x: f32, y: f32) -> bool {
        let d = &self.menus[m].def.items[i];
        if d.window.static_flags & statf::DECORATION != 0
            || self.menus[m].items[i].dyn_flags & dynf::VISIBLE == 0
        {
            return false;
        }
        if self.menus[m].items[i].dyn_flags & dynf::HASFOCUS != 0 {
            return true;
        }
        if let Some(&top) = self.stack.last()
            && top != m
            && self.menu_contains(top, x, y)
            && self.menu_contains(m, x, y)
        {
            return false;
        }
        self.set_focus(host, m, i);
        true
    }

    fn menu_contains(&self, m: usize, x: f32, y: f32) -> bool {
        let r = &self.menus[m].rect;
        self.place
            .rect(r.x, r.y, r.w, r.h, r.horz_align, r.vert_align)
            .contains(x, y)
    }

    /// `Menu_OverActiveItem`: an item of the visible menu that the pointer would focus.
    fn over_active_item(&mut self, host: &mut dyn Host, m: usize) -> bool {
        if self.menus[m].dyn_flags & dynf::VISIBLE == 0 {
            return false;
        }
        let (x, y) = self.cursor;
        (0..self.menus[m].items.len()).any(|i| {
            self.menus[m].def.items[i].window.static_flags & statf::DECORATION == 0
                && self.item_live(host, m, i)
                && self.item_contains(m, i, x, y)
                && self.over_text(&*host, m, i, x, y)
        })
    }

    /// `Menus_HandleOOBClick`: a click outside a menu that is neither a popup nor full screen closes it when it asked
    /// to be closed that way, and goes to the topmost menu that has an item under the pointer, which takes focus.
    fn oob_click(&mut self, host: &mut dyn Host, m: usize, key: UiKey) -> bool {
        if self.menus[m].def.window.static_flags & statf::OUT_OF_BOUNDS_CLICK != 0 {
            self.close(host, m);
        }
        for o in self.stack.clone().into_iter().rev() {
            if self.over_active_item(host, o) {
                for s in self.stack.clone() {
                    self.menus[s].dyn_flags &= !dynf::HASFOCUS;
                }
                self.stack.retain(|&s| s != o);
                self.stack.push(o);
                self.menus[o].dyn_flags |= dynf::VISIBLE | dynf::HASFOCUS;
                let (x, y) = self.cursor;
                self.mouse_move(host, x, y);
                return self.key_in(host, o, key, false);
            }
        }
        true
    }

    fn item_pixels(&self, m: usize, i: usize) -> place::Px {
        let it = &self.menus[m].items[i];
        let r = &it.rect;
        self.place
            .rect(r.x, r.y, r.w, r.h, r.horz_align, r.vert_align)
    }

    fn item_contains(&self, m: usize, i: usize, x: f32, y: f32) -> bool {
        self.item_pixels(m, i).contains(x, y)
    }

    /// True while a bind item waits for the key to bind (the next key press goes to [`Ui::bind_capture`]).
    pub fn bind_pending(&self) -> bool {
        self.bind_pending.is_some()
    }

    /// The key `name` (the input layer's name: `q`, `mouse1`, `backspace`, ...) answers a pending bind item:
    /// Backspace clears the command's keys, any other key replaces them (`Item_Bind_HandleKey`: a command holds at
    /// most two keys, and a third press starts over). Returns whether a bind was pending.
    pub fn bind_capture(&mut self, host: &mut dyn Host, name: &str) -> bool {
        let Some((m, i)) = self.bind_pending.take() else {
            return false;
        };
        let Some(cmd) = self.menus[m].def.items[i].dvar.clone() else {
            return true;
        };
        let held = host.key_bindings(&cmd);
        if name == "backspace" || held.len() >= 2 {
            for k in held {
                host.set_bind(&k, "");
            }
        }
        if name != "backspace" {
            host.set_bind(name, &cmd);
        }
        true
    }

    /// The localized name of a key (`KEY_MOUSE1`); a key without a translation shows its bare name.
    pub fn localize_key(&self, id: &str) -> String {
        match self.assets.translate(id) {
            Some(t) => t.to_string(),
            None => id.strip_prefix("KEY_").unwrap_or(id).to_owned(),
        }
    }

    /// What a bind item shows for `command`: its keys (at most two, joined by "or"), or the unbound text.
    pub(crate) fn bind_label(&self, host: &dyn Host, command: &str) -> String {
        let keys: Vec<String> = host
            .key_bindings(command)
            .iter()
            .take(2)
            .map(|k| self.localize_key(k))
            .collect();
        match keys.as_slice() {
            [] => self.localize_key("KEY_UNBOUND"),
            [a] => a.clone(),
            [a, b] => format!("{a} {} {b}", self.localize_key("KEY_OR")),
            _ => unreachable!(),
        }
    }

    /// Handles a key press. Returns whether the menus used it.
    pub fn key(&mut self, host: &mut dyn Host, key: UiKey) -> bool {
        let Some(&m) = self.stack.last() else {
            return false;
        };
        self.key_in(host, m, key, true)
    }

    /// `Menu_CheckOnKey`: the menu's `execKey` handler for `code`, else that of a visible item that has focus (or is
    /// a decoration). Returns whether one ran.
    fn check_on_key(&mut self, host: &mut dyn Host, m: usize, code: i32) -> bool {
        let def = self.menus[m].def.clone();
        let mut found = def.on_key.iter().find(|h| h.key == code);
        if found.is_none() {
            for (i, d) in def.items.iter().enumerate() {
                let flags = self.menus[m].items[i].dyn_flags;
                if (flags & dynf::HASFOCUS != 0 || d.window.static_flags & statf::DECORATION != 0)
                    && self.item_live(host, m, i)
                    && let Some(h) = d.on_key.iter().find(|h| h.key == code)
                {
                    found = Some(h);
                    break;
                }
            }
        }
        let Some(h) = found else { return false };
        if let Some(s) = &h.action {
            self.run_script(host, Some(m), None, s);
        }
        true
    }

    /// Keys for the edit field `i` being typed in: `Some(used)` when it took the key, `None` when it ended the edit
    /// and the key goes on to the usual handling.
    fn edit_key(&mut self, host: &mut dyn Host, m: usize, i: usize, key: &UiKey) -> Option<bool> {
        let def = self.menus[m].def.clone();
        let d = &def.items[i];
        match key {
            UiKey::Char(c) if !c.is_control() => {
                let max = match &d.data {
                    ::assets::zone::menu::ItemData::EditField(Some(e)) if e.max_chars > 0 => {
                        e.max_chars as usize
                    }
                    _ => 256,
                };
                let it = &mut self.menus[m].items[i];
                if it.edit.chars().count() < max {
                    it.edit.push(*c);
                }
                self.commit_edit(host, m, i);
                Some(true)
            }
            UiKey::Backspace => {
                self.menus[m].items[i].edit.pop();
                self.commit_edit(host, m, i);
                Some(true)
            }
            UiKey::Enter => {
                self.menus[m].items[i].editing = false;
                self.commit_edit(host, m, i);
                if let Some(s) = &d.on_accept {
                    self.run_script(host, Some(m), Some(i), s);
                }
                self.move_focus(host, m, 1);
                Some(true)
            }
            UiKey::Escape => {
                self.menus[m].items[i].editing = false;
                Some(true)
            }
            UiKey::Mouse1 | UiKey::Mouse2 | UiKey::Tab => {
                self.menus[m].items[i].editing = false;
                None
            }
            _ => None,
        }
    }

    fn key_in(&mut self, host: &mut dyn Host, m: usize, key: UiKey, oob: bool) -> bool {
        if self.bind_pending.is_some() {
            // The next key is the new binding (`bind_capture`); Escape and text input only get in the way.
            if key == UiKey::Escape {
                self.bind_pending = None;
            }
            return true;
        }
        let def = self.menus[m].def.clone();
        if oob
            && matches!(key, UiKey::Mouse1 | UiKey::Mouse2)
            && def.window.static_flags & statf::POPUP == 0
            && def.full_screen == 0
            && !self.menu_contains(m, self.cursor.0, self.cursor.1)
        {
            return self.oob_click(host, m, key);
        }
        // An edit field in use owns the keys, wherever the pointer is (`g_editingField`): typing, Enter and Escape
        // end up in it; a mouse button or Tab ends the edit and goes on to the usual handling.
        if let Some(i) = self.menus[m].items.iter().position(|it| it.editing)
            && let Some(used) = self.edit_key(host, m, i, &key)
        {
            return used;
        }
        // The focused item's own key handling comes before the menu's `execKey` handlers (`Menu_HandleKey`).
        let item_keys = self.menus[m].cursor.is_some_and(|i| {
            let enter = matches!(key, UiKey::Enter | UiKey::Mouse1);
            let arrows = matches!(key, UiKey::Left | UiKey::Right);
            match def.items[i].ty {
                ity::LISTBOX => {
                    enter
                        || matches!(
                            key,
                            UiKey::Up | UiKey::Down | UiKey::WheelUp | UiKey::WheelDown
                        )
                }
                ity::YESNO | ity::MULTI | ity::DVARENUM | ity::SLIDER => enter || arrows,
                ity::BIND => enter,
                _ => false,
            }
        });
        if !item_keys
            && let Some(code) = key_code(&key)
            && self.check_on_key(host, m, code)
        {
            return true;
        }
        // Key handlers of the menu (`onKey`), keyed by the original's key numbers: only Esc matters here.
        if key == UiKey::Escape {
            match def.on_esc.as_deref() {
                Some(s) if !s.trim().is_empty() && s.trim() != ";" => {
                    self.run_script(host, Some(m), None, s)
                }
                // No handler: the original closes every menu unless a full-screen one is up (the way back into the
                // game from options and vote menus that have no `onESC`), and closes a popup or a blank handler's menu.
                _ => {
                    let stack = self.stack.clone();
                    let full = stack
                        .iter()
                        .any(|&o| self.menus[o].def.full_screen != 0 && self.menu_visible(host, o));
                    if def.on_esc.is_none() && !full && host.in_game() {
                        self.close_all(host);
                    } else if def.window.static_flags & statf::POPUP != 0 || def.on_esc.is_some() {
                        self.close(host, m);
                    }
                }
            }
            return true;
        }
        let Some(i) = self.menus[m].cursor else {
            return match key {
                UiKey::Up | UiKey::Down | UiKey::Tab | UiKey::Enter => {
                    self.move_focus(host, m, 1);
                    true
                }
                _ => def.window.static_flags & statf::POPUP != 0,
            };
        };
        let d = &def.items[i];
        match key {
            UiKey::Up => {
                if d.ty == ity::LISTBOX {
                    self.list_step(host, m, i, -1);
                } else {
                    self.move_focus(host, m, -1);
                }
                true
            }
            UiKey::Down => {
                if d.ty == ity::LISTBOX {
                    self.list_step(host, m, i, 1);
                } else {
                    self.move_focus(host, m, 1);
                }
                true
            }
            UiKey::Tab => {
                self.move_focus(host, m, 1);
                true
            }
            UiKey::WheelUp | UiKey::WheelDown => {
                let dir = if key == UiKey::WheelUp { -1 } else { 1 };
                // The wheel scrolls the list box under the cursor, else the focused one.
                let (cx, cy) = self.cursor;
                let target = (0..self.menus[m].items.len())
                    .rev()
                    .find(|&j| def.items[j].ty == ity::LISTBOX && self.item_contains(m, j, cx, cy))
                    .or((d.ty == ity::LISTBOX).then_some(i));
                if let Some(j) = target {
                    self.list_scroll(host, m, j, dir);
                }
                true
            }
            UiKey::Left | UiKey::Right => {
                let dir = if key == UiKey::Left { -1 } else { 1 };
                match d.ty {
                    ity::SLIDER | ity::MULTI | ity::DVARENUM | ity::YESNO => {
                        self.item_adjust(host, m, i, dir);
                        true
                    }
                    ity::OWNERDRAW => host.owner_key(&*self, d.window.owner_draw, &key),
                    _ => false,
                }
            }
            UiKey::Enter | UiKey::Mouse1 => {
                if key == UiKey::Mouse1 {
                    let (cx, cy) = self.cursor;
                    if !self.item_contains(m, i, cx, cy) {
                        // A click outside any item: popups close on an outside click.
                        if def.window.static_flags & statf::OUT_OF_BOUNDS_CLICK != 0 {
                            self.close(host, m);
                        }
                        return true;
                    }
                }
                if d.ty == ity::OWNERDRAW {
                    host.owner_key(&*self, d.window.owner_draw, &key);
                }
                self.activate(host, m, i, key == UiKey::Mouse1);
                true
            }
            UiKey::Mouse2 if d.ty == ity::OWNERDRAW => {
                host.owner_key(&*self, d.window.owner_draw, &key)
            }
            _ => def.window.static_flags & statf::POPUP != 0,
        }
    }

    fn commit_edit(&mut self, host: &mut dyn Host, m: usize, i: usize) {
        if let Some(dv) = self.menus[m].def.items[i].dvar.clone() {
            let v = self.menus[m].items[i].edit.clone();
            host.set_dvar(&dv, &v);
        }
    }

    fn activate(&mut self, host: &mut dyn Host, m: usize, i: usize, mouse: bool) {
        let def = self.menus[m].def.clone();
        let d = &def.items[i];
        match d.ty {
            ity::EDITFIELD
            | ity::NUMERICFIELD
            | ity::VALIDFILEFIELD
            | ity::DECIMALFIELD
            | ity::UPREDITFIELD => {
                self.begin_edit(host, m, i);
            }
            ity::YESNO | ity::MULTI | ity::DVARENUM | ity::SLIDER => {
                if d.ty == ity::SLIDER && mouse {
                    self.slider_click(host, m, i);
                } else {
                    self.item_adjust(host, m, i, 1);
                }
            }
            ity::LISTBOX => {
                if mouse {
                    self.list_click(host, m, i);
                }
                if let Some(s) = &d.action {
                    self.run_script(host, Some(m), Some(i), s);
                }
            }
            ity::BIND => self.bind_pending = Some((m, i)),
            _ => {
                if let Some(s) = &d.action {
                    host.play("mouse_click");
                    self.run_script(host, Some(m), Some(i), s);
                }
            }
        }
    }

    /// Left/right (or a click) on a yes/no, multi, dvar-enum or slider: step the dvar.
    fn item_adjust(&mut self, host: &mut dyn Host, m: usize, i: usize, dir: i32) {
        use ::assets::zone::menu::ItemData;
        let def = self.menus[m].def.clone();
        let d = &def.items[i];
        let Some(dv) = d.dvar.clone() else { return };
        let cur = host.dvar(&dv);
        match (&d.data, d.ty) {
            (_, ity::YESNO) => {
                let on = cur.trim().parse::<f64>().unwrap_or(0.0) != 0.0;
                host.set_dvar(&dv, if on { "0" } else { "1" });
            }
            (ItemData::Multi(Some(mu)), ity::MULTI) => {
                let n = mu.count.max(1);
                let pos = multi_index(mu, &cur).unwrap_or(0) as i32;
                let next = (pos + dir).rem_euclid(n) as usize;
                if mu.str_def != 0 {
                    host.set_dvar(
                        &dv,
                        mu.dvar_str
                            .get(next)
                            .and_then(|s| s.as_deref())
                            .unwrap_or(""),
                    );
                } else {
                    host.set_dvar(
                        &dv,
                        &format!("{}", mu.dvar_value.get(next).copied().unwrap_or(0.0)),
                    );
                }
            }
            (ItemData::EditField(Some(e)), ity::SLIDER) => {
                let step = (e.max_val - e.min_val) / 20.0;
                let v = (cur.trim().parse::<f32>().unwrap_or(e.def_val) + dir as f32 * step)
                    .clamp(e.min_val, e.max_val);
                host.set_dvar(&dv, &format!("{v}"));
            }
            (ItemData::EnumDvarName(name), ity::DVARENUM) => {
                let list = host.dvar_enum(name.as_deref().unwrap_or(""));
                if !list.is_empty() {
                    let n = list.len() as i32;
                    let next = (enum_index(&list, &cur) as i32 + dir).rem_euclid(n);
                    host.set_dvar(&dv, &next.to_string());
                }
            }
            _ => {}
        }
        if let Some(s) = &d.action {
            self.run_script(host, Some(m), Some(i), s);
        }
    }

    fn slider_click(&mut self, host: &mut dyn Host, m: usize, i: usize) {
        use ::assets::zone::menu::ItemData;
        let def = self.menus[m].def.clone();
        let d = &def.items[i];
        let (Some(dv), ItemData::EditField(Some(e))) = (d.dvar.clone(), &d.data) else {
            return;
        };
        let it = &self.menus[m].items[i];
        let r = it.rect;
        let travel = self.place.rect(
            slider_bar_x(&r, d) + SLIDER_INSET,
            0.0,
            SLIDER_TRAVEL,
            0.0,
            r.horz_align,
            r.vert_align,
        );
        let v = slider_value_at(e, (self.cursor.0 - travel.x) / travel.w.max(1.0));
        host.set_dvar(&dv, &format!("{v}"));
    }

    // ---- list boxes ------------------------------------------------------------------------------------------

    fn list_rows_visible(&self, m: usize, i: usize) -> i32 {
        use ::assets::zone::menu::ItemData;
        let d = &self.menus[m].def.items[i];
        let (eh, horizontal) = match &d.data {
            ItemData::ListBox(Some(l)) => (l.element_height.max(1.0), l.element_style != 0),
            _ => (16.0, false),
        };
        if horizontal {
            return 1;
        }
        (self.menus[m].items[i].rect.h / eh).floor().max(1.0) as i32
    }

    fn list_step(&mut self, host: &mut dyn Host, m: usize, i: usize, dir: i32) {
        let feeder = self.menus[m].def.items[i].special as i32;
        let n = host.feeder_count(feeder) as i32;
        if n == 0 {
            return;
        }
        let vis = self.list_rows_visible(m, i);
        let it = &mut self.menus[m].items[i];
        it.list_cursor = (it.list_cursor + dir).clamp(0, n - 1);
        if it.list_cursor < it.list_start {
            it.list_start = it.list_cursor;
        } else if it.list_cursor >= it.list_start + vis {
            it.list_start = it.list_cursor - vis + 1;
        }
        let row = it.list_cursor as usize;
        host.feeder_select(feeder, row);
    }

    /// Picks `row` in every open list that shows `feeder`, scrolling it into view, and tells the host (the original's
    /// `Menu_SetFeederSelection`).
    pub fn select_feeder_row(&mut self, host: &mut dyn Host, feeder: i32, row: usize) {
        for m in self.stack.clone() {
            let def = self.menus[m].def.clone();
            for (i, d) in def.items.iter().enumerate() {
                if d.ty != ity::LISTBOX || d.special as i32 != feeder {
                    continue;
                }
                let vis = self.list_rows_visible(m, i);
                let it = &mut self.menus[m].items[i];
                it.list_cursor = row as i32;
                if it.list_cursor < it.list_start {
                    it.list_start = it.list_cursor;
                } else if it.list_cursor >= it.list_start + vis {
                    it.list_start = it.list_cursor - vis + 1;
                }
            }
        }
        host.feeder_select(feeder, row);
    }

    /// Moves the highlight of every open list that shows `feeder` to `row` (-1: none) without telling the host: for a
    /// list whose selection the host keeps itself and whose rows move under it.
    pub fn sync_feeder_cursor(&mut self, feeder: i32, row: i32) {
        for m in self.stack.clone() {
            let def = self.menus[m].def.clone();
            for (i, d) in def.items.iter().enumerate() {
                if d.ty == ity::LISTBOX && d.special as i32 == feeder {
                    self.menus[m].items[i].list_cursor = row;
                }
            }
        }
    }

    fn list_scroll(&mut self, host: &mut dyn Host, m: usize, i: usize, dir: i32) {
        let feeder = self.menus[m].def.items[i].special as i32;
        let n = host.feeder_count(feeder) as i32;
        let vis = self.list_rows_visible(m, i);
        let it = &mut self.menus[m].items[i];
        it.list_start = (it.list_start + dir * 3).clamp(0, (n - vis).max(0));
    }

    fn list_click(&mut self, host: &mut dyn Host, m: usize, i: usize) {
        use ::assets::zone::menu::ItemData;
        let def = self.menus[m].def.clone();
        let d = &def.items[i];
        let feeder = d.special as i32;
        let n = host.feeder_count(feeder) as i32;
        let eh = match &d.data {
            ItemData::ListBox(Some(l)) => l.element_height.max(1.0),
            _ => 16.0,
        };
        let p = self.item_pixels(m, i);
        let row_h = eh * self.place.scale.1;
        let rel = ((self.cursor.1 - p.y) / row_h).floor() as i32;
        let row = self.menus[m].items[i].list_start + rel;
        if rel >= 0 && row < n {
            self.menus[m].items[i].list_cursor = row;
            host.feeder_select(feeder, row as usize);
        }
    }

    /// The visible item of the top menu whose name or text is `want` (case-insensitive, localized), or `#id` for its
    /// owner-draw id; `a|b` takes the
    /// first of the alternatives that has one.
    fn find_item(&mut self, host: &mut dyn Host, want: &str) -> Option<(usize, usize)> {
        want.split('|').find_map(|w| self.find_one_item(host, w))
    }

    fn find_one_item(&mut self, host: &mut dyn Host, want: &str) -> Option<(usize, usize)> {
        let &m = self.stack.last()?;
        let def = self.menus[m].def.clone();
        for (i, d) in def.items.iter().enumerate() {
            // `#245` names an item by its owner-draw id, for those that have no name or text of their own.
            let named = d
                .window
                .name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(want))
                || want
                    .strip_prefix('#')
                    .and_then(|id| id.parse::<i32>().ok())
                    .is_some_and(|id| id == d.window.owner_draw);
            let texted = !named && {
                let t = self.item_text(&*host, m, i, d);
                !t.is_empty() && t.eq_ignore_ascii_case(want)
            };
            if (named || texted) && self.item_visible(host, m, i) {
                return Some((m, i));
            }
        }
        None
    }

    /// Whether the top menu shows an item named or labelled `want`.
    pub fn has_item(&mut self, host: &mut dyn Host, want: &str) -> bool {
        self.find_item(host, want).is_some()
    }

    /// Activates the visible item of the top menu whose name or text is `want`: what a click on it does. `true` if
    /// there was one.
    pub fn click(&mut self, host: &mut dyn Host, want: &str) -> bool {
        let Some((m, i)) = self.find_item(host, want) else {
            return false;
        };
        self.set_focus(host, m, i);
        self.activate(host, m, i, false);
        true
    }

    /// Clicks the item as a mouse would: the pointer moves onto its centre, then the button goes down. A locked or
    /// hidden item takes the click without effect. `true` if there was such an item.
    pub fn mouse_click(&mut self, host: &mut dyn Host, want: &str) -> bool {
        let Some((m, i)) = self.find_item(host, want) else {
            return false;
        };
        // A static text item answers the pointer over its text only.
        let p = self
            .text_rect_px(&*host, m, i)
            .filter(|_| self.menus[m].def.items[i].ty == ity::TEXT)
            .unwrap_or_else(|| self.item_pixels(m, i));
        self.mouse_move(host, p.x + p.w / 2.0, p.y + p.h / 2.0);
        self.key(host, UiKey::Mouse1);
        true
    }

    // ---- accessors for painting ---------------------------------------------------------------------------------
}

#[cfg(test)]
mod tests {
    use super::*;
    use server::content::Install;

    /// A host with no match behind it: menus open and close, nothing answers.
    #[derive(Default)]
    struct Dummy {
        /// `(key, command)` binds.
        binds: Vec<(String, String)>,
        /// What the menus sent the server (`scriptMenuResponse`).
        responses: Vec<(String, String)>,
        /// The UI sounds played.
        played: Vec<String>,
    }
    impl env::World for Dummy {
        fn key_bindings(&self, c: &str) -> Vec<String> {
            self.binds
                .iter()
                .filter(|(_, cmd)| cmd == c)
                .map(|(k, _)| k.clone())
                .collect()
        }
    }
    impl Host for Dummy {
        fn dvar(&self, _: &str) -> String {
            String::new()
        }
        fn set_dvar(&mut self, _: &str, _: &str) {}
        fn exec(&mut self, _: &Ui, _: &str) {}
        fn play(&mut self, a: &str) {
            self.played.push(a.into());
        }
        fn menu_response(&mut self, menu: &str, response: &str) {
            self.responses.push((menu.into(), response.into()));
        }
        fn set_bind(&mut self, key: &str, command: &str) {
            self.binds.retain(|(k, _)| k != key);
            if !command.is_empty() {
                self.binds.push((key.into(), command.into()));
            }
        }
        fn ui_script(&mut self, _: &mut Ui, _: &str, _: &[String]) -> bool {
            false
        }
        fn in_game(&self) -> bool {
            true
        }
        fn time_ms(&self) -> i32 {
            0
        }
        fn feeder_count(&mut self, _: i32) -> usize {
            0
        }
        fn feeder_text(&mut self, _: i32, _: usize, _: usize) -> String {
            String::new()
        }
        fn feeder_select(&mut self, _: i32, _: usize) {}
        fn feeder_image(&mut self, _: i32, _: usize, _: usize) -> String {
            String::new()
        }
        fn owner_key(&mut self, _: &Ui, _: i32, _: &UiKey) -> bool {
            false
        }
        fn owner_draw(
            &mut self,
            _: &Ui,
            _: &mut paint::Painter,
            _: &ItemDef,
            _: place::Px,
            _: [f32; 4],
            _: &str,
        ) {
        }
    }

    /// The pointer of the Escape menu is drawn even after the server opened a menu without a mouse (the class
    /// overlay and the like), and moving it focuses what is under it.
    #[test]
    fn a_menu_opened_after_a_no_mouse_menu_still_has_a_cursor() {
        let root =
            std::env::var_os("COD4_PATH").map_or_else(|| "COD4".into(), std::path::PathBuf::from);
        let Some(install) = Install::open(&root)
            .ok()
            .filter(|i| i.zone_path("ui_mp").is_some())
        else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let assets = assets::UiAssets::load(&install).expect("ui assets");
        let mut ui = Ui::new(assets, (1280, 720));
        let mut host = Dummy::default();
        ui.open_by_name(&mut host, "team_marinesopfor");
        // What `openmenunomouse` does after the open.
        ui.cursor_visible = false;
        ui.open_by_name(&mut host, "popup_leavegame");
        assert!(ui.cursor_visible, "the next menu opened without a pointer");
        assert!(ui.captures_input());
    }

    fn multi(count: i32, str_def: i32) -> ::assets::zone::menu::MultiDef {
        let name = |s: &str| Some(Arc::<str>::from(s));
        ::assets::zone::menu::MultiDef {
            dvar_list: vec![name("a"), name("b"), name("c")],
            dvar_str: vec![name("auto"), name("standard"), name("wide")],
            dvar_value: vec![1.0, 2.0, 4.0],
            count,
            str_def,
        }
    }

    #[test]
    fn a_multi_item_finds_its_entry_by_number_or_by_string() {
        assert_eq!(multi_index(&multi(3, 0), "2"), Some(1));
        assert_eq!(multi_index(&multi(3, 0), "4.0"), Some(2));
        assert_eq!(multi_index(&multi(3, 0), "3"), None, "no entry holds 3");
        assert_eq!(
            multi_index(&multi(3, 0), ""),
            None,
            "an unset dvar matches nothing"
        );
        assert_eq!(multi_index(&multi(3, 1), "WIDE"), Some(2));
        assert_eq!(
            multi_index(&multi(2, 1), "wide"),
            None,
            "only `count` entries count"
        );
    }

    #[test]
    fn a_dvar_enum_item_takes_an_index_or_a_string() {
        let list: Vec<String> = ["640x480", "1280x720", "1920x1080"]
            .map(String::from)
            .into();
        assert_eq!(enum_index(&list, "2"), 2);
        assert_eq!(enum_index(&list, "1280x720"), 1);
        assert_eq!(
            enum_index(&list, "7"),
            0,
            "out of range falls back to the first"
        );
        assert_eq!(enum_index(&list, ""), 0);
        assert_eq!(enum_index(&[], "3"), 0);
    }

    #[test]
    fn the_slider_thumb_follows_the_value_over_the_bar() {
        let e = ::assets::zone::menu::EditFieldDef {
            min_val: 0.5,
            max_val: 3.0,
            def_val: 1.0,
            range: 0.0,
            max_chars: 0,
            max_chars_goto_next: 0,
            max_paint_chars: 0,
            paint_offset: 0,
        };
        let bar = 100.0;
        assert_eq!(
            slider_thumb_x(bar, &e, 0.5),
            106.0,
            "minimum: 6 in from the bar's left"
        );
        assert_eq!(slider_thumb_x(bar, &e, 3.0), 190.0, "maximum: 84 further");
        assert_eq!(slider_thumb_x(bar, &e, 99.0), 190.0, "clamped");
        assert!(
            (slider_thumb_x(bar, &e, 1.75) - 148.0).abs() < 1e-3,
            "halfway"
        );
        assert_eq!(slider_value_at(&e, 0.5), 1.75);
        assert_eq!(slider_value_at(&e, -1.0), 0.5);
        assert_eq!(slider_value_at(&e, 9.0), 3.0);
    }

    /// A bind item of a stock controls menu shows the keys of its command and rebinds on the next key.
    #[test]
    fn a_bind_item_shows_its_keys_and_takes_the_next_key() {
        let root =
            std::env::var_os("COD4_PATH").map_or_else(|| "COD4".into(), std::path::PathBuf::from);
        let Some(install) = Install::open(&root)
            .ok()
            .filter(|i| i.zone_path("ui_mp").is_some())
        else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let mut ui = Ui::new(
            assets::UiAssets::load(&install).expect("ui assets"),
            (1280, 720),
        );
        let mut host = Dummy::default();
        let m = ui.menu_index("options_look").unwrap();
        let def = ui.menus[m].def.clone();
        let i = def
            .items
            .iter()
            .position(|d| d.ty == ity::BIND && d.dvar.as_deref() == Some("+leanleft"))
            .expect("the lean-left bind item");
        assert_eq!(
            ui.bind_label(&host, "+leanleft"),
            ui.localize_key("KEY_UNBOUND")
        );
        host.set_bind("Q", "+leanleft");
        assert_eq!(ui.bind_label(&host, "+leanleft"), "Q");
        ui.activate(&mut host, m, i, true);
        assert!(ui.bind_pending());
        assert!(ui.bind_capture(&mut host, "j"));
        assert_eq!(
            host.key_bindings("+leanleft"),
            ["Q", "j"],
            "a command holds two keys"
        );
        assert!(!ui.bind_pending());
        assert!(
            ui.bind_label(&host, "+leanleft")
                .contains(&ui.localize_key("KEY_OR"))
        );
        // A third key starts over; Backspace clears.
        ui.activate(&mut host, m, i, true);
        ui.bind_capture(&mut host, "l");
        assert_eq!(host.key_bindings("+leanleft"), ["l"]);
        ui.activate(&mut host, m, i, true);
        ui.bind_capture(&mut host, "backspace");
        assert!(host.key_bindings("+leanleft").is_empty());
    }

    /// Escape in a menu of a match always leads back to the game: the menu closes, or the script menus that handle
    /// it themselves (`onESC` answering `back`) tell the server, which closes them.
    #[test]
    fn escape_leaves_every_menu_a_match_opens() {
        let root =
            std::env::var_os("COD4_PATH").map_or_else(|| "COD4".into(), std::path::PathBuf::from);
        let Some(install) = Install::open(&root)
            .ok()
            .filter(|i| i.zone_path("ui_mp").is_some())
        else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let assets = assets::UiAssets::load(&install).expect("ui assets");
        let mut ui = Ui::new(assets, (1280, 720));
        for name in [
            "class_marines",
            "class_opfor",
            "class",
            "changeclass",
            "team_marinesopfor",
            "main_controls",
            "ingame_controls",
            "main_options",
            "options_graphics",
            "callvote",
            "muteplayer",
            "popup_leavegame",
            "popup_endgame",
        ] {
            let mut host = Dummy::default();
            ui.close_all(&mut host);
            ui.open_by_name(&mut host, name);
            assert!(ui.captures_input(), "{name} opened nothing");
            // A sub-menu may step back to its parent (`callvote` to `class`) first.
            for _ in 0..3 {
                if ui.captures_input() && !host.responses.iter().any(|(_, r)| r == "back") {
                    ui.key(&mut host, UiKey::Escape);
                }
            }
            let asked_back = host.responses.iter().any(|(_, r)| r == "back");
            assert!(
                !ui.captures_input() || asked_back,
                "Escape in {name} left {:?} open",
                ui.open_menus()
            );
        }
    }

    fn stock_ui() -> Option<Ui> {
        let root =
            std::env::var_os("COD4_PATH").map_or_else(|| "COD4".into(), std::path::PathBuf::from);
        let install = Install::open(&root)
            .ok()
            .filter(|i| i.zone_path("ui_mp").is_some())?;
        Some(Ui::new(
            assets::UiAssets::load(&install).expect("ui assets"),
            (1280, 720),
        ))
    }

    /// A number key in the quick-chat menu runs the menu's `execKey` handler: it answers the server and closes.
    #[test]
    fn a_number_key_runs_the_menus_exec_key_handler() {
        let Some(mut ui) = stock_ui() else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let mut host = Dummy::default();
        ui.open_by_name(&mut host, "quickcommands");
        assert!(ui.key(&mut host, UiKey::Char('2')));
        assert_eq!(host.responses, [("quickcommands".into(), "2".into())]);
        assert!(!ui.is_open("quickcommands"));
    }

    /// Hovering an item runs `mouseEnter` once; leaving it clears the hover.
    #[test]
    fn hovering_an_item_runs_its_mouse_enter_script_once() {
        let Some(mut ui) = stock_ui() else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let mut host = Dummy::default();
        ui.open_by_name(&mut host, "createserver");
        let m = ui.menu_index("createserver").unwrap();
        let i = ui.menus[m]
            .def
            .items
            .iter()
            .position(|d| d.window.name.as_deref() == Some("back"))
            .expect("the back button");
        host.played.clear();
        let p = ui.item_pixels(m, i);
        let (x, y) = (p.x + p.w / 2.0, p.y + p.h / 2.0);
        ui.mouse_move(&mut host, x, y);
        ui.mouse_move(&mut host, x + 1.0, y);
        assert_eq!(host.played, ["mouse_over"], "once, not on every move");
        let over = |ui: &Ui| {
            ui.menus[m]
                .items
                .iter()
                .any(|it| it.dyn_flags & dynf::MOUSEOVER != 0)
        };
        assert!(over(&ui));
        ui.mouse_move(&mut host, x, p.y + p.h + 30.0);
        assert!(!over(&ui));
    }

    /// A server menu that comes while a non-script menu has focus waits, answers the one that waited before with
    /// `noop`, and opens when the blocker closes; an unknown menu is answered `bad`.
    #[test]
    fn a_script_menu_waits_for_the_menu_in_the_way() {
        let Some(mut ui) = stock_ui() else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let mut host = Dummy::default();
        ui.open_by_name(&mut host, "popup_leavegame");
        ui.open_script_menu(&mut host, "quickcommands", true);
        assert!(!ui.is_open("quickcommands"));
        ui.open_script_menu(&mut host, "quickcommands", true);
        assert!(
            host.responses.is_empty(),
            "the same menu again changes nothing"
        );
        ui.open_script_menu(&mut host, "team_marinesopfor", true);
        assert_eq!(host.responses, [("quickcommands".into(), "noop".into())]);
        ui.open_script_menu(&mut host, "no_such_menu", true);
        assert_eq!(host.responses[1], ("no_such_menu".into(), "bad".into()));
        ui.close_all(&mut host);
        ui.check_waiting_menu(&mut host);
        assert!(ui.is_open("team_marinesopfor"));
    }

    /// `scriptMenuResponse` is silent while menus close because the level changed.
    #[test]
    fn menu_responses_are_suppressed_when_not_allowed() {
        let Some(mut ui) = stock_ui() else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let mut host = Dummy::default();
        ui.open_by_name(&mut host, "quickcommands");
        ui.allow_menu_response = false;
        ui.key(&mut host, UiKey::Char('1'));
        assert!(host.responses.is_empty());
    }

    /// The center of item `i` of menu `m`, in pixels.
    fn center(ui: &Ui, m: usize, i: usize) -> (f32, f32) {
        let p = ui.item_pixels(m, i);
        (p.x + p.w / 2.0, p.y + p.h / 2.0)
    }

    /// Typing in an edit field goes on after the pointer leaves it, and Escape ends the edit without closing the menu.
    #[test]
    fn an_edit_field_keeps_the_keys_when_the_pointer_leaves() {
        let Some(mut ui) = stock_ui() else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let mut host = Dummy::default();
        ui.open_by_name(&mut host, "createserver");
        let m = ui.menu_index("createserver").unwrap();
        let i = (0..ui.menus[m].items.len())
            .find(|&i| {
                ui.menus[m].def.items[i].ty == ity::EDITFIELD && ui.item_visible(&mut host, m, i)
            })
            .expect("a visible edit field");
        let (x, y) = center(&ui, m, i);
        ui.mouse_move(&mut host, x, y);
        assert!(
            ui.menus[m].items[i].editing,
            "hovering the field starts the edit"
        );
        ui.mouse_move(&mut host, 2.0, 2.0);
        assert!(ui.key(&mut host, UiKey::Char('x')));
        assert_eq!(ui.menus[m].items[i].edit, "x");
        ui.key(&mut host, UiKey::Escape);
        assert!(!ui.menus[m].items[i].editing);
        assert!(ui.is_open("createserver"), "Escape only ended the edit");
    }

    /// A click outside a plain menu that asks for it closes it; one inside does not.
    #[test]
    fn a_click_outside_a_menu_closes_it_when_it_asks() {
        let Some(mut ui) = stock_ui() else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let Some(m) = (0..ui.menus.len()).find(|&m| {
            let d = &ui.menus[m].def;
            d.window.static_flags & statf::OUT_OF_BOUNDS_CLICK != 0
                && d.window.static_flags & statf::POPUP == 0
                && d.full_screen == 0
        }) else {
            eprintln!("no such stock menu; skipping");
            return;
        };
        let mut host = Dummy::default();
        let name = ui.menus[m].name.clone();
        ui.open_by_name(&mut host, &name);
        let r = ui.menus[m].rect;
        let inside = ui
            .place
            .rect(r.x, r.y, r.w, r.h, r.horz_align, r.vert_align);
        ui.cursor = (inside.x + inside.w / 2.0, inside.y + inside.h / 2.0);
        ui.key(&mut host, UiKey::Mouse1);
        ui.open_by_name(&mut host, &name);
        ui.cursor = (-5.0, -5.0);
        ui.key(&mut host, UiKey::Mouse1);
        assert!(
            !ui.is_open(&name),
            "{name} stayed open after an outside click"
        );
    }

    /// Closing a menu with the pointer on an item runs the item's exit (clears the hover).
    #[test]
    fn closing_a_menu_ends_the_hover() {
        let Some(mut ui) = stock_ui() else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let mut host = Dummy::default();
        ui.open_by_name(&mut host, "createserver");
        let m = ui.menu_index("createserver").unwrap();
        let i = ui.menus[m]
            .def
            .items
            .iter()
            .position(|d| d.window.name.as_deref() == Some("back"))
            .unwrap();
        let (x, y) = center(&ui, m, i);
        ui.mouse_move(&mut host, x, y);
        let over = |ui: &Ui| {
            ui.menus[m]
                .items
                .iter()
                .any(|it| it.dyn_flags & dynf::MOUSEOVER != 0)
        };
        assert!(over(&ui));
        ui.close(&mut host, m);
        assert!(!over(&ui));
    }

    /// The Escape menu of a match is closed by the server's `closeingamemenu`; a menu of another kind is not.
    #[test]
    fn close_ingame_menu_only_closes_the_ingame_menu() {
        let Some(mut ui) = stock_ui() else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let mut host = Dummy::default();
        ui.open_as(&mut host, "popup_leavegame", MenuKind::Other);
        ui.close_ingame_menu(&mut host);
        assert!(ui.is_open("popup_leavegame"));
        ui.open_as(&mut host, "popup_leavegame", MenuKind::Ingame);
        ui.close_ingame_menu(&mut host);
        assert!(!ui.captures_input());
    }

    /// Hovering another item after a field ends the edit: Escape then leaves the menu instead of vanishing into the
    /// field.
    #[test]
    fn hovering_another_item_ends_the_edit() {
        let Some(mut ui) = stock_ui() else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let mut host = Dummy::default();
        ui.open_by_name(&mut host, "createserver");
        let m = ui.menu_index("createserver").unwrap();
        let field = (0..ui.menus[m].items.len())
            .find(|&i| {
                ui.menus[m].def.items[i].ty == ity::EDITFIELD && ui.item_visible(&mut host, m, i)
            })
            .expect("a visible edit field");
        let (x, y) = center(&ui, m, field);
        ui.mouse_move(&mut host, x, y);
        assert!(ui.menus[m].items[field].editing);
        let other = (0..ui.menus[m].items.len())
            .find(|&i| {
                let (x, y) = center(&ui, m, i);
                i != field
                    && ui.menus[m].def.items[i].ty == ity::BUTTON
                    && ui.item_visible(&mut host, m, i)
                    && !ui.item_contains(m, field, x, y)
            })
            .expect("a button elsewhere");
        let (x, y) = center(&ui, m, other);
        ui.mouse_move(&mut host, x, y);
        assert!(ui.menus[m].items.iter().all(|it| !it.editing));
        ui.key(&mut host, UiKey::Escape);
        assert!(!ui.is_open("createserver"), "Escape was eaten by the field");
    }
}
