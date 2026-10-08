// SPDX-License-Identifier: GPL-3.0-only
//! Drawing menus: windows, borders, items and their text onto the 2D layer.

use super::assets::UiAssets;
use super::place::Px;
use super::{
    Host, SLIDER_H, SLIDER_W, Ui, dynf, enum_index, ity, multi_index, slider_bar_x, slider_thumb_x,
    statf,
};
use ::assets::zone::gfx::Material;
use ::assets::zone::menu::{ItemData, ItemDef, Rect};
use ::assets::zone::text::Font;
use render::TextureCache;
use render::ui2d::{self, TextStyle, Ui2d, UiImage};
use std::collections::HashMap;
use std::sync::Arc;

/// The 2D layer, the texture cache behind it and the images resolved so far.
pub struct Painter<'a> {
    pub g: &'a mut Ui2d,
    pub cache: &'a mut TextureCache,
    pub images: &'a mut HashMap<String, UiImage>,
}

impl Painter<'_> {
    /// The image of a material given by name (stock UI images without a material asset load as plain images).
    pub fn named(&mut self, assets: &UiAssets, name: &str) -> UiImage {
        let key = name.trim_start_matches(',').to_ascii_lowercase();
        if let Some(i) = self.images.get(&key) {
            return i.clone();
        }
        let img = match assets.material(&key).filter(|m| !m.textures.is_empty()) {
            Some(m) => self.g.image(self.cache, m),
            None => self.g.image_named(self.cache, &key),
        };
        self.images.insert(key, img.clone());
        img
    }

    /// The image of a decoded material (menus carry their own copy).
    pub fn material(&mut self, assets: &UiAssets, m: &Material) -> UiImage {
        match m.name.as_deref() {
            Some(n) => self.named(assets, n),
            None => self.g.white(),
        }
    }

    pub fn fill(&mut self, r: Px, color: [f32; 4]) {
        let w = self.g.white();
        self.g
            .quad(&w, [r.x, r.y, r.w, r.h], [0.0, 0.0, 1.0, 1.0], color);
    }

    pub fn pic(&mut self, img: &UiImage, r: Px, color: [f32; 4]) {
        self.g
            .quad(img, [r.x, r.y, r.w, r.h], [0.0, 0.0, 1.0, 1.0], color);
    }
}

/// Fonts by the menu font number (`font_enum`) and the size the text will have in pixels per virtual unit.
pub fn pick_font(assets: &UiAssets, font_enum: i32, scale: f32, unit: f32) -> Option<&Arc<Font>> {
    // `ui_smallFont`, `ui_bigFont` and `ui_extraBigFont` default to these.
    const SMALL: f32 = 0.25;
    const BIG: f32 = 0.4;
    const EXTRA: f32 = 0.55;
    let name = match font_enum {
        2 => "fonts/bigfont",
        3 => "fonts/smallfont",
        5 => "fonts/consolefont",
        6 => "fonts/objectivefont",
        _ => {
            let s = unit * scale;
            if font_enum == 4 {
                if s <= SMALL {
                    "fonts/smallfont"
                } else if s < BIG {
                    "fonts/normalfont"
                } else {
                    "fonts/boldfont"
                }
            } else if s <= SMALL {
                "fonts/smallfont"
            } else if s < EXTRA {
                if s < BIG {
                    "fonts/normalfont"
                } else {
                    "fonts/bigfont"
                }
            } else {
                "fonts/extrabigfont"
            }
        }
    };
    assets.fonts.get(name)
}

fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}

/// `^x` colour escapes make text width and drawing differ from `chars().count()`; text goes through this to draw.
pub struct TextDraw<'a> {
    pub text: &'a str,
    pub font_enum: i32,
    pub scale: f32,
    pub style: i32,
    pub color: [f32; 4],
    /// Origin in virtual units and its alignments; `y` is the baseline.
    pub x: f32,
    pub y: f32,
    pub horz: i32,
    pub vert: i32,
}

impl Ui {
    /// Text width in virtual units, truncated like `UI_TextWidth`.
    pub fn text_width(&self, text: &str, font_enum: i32, scale: f32) -> f32 {
        match pick_font(&self.assets, font_enum, scale, self.place.unit()) {
            Some(f) => ui2d::text_width(f, text, 0, scale).trunc(),
            None => 0.0,
        }
    }

    pub fn text_height(&self, font_enum: i32, scale: f32) -> f32 {
        match pick_font(&self.assets, font_enum, scale, self.place.unit()) {
            Some(f) => ui2d::text_height(f, scale).trunc(),
            None => 0.0,
        }
    }

    pub fn draw_text(&self, p: &mut Painter, t: &TextDraw) {
        self.draw_text_fx(p, t, None, 0);
    }

    /// Like [`Ui::draw_text`], with a glow colour and a limit on the letters drawn (0: all), as hud elements use.
    pub fn draw_text_fx(
        &self,
        p: &mut Painter,
        t: &TextDraw,
        glow: Option<[f32; 4]>,
        max_chars: usize,
    ) {
        let Some(font) = pick_font(&self.assets, t.font_enum, t.scale, self.place.unit()) else {
            return;
        };
        let fimg = p.material_of_font(&self.assets, font, false);
        let gimg = p.material_of_font(&self.assets, font, true);
        let (x, y) = (
            self.place.x(t.x, t.horz).round(),
            self.place.y(t.y, t.vert).round(),
        );
        let scale = t.scale * self.place.scale.0;
        let style = TextStyle::from_menu_style(t.style, glow);
        p.g.draw_text(
            font,
            &fimg,
            Some(&gimg),
            t.text,
            x,
            y,
            scale,
            t.color,
            style,
            max_chars,
        );
    }
}

impl Painter<'_> {
    fn material_of_font(&mut self, assets: &UiAssets, font: &Font, glow: bool) -> UiImage {
        let m = if glow {
            &font.glow_material
        } else {
            &font.material
        };
        match m {
            Some(m) => match assets.material(m.name.as_deref().unwrap_or("")) {
                Some(full) if !full.textures.is_empty() => self.material(assets, full),
                _ => self.material(assets, m),
            },
            None => self.g.white(),
        }
    }
}

impl Ui {
    /// Paints the HUD menus (when `in_game`), then the open menus bottom to top.
    pub fn paint(&mut self, host: &mut dyn Host, p: &mut Painter, in_game: bool) {
        self.now_ms = host.time_ms();
        if in_game {
            for m in self.hud.clone() {
                if !self.stack.contains(&m) {
                    self.paint_menu(host, p, m);
                }
            }
        }
        for m in self.stack.clone() {
            self.paint_menu(host, p, m);
        }
        if self.cursor_visible && !self.stack.is_empty() {
            let (x, y) = self.cursor;
            // 32 virtual units square, centred on the pointer (what the original draws; the system cursor is hidden).
            let (w, h) = (self.place.scale.0 * 32.0, self.place.scale.1 * 32.0);
            let img = p.named(&self.assets, "ui_cursor");
            p.pic(
                &img,
                Px {
                    x: x - w * 0.5,
                    y: y - h * 0.5,
                    w,
                    h,
                },
                [1.0; 4],
            );
        }
    }

    fn paint_menu(&mut self, host: &mut dyn Host, p: &mut Painter, m: usize) {
        if !self.menu_visible(host, m) {
            return;
        }
        let def = self.menus[m].def.clone();
        if let Some(e) = &self.menus[m].rect_x {
            self.menus[m].rect.x = self.eval_float(&*host, e);
        }
        if let Some(e) = &self.menus[m].rect_y {
            self.menus[m].rect.y = self.eval_float(&*host, e);
        }
        let mr = self.menus[m].rect;
        if def.full_screen != 0
            && let Some(bg) = &def.window.background
        {
            let r = self
                .place
                .rect(0.0, 0.0, 640.0, 480.0, mr.horz_align, mr.vert_align);
            let img = p.material(&self.assets, bg);
            p.pic(&img, r, [1.0; 4]);
        }
        let (fore, back) = (def.window.fore_color, def.window.back_color);
        self.paint_window(p, &def.window, &mr, fore, back, def.window.dynamic_flags);
        for i in 0..self.menus[m].items.len() {
            self.paint_item(host, p, m, i);
        }
    }

    fn paint_window(
        &self,
        p: &mut Painter,
        w: &::assets::zone::menu::WindowDef,
        rect: &Rect,
        fore: [f32; 4],
        back: [f32; 4],
        dyn_flags: u32,
    ) {
        if w.style == 0 && w.border == 0 {
            return;
        }
        let mut fill = Rect { ..*rect };
        if w.border != 0 {
            fill.x += w.border_size;
            fill.y += w.border_size;
            fill.w -= w.border_size + 1.0;
            fill.h -= w.border_size + 1.0;
        }
        let px = self.place.rect(
            fill.x,
            fill.y,
            fill.w,
            fill.h,
            rect.horz_align,
            rect.vert_align,
        );
        let tint = if dyn_flags & dynf::FORECOLOR_SET != 0 {
            fore
        } else {
            [1.0; 4]
        };
        match w.style {
            1 => match &w.background {
                Some(bg) => {
                    let img = p.material(&self.assets, bg);
                    p.pic(&img, px, back);
                }
                None => p.fill(px, back),
            },
            3 | 5 => {
                if let Some(bg) = &w.background {
                    let img = p.material(&self.assets, bg);
                    p.pic(&img, px, tint);
                }
            }
            _ => {}
        }
        if w.border != 0 {
            let r = self.place.rect(
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                rect.horz_align,
                rect.vert_align,
            );
            let t = (w.border_size * self.place.unit()).max(1.0);
            let c = w.border_color;
            let side =
                |p: &mut Painter, x: f32, y: f32, w_: f32, h: f32| p.fill(Px { x, y, w: w_, h }, c);
            let (top_bottom, sides) = match w.border {
                1 | 5 | 6 => (true, true),
                2 => (true, false),
                3 => (false, true),
                _ => (false, false),
            };
            if top_bottom {
                side(p, r.x, r.y, r.w, t);
                side(p, r.x, r.y + r.h - t, r.w, t);
            }
            if sides {
                side(p, r.x, r.y, t, r.h);
                side(p, r.x + r.w - t, r.y, t, r.h);
            }
        }
    }

    pub(super) fn item_text(&self, host: &dyn Host, m: usize, i: usize, d: &ItemDef) -> String {
        let it = &self.menus[m].items[i];
        let raw = if let Some(t) = &d.text {
            t.to_string()
        } else if let Some(e) = &it.text {
            self.eval_string(host, e)
        } else if let Some(dv) = d.dvar.as_deref().filter(|s| !s.is_empty()) {
            host.dvar(dv)
        } else {
            return String::new();
        };
        match raw.strip_prefix('@') {
            Some(k) => self
                .assets
                .translate(k)
                .map_or(raw.clone(), |s| s.to_string()),
            None => raw,
        }
    }

    fn text_color(&self, m: usize, i: usize, d: &ItemDef, host: &dyn Host) -> [f32; 4] {
        let it = &self.menus[m].items[i];
        let menu = &self.menus[m].def;
        let mut c = it.fore;
        if it.dyn_flags & dynf::HASFOCUS != 0 {
            let f = menu.focus_color;
            let low = f.map(|v| v * 0.8);
            let t = ((self.now_ms / 75) as f32).sin() * 0.5 + 0.5;
            c = lerp4(f, low, t);
        } else if d.text_style == 1 && (self.now_ms / 256) & 1 == 0 {
            let low = it.fore.map(|v| v * 0.8);
            c = lerp4(it.fore, low, ((self.now_ms / 75) as f32).sin() * 0.5 + 0.5);
        }
        if d.enable_dvar.as_deref().is_some_and(|s| !s.is_empty())
            && d.dvar_test.as_deref().is_some_and(|s| !s.is_empty())
            && d.dvar_flags & 3 != 0
            && !self.enable_dvar_matches(host, d, 1 | 2)
        {
            let dc = menu.disable_color;
            c = [dc[0], dc[1], dc[2], dc[3]];
        }
        c
    }

    fn paint_item(&mut self, host: &mut dyn Host, p: &mut Painter, m: usize, i: usize) {
        let def = self.menus[m].def.clone();
        let d = &def.items[i];
        if let Some(e) = &self.menus[m].items[i].fore_a {
            let a = self.eval_float(&*host, e);
            self.menus[m].items[i].fore[3] = a;
        }
        if !self.item_visible(host, m, i) {
            return;
        }
        let mrect = self.menus[m].rect;
        {
            let (rx, ry, rw, rh) = {
                let it = &self.menus[m].items[i];
                (
                    it.rect_x
                        .as_ref()
                        .map(|e| self.eval_float(&*host, e) + mrect.x),
                    it.rect_y
                        .as_ref()
                        .map(|e| self.eval_float(&*host, e) + mrect.y),
                    it.rect_w.as_ref().map(|e| self.eval_float(&*host, e)),
                    it.rect_h.as_ref().map(|e| self.eval_float(&*host, e)),
                )
            };
            let r = &mut self.menus[m].items[i].rect;
            if let Some(v) = rx {
                r.x = v;
            }
            if let Some(v) = ry {
                r.y = v;
            }
            if let Some(v) = rw {
                r.w = v;
            }
            if let Some(v) = rh {
                r.h = v;
            }
        }
        // The material expression picks the image of a shader-style window.
        let mut win = ::assets::zone::menu::WindowDef {
            name: None,
            rect: def.items[i].window.rect,
            rect_client: def.items[i].window.rect_client,
            group: None,
            style: d.window.style,
            border: d.window.border,
            owner_draw: d.window.owner_draw,
            owner_draw_flags: d.window.owner_draw_flags,
            border_size: d.window.border_size,
            static_flags: d.window.static_flags,
            dynamic_flags: 0,
            next_time: 0,
            fore_color: d.window.fore_color,
            back_color: d.window.back_color,
            border_color: d.window.border_color,
            outline_color: d.window.outline_color,
            background: d.window.background.clone(),
        };
        let mat_name = self.menus[m].items[i]
            .material
            .as_ref()
            .map(|e| self.eval_string(&*host, e));
        if let Some(n) = mat_name.filter(|n| !n.is_empty()) {
            // A name, not a decoded material: resolve at draw time through a one-off window below.
            let rect = self.menus[m].items[i].rect;
            let px = self.place.rect(
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                rect.horz_align,
                rect.vert_align,
            );
            let tint = if self.menus[m].items[i].dyn_flags & dynf::FORECOLOR_SET != 0 {
                self.menus[m].items[i].fore
            } else {
                [1.0; 4]
            };
            let img = p.named(&self.assets, &n);
            p.pic(&img, px, tint);
            win.style = 0;
        }
        let rect = self.menus[m].items[i].rect;
        let (fore, flags) = (
            self.menus[m].items[i].fore,
            self.menus[m].items[i].dyn_flags,
        );
        self.paint_window(p, &win, &rect, fore, d.window.back_color, flags);
        match d.ty {
            ity::TEXT | ity::BUTTON => self.paint_item_text(host, p, m, i, d, None),
            ity::EDITFIELD
            | ity::NUMERICFIELD
            | ity::VALIDFILEFIELD
            | ity::DECIMALFIELD
            | ity::UPREDITFIELD => {
                let it = &self.menus[m].items[i];
                let value = if it.editing {
                    it.edit.clone()
                } else {
                    host.dvar(d.dvar.as_deref().unwrap_or(""))
                };
                let caret = it.editing && (self.now_ms / 300) & 1 == 0;
                let shown = if caret { format!("{value}|") } else { value };
                self.paint_item_text(host, p, m, i, d, Some(&shown));
            }
            ity::YESNO => {
                let on = host
                    .dvar(d.dvar.as_deref().unwrap_or(""))
                    .trim()
                    .parse::<f64>()
                    .unwrap_or(0.0)
                    != 0.0;
                let v = self
                    .assets
                    .translate(if on { "MENU_YES" } else { "MENU_NO" })
                    .map_or_else(
                        || if on { "Yes" } else { "No" }.to_owned(),
                        |s| s.to_string(),
                    );
                self.paint_item_text(host, p, m, i, d, Some(&v));
            }
            ity::MULTI => {
                let cur = host.dvar(d.dvar.as_deref().unwrap_or(""));
                let label = match &d.data {
                    ItemData::Multi(Some(mu)) => multi_index(mu, &cur)
                        .and_then(|k| mu.dvar_list.get(k))
                        .and_then(|s| s.as_deref())
                        .unwrap_or(""),
                    _ => "",
                };
                let label = self.translate(label);
                self.paint_item_text(host, p, m, i, d, Some(&label));
            }
            ity::DVARENUM => {
                let name = match &d.data {
                    ItemData::EnumDvarName(n) => n.as_deref().unwrap_or(""),
                    _ => "",
                };
                let list = host.dvar_enum(name);
                let cur = host.dvar(d.dvar.as_deref().unwrap_or(""));
                let label = list
                    .get(enum_index(&list, &cur))
                    .cloned()
                    .unwrap_or_default();
                let label = self.translate(&label);
                self.paint_item_text(host, p, m, i, d, Some(&label));
            }
            ity::SLIDER => {
                if let ItemData::EditField(Some(e)) = &d.data {
                    let v = host
                        .dvar(d.dvar.as_deref().unwrap_or(""))
                        .trim()
                        .parse::<f32>()
                        .unwrap_or(0.0);
                    let it = &self.menus[m].items[i];
                    let (r, color) = (it.rect, self.text_color(m, i, d, &*host));
                    let x = slider_bar_x(&r, d);
                    let y = Self::item_align_y(
                        d.text_align_mode & 0xC,
                        r.y + d.text_align_y,
                        r.h,
                        SLIDER_H,
                    );
                    let bar = self
                        .place
                        .rect(x, y, SLIDER_W, SLIDER_H, r.horz_align, r.vert_align);
                    let img = p.named(&self.assets, "ui_slider2");
                    p.pic(&img, bar, color);
                    let tx = slider_thumb_x(x, e, v);
                    let thumb =
                        self.place
                            .rect(tx - 5.0, y - 2.0, 10.0, 20.0, r.horz_align, r.vert_align);
                    let img = p.named(&self.assets, "ui_sliderbutt_1");
                    p.pic(&img, thumb, color);
                }
            }
            ity::BIND => {
                let text = if self.bind_pending == Some((m, i)) {
                    self.translate("@MENU_BIND_KEY_PENDING")
                } else {
                    self.bind_label(&*host, d.dvar.as_deref().unwrap_or(""))
                };
                self.paint_item_text(host, p, m, i, d, Some(&text));
            }
            ity::LISTBOX => self.paint_listbox(host, p, m, i, d),
            ity::GAME_MSG_WINDOW => {
                let r = self.menus[m].items[i].rect;
                let color = self.text_color(m, i, d, &*host);
                host.game_message_window(&*self, p, d, &r, color);
            }
            ity::OWNERDRAW => {
                let r = self.menus[m].items[i].rect;
                let px = self
                    .place
                    .rect(r.x, r.y, r.w, r.h, r.horz_align, r.vert_align);
                let color = self.text_color(m, i, d, &*host);
                let text = self.item_text(&*host, m, i, d);
                host.owner_draw(&*self, p, d, px, color, &text);
            }
            _ => {}
        }
    }

    /// Where a box of height `self_h` sits in a container (`Item_GetRectPlacementY`).
    pub(super) fn item_align_y(mode: i32, y0: f32, container: f32, self_h: f32) -> f32 {
        match mode {
            12 => container - self_h + y0,
            8 => (container - self_h) * 0.5 + y0,
            _ => y0,
        }
    }

    fn text_y(&self, mode: i32, container: f32, h: f32) -> f32 {
        match mode {
            4 => h,
            8 => (container + h) * 0.5,
            12 => container,
            _ => 0.0,
        }
    }

    fn paint_item_text(
        &self,
        host: &dyn Host,
        p: &mut Painter,
        m: usize,
        i: usize,
        d: &ItemDef,
        over: Option<&str>,
    ) {
        let text = match over {
            Some(t) => t.to_owned(),
            None => self.item_text(host, m, i, d),
        };
        if text.is_empty() {
            return;
        }
        let r = self.menus[m].items[i].rect;
        let color = self.text_color(m, i, d, host);
        let h = self.text_height(d.font_enum, d.text_scale);
        let w = self.text_width(&text, d.font_enum, d.text_scale);
        let bsz = if d.window.border != 0 {
            d.window.border_size
        } else {
            0.0
        };
        let mode = d.text_align_mode;
        let mut x = d.text_align_x;
        match mode & 3 {
            1 => x += (r.w - w) * 0.5,
            2 => x += r.w - w,
            _ => {}
        }
        let y = d.text_align_y + self.text_y(mode & 0xC, r.h, h);
        let (x, y) = (x + bsz + r.x, y + bsz + r.y);
        if d.window.static_flags & statf::AUTOWRAPPED != 0 && r.w > 0.0 {
            self.paint_wrapped(p, d, &text, color, x, y, r, mode);
            return;
        }
        self.draw_text(
            p,
            &TextDraw {
                text: &text,
                font_enum: d.font_enum,
                scale: d.text_scale,
                style: d.text_style,
                color,
                x,
                y,
                horz: r.horz_align,
                vert: r.vert_align,
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_wrapped(
        &self,
        p: &mut Painter,
        d: &ItemDef,
        text: &str,
        color: [f32; 4],
        x: f32,
        y: f32,
        r: Rect,
        mode: i32,
    ) {
        let line_h =
            self.text_height(d.font_enum, d.text_scale) * 1.2 + d.text_align_y.max(0.0) * 0.0;
        let mut line = String::new();
        let mut yy = y;
        let mut lines = Vec::new();
        for word in text.split_whitespace() {
            let cand = if line.is_empty() {
                word.to_owned()
            } else {
                format!("{line} {word}")
            };
            if !line.is_empty() && self.text_width(&cand, d.font_enum, d.text_scale) > r.w {
                lines.push(std::mem::take(&mut line));
                line = word.to_owned();
            } else {
                line = cand;
            }
        }
        lines.push(line);
        for l in lines {
            let w = self.text_width(&l, d.font_enum, d.text_scale);
            let lx = match mode & 3 {
                1 => x + (r.w - w) * 0.5 - (x - r.x) * 0.0,
                2 => x + r.w - w,
                _ => x,
            };
            self.draw_text(
                p,
                &TextDraw {
                    text: &l,
                    font_enum: d.font_enum,
                    scale: d.text_scale,
                    style: d.text_style,
                    color,
                    x: lx,
                    y: yy,
                    horz: r.horz_align,
                    vert: r.vert_align,
                },
            );
            yy += line_h;
        }
    }

    fn paint_listbox(&self, host: &mut dyn Host, p: &mut Painter, m: usize, i: usize, d: &ItemDef) {
        let ItemData::ListBox(Some(l)) = &d.data else {
            return;
        };
        let it = &self.menus[m].items[i];
        let r = it.rect;
        let feeder = d.special as i32;
        let n = host.feeder_count(feeder);
        let eh = l.element_height.max(1.0);
        let rows = (r.h / eh).floor().max(1.0) as usize;
        let focus = it.dyn_flags & dynf::HASFOCUS != 0;
        let clip = self
            .place
            .rect(r.x, r.y, r.w, r.h, r.horz_align, r.vert_align);
        p.g.scissor(Some([clip.x, clip.y, clip.w, clip.h]));
        for row in 0..rows {
            let idx = it.list_start as usize + row;
            if idx >= n {
                break;
            }
            let y = r.y + row as f32 * eh;
            if idx as i32 == it.list_cursor {
                let px = self.place.rect(r.x, y, r.w, eh, r.horz_align, r.vert_align);
                let c = if focus {
                    l.select_border
                } else {
                    [
                        l.select_border[0],
                        l.select_border[1],
                        l.select_border[2],
                        l.select_border[3] * 0.5,
                    ]
                };
                p.fill(px, [c[0], c[1], c[2], c[3].max(0.25) * 0.5]);
            }
            let cols: Vec<(f32, f32, i32, i32)> = if l.columns.is_empty() {
                vec![(0.0, r.w, 0, 0)]
            } else {
                l.columns
                    .iter()
                    .map(|c| (c.pos as f32, c.width as f32, c.max_chars, c.alignment))
                    .collect()
            };
            for (ci, (pos, width, max_chars, align)) in cols.iter().enumerate() {
                let img = host.feeder_image(feeder, idx, ci);
                if !img.is_empty() {
                    let px = self
                        .place
                        .rect(r.x + pos, y, eh, eh, r.horz_align, r.vert_align);
                    let im = p.named(&self.assets, &img);
                    p.pic(&im, px, [1.0; 4]);
                    continue;
                }
                let mut text = host.feeder_text(feeder, idx, ci);
                if let Some(k) = text.strip_prefix('@') {
                    text = self
                        .assets
                        .translate(k)
                        .map_or(text.clone(), |s| s.to_string());
                }
                if *max_chars > 0 {
                    text = text.chars().take(*max_chars as usize).collect();
                }
                if text.is_empty() {
                    continue;
                }
                let h = self.text_height(d.font_enum, d.text_scale);
                let w = self.text_width(&text, d.font_enum, d.text_scale);
                let x0 = r.x + pos + d.text_align_x;
                let x = match align {
                    1 => x0 + (width - w) * 0.5,
                    2 => x0 + width - w,
                    _ => x0,
                };
                let color = if idx as i32 == it.list_cursor {
                    self.menus[m].def.focus_color
                } else {
                    it.fore
                };
                self.draw_text(
                    p,
                    &TextDraw {
                        text: &text,
                        font_enum: d.font_enum,
                        scale: d.text_scale,
                        style: d.text_style,
                        color,
                        x,
                        y: y + d.text_align_y + (eh + h) * 0.5,
                        horz: r.horz_align,
                        vert: r.vert_align,
                    },
                );
            }
        }
        p.g.scissor(None);
    }
}
