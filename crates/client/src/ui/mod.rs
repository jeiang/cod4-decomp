// SPDX-License-Identifier: GPL-3.0-or-later
//! The stock menu system: menu and item state, the open-menu stack, focus, the action language and input.
//!
//! Menus are the decoded `menuDef` assets (immutable, shared); [`Ui`] keeps the per-menu and per-item runtime state
//! (dynamic flags, colours, focus, list cursors). Everything the menus need from the rest of the client (dvars, the
//! console, the server, sound, the game state) comes through [`Host`].

pub mod assets;
pub mod env;
pub mod expr;
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

/// Window dynamic flags.
pub mod dynf {
    pub const HASFOCUS: u32 = 0x2;
    pub const VISIBLE: u32 = 0x4;
    pub const FADINGOUT: u32 = 0x10;
    pub const FADINGIN: u32 = 0x20;
    pub const FORECOLOR_SET: u32 = 0x10000;
}

/// Window static flags.
pub mod statf {
    pub const DECORATION: u32 = 0x10_0000;
    pub const AUTOWRAPPED: u32 = 0x80_0000;
    pub const POPUP: u32 = 0x100_0000;
    pub const OUT_OF_BOUNDS_CLICK: u32 = 0x200_0000;
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
}

impl Ui {
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
                if let Some(m) = menu {
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
                if hit && let Some(m) = menu {
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
                    eprintln!("ui: unknown uiScript '{n}'");
                }
            }
            other => eprintln!("ui: unknown menu command '{other}'"),
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
        if had_focus && let Some(&top) = self.stack.last() {
            self.menus[top].dyn_flags |= dynf::HASFOCUS;
            self.gain_focus(host, top);
        }
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
        self.menus[m].cursor = Some(i);
        self.menus[m].items[i].dyn_flags |= dynf::HASFOCUS;
        let d = self.menus[m].def.clone();
        if let Some(s) = &d.items[i].on_focus {
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

    /// The mouse moved to pixel `(x, y)`: focus follows the item under it in the top menu.
    pub fn mouse_move(&mut self, host: &mut dyn Host, x: f32, y: f32) {
        self.cursor = (x, y);
        let Some(&m) = self.stack.last() else { return };
        for i in (0..self.menus[m].items.len()).rev() {
            if self.item_contains(m, i, x, y) && self.selectable(host, m, i) {
                self.set_focus(host, m, i);
                return;
            }
        }
    }

    fn item_pixels(&self, m: usize, i: usize) -> place::Px {
        let it = &self.menus[m].items[i];
        let r = &it.rect;
        self.place
            .rect(r.x, r.y, r.w, r.h, r.horz_align, r.vert_align)
    }

    fn item_contains(&self, m: usize, i: usize, x: f32, y: f32) -> bool {
        let p = self.item_pixels(m, i);
        x >= p.x && x < p.x + p.w && y >= p.y && y < p.y + p.h
    }

    /// Handles a key press. Returns whether the menus used it.
    pub fn key(&mut self, host: &mut dyn Host, key: UiKey) -> bool {
        let Some(&m) = self.stack.last() else {
            return false;
        };
        let def = self.menus[m].def.clone();
        // Key handlers of the menu (`onKey`), keyed by the original's key numbers: only Esc matters here.
        if key == UiKey::Escape {
            if let Some(i) = self.menus[m].cursor
                && self.menus[m].items[i].editing
            {
                self.menus[m].items[i].editing = false;
            }
            match def.on_esc.as_deref() {
                Some(s) if !s.trim().is_empty() && s.trim() != ";" => {
                    self.run_script(host, Some(m), None, s)
                }
                _ => {
                    if def.window.static_flags & statf::POPUP != 0 || def.on_esc.is_some() {
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
        // Edit fields eat characters.
        if self.menus[m].items[i].editing {
            match &key {
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
                    return true;
                }
                UiKey::Backspace => {
                    self.menus[m].items[i].edit.pop();
                    self.commit_edit(host, m, i);
                    return true;
                }
                UiKey::Enter => {
                    self.menus[m].items[i].editing = false;
                    self.commit_edit(host, m, i);
                    if let Some(s) = &d.on_accept {
                        self.run_script(host, Some(m), Some(i), s);
                    }
                    self.move_focus(host, m, 1);
                    return true;
                }
                _ => {}
            }
        }
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
                self.activate(host, m, i, key == UiKey::Mouse1);
                true
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
            ity::BIND => {}
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
                let pos = (0..n as usize)
                    .find(|&k| {
                        if mu.str_def != 0 {
                            mu.dvar_str
                                .get(k)
                                .and_then(|s| s.as_deref())
                                .is_some_and(|s| s.eq_ignore_ascii_case(&cur))
                        } else {
                            mu.dvar_value.get(k).is_some_and(|v| {
                                (cur.trim().parse::<f32>().unwrap_or(f32::NAN) - v).abs() < 1e-4
                            })
                        }
                    })
                    .unwrap_or(0) as i32;
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
            (ItemData::EnumDvarName(list), ity::DVARENUM) => {
                let _ = list;
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
        let p = self.item_pixels(m, i);
        // The slider bar is the right part of the item; its width follows the text offset like the original.
        let bar_x = p.x + p.w * 0.5;
        let t = ((self.cursor.0 - bar_x) / (p.w * 0.5).max(1.0)).clamp(0.0, 1.0);
        let v = e.min_val + t * (e.max_val - e.min_val);
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

    /// Activates the visible item of the top menu whose name or text is `want` (case-insensitive, localized):
    /// what a click on it does. `true` if there was one.
    pub fn click(&mut self, host: &mut dyn Host, want: &str) -> bool {
        let Some(&m) = self.stack.last() else {
            return false;
        };
        let def = self.menus[m].def.clone();
        for (i, d) in def.items.iter().enumerate() {
            let named = d
                .window
                .name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(want));
            let texted = !named && {
                let t = self.item_text(&*host, m, i, d);
                !t.is_empty() && t.eq_ignore_ascii_case(want)
            };
            if (named || texted) && self.item_visible(host, m, i) {
                self.set_focus(host, m, i);
                self.activate(host, m, i, false);
                return true;
            }
        }
        false
    }

    // ---- accessors for painting ---------------------------------------------------------------------------------
}
