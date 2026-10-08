// SPDX-License-Identifier: GPL-3.0-or-later
//! The HUD pieces the stock menus ask the client to draw (`ownerdraw` items): health, ammunition and weapon,
//! grenades, stance, sprint, hints, the corner compass and the full-screen map.
//!
//! Each piece reads [`HudFacts`] (filled every frame from the network play) and the `hud_*`/`compass*` dvars and
//! draws into the pixel rect the menu gave it. Behavior follows the original's cgame owner-draw functions; the
//! numbers are in virtual 640x480 units scaled by the UI's pixel size where the original works in them.
//! Pieces that mark objectives (`152`, `182`) and the message and obituary windows belong to the HUD drawing
//! code, not here.

use crate::compass::{self, MapInfo};
use crate::hudstate::{self, Counter, HudFacts, OverlayParams, Stance};
use crate::shell::HostCx;
use crate::ui::Ui;
use crate::ui::env::World;
use crate::ui::paint::{Painter, TextDraw};
use crate::ui::place::Px;
use ::assets::zone::menu::ItemDef;

const LOW_AMMO_COLOR: [f32; 3] = [0.89, 0.18, 0.01];
const WHITE: [f32; 4] = [1.0; 4];

/// The dvars the pieces read, with the original's defaults.
struct Cfg {
    enable: bool,
    draw_health: bool,
    breath_hint: bool,
    mantle_hint: bool,
    cursor_hints: i32,
    fade_ammo: f32,
    fade_health: f32,
    fade_compass: f32,
    fade_stance: f32,
    fade_offhand: f32,
    fade_sprint: f32,
    compass_size: f32,
    max_range: f32,
    rotation: bool,
    show_enemies: bool,
}

impl Cfg {
    fn read(c: &crate::input::Cvars) -> Self {
        let f = |n: &str, d: f32| c.get(n).and_then(|v| v.trim().parse().ok()).unwrap_or(d);
        let b = |n: &str, d: bool| f(n, f32::from(u8::from(d))) != 0.0;
        Self {
            enable: b("hud_enable", true),
            draw_health: b("cg_drawHealth", false),
            breath_hint: b("cg_drawBreathHint", true),
            mantle_hint: b("cg_drawMantleHint", true),
            cursor_hints: f("cg_cursorHints", 4.0) as i32,
            fade_ammo: f("hud_fade_ammodisplay", 0.0),
            fade_health: f("hud_fade_healthbar", 2.0),
            fade_compass: f("hud_fade_compass", 0.0),
            fade_stance: f("hud_fade_stance", 1.7),
            fade_offhand: f("hud_fade_offhand", 0.0),
            fade_sprint: f("hud_fade_sprint", 1.7),
            compass_size: f("compassSize", 1.0),
            max_range: f("compassMaxRange", 3500.0).max(0.0001),
            rotation: b("compassRotation", true),
            show_enemies: b("g_compassShowEnemies", false),
        }
    }
}

/// The alpha the ammunition pieces share, 0 while the weapon is disabled.
fn ammo_alpha(cfg: &Cfg, h: &HudFacts) -> f32 {
    if !cfg.enable || h.weapon_disabled {
        return 0.0;
    }
    hudstate::fade_hud(cfg.fade_ammo, h.ammo_fade, h.now)
}

/// `hudfade("...")` in menu expressions: the alpha of a HUD group, 0 outside a match.
pub fn menu_fade(h: &HudFacts, c: &crate::input::Cvars, which: crate::ui::expr::HudFade) -> f32 {
    use crate::ui::expr::HudFade;
    let cfg = Cfg::read(c);
    if !h.live {
        return 0.0;
    }
    match which {
        HudFade::Dpad | HudFade::Weapon => ammo_alpha(&cfg, h),
        HudFade::Compass => hudstate::fade_hud(cfg.fade_compass, h.compass_fade, h.now),
        HudFade::Scoreboard => 1.0,
    }
}

/// The two numbers of the ammunition display: the magazine as `%2i` and the reserve as `%3i`, each only when the
/// weapon has one, both capped at 999.
pub fn ammo_texts(clip: i32, stock: i32) -> (Option<String>, Option<String>) {
    (
        (clip >= 0).then(|| format!("{:2}", clip.min(999))),
        (stock >= 0).then(|| format!("{:3}", stock.min(999))),
    )
}

/// A localized string: the key's text, or the key itself when the strings do not have it.
fn tr(ui: &Ui, key: &str) -> String {
    ui.assets
        .translate(key)
        .map_or_else(|| key.to_owned(), |s| s.to_string())
}

fn with_alpha(mut c: [f32; 4], a: f32) -> [f32; 4] {
    c[3] = a;
    c
}

fn lerp3(a: [f32; 4], b: [f32; 3], t: f32) -> [f32; 4] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3],
    ]
}

/// What a piece draws with: the UI for fonts and units, the painter, the item's own settings and rect.
struct Dc<'a, 'p> {
    ui: &'a Ui,
    p: &'a mut Painter<'p>,
    d: &'a ItemDef,
    r: Px,
    color: [f32; 4],
    cfg: &'a Cfg,
}

impl Dc<'_, '_> {
    fn u(&self) -> f32 {
        self.ui.place.unit()
    }

    /// The item's own material (its `background`), if it has one.
    fn bg(&mut self) -> Option<render::ui2d::UiImage> {
        let m = self.d.window.background.as_ref()?;
        Some(self.p.material(&self.ui.assets, m))
    }

    fn pic(&mut self, name: &str, r: Px, color: [f32; 4]) {
        let img = self.p.named(&self.ui.assets, name);
        self.p.pic(&img, r, color);
    }

    /// Text width in pixels.
    fn tw(&self, text: &str) -> f32 {
        self.ui
            .text_width(text, self.d.font_enum, self.d.text_scale)
            * self.ui.place.scale.0
    }

    fn text_h(&self) -> f32 {
        self.ui.text_height(self.d.font_enum, self.d.text_scale) * self.ui.place.scale.1
    }

    /// Text with its baseline at pixel `y`.
    fn text(&mut self, x: f32, y: f32, color: [f32; 4], text: &str) {
        self.ui.draw_text(
            self.p,
            &TextDraw {
                text,
                font_enum: self.d.font_enum,
                scale: self.d.text_scale,
                style: self.d.text_style,
                color,
                x,
                y,
                horz: 5,
                vert: 5,
            },
        );
    }
}

/// Draws owner-draw `id`; `false` when it is not one of these pieces.
pub fn draw(
    cx: &mut HostCx<'_>,
    ui: &Ui,
    p: &mut Painter,
    d: &ItemDef,
    r: Px,
    color: [f32; 4],
) -> bool {
    let id = d.window.owner_draw;
    if !matches!(
        id,
        5 | 6
            | 20
            | 71
            | 72
            | 79
            | 80
            | 81
            | 82
            | 98
            | 103..=110
            | 112..=120
            | 145
            | 146
            | 150
            | 151
            | 152
            | 158
            | 159
            | 170
            | 181
            | 182
            | 183
            | 185
            | 187
            | 188
    ) {
        return false;
    }
    let cfg = Cfg::read(&cx.input.cvars);
    if !cx.st.game.hud.live || !cfg.enable {
        return true;
    }
    // Key names are looked up while the facts are not borrowed.
    let breath_key = [
        cx.key_binding("+holdbreath"),
        cx.key_binding("+melee_breath"),
        cx.key_binding("+breath_sprint"),
    ]
    .into_iter()
    .find(|k| k != "KEY_UNBOUND")
    .map_or_else(|| ui.localize_key("KEY_UNBOUND"), |k| ui.localize_key(&k));
    let mantle_key = [cx.key_binding("+gostand"), cx.key_binding("+moveup")]
        .into_iter()
        .find(|k| k != "KEY_UNBOUND")
        .map_or_else(|| ui.localize_key("KEY_UNBOUND"), |k| ui.localize_key(&k));
    let use_key = ui.localize_key(&cx.key_binding("+activate"));
    let objectives = cx.st.live.objectives.clone();
    let h = &mut cx.st.game.hud;
    *h.drawn.entry(id).or_default() += 1;
    let mut dc = Dc {
        ui,
        p,
        d,
        r,
        color,
        cfg: &cfg,
    };
    match id {
        112 => low_health_overlay(&mut dc, h),
        79 => health_bar(&mut dc, h),
        98 => health_bar_back(&mut dc, h),
        5 => ammo_value(&mut dc, h),
        6 => ammo_backdrop(&mut dc, h),
        20 => stance(&mut dc, h),
        71 => breath_hint(&mut dc, h, &breath_key),
        80 => mantle_hint(&mut dc, h, &mantle_key),
        113 => invalid_cmd_hint(&mut dc, h),
        81 => weapon_name(&mut dc, h),
        82 => weapon_name_back(&mut dc, h),
        103..=108 => offhand(&mut dc, h, id),
        114 => sprint_meter(&mut dc, h),
        115 => sprint_back(&mut dc, h),
        116 => weapon_background(&mut dc, h),
        117 => clip_graphic(&mut dc, h),
        118 => weapon_icon(&mut dc, h),
        119 => ammo_stock(&mut dc, h),
        120 => low_ammo_warning(&mut dc, h),
        151 => compass_back(&mut dc, h),
        145 | 146 => compass_tape(&mut dc, h),
        150 => compass_player(&mut dc, h, false),
        159 => compass_map(&mut dc, h),
        152 => compass_objectives(&mut dc, h, false, &objectives),
        182 => compass_objectives(&mut dc, h, true, &objectives),
        158 => compass_friendlies(&mut dc, h, false),
        170 => compass_enemies(&mut dc, h, false),
        183 => compass_player(&mut dc, h, true),
        181 => map_image(&mut dc, h),
        185 => compass_friendlies(&mut dc, h, true),
        188 => compass_enemies(&mut dc, h, true),
        187 => map_border(&mut dc, h),
        72 => cursor_hint(&mut dc, h, &use_key),
        // 109 and
        // 110 mark the offhand weapon the player has equipped, which only a controller cycles.
        _ => {}
    }
    true
}

// ---- health -----------------------------------------------------------------------------------------------

fn low_health_overlay(dc: &mut Dc, h: &mut HudFacts) {
    let frac = hudstate::health_fraction(h.health, h.max_health, h.pm_dead);
    if frac == 0.0 {
        return;
    }
    h.overlay.pulse(h.now, frac, &OverlayParams::default());
    let a = h.overlay.alpha(h.now);
    if a > 0.0
        && let Some(img) = dc.bg()
    {
        let c = with_alpha(dc.color, a);
        dc.p.pic(&img, dc.r, c);
    }
}

fn health_bar(dc: &mut Dc, h: &mut HudFacts) {
    if !dc.cfg.draw_health {
        return;
    }
    let health = hudstate::health_fraction(h.health, h.max_health, h.pm_dead);
    let a = hudstate::fade_hud(dc.cfg.fade_health, h.health_fade, h.now);
    if a == 0.0 {
        return;
    }
    let Some(img) = dc.bg() else { return };
    let r = dc.r;
    let mut c = with_alpha(dc.color, a);
    if health > 0.0 {
        // Green at the ends of the range's lower half, fading to red-free yellow-white higher up.
        if health <= 0.5 {
            c[1] = (health + 0.2) * c[1] + 0.3;
        } else {
            c[0] *= (1.0 - health) * 2.0;
            c[2] *= (1.0 - health) * 2.0;
        }
        dc.p.g.quad(
            &img,
            [r.x, r.y, r.w * health, r.h],
            [0.0, 0.0, health, 1.0],
            c,
        );
    }
    // The part that was just lost shows red and drains away.
    let ms = 16;
    if h.bar_trail <= health {
        h.bar_trail = health;
        h.bar_trail_delay = 1;
    } else if h.bar_trail_delay > 0 {
        h.bar_trail_delay = (h.bar_trail_delay - ms).max(0);
    } else {
        h.bar_trail -= ms as f32 * 0.0012;
        if health >= h.bar_trail {
            h.bar_trail = health;
            h.bar_trail_delay = 1;
        }
    }
    if health < h.bar_trail {
        let x = r.x + r.w * health;
        dc.p.g.quad(
            &img,
            [x, r.y, (h.bar_trail - health) * r.w, r.h],
            [health, 0.0, h.bar_trail, 1.0],
            [1.0, 0.0, 0.0, a],
        );
    }
}

fn health_bar_back(dc: &mut Dc, h: &mut HudFacts) {
    if !dc.cfg.draw_health {
        return;
    }
    let a = hudstate::fade_hud(dc.cfg.fade_health, h.health_fade, h.now);
    if a == 0.0 {
        return;
    }
    let Some(img) = dc.bg() else { return };
    dc.p.pic(&img, dc.r, with_alpha(dc.color, a));
    let health = hudstate::health_fraction(h.health, h.max_health, h.pm_dead);
    if health == 0.0 {
        return;
    }
    let flash = if health < 0.33 {
        500
    } else if health < 1.0 {
        1000
    } else {
        0
    };
    if flash > 0 {
        if h.last_bar_pulse > h.now || h.last_bar_pulse + flash < h.now {
            h.last_bar_pulse = h.now;
        }
        let k = ((flash + h.last_bar_pulse - h.now) as f32 / flash as f32).min(a);
        dc.p.pic(&img, dc.r, [0.89, 0.18, 0.01, k]);
    }
}

// ---- ammunition and the weapon ------------------------------------------------------------------------------

fn ammo_backdrop(dc: &mut Dc, h: &mut HudFacts) {
    let a = ammo_alpha(dc.cfg, h);
    let Some(w) = h.weapon.as_ref().filter(|_| a > 0.0) else {
        return;
    };
    let mut c = with_alpha(dc.color, a);
    if w.low_ammo {
        c = lerp3(c, LOW_AMMO_COLOR, 1.0);
    }
    if let Some(img) = dc.bg() {
        dc.p.pic(&img, dc.r, c);
    }
}

fn ammo_value(dc: &mut Dc, h: &mut HudFacts) {
    let a = ammo_alpha(dc.cfg, h);
    let Some(w) = h.weapon.clone().filter(|_| a > 0.0) else {
        return;
    };
    let (clip, stock) = ammo_texts(w.clip, w.stock);
    let r = dc.r;
    let u = dc.u();
    let base = with_alpha(dc.color, a);
    let ammo_c = if w.low_ammo {
        lerp3(base, LOW_AMMO_COLOR, 1.0)
    } else {
        base
    };
    let flash = if w.low_clip {
        if h.last_clip_flash > h.now || h.last_clip_flash + 800 < h.now {
            h.last_clip_flash = h.now;
        }
        let k = ((h.last_clip_flash + 800 - h.now) as f32 / 800.0).min(a);
        Some([LOW_AMMO_COLOR[0], LOW_AMMO_COLOR[1], LOW_AMMO_COLOR[2], k])
    } else {
        None
    };
    match (clip, stock) {
        (Some(clip), Some(stock)) => {
            dc.text(r.x, r.y, base, &clip);
            if let Some(f) = flash {
                dc.text(r.x, r.y, f, &clip);
            }
            let x = r.x + r.w - dc.tw(&stock);
            dc.text(x, r.y, ammo_c, &stock);
            let x = (r.w - dc.tw("|")) * 0.5 + r.x - 5.0 * u;
            dc.text(x, r.y, ammo_c, "|");
        }
        (Some(clip), None) => {
            let x = (r.w - dc.tw(&clip)) * 0.5 + r.x;
            dc.text(x, r.y, base, &clip);
            if let Some(f) = flash {
                dc.text(x, r.y, f, &clip);
            }
        }
        (None, Some(stock)) => {
            let x = (r.w - dc.tw(&stock)) * 0.5 + r.x;
            dc.text(x, r.y, ammo_c, &stock);
        }
        (None, None) => {}
    }
}

/// `name / mode`, translated.
fn weapon_label(ui: &Ui, w: &hudstate::WeaponFacts) -> String {
    let name = tr(ui, &w.display);
    if w.mode.is_empty() {
        name
    } else {
        format!("{name} / {}", tr(ui, &w.mode))
    }
}

fn weapon_name(dc: &mut Dc, h: &mut HudFacts) {
    let Some(a) = hudstate::fade_color(h.now, h.weapon_select_time, 1800, 700) else {
        return;
    };
    let Some(w) = h.weapon.as_ref().filter(|_| !h.weapon_disabled) else {
        return;
    };
    let label = weapon_label(dc.ui, w);
    let x = dc.r.x + dc.r.w - dc.tw(&label) - 28.0 * dc.u();
    let c = with_alpha(dc.color, a);
    dc.text(x, dc.r.y, c, &label);
}

fn weapon_name_back(dc: &mut Dc, h: &mut HudFacts) {
    let a = if dc.cfg.fade_ammo == 0.0 {
        1.0
    } else {
        hudstate::fade_color(h.now, h.weapon_select_time, 1800, 700).unwrap_or(0.0)
    };
    let Some(w) = h.weapon.as_ref().filter(|_| a > 0.0) else {
        return;
    };
    let label = weapon_label(dc.ui, w);
    let width = dc.tw(&label) + 36.0 * dc.u();
    let Some(img) = dc.bg() else { return };
    let r = Px {
        x: dc.r.x + dc.r.w - width,
        w: width,
        ..dc.r
    };
    dc.p.pic(&img, r, with_alpha(dc.color, a));
}

fn weapon_background(dc: &mut Dc, h: &mut HudFacts) {
    let a = ammo_alpha(dc.cfg, h) * dc.color[3];
    if a > 0.0
        && let Some(img) = dc.bg()
    {
        dc.p.pic(&img, dc.r, with_alpha(dc.color, a));
    }
}

fn weapon_icon(dc: &mut Dc, h: &mut HudFacts) {
    let a = ammo_alpha(dc.cfg, h) * dc.color[3];
    let Some(icon) = h
        .weapon
        .as_ref()
        .filter(|_| a > 0.0)
        .and_then(|w| w.icon.clone().map(|i| (i, w.icon_ratio)))
    else {
        return;
    };
    let mut r = dc.r;
    match icon.1 {
        1 => {
            r.x -= r.w;
            r.w *= 2.0;
        }
        2 => {
            r.x -= r.w * 3.0;
            r.w *= 4.0;
        }
        _ => {}
    }
    dc.pic(&icon.0, r, with_alpha(dc.color, a));
}

fn ammo_stock(dc: &mut Dc, h: &mut HudFacts) {
    let a = ammo_alpha(dc.cfg, h) * dc.color[3];
    let Some(w) = h.weapon.as_ref().filter(|_| a > 0.0) else {
        return;
    };
    let Some(stock) = w.stock_shown else { return };
    let mut c = with_alpha(dc.color, a);
    if w.stock_low {
        c = [1.0, 0.3, 0.3, a];
    }
    let text = format!("{:3}", stock.min(999));
    dc.text(dc.r.x, dc.r.y, c, &text);
}

/// The magazine as a row of rounds, the ones fired grayed out (`DrawClipAmmo`).
fn clip_graphic(dc: &mut Dc, h: &mut HudFacts) {
    let a = ammo_alpha(dc.cfg, h) * dc.color[3];
    let Some(w) = h.weapon.clone().filter(|_| a > 0.0) else {
        return;
    };
    let Some((mat, bw, bh)) = w.counter.bullet() else {
        return;
    };
    let mut c = with_alpha(dc.color, a);
    if w.counter_low {
        let t = (h.now - h.last_clip_flash) as f32 / (40.0 * std::f32::consts::PI);
        let k = t.sin() * 0.5 + 0.5;
        c = lerp3(c, [1.0, 0.3, 0.3], k);
    } else {
        h.last_clip_flash = h.now;
    }
    let u = dc.u();
    let (bw, bh) = (bw * u, bh * u);
    let img = dc.p.named(&dc.ui.assets, mat);
    let base = (dc.r.x, dc.r.y);
    let mut pos: Vec<[f32; 2]> = Vec::with_capacity(w.counter_clip_size.max(0) as usize);
    match w.counter {
        Counter::Beltfed => {
            let mut step = 8.0 * u;
            let (mut x, mut y) = (
                base.0,
                bh * 0.25 * (w.counter_clip_size / 20) as f32 + base.1,
            );
            for i in 0..w.counter_clip_size {
                if i % 20 == 0 {
                    step = -step;
                    y += -2.0 * u;
                    x += step;
                }
                pos.push([x, y]);
                x += step;
            }
        }
        k => {
            let (sx, y) = match k {
                Counter::Magazine => (4.0, base.1 - bh * 0.5),
                Counter::ShortMagazine => (40.0, base.1 - bh * 0.5),
                Counter::Shotgun => (20.0, base.1 - bh * 0.5),
                _ => (72.0, base.1 - bh * 0.5),
            };
            let first = base.0 - bw;
            for i in 0..w.counter_clip_size {
                pos.push([first - i as f32 * sx * u, y]);
            }
        }
    }
    for (i, [x, y]) in pos.into_iter().enumerate() {
        if i as i32 >= w.counter_clip {
            c[0] = 0.3;
            c[1] = 0.3;
            c[2] = 0.3;
        }
        dc.p.pic(&img, Px { x, y, w: bw, h: bh }, c);
    }
}

/// "Reload" and "no ammo" warnings over the weapon box (`CG_DrawPlayerWeaponLowAmmoWarning`).
fn low_ammo_warning(dc: &mut Dc, h: &mut HudFacts) {
    let fade = ammo_alpha(dc.cfg, h);
    let Some(w) = h
        .weapon
        .as_ref()
        .filter(|w| fade > 0.0 && w.low_clip && !h.pm_dead && !h.spectator && !w.hide_warning)
    else {
        return;
    };
    if w.counter == Counter::None {
        return;
    }
    let (text, c1, c2) = if w.can_reload {
        if w.clip_size == 1 {
            return;
        }
        (
            "PLATFORM_RELOAD",
            [0.9, 0.9, 0.9, 0.8],
            [1.0, 1.0, 1.0, 1.0],
        )
    } else if w.empty {
        ("WEAPON_NO_AMMO", [0.8, 0.0, 0.0, 0.8], [1.0, 0.0, 0.0, 1.0])
    } else {
        (
            "PLATFORM_LOW_AMMO_NO_RELOAD",
            [0.7, 0.7, 0.0, 0.8],
            [1.0, 1.0, 0.0, 1.0],
        )
    };
    // lowAmmoWarningPulseMin/Max/Freq: a sine between the two colors.
    let (min, max, freq) = (0.0f32, 1.5f32, 1.7f32);
    let amp = (max - min) * 0.5;
    let k = (freq * h.now as f32 * 0.006_283_185).sin() * amp + min + amp;
    let mut c: [f32; 4] = std::array::from_fn(|i| c1[i] + (c2[i] - c1[i]) * k);
    c[3] *= fade;
    if let Some(img) = dc.bg() {
        dc.p.pic(&img, dc.r, c);
    }
    let text = tr(dc.ui, text);
    let u = dc.u();
    let x = dc.r.x + dc.d.text_align_x * u;
    let y = dc.r.y + dc.d.text_align_y * u;
    dc.text(x, y, c, &text);
}

// ---- grenades -------------------------------------------------------------------------------------------------

fn offhand(dc: &mut Dc, h: &mut HudFacts, id: i32) {
    if h.pm_dead || h.weapon_disabled {
        return;
    }
    let second = matches!(id, 104 | 106 | 108);
    let Some(slot) = (if second { &h.second } else { &h.frag }).clone() else {
        return;
    };
    let a = hudstate::fade_hud(dc.cfg.fade_offhand, h.offhand_fade, h.now) * dc.color[3];
    if a == 0.0 {
        return;
    }
    let low = |c: [f32; 4]| {
        if slot.ammo == 0 {
            [
                LOW_AMMO_COLOR[0],
                LOW_AMMO_COLOR[1],
                LOW_AMMO_COLOR[2],
                c[3],
            ]
        } else {
            c
        }
    };
    let c = with_alpha(dc.color, a);
    match id {
        103 | 104 => {
            if let Some(icon) = &slot.icon {
                dc.pic(icon, dc.r, c);
            }
        }
        105 | 106 => {
            let t = slot.ammo.to_string();
            dc.text(dc.r.x, dc.r.y, low(c), &t);
        }
        _ => {
            let key = match (second, h.second_is_flash) {
                (false, _) => "WEAPON_FRAGGRENADE",
                (true, false) => "WEAPON_SMOKEGRENADE",
                (true, true) => "WEAPON_FLASHGRENADE",
            };
            let t = tr(dc.ui, key);
            dc.text(dc.r.x, dc.r.y, c, &t);
        }
    }
}

// ---- stance, sprint --------------------------------------------------------------------------------------------

fn stance(dc: &mut Dc, h: &mut HudFacts) {
    let fade = hudstate::fade_hud(dc.cfg.fade_stance, h.stance_fade, h.now);
    if fade == 0.0 {
        return;
    }
    let icon = match h.stance {
        Stance::Stand => "stance_stand",
        Stance::Crouch => "stance_crouch",
        Stance::Prone => "stance_prone",
    };
    // A blocked prone blinks its reason for a second and a half.
    if h.prone_blocked_end > h.now {
        let key = if h.weapon.as_ref().is_some_and(|w| w.blocks_prone) {
            "CGAME_PRONE_BLOCKED_WEAPON"
        } else {
            "CGAME_PRONE_BLOCKED"
        };
        let text = tr(dc.ui, key);
        let left = (h.prone_blocked_end - h.now) as f32;
        let alpha = (left / 1500.0 * 540.0).to_radians().sin().abs();
        let x = dc.ui.place.x(0.0, 7) - dc.tw(&text) * 0.5;
        let y = dc.ui.place.y(-160.0, 3);
        let c = [dc.color[0], dc.color[1], dc.color[2], alpha];
        dc.text(x, y, c, &text);
    }
    dc.pic(icon, dc.r, with_alpha(dc.color, dc.color[3] * fade));
}

fn sprint_back(dc: &mut Dc, h: &mut HudFacts) {
    let a = hudstate::fade_hud(dc.cfg.fade_sprint, h.sprint_fade, h.now);
    if a > 0.0
        && let Some(img) = dc.bg()
    {
        dc.p.pic(&img, dc.r, with_alpha(dc.color, dc.color[3] * a));
    }
}

/// The sprint bar's color: full toward empty, or the disabled red when none is left.
pub fn sprint_color(left: i32, max: i32, dead: bool) -> [f32; 3] {
    const FULL: [f32; 3] = [0.8, 0.8, 0.8];
    const EMPTY: [f32; 3] = [0.7, 0.5, 0.2];
    const DISABLED: [f32; 3] = [0.8, 0.1, 0.1];
    if dead || max == 0 {
        return FULL;
    }
    if left == 0 {
        return DISABLED;
    }
    let k = left as f32 / max as f32;
    std::array::from_fn(|i| EMPTY[i] + (FULL[i] - EMPTY[i]) * k)
}

fn sprint_meter(dc: &mut Dc, h: &mut HudFacts) {
    let a = hudstate::fade_hud(dc.cfg.fade_sprint, h.sprint_fade, h.now);
    if a == 0.0 || h.sprint_max <= 0 {
        return;
    }
    let frac = h.sprint_left as f32 / h.sprint_max as f32;
    if frac <= 0.0 {
        return;
    }
    let img = match dc.bg() {
        Some(i) => i,
        None => dc.p.g.white(),
    };
    let rgb = sprint_color(h.sprint_left, h.sprint_max, h.pm_dead);
    let r = dc.r;
    dc.p.g.quad(
        &img,
        [r.x, r.y, r.w * frac, r.h],
        [0.0, 0.0, frac, 1.0],
        [rgb[0], rgb[1], rgb[2], dc.color[3] * a],
    );
}

// ---- hints ------------------------------------------------------------------------------------------------------

/// Puts the key name into the string's `&&1`.
fn with_key(text: &str, key: &str) -> String {
    text.replace("&&1", key)
}

fn breath_hint(dc: &mut Dc, h: &mut HudFacts, key: &str) {
    if !dc.cfg.breath_hint || !h.breath_hint {
        return;
    }
    let text = with_key(&tr(dc.ui, "PLATFORM_HOLD_BREATH"), key);
    let x = dc.r.x - (dc.tw(&text) * 0.5).round();
    dc.text(x, dc.r.y, WHITE, &text);
}

fn mantle_hint(dc: &mut Dc, h: &mut HudFacts, key: &str) {
    if !dc.cfg.mantle_hint || !h.mantle_hint {
        return;
    }
    let text = with_key(&tr(dc.ui, "PLATFORM_MANTLE"), key);
    let len = dc.tw(&text);
    let r = dc.r;
    let x = r.x - (r.w + len) * 0.5;
    let y = dc.text_h() * 0.5 + r.y;
    dc.text(x, y, dc.color, &text);
    dc.pic(
        "hint_mantle",
        Px {
            x: x + len,
            y: r.y - r.h * 0.5,
            w: r.w,
            h: r.h,
        },
        dc.color,
    );
}

/// `CG_DrawCursorhint`: the use hint under the crosshair (icon and text), for as long as the server's player state
/// names a use trigger and 100 ms after.
fn cursor_hint(dc: &mut Dc, h: &mut HudFacts, key: &str) {
    const HINT_NOICON: u8 = 1;
    let mode = dc.cfg.cursor_hints;
    if mode == 0 || h.cursor_hint == 0 {
        return;
    }
    let Some(fade) = hudstate::fade_color(h.now, h.cursor_hint_time, 100, 100) else {
        return;
    };
    let mut color = with_alpha(dc.color, dc.color[3] * fade);
    let pulse = (h.now as f32 / 150.0).sin() * 0.5 + 0.5;
    if mode == 3 {
        color[3] *= pulse;
    }
    let scale = if mode == 2 {
        (h.cursor_hint_time % 1000) as f32 / 100.0
    } else if mode < 3 {
        pulse * 10.0
    } else {
        0.0
    };
    let raw = if h.cursor_hint_text.is_empty() && h.cursor_hint == 3 {
        "&PLATFORM_PICKUPHEALTH"
    } else {
        h.cursor_hint_text.as_str()
    };
    let mut keys = std::collections::HashMap::new();
    keys.insert("+activate".to_owned(), key.to_owned());
    let text = crate::hud::expand_keys(&crate::hud::localize(&dc.ui.assets, raw), &keys)
        .replace("&&1", key);
    let r = dc.r;
    let len = dc.tw(&text);
    let y = dc.text_h() * 0.5 + r.y;
    if h.cursor_hint == HINT_NOICON {
        if !text.is_empty() {
            dc.text(r.x - (scale + len) * 0.5, y, color, &text);
        }
        return;
    }
    let icon = match h.cursor_hint {
        3 => "hint_health",
        4 => "hint_friendly",
        _ => "hint_usable",
    };
    if text.is_empty() {
        let half = scale * 0.5;
        dc.pic(
            icon,
            Px {
                x: r.x - (r.w + half) * 0.5,
                y: r.y - half,
                w: r.w + scale,
                h: r.h + scale,
            },
            color,
        );
    } else {
        let x = r.x - (r.w + scale + len) * 0.5;
        dc.text(x, y, color, &text);
        dc.pic(
            icon,
            Px {
                x: x + len,
                y: r.y - r.h * 0.5,
                w: r.w + scale,
                h: r.h + scale,
            },
            color,
        );
    }
}

fn invalid_cmd_hint(dc: &mut Dc, h: &mut HudFacts) {
    const SHOWN_MS: i32 = 1800;
    const BLINK_MS: i32 = 600;
    let Some((key, since)) = h.invalid_cmd else {
        return;
    };
    if since + SHOWN_MS < h.now {
        h.invalid_cmd = None;
        return;
    }
    let text = tr(dc.ui, key);
    let a = ((h.now - since) % BLINK_MS) as f32 / BLINK_MS as f32;
    let x = dc.r.x - (dc.tw(&text) * 0.5).round();
    dc.text(x, dc.r.y, with_alpha(dc.color, a), &text);
}

// ---- compass ------------------------------------------------------------------------------------------------------

fn compass_alpha(dc: &Dc, h: &HudFacts) -> f32 {
    if h.map.is_none() {
        return 0.0;
    }
    hudstate::fade_hud(dc.cfg.fade_compass, h.compass_fade, h.now)
}

fn compass_back(dc: &mut Dc, h: &mut HudFacts) {
    let a = compass_alpha(dc, h);
    if a > 0.0
        && let Some(img) = dc.bg()
    {
        dc.p.pic(&img, dc.r, with_alpha(dc.color, dc.color[3] * a));
    }
}

fn compass_tape(dc: &mut Dc, h: &mut HudFacts) {
    let a = compass_alpha(dc, h).min(dc.color[3]);
    let Some(img) = dc.bg().filter(|_| a > 0.0) else {
        return;
    };
    let north = h.map.as_ref().map_or(0.0, |m| m.north_yaw);
    let (l, rgt) = compass::tape_span(h.yaw, north, 0.5);
    let r = dc.r;
    dc.p.g.quad(
        &img,
        [r.x, r.y, r.w, r.h],
        [l, 0.0, rgt, 1.0],
        with_alpha(dc.color, a),
    );
}

/// The minimap: the map image placed so the player is in the middle, turned so the view direction is up.
fn compass_map(dc: &mut Dc, h: &mut HudFacts) {
    let a = compass_alpha(dc, h);
    let Some(map) = h.map.as_ref().filter(|_| a > 0.0) else {
        return;
    };
    let r = dc.r;
    let (w, ht) = (r.w * dc.cfg.compass_size, r.h * dc.cfg.compass_size);
    let per_inch = ht / dc.cfg.max_range;
    let corner = map.corner_from([h.origin[0], h.origin[1]]);
    let (cx, cy) = (r.x + w * 0.5, r.y + ht * 0.5);
    let img = dc.p.named(&dc.ui.assets, &map.material);
    let rot = if dc.cfg.rotation {
        (h.yaw - map.north_yaw).to_radians()
    } else {
        0.0
    };
    dc.p.g.scissor(Some([r.x, r.y, w, ht]));
    dc.p.g.quad_rot(
        &img,
        [
            cx + corner[0] * per_inch,
            cy + corner[1] * per_inch,
            map.world_size[0] * per_inch,
            map.world_size[1] * per_inch,
        ],
        [0.0, 0.0, 1.0, 1.0],
        with_alpha(dc.color, a),
        rot,
        [cx, cy],
    );
    dc.p.g.scissor(None);
}

/// The player's arrow: fixed at the middle of the minimap, or placed on the full map.
fn compass_player(dc: &mut Dc, h: &mut HudFacts, full: bool) {
    let a = if full { 1.0 } else { compass_alpha(dc, h) } * dc.color[3];
    let (Some(map), Some(img)) = (h.map.as_ref(), dc.bg()) else {
        return;
    };
    if a == 0.0 {
        return;
    }
    let u = dc.u();
    let (c, size, angle) = if full {
        let m = full_rect(dc, map);
        let xy = map.to_map((m.w, m.h), [h.origin[0], h.origin[1]]);
        (
            [m.x + m.w * 0.5 + xy[0], m.y + m.h * 0.5 + xy[1]],
            20.0 * u,
            compass::angle_delta(map.north_yaw, h.yaw),
        )
    } else {
        let s = 18.75 * dc.cfg.compass_size * u;
        let r = dc.r;
        let angle = if dc.cfg.rotation {
            0.0
        } else {
            compass::angle_delta(map.north_yaw, h.yaw)
        };
        (
            [
                r.x + r.w * dc.cfg.compass_size * 0.5,
                r.y + r.h * dc.cfg.compass_size * 0.5,
            ],
            s,
            angle,
        )
    };
    dc.p.g.quad_rot(
        &img,
        [c[0] - size * 0.5, c[1] - size * 0.5, size, size],
        [0.0, 0.0, 1.0, 1.0],
        with_alpha(dc.color, a),
        angle.to_radians(),
        c,
    );
}

/// An icon on the minimap or the full map at a world position, turned by `angle` degrees clockwise.
struct Mark {
    pos: [f32; 2],
    angle: f32,
    alpha: f32,
}

fn draw_marks(dc: &mut Dc, h: &HudFacts, map: &MapInfo, full: bool, icon: &str, marks: &[Mark]) {
    if marks.is_empty() {
        return;
    }
    let u = dc.u();
    let img = dc.p.named(&dc.ui.assets, icon);
    let (center, size, area) = if full {
        let m = full_rect(dc, map);
        ([m.x + m.w * 0.5, m.y + m.h * 0.5], 15.0 * u, (m.w, m.h))
    } else {
        let r = dc.r;
        let k = dc.cfg.compass_size;
        (
            [r.x + r.w * k * 0.5, r.y + r.h * k * 0.5],
            18.75 * k * u,
            (r.w * k, r.h * k),
        )
    };
    let up = compass::up_vector(dc.cfg.rotation, h.yaw, map.north_yaw);
    for m in marks {
        let xy = if full {
            map.to_map(area, m.pos)
        } else {
            compass::to_compass(
                up,
                [h.origin[0], h.origin[1]],
                m.pos,
                area.1,
                dc.cfg.max_range,
            )
        };
        let (xy, _) = compass::clip_to_rect(xy, area.0, area.1);
        let c = [center[0] + xy[0], center[1] + xy[1]];
        dc.p.g.quad_rot(
            &img,
            [c[0] - size * 0.5, c[1] - size * 0.5, size, size],
            [0.0, 0.0, 1.0, 1.0],
            with_alpha(dc.color, m.alpha),
            m.angle.to_radians(),
            c,
        );
    }
}

/// The scripts' objectives (`objective_add` with a position) on the minimap and the full map, the current one over
/// the others; each keeps its own icon.
fn compass_objectives(
    dc: &mut Dc,
    h: &mut HudFacts,
    full: bool,
    objectives: &[crate::hud::LiveObjective],
) {
    let a = if full { 1.0 } else { compass_alpha(dc, h) };
    h.objectives_listed = h.objectives_listed.max(objectives.len() as u32);
    h.objectives_alpha = h.objectives_alpha.max(a);
    let Some(map) = h.map.clone().filter(|_| a > 0.0) else {
        return;
    };
    for current in [false, true] {
        let mut icons: Vec<&str> = Vec::new();
        for o in objectives
            .iter()
            .filter(|o| o.current == current && !o.icon.is_empty())
        {
            if !icons.contains(&o.icon.as_str()) {
                icons.push(&o.icon);
            }
        }
        for icon in icons {
            let marks: Vec<Mark> = objectives
                .iter()
                .filter(|o| o.current == current && o.icon == icon)
                .map(|o| Mark {
                    pos: [o.pos[0], o.pos[1]],
                    angle: 0.0,
                    alpha: a,
                })
                .collect();
            h.objective_marks += marks.len() as u32;
            draw_marks(dc, h, &map, full, icon, &marks);
        }
    }
}

/// Which way an icon turns for an actor facing `yaw`: the same way as in the world relative to the compass's up.
fn mark_angle(rotation_up: bool, view_yaw: f32, north_yaw: f32, yaw: f32) -> f32 {
    if rotation_up {
        compass::norm360(view_yaw - yaw)
    } else {
        compass::norm360(north_yaw - yaw)
    }
}

const FRIEND_RANGE_MS: i32 = 1500;
const PING_MS: i32 = 2000;

fn compass_friendlies(dc: &mut Dc, h: &mut HudFacts, full: bool) {
    let a = if full { 1.0 } else { compass_alpha(dc, h) };
    let Some(map) = h.map.clone().filter(|_| a > 0.0 && h.team_known) else {
        return;
    };
    let rot = dc.cfg.rotation && !full;
    let mut base = Vec::new();
    let mut firing = Vec::new();
    for (&c, ac) in &h.actors {
        if !ac.friendly || c == h.own_client || ac.last_update < h.now - FRIEND_RANGE_MS {
            continue;
        }
        let angle = mark_angle(rot, h.yaw, map.north_yaw, ac.yaw);
        base.push(Mark {
            pos: ac.pos,
            angle,
            alpha: a,
        });
        if ac.fire_time != 0 && h.now - ac.fire_time <= PING_MS {
            firing.push(Mark {
                pos: ac.pos,
                angle,
                alpha: a * (1.0 - (h.now - ac.fire_time) as f32 / PING_MS as f32),
            });
        }
    }
    draw_marks(dc, h, &map, full, "compassping_friendly_mp", &base);
    draw_marks(dc, h, &map, full, "compassping_friendlyfiring_mp", &firing);
}

/// Enemies show where they last fired (or everywhere with `g_compassShowEnemies`).
fn compass_enemies(dc: &mut Dc, h: &mut HudFacts, full: bool) {
    let a = if full { 1.0 } else { compass_alpha(dc, h) };
    let Some(map) = h.map.clone().filter(|_| a > 0.0 && h.team_known) else {
        return;
    };
    let (mut seen, mut firing) = (Vec::new(), Vec::new());
    for ac in h.actors.values().filter(|ac| !ac.friendly) {
        if dc.cfg.show_enemies && ac.last_update >= h.now - FRIEND_RANGE_MS {
            seen.push(Mark {
                pos: ac.pos,
                angle: 0.0,
                alpha: a,
            });
        }
        if ac.fire_time != 0 && h.now - ac.fire_time <= PING_MS {
            firing.push(Mark {
                pos: ac.fire_pos,
                angle: 0.0,
                alpha: a * (1.0 - (h.now - ac.fire_time) as f32 / PING_MS as f32),
            });
        }
    }
    draw_marks(dc, h, &map, full, "compassping_enemy", &seen);
    draw_marks(dc, h, &map, full, "compassping_enemyfiring", &firing);
}

// ---- full-screen map -----------------------------------------------------------------------------------------------

/// The map's picture rect inside the menu item's rect.
fn full_rect(dc: &Dc, map: &MapInfo) -> Px {
    let r = dc.r;
    let [x, y, w, h] = compass::fit_map(map.world_size, [r.x, r.y, r.w, r.h], 2.0 * dc.u());
    Px { x, y, w, h }
}

fn map_image(dc: &mut Dc, h: &mut HudFacts) {
    let Some(map) = h.map.as_ref() else { return };
    let m = full_rect(dc, map);
    let img = dc.p.named(&dc.ui.assets, &map.material);
    dc.p.pic(&img, m, dc.color);
}

/// The frame around the full map: the border image is a two by two grid of corner pieces and edge slices.
fn map_border(dc: &mut Dc, h: &mut HudFacts) {
    let Some(map) = h.map.as_ref() else { return };
    let Some(img) = dc.bg() else { return };
    let m = full_rect(dc, map);
    let b = (2.0 * dc.u()).min(m.w * 0.5).min(m.h * 0.5);
    let w2 = b * 2.0;
    let (x0, y0, x1, y1) = (m.x - b, m.y - b, m.x + m.w - b, m.y + m.h - b);
    // (x, y, w, h, s0, t0, s1, t1)
    let pieces = [
        (x0, y1, w2, w2, 0.0, 0.5, 0.5, 1.0),
        (x0, y0, w2, w2, 0.0, 0.0, 0.5, 0.5),
        (x1, y1, w2, w2, 0.5, 0.5, 1.0, 1.0),
        (x1, y0, w2, w2, 0.5, 0.0, 1.0, 0.5),
        (x0, m.y + b, w2, m.h - w2, 0.0, 0.5, 0.5, 0.5),
        (x1, m.y + b, w2, m.h - w2, 0.5, 0.5, 1.0, 0.5),
        (m.x + b, y0, m.w - w2, w2, 0.5, 0.0, 0.5, 0.5),
        (m.x + b, y1, m.w - w2, w2, 0.5, 0.5, 0.5, 1.0),
    ];
    for (x, y, w, ht, s0, t0, s1, t1) in pieces {
        dc.p.g.quad(&img, [x, y, w, ht], [s0, t0, s1, t1], dc.color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ammunition_display_pads_and_hides_what_the_weapon_lacks() {
        assert_eq!(ammo_texts(30, 120), (Some("30".into()), Some("120".into())));
        assert_eq!(ammo_texts(5, 7), (Some(" 5".into()), Some("  7".into())));
        assert_eq!(ammo_texts(-1, 12), (None, Some(" 12".into())));
        assert_eq!(ammo_texts(8, -1), (Some(" 8".into()), None));
        assert_eq!(
            ammo_texts(2000, 5000),
            (Some("999".into()), Some("999".into()))
        );
    }

    #[test]
    fn the_sprint_bar_runs_from_amber_to_gray_and_goes_red_when_spent() {
        assert_eq!(sprint_color(100, 100, false), [0.8, 0.8, 0.8]);
        assert_eq!(sprint_color(0, 100, false), [0.8, 0.1, 0.1]);
        let half = sprint_color(50, 100, false);
        assert!((half[0] - 0.75).abs() < 1e-6 && (half[1] - 0.65).abs() < 1e-6);
        assert_eq!(sprint_color(0, 100, true), [0.8, 0.8, 0.8]);
    }

    #[test]
    fn an_icon_faces_the_way_the_player_it_marks_faces() {
        // Rotating compass: someone facing where I face points up, someone facing my left points left.
        assert_eq!(mark_angle(true, 90.0, 0.0, 90.0), 0.0);
        assert_eq!(mark_angle(true, 90.0, 0.0, 180.0), 270.0);
        // Fixed north-up map: someone facing north points up, facing west (north + 90) points left.
        assert_eq!(mark_angle(false, 0.0, 0.0, 0.0), 0.0);
        assert_eq!(mark_angle(false, 0.0, 0.0, 90.0), 270.0);
    }

    #[test]
    fn the_key_name_goes_where_the_string_asks_for_it() {
        assert_eq!(
            with_key("Hold &&1 to steady", "SHIFT"),
            "Hold SHIFT to steady"
        );
        assert_eq!(with_key("no key", "X"), "no key");
    }
}
