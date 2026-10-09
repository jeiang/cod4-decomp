// SPDX-License-Identifier: GPL-3.0-only
//! What the server tells a client's user interface, and the store the client reads it from.
//!
//! The client crate consumes this module only: it never parses a reliable command or a snapshot
//! itself. Three streams feed one [`ClientUiState`]:
//!
//! * **Reliable commands** ([`ServerCmd`], text on the existing reliable stream, in order):
//!   configstring updates, `setclientdvar`, `openmenu`/`closemenu`, print lines, announcements and
//!   chat. The ones the player must see happen in order become [`UiEvent`]s to
//!   [`ClientUiState::drain_events`]; the configstring table and the client dvars are state.
//! * **Snapshots**: the HUD elements script code made visible to this client ([`HudElem`], at
//!   most [`MAX_HUD_PER_GROUP`] archived and as many not archived, delta coded like entities) and
//!   the compass objectives ([`Objective`]).
//! * **Configstrings** ([`cs`]): the table of strings scripts refer to by index. `settext`
//!   stores a localized-string index, `setshader` a material index, and so on; the table turns
//!   them back into names ([`ClientUiState::localized`], [`ClientUiState::material`]).
//!
//! Typical frame: `net.pump(..)`, then `for ev in ui.drain_events() { menu runtime applies ev }`,
//! then draw `ui.hud()` sorted by `(sort, id)` at the interpolated server time using the
//! `*_at` helpers of [`HudElem`].
//!
//! Client to server, the only UI command is `menuresponse <menu name> <response>`
//! ([`menu_response`]); the server maps it to the script `menuresponse` notify.

use crate::bits::{BitReader, BitWriter, Overflow};
use crate::field::{Field, Kind, changed_count, read_delta, write_delta};
use std::collections::HashMap;
use std::sync::LazyLock;

/// What a chat line says about its speaker ([`ServerCmd::Chat`] `tag`): the original's `(Dead)` and `(Spectator)`.
pub mod chat_tag {
    pub const NORMAL: u8 = 0;
    pub const DEAD: u8 = 1;
    pub const SPECTATOR: u8 = 2;
}

/// Configstring layout: the index ranges the scripts and the client agree on. A string index a
/// script gives to `settext`/`setshader`/`openmenu` is `BASE + n`; 0 is "none" in every range, so
/// the first entry of a range is `BASE + 1`.
pub mod cs {
    pub const SERVERINFO: u16 = 0;
    /// The variables flagged `SYSTEMINFO` (`sv_cheats`, `sv_serverid`, ...).
    pub const SYSTEMINFO: u16 = 1;
    /// The movement tunables that are not stock, as an info string (`sim::pm::Params::info_diff`), so the client's
    /// prediction moves as the server does.
    pub const MOVEMENT: u16 = 2;
    /// Names of the variables flagged `CODINFO` and, from [`CODINFO_VALUE`], their values.
    pub const CODINFO: u16 = 20;
    pub const CODINFO_COUNT: u16 = 128;
    pub const CODINFO_VALUE: u16 = 148;
    pub const MESSAGE: u16 = 3;
    pub const SCORES_ALLIES: u16 = 4;
    pub const SCORES_AXIS: u16 = 5;
    pub const GAMEENDTIME: u16 = 11;
    /// The vote in progress: `<end server ms> <server id>` (empty when none), the shown text, the yes and no counts.
    pub const VOTE_TIME: u16 = 13;
    pub const VOTE_STRING: u16 = 14;
    pub const VOTE_YES: u16 = 15;
    pub const VOTE_NO: u16 = 16;
    /// Map name shown by `setmapnamestring` hud elements (`mapname` of the level).
    pub const MAPNAME: u16 = 17;
    /// Gametype shown by `setgametypestring` hud elements.
    pub const GAMETYPE: u16 = 18;
    pub const MULTI_MAPWINNER: u16 = 19;
    /// `setwinningplayer` / `setwinningteam` values.
    pub const WINNING_PLAYER: u16 = 20;
    pub const WINNING_TEAM: u16 = 21;
    pub const USE_TRIG_STRINGS: u16 = 277;
    pub const USE_TRIG_STRINGS_COUNT: u16 = 32;
    /// Localized strings: `precachestring` and `settext` text. Entry `n` is `LOCALIZED + n`.
    pub const LOCALIZED: u16 = 309;
    pub const LOCALIZED_COUNT: u16 = 512;
    pub const AMBIENT: u16 = 821;
    pub const NORTHYAW: u16 = 822;
    /// `"<material>" <upper left x> <y> <lower right x> <y>`, set by the map script's `setMiniMap`.
    pub const MINIMAP: u16 = 823;
    pub const MODELS: u16 = 830;
    pub const MODELS_COUNT: u16 = 512;
    /// Menus registered with `precachemenu`.
    pub const SCRIPT_MENUS: u16 = 1970;
    pub const SCRIPT_MENUS_COUNT: u16 = 32;
    /// Materials: `precacheshader`, `setshader`, objective icons, status and head icons.
    pub const MATERIALS: u16 = 2002;
    pub const MATERIALS_COUNT: u16 = 256;
    /// One `n\<name>\t\<team>\...` string per client slot (see [`client_info`]).
    pub const CLIENTINFO: u16 = 2315;
    pub const CLIENTINFO_COUNT: u16 = 64;
    /// One past the last index.
    pub const MAX: u16 = CLIENTINFO + CLIENTINFO_COUNT;
}

/// Hud elements one client sees: this many archived (kept in killcam replays) and this many not.
pub const MAX_HUD_PER_GROUP: usize = 31;
pub const MAX_OBJECTIVES: usize = 16;
/// "No entity" in [`HudElem::target_ent`] and [`Objective::entity`].
pub const NO_ENTITY: u16 = 1023;

/// [`HudElem::kind`] values (`he_type_t`).
pub mod he {
    pub const FREE: u8 = 0;
    pub const TEXT: u8 = 1;
    pub const VALUE: u8 = 2;
    /// `value` is the client number.
    pub const PLAYERNAME: u8 = 3;
    /// Draws configstring [`super::cs::MAPNAME`].
    pub const MAPNAME: u8 = 4;
    /// Draws the display name of the gametype in configstring [`super::cs::GAMETYPE`].
    pub const GAMETYPE: u8 = 5;
    pub const MATERIAL: u8 = 6;
    /// Counts down to `time` (server time, ms); shown as `m:ss`.
    pub const TIMER_DOWN: u8 = 7;
    pub const TIMER_UP: u8 = 8;
    /// As the timers, shown with a tenth of a second.
    pub const TENTHS_TIMER_DOWN: u8 = 9;
    pub const TENTHS_TIMER_UP: u8 = 10;
    /// A material drawn as a clock face over `duration` ms ending at `time`.
    pub const CLOCK_DOWN: u8 = 11;
    pub const CLOCK_UP: u8 = 12;
    /// 3D waypoint: `value` is the target; `offscreen_material` is drawn at the screen edge.
    pub const WAYPOINT: u8 = 13;
}

/// [`HudElem::flags`] bits.
pub mod hf {
    pub const FOREGROUND: u8 = 1;
    pub const HIDE_WHEN_DEAD: u8 = 2;
    pub const HIDE_WHEN_IN_MENU: u8 = 4;
    /// Shown in killcam and final killcam replays too.
    pub const ARCHIVED: u8 = 8;
}

/// `font` values, as the scripts name them.
pub const FONTS: [&str; 6] = [
    "default",
    "bigfixed",
    "smallfixed",
    "objective",
    "big",
    "small",
];
/// `alignx` names by [`HudElem::align_x`].
pub const ALIGN_X: [&str; 3] = ["left", "center", "right"];
/// `aligny` names by [`HudElem::align_y`].
pub const ALIGN_Y: [&str; 3] = ["top", "middle", "bottom"];
/// `horzalign` names by [`HudElem::horz_align`].
pub const HORZ_ALIGN: [&str; 8] = [
    "subleft",
    "left",
    "center",
    "right",
    "fullscreen",
    "noscale",
    "alignto640",
    "center_safearea",
];
/// `vertalign` names by [`HudElem::vert_align`].
pub const VERT_ALIGN: [&str; 8] = [
    "subtop",
    "top",
    "middle",
    "bottom",
    "fullscreen",
    "noscale",
    "alignto480",
    "center_safearea",
];

/// One script hud element as a client draws it (`hudelem_s` of the original). Positions are in
/// 640x480 virtual units placed by the alignments; times are server time in ms.
#[derive(Debug, Clone, PartialEq)]
pub struct HudElem {
    /// Slot in the server's table; the key of the snapshot delta, unique per server.
    pub id: u16,
    /// One of [`he`]; [`he::FREE`] never reaches a client.
    pub kind: u8,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// `settargetent`: draw at this entity ([`NO_ENTITY`] = none).
    pub target_ent: u16,
    pub font_scale: f32,
    /// Index into [`FONTS`].
    pub font: u8,
    /// `aligny | alignx << 2`; see [`Self::align_x`].
    pub align_org: u8,
    /// `vertalign | horzalign << 3`; see [`Self::horz_align`].
    pub align_screen: u8,
    /// RGBA, the value `fadeovertime` fades to.
    pub color: [u8; 4],
    /// RGBA at `fade_start`.
    pub from_color: [u8; 4],
    pub fade_start: i32,
    pub fade_time: i32,
    /// Localized-string index (`label`); the text has `&1` where the value goes.
    pub label: u16,
    pub width: u16,
    pub height: u16,
    /// Material index (`setshader`, `setclock`).
    pub material: u16,
    pub offscreen_material: u16,
    pub from_width: u16,
    pub from_height: u16,
    pub scale_start: i32,
    pub scale_time: i32,
    pub from_x: f32,
    pub from_y: f32,
    pub from_align_org: u8,
    pub from_align_screen: u8,
    pub move_start: i32,
    pub move_time: i32,
    /// Timer end, or the clock's end, server time.
    pub time: i32,
    pub duration: i32,
    pub value: f32,
    /// Localized-string index (`settext`).
    pub text: u16,
    pub sort: f32,
    pub glow_color: [u8; 4],
    /// `setpulsefx`: birth time, ms per letter, decay start and duration.
    pub fx_birth: i32,
    pub fx_letter: i32,
    pub fx_decay_start: i32,
    pub fx_decay_duration: i32,
    /// [`hf`] bits.
    pub flags: u8,
}

impl Default for HudElem {
    fn default() -> Self {
        Self {
            id: 0,
            kind: he::FREE,
            x: 0.0,
            y: 0.0,
            z: 0.0,
            target_ent: NO_ENTITY,
            font_scale: 0.0,
            font: 0,
            align_org: 0,
            align_screen: 0,
            color: [255; 4],
            from_color: [0; 4],
            fade_start: 0,
            fade_time: 0,
            label: 0,
            width: 0,
            height: 0,
            material: 0,
            offscreen_material: 0,
            from_width: 0,
            from_height: 0,
            scale_start: 0,
            scale_time: 0,
            from_x: 0.0,
            from_y: 0.0,
            from_align_org: 0,
            from_align_screen: 0,
            move_start: 0,
            move_time: 0,
            time: 0,
            duration: 0,
            value: 0.0,
            text: 0,
            sort: 0.0,
            glow_color: [0; 4],
            fx_birth: 0,
            fx_letter: 0,
            fx_decay_start: 0,
            fx_decay_duration: 0,
            flags: hf::ARCHIVED,
        }
    }
}

/// How far a timed change that started at `start` and takes `time` ms has got, 0..=1 (1 when it
/// never started).
fn progress(start: i32, time: i32, now: i32) -> f32 {
    if time <= 0 {
        return 1.0;
    }
    (now.wrapping_sub(start) as f32 / time as f32).clamp(0.0, 1.0)
}

impl HudElem {
    /// A fresh element with the original's defaults (`HudElem_SetDefaults`).
    pub fn new(id: u16) -> Self {
        Self {
            id,
            kind: he::TEXT,
            ..Self::default()
        }
    }

    /// 0 left, 1 center, 2 right.
    pub fn align_x(&self) -> u8 {
        (self.align_org >> 2) & 3
    }
    /// 0 top, 1 middle, 2 bottom.
    pub fn align_y(&self) -> u8 {
        self.align_org & 3
    }
    /// Index into [`HORZ_ALIGN`].
    pub fn horz_align(&self) -> u8 {
        (self.align_screen >> 3) & 7
    }
    /// Index into [`VERT_ALIGN`].
    pub fn vert_align(&self) -> u8 {
        self.align_screen & 7
    }
    pub fn set_align_x(&mut self, v: u8) {
        self.align_org = self.align_org & !(3 << 2) | (v & 3) << 2;
    }
    pub fn set_align_y(&mut self, v: u8) {
        self.align_org = self.align_org & !3 | (v & 3);
    }
    pub fn set_horz_align(&mut self, v: u8) {
        self.align_screen = self.align_screen & !(7 << 3) | (v & 7) << 3;
    }
    pub fn set_vert_align(&mut self, v: u8) {
        self.align_screen = self.align_screen & !7 | (v & 7);
    }

    pub fn archived(&self) -> bool {
        self.flags & hf::ARCHIVED != 0
    }

    /// Progress of `moveovertime` (0 = at `from_x`/`from_y` and their alignments, 1 = at `x`/`y`).
    pub fn move_progress(&self, now: i32) -> f32 {
        progress(self.move_start, self.move_time, now)
    }

    /// Progress of `fadeovertime`.
    pub fn fade_progress(&self, now: i32) -> f32 {
        progress(self.fade_start, self.fade_time, now)
    }

    /// Progress of `scaleovertime`.
    pub fn scale_progress(&self, now: i32) -> f32 {
        progress(self.scale_start, self.scale_time, now)
    }

    /// The color to draw at server time `now` (`BG_LerpHudColors`).
    pub fn color_at(&self, now: i32) -> [u8; 4] {
        lerp_rgba(self.from_color, self.color, self.fade_progress(now))
    }

    /// The `(width, height)` to draw at `now`.
    pub fn size_at(&self, now: i32) -> (f32, f32) {
        let t = self.scale_progress(now);
        let l = |a: u16, b: u16| f32::from(a) + (f32::from(b) - f32::from(a)) * t;
        (
            l(self.from_width, self.width),
            l(self.from_height, self.height),
        )
    }

    /// Seconds left (down timers) or elapsed (up timers) at `now`, never negative.
    pub fn timer_seconds(&self, now: i32) -> f32 {
        let ms = match self.kind {
            he::TIMER_UP | he::TENTHS_TIMER_UP | he::CLOCK_UP => now.wrapping_sub(self.time),
            _ => self.time.wrapping_sub(now),
        };
        ms.max(0) as f32 * 0.001
    }
}

fn lerp_rgba(from: [u8; 4], to: [u8; 4], t: f32) -> [u8; 4] {
    let mut out = [0; 4];
    for i in 0..4 {
        let (a, b) = (f32::from(from[i]), f32::from(to[i]));
        out[i] = (a + (b - a) * t).round() as u8;
    }
    out
}

fn pack(c: [u8; 4]) -> u32 {
    u32::from_le_bytes(c)
}

fn unpack(w: u32) -> [u8; 4] {
    w.to_le_bytes()
}

macro_rules! int {
    ($s:ident, $place:expr, $k:expr) => {
        Field::<HudElem> {
            get: |$s| $place as u32,
            set: |$s, v| $place = v as _,
            kind: $k,
        }
    };
}

macro_rules! num {
    ($s:ident, $place:expr) => {
        Field::<HudElem> {
            get: |$s| $place.to_bits(),
            set: |$s, v| $place = f32::from_bits(v),
            kind: Kind::Float,
        }
    };
}

macro_rules! rgba {
    ($s:ident, $place:expr) => {
        Field::<HudElem> {
            get: |$s| pack($place),
            set: |$s, v| $place = unpack(v),
            kind: Kind::Bits(32),
        }
    };
}

fn hud_table() -> Vec<Field<HudElem>> {
    use Kind::Bits;
    vec![
        int!(s, s.kind, Bits(4)),
        num!(s, s.x),
        num!(s, s.y),
        num!(s, s.z),
        int!(s, s.target_ent, Bits(10)),
        num!(s, s.font_scale),
        int!(s, s.font, Bits(3)),
        int!(s, s.align_org, Bits(4)),
        int!(s, s.align_screen, Bits(6)),
        rgba!(s, s.color),
        rgba!(s, s.from_color),
        int!(s, s.fade_start, Bits(32)),
        int!(s, s.fade_time, Bits(32)),
        int!(s, s.label, Bits(10)),
        int!(s, s.width, Bits(16)),
        int!(s, s.height, Bits(16)),
        int!(s, s.material, Bits(10)),
        int!(s, s.offscreen_material, Bits(10)),
        int!(s, s.from_width, Bits(16)),
        int!(s, s.from_height, Bits(16)),
        int!(s, s.scale_start, Bits(32)),
        int!(s, s.scale_time, Bits(32)),
        num!(s, s.from_x),
        num!(s, s.from_y),
        int!(s, s.from_align_org, Bits(4)),
        int!(s, s.from_align_screen, Bits(6)),
        int!(s, s.move_start, Bits(32)),
        int!(s, s.move_time, Bits(32)),
        int!(s, s.time, Bits(32)),
        int!(s, s.duration, Bits(32)),
        num!(s, s.value),
        int!(s, s.text, Bits(10)),
        num!(s, s.sort),
        rgba!(s, s.glow_color),
        int!(s, s.fx_birth, Bits(32)),
        int!(s, s.fx_letter, Bits(32)),
        int!(s, s.fx_decay_start, Bits(32)),
        int!(s, s.fx_decay_duration, Bits(32)),
        int!(s, s.flags, Bits(4)),
    ]
}

fn hud_fields() -> &'static [Field<HudElem>] {
    static T: LazyLock<Vec<Field<HudElem>>> = LazyLock::new(hud_table);
    &T
}

/// Highest [`HudElem::id`] plus one.
pub const MAX_HUD_IDS: usize = 1024;

/// Elements that changed, appeared or vanished, ascending by id: the gap from the previous id
/// plus one, a removal flag, and a field delta for the rest. A gap of 0 ends the list.
pub fn write_hud(w: &mut BitWriter, old: &[HudElem], new: &[HudElem]) {
    let (mut i, mut j) = (0, 0);
    let mut last = 0u32;
    let blank = HudElem::default();
    while i < old.len() || j < new.len() {
        let (oid, nid) = (
            old.get(i).map_or(u16::MAX, |e| e.id),
            new.get(j).map_or(u16::MAX, |e| e.id),
        );
        let id = oid.min(nid);
        let o = (oid == id).then(|| &old[i]);
        let n = (nid == id).then(|| &new[j]);
        i += usize::from(o.is_some());
        j += usize::from(n.is_some());
        match n {
            None => {
                w.write_uvar(u32::from(id) + 1 - last);
                w.write_bool(true);
            }
            Some(n) => {
                let from = o.unwrap_or(&blank);
                if o.is_some() && changed_count(hud_fields(), from, n) == 0 {
                    continue;
                }
                w.write_uvar(u32::from(id) + 1 - last);
                w.write_bool(false);
                write_delta(w, hud_fields(), from, n);
            }
        }
        last = u32::from(id) + 1;
    }
    w.write_uvar(0);
}

pub fn read_hud(r: &mut BitReader, old: &[HudElem]) -> Result<Vec<HudElem>, Overflow> {
    let mut out = Vec::with_capacity(old.len() + 4);
    let (mut i, mut last) = (0, 0u32);
    loop {
        let gap = r.read_uvar()?;
        let at = if gap == 0 {
            u32::MAX
        } else {
            last = last.checked_add(gap).ok_or(Overflow)?;
            if last as usize > MAX_HUD_IDS {
                return Err(Overflow);
            }
            last - 1
        };
        while i < old.len() && u32::from(old[i].id) < at {
            out.push(old[i].clone());
            i += 1;
        }
        if gap == 0 {
            return Ok(out);
        }
        let mut e = if i < old.len() && u32::from(old[i].id) == at {
            i += 1;
            old[i - 1].clone()
        } else {
            HudElem::default()
        };
        if r.read_bool()? {
            continue;
        }
        read_delta(r, hud_fields(), &mut e)?;
        e.id = at as u16;
        out.push(e);
    }
}

/// [`Objective::state`] values (`objective_state`).
pub mod obj {
    pub const EMPTY: u8 = 0;
    pub const ACTIVE: u8 = 1;
    pub const INVISIBLE: u8 = 2;
    pub const CURRENT: u8 = 3;
}

/// One compass / world objective icon (`objective_add` and friends).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Objective {
    /// One of [`obj`]; [`obj::EMPTY`] and [`obj::INVISIBLE`] are not drawn.
    pub state: u8,
    /// World position; for an `objective_onentity` objective the entity's position of the tick.
    pub origin: [f32; 3],
    /// The entity the objective follows ([`NO_ENTITY`] = none).
    pub entity: u16,
    /// Material index of the icon.
    pub icon: u16,
}

impl Objective {
    pub fn visible(&self) -> bool {
        matches!(self.state, obj::ACTIVE | obj::CURRENT)
    }

    fn canonical(mut self) -> Self {
        crate::field::canonicalize(obj_fields(), &mut self);
        self
    }
}

fn obj_fields() -> &'static [Field<Objective>] {
    static T: LazyLock<Vec<Field<Objective>>> = LazyLock::new(|| {
        macro_rules! pos {
            ($i:expr) => {
                Field::<Objective> {
                    get: |s| s.origin[$i].to_bits(),
                    set: |s, v| s.origin[$i] = f32::from_bits(v),
                    kind: Kind::Fixed {
                        bits: 22,
                        step: 1.0 / 16.0,
                    },
                }
            };
        }
        vec![
            Field {
                get: |s| u32::from(s.state),
                set: |s, v| s.state = v as u8,
                kind: Kind::Bits(2),
            },
            pos!(0),
            pos!(1),
            pos!(2),
            Field {
                get: |s| u32::from(s.entity),
                set: |s, v| s.entity = v as u16,
                kind: Kind::Bits(10),
            },
            Field {
                get: |s| u32::from(s.icon),
                set: |s, v| s.icon = v as u16,
                kind: Kind::Bits(10),
            },
        ]
    });
    &T
}

/// Rounds the objectives as the wire does, so a sender's copy equals the receiver's.
pub fn canonical_objectives(o: [Objective; MAX_OBJECTIVES]) -> [Objective; MAX_OBJECTIVES] {
    o.map(Objective::canonical)
}

pub fn write_objectives(
    w: &mut BitWriter,
    old: &[Objective; MAX_OBJECTIVES],
    new: &[Objective; MAX_OBJECTIVES],
) {
    for (o, n) in old.iter().zip(new) {
        let changed = changed_count(obj_fields(), o, n) != 0;
        w.write_bool(changed);
        if changed {
            write_delta(w, obj_fields(), o, n);
        }
    }
}

pub fn read_objectives(
    r: &mut BitReader,
    base: &mut [Objective; MAX_OBJECTIVES],
) -> Result<(), Overflow> {
    for o in base.iter_mut() {
        if r.read_bool()? {
            read_delta(r, obj_fields(), o)?;
        }
    }
    Ok(())
}

// ---- reliable commands ---------------------------------------------------------------------

/// How a line from `iprintln`, `iprintlnbold` or `clientprint` is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintKind {
    /// Console only (`clientprint`).
    Console,
    /// The small feed at the top left (`iprintln`).
    Normal,
    /// The large centered message (`iprintlnbold`).
    Bold,
}

/// A command the server sends to one client's UI.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerCmd {
    /// A new level: every configstring, hud element and objective of the old one is void and the
    /// full configstring set follows. The client loads this map.
    Map {
        name: String,
    },
    /// Configstring updates, `(index, string)`; an empty string clears the entry.
    ConfigStrings(Vec<(u16, String)>),
    /// `setclientdvar`: set a dvar on the client.
    SetDvar {
        name: String,
        value: String,
    },
    /// `openmenu` (`mouse`) / `openmenunomouse`: a menu registered with `precachemenu`.
    OpenMenu {
        name: String,
        mouse: bool,
    },
    /// `closemenu`: the open script menu; `name` empty when the script gave none.
    CloseMenu {
        name: String,
    },
    /// `closeingamemenu`: the escape menu.
    CloseIngameMenu,
    Print {
        kind: PrintKind,
        text: String,
    },
    /// `announcement` / `clientannouncement`: the large centered game message.
    Announce {
        text: String,
    },
    /// A chat line (`sayall`/`sayteam` and what players type). `client` is the speaker's slot.
    Chat {
        team: bool,
        client: u16,
        /// The speaker's state when the line was said ([`chat_tag`]).
        tag: u8,
        text: String,
    },
    /// Scoreboard rows (see [`Scoreboard`]): the rows `start..start + rows.len()` of `total`, the
    /// team scores and the score limit. Sent when the client asks ([`SCORES_REQUEST`]) and by the
    /// script `showscoreboard`.
    Scores {
        axis: i32,
        allies: i32,
        limit: i32,
        start: u16,
        total: u16,
        rows: Vec<ScoreRow>,
    },
    /// A kill, for the obituary feed and the kill icons.
    Obituary(Obituary),
    /// `setstat`: this client's persistent stat `index` is now `value` (what menus read with
    /// `stat(index)`).
    Stat {
        index: i32,
        value: i32,
    },
    /// `visionsetnaked` / `visionsetnight`: the named vision file (`vision/<name>.vision`) becomes the picture's
    /// glow and film, blended over `ms`. The renderer, not the interface, acts on it.
    Vision {
        night: bool,
        name: String,
        ms: i32,
    },
    /// `shellshock` on a player: the named shell shock (`shellshock/<name>.shock`) for `ms`; an empty name ends it.
    ShellShock {
        name: String,
        ms: i32,
    },
}

/// The client to server command that asks for the scoreboard; repeat it every couple of seconds
/// while the scoreboard is up.
pub const SCORES_REQUEST: &str = "score";

/// A player's [`ScoreRow::state`].
pub mod pstate {
    pub const PLAYING: u8 = 0;
    pub const DEAD: u8 = 1;
    pub const SPECTATING: u8 = 2;
}

/// One scoreboard line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScoreRow {
    pub client: u16,
    pub score: i32,
    /// Milliseconds; -1 for a bot.
    pub ping: i32,
    pub deaths: i32,
    pub kills: i32,
    pub assists: i32,
    /// Material index of the status icon scripts set (`ui.material(n)`), 0 = none.
    pub status_icon: u16,
    /// 0 free, 1 axis, 2 allies, 3 spectator.
    pub team: u8,
    /// [`pstate`].
    pub state: u8,
}

impl ScoreRow {
    fn word(&self) -> String {
        format!(
            "{},{},{},{},{},{},{},{},{}",
            self.client,
            self.score,
            self.ping,
            self.deaths,
            self.kills,
            self.assists,
            self.status_icon,
            self.team,
            self.state
        )
    }

    fn from_word(w: &str) -> Option<Self> {
        let mut it = w.split(',');
        let mut n = || it.next()?.parse::<i32>().ok();
        Some(Self {
            client: u16::try_from(n()?).ok()?,
            score: n()?,
            ping: n()?,
            deaths: n()?,
            kills: n()?,
            assists: n()?,
            status_icon: u16::try_from(n()?).ok()?,
            team: u8::try_from(n()?).ok()?,
            state: u8::try_from(n()?).ok()?,
        })
    }
}

/// A kill (`player_die`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Obituary {
    /// The killing client, [`NO_ENTITY`] when the world or a non-player did it.
    pub killer: u16,
    pub victim: u16,
    /// Weapon name as scripts give it (`ak47_mp`), empty for none.
    pub weapon: String,
    /// `MOD_RIFLE_BULLET`, `MOD_SUICIDE`, ...
    pub mean: String,
    pub headshot: bool,
}

impl Obituary {
    pub fn suicide(&self) -> bool {
        self.killer == self.victim || self.killer == NO_ENTITY
    }
}

/// The scoreboard as the newest complete [`ServerCmd::Scores`] left it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scoreboard {
    pub axis: i32,
    pub allies: i32,
    pub limit: i32,
    /// In the order the server sent them (best first).
    pub rows: Vec<ScoreRow>,
}

fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn words(s: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut it = s.chars().peekable();
    loop {
        while it.next_if(|c| *c == ' ').is_some() {}
        let Some(&c) = it.peek() else {
            return Some(out);
        };
        let mut w = String::new();
        if c == '"' {
            it.next();
            loop {
                match it.next()? {
                    '"' => break,
                    '\\' => w.push(match it.next()? {
                        'n' => '\n',
                        c => c,
                    }),
                    c => w.push(c),
                }
            }
        } else {
            while let Some(c) = it.next_if(|c| *c != ' ') {
                w.push(c);
            }
        }
        out.push(w);
    }
}

impl PrintKind {
    fn word(self) -> &'static str {
        match self {
            PrintKind::Console => "console",
            PrintKind::Normal => "normal",
            PrintKind::Bold => "bold",
        }
    }
    fn from_word(s: &str) -> Option<Self> {
        Some(match s {
            "console" => PrintKind::Console,
            "normal" => PrintKind::Normal,
            "bold" => PrintKind::Bold,
            _ => return None,
        })
    }
}

impl ServerCmd {
    /// The reliable command string.
    pub fn encode(&self) -> String {
        let mut s = String::new();
        let arg = |s: &mut String, a: &str| {
            s.push(' ');
            quote(a, s);
        };
        match self {
            ServerCmd::Map { name } => {
                s.push_str("map");
                arg(&mut s, name);
            }
            ServerCmd::ConfigStrings(v) => {
                s.push_str("cs");
                for (i, t) in v {
                    arg(&mut s, &i.to_string());
                    arg(&mut s, t);
                }
            }
            ServerCmd::SetDvar { name, value } => {
                s.push_str("dvar");
                arg(&mut s, name);
                arg(&mut s, value);
            }
            ServerCmd::OpenMenu { name, mouse } => {
                s.push_str("openmenu");
                arg(&mut s, name);
                arg(&mut s, if *mouse { "1" } else { "0" });
            }
            ServerCmd::CloseMenu { name } => {
                s.push_str("closemenu");
                arg(&mut s, name);
            }
            ServerCmd::CloseIngameMenu => s.push_str("closeingame"),
            ServerCmd::Print { kind, text } => {
                s.push_str("print");
                arg(&mut s, kind.word());
                arg(&mut s, text);
            }
            ServerCmd::Announce { text } => {
                s.push_str("announce");
                arg(&mut s, text);
            }
            ServerCmd::Chat {
                team,
                client,
                tag,
                text,
            } => {
                s.push_str("chat");
                arg(&mut s, if *team { "team" } else { "all" });
                arg(&mut s, &client.to_string());
                arg(&mut s, &tag.to_string());
                arg(&mut s, text);
            }
            ServerCmd::Scores {
                axis,
                allies,
                limit,
                start,
                total,
                rows,
            } => {
                s.push_str("scores");
                for v in [i32::from(*start), i32::from(*total), *axis, *allies, *limit] {
                    arg(&mut s, &v.to_string());
                }
                for r in rows {
                    arg(&mut s, &r.word());
                }
            }
            ServerCmd::Obituary(o) => {
                s.push_str("obit");
                arg(&mut s, &o.killer.to_string());
                arg(&mut s, &o.victim.to_string());
                arg(&mut s, &o.weapon);
                arg(&mut s, &o.mean);
                arg(&mut s, if o.headshot { "1" } else { "0" });
            }
            ServerCmd::Stat { index, value } => {
                s.push_str("stat");
                arg(&mut s, &index.to_string());
                arg(&mut s, &value.to_string());
            }
            ServerCmd::Vision { night, name, ms } => {
                s.push_str("vision");
                arg(&mut s, if *night { "night" } else { "naked" });
                arg(&mut s, name);
                arg(&mut s, &ms.to_string());
            }
            ServerCmd::ShellShock { name, ms } => {
                s.push_str("shellshock");
                arg(&mut s, name);
                arg(&mut s, &ms.to_string());
            }
        }
        s
    }

    /// `None` for a string that is not a UI command (the caller keeps it).
    pub fn parse(line: &str) -> Option<Self> {
        let w = words(line)?;
        let a = |i: usize| w.get(i).map(String::as_str);
        Some(match (a(0)?, w.len()) {
            ("map", 2) => ServerCmd::Map { name: w[1].clone() },
            ("cs", n) if n >= 3 && n % 2 == 1 => ServerCmd::ConfigStrings(
                w[1..]
                    .chunks(2)
                    .map(|p| Some((p[0].parse().ok()?, p[1].clone())))
                    .collect::<Option<_>>()?,
            ),
            ("dvar", 3) => ServerCmd::SetDvar {
                name: w[1].clone(),
                value: w[2].clone(),
            },
            ("openmenu", 3) => ServerCmd::OpenMenu {
                name: w[1].clone(),
                mouse: a(2)? == "1",
            },
            ("closemenu", 2) => ServerCmd::CloseMenu { name: w[1].clone() },
            ("closeingame", 1) => ServerCmd::CloseIngameMenu,
            ("print", 3) => ServerCmd::Print {
                kind: PrintKind::from_word(a(1)?)?,
                text: w[2].clone(),
            },
            ("announce", 2) => ServerCmd::Announce { text: w[1].clone() },
            ("scores", n) if n >= 6 => {
                let num = |i: usize| w[i].parse::<i32>().ok();
                ServerCmd::Scores {
                    start: u16::try_from(num(1)?).ok()?,
                    total: u16::try_from(num(2)?).ok()?,
                    axis: num(3)?,
                    allies: num(4)?,
                    limit: num(5)?,
                    rows: w[6..]
                        .iter()
                        .map(|r| ScoreRow::from_word(r))
                        .collect::<Option<_>>()?,
                }
            }
            ("obit", 6) => ServerCmd::Obituary(Obituary {
                killer: w[1].parse().ok()?,
                victim: w[2].parse().ok()?,
                weapon: w[3].clone(),
                mean: w[4].clone(),
                headshot: w[5] == "1",
            }),
            ("stat", 3) => ServerCmd::Stat {
                index: w[1].parse().ok()?,
                value: w[2].parse().ok()?,
            },
            ("vision", 4) => ServerCmd::Vision {
                night: a(1)? == "night",
                name: w[2].clone(),
                ms: w[3].parse().ok()?,
            },
            ("shellshock", 3) => ServerCmd::ShellShock {
                name: w[1].clone(),
                ms: w[2].parse().ok()?,
            },
            ("chat", 5) => ServerCmd::Chat {
                team: a(1)? == "team",
                client: a(2)?.parse().ok()?,
                tag: a(3)?.parse().ok()?,
                text: w[4].clone(),
            },
            _ => return None,
        })
    }
}

/// The client to server command a menu sends for a click (`scriptMenuResponse`): the server
/// notifies the player's scripts with `menuresponse` and these two strings.
pub fn menu_response(menu: &str, response: &str) -> String {
    let mut s = String::from("menuresponse");
    s.push(' ');
    quote(menu, &mut s);
    s.push(' ');
    quote(response, &mut s);
    s
}

/// A scripted player's answers to the menus the server opens, for the autoplay of the game client
/// and for headless test clients: pick a team, then cycle through the stock classes. Feed it every
/// [`UiEvent`]; send what it returns with [`crate::client::NetClient::command`].
#[derive(Debug, Clone, Default)]
pub struct AutoJoin {
    classes: usize,
}

/// The stock classes a script `menuresponse "changeclass"` accepts.
pub const STOCK_CLASSES: [&str; 5] = [
    "assault_mp",
    "specops_mp",
    "heavygunner_mp",
    "demolitions_mp",
    "sniper_mp",
];

impl AutoJoin {
    /// The command answering `ev`, if it is one of the join menus.
    pub fn step(&mut self, ev: &UiEvent) -> Option<String> {
        let UiEvent::OpenMenu { name, .. } = ev else {
            return None;
        };
        if name.starts_with("team_") {
            Some(menu_response(name, "autoassign"))
        } else if name.starts_with("changeclass") {
            self.classes += 1;
            Some(menu_response(
                "changeclass",
                STOCK_CLASSES[self.classes % STOCK_CLASSES.len()],
            ))
        } else {
            None
        }
    }

    /// Answers the join menus among `events` and returns the answers and the events the screen still shows.
    /// A scripted player never sees the server's menus. A person's `--listen`/`--connect` session answers only the
    /// first team and class menus of a level, so the join is quick; after that the menus are the player's own
    /// (Escape's Choose Class and Change Team open the server's menus, and Escape closes them again). A new level
    /// starts the join over.
    pub fn filter(&mut self, events: Vec<UiEvent>, scripted: bool) -> (Vec<String>, Vec<UiEvent>) {
        let (mut answers, mut shown) = (Vec::new(), Vec::new());
        for ev in events {
            if matches!(ev, UiEvent::Map { .. }) {
                self.classes = 0;
            }
            let joining = self.classes == 0;
            answers.extend(self.step(&ev).filter(|_| scripted || joining));
            let menu = matches!(
                ev,
                UiEvent::OpenMenu { .. } | UiEvent::CloseMenu { .. } | UiEvent::CloseIngameMenu
            );
            if !(menu && (scripted || joining)) {
                shown.push(ev);
            }
        }
        (answers, shown)
    }
}

/// The parts of a [`cs::CLIENTINFO`] string.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClientInfo {
    pub name: String,
    /// 0 free, 1 axis, 2 allies, 3 spectator.
    pub team: u8,
    /// Live score, kills and deaths (`updatescores` keeps the engine's copy current; here it is always current).
    pub score: i32,
    pub kills: i32,
    pub deaths: i32,
    /// `setrank`: row of `mp/rankIconTable.csv` and the prestige column offset.
    pub rank: u8,
    pub prestige: u8,
}

/// Builds the `n\<name>\t\<team>` string for a client slot.
pub fn client_info_string(info: &ClientInfo) -> String {
    format!(
        "n\\{}\\t\\{}\\s\\{}\\k\\{}\\d\\{}\\r\\{}\\p\\{}",
        info.name.replace('\\', ""),
        info.team,
        info.score,
        info.kills,
        info.deaths,
        info.rank,
        info.prestige
    )
}

/// Parses a [`cs::CLIENTINFO`] string; `None` for an empty slot.
pub fn client_info(s: &str) -> Option<ClientInfo> {
    if s.is_empty() {
        return None;
    }
    let mut it = s.split('\\');
    let mut out = ClientInfo::default();
    while let (Some(k), Some(v)) = (it.next(), it.next()) {
        match k {
            "n" => out.name = v.to_owned(),
            "t" => out.team = v.parse().unwrap_or(0),
            "s" => out.score = v.parse().unwrap_or(0),
            "k" => out.kills = v.parse().unwrap_or(0),
            "d" => out.deaths = v.parse().unwrap_or(0),
            "r" => out.rank = v.parse().unwrap_or(0),
            "p" => out.prestige = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    Some(out)
}

// ---- client store --------------------------------------------------------------------------

/// Something the player's screen reacts to, in the order the server sent it.
#[derive(Debug, Clone, PartialEq)]
pub enum UiEvent {
    /// Load this map; the configstring table was reset just before.
    Map {
        name: String,
    },
    SetDvar {
        name: String,
        value: String,
    },
    OpenMenu {
        name: String,
        mouse: bool,
    },
    CloseMenu {
        name: String,
    },
    CloseIngameMenu,
    Print {
        kind: PrintKind,
        text: String,
    },
    Announce {
        text: String,
    },
    Chat {
        team: bool,
        client: u16,
        tag: u8,
        text: String,
    },
    Obituary(Obituary),
    /// A complete scoreboard arrived; read it with [`ClientUiState::scoreboard`].
    Scores,
}

/// Everything the UI knows about the server's state, fed by [`crate::ClientLink`]. Pure data:
/// no window, no GPU.
#[derive(Debug, Clone)]
pub struct ClientUiState {
    cs: Vec<String>,
    events: Vec<UiEvent>,
    dvars: HashMap<String, String>,
    hud: Vec<HudElem>,
    objectives: [Objective; MAX_OBJECTIVES],
    server_time: i32,
    scores: Scoreboard,
    /// Rows of a multi-part scoreboard received so far.
    scores_partial: Vec<ScoreRow>,
    stats: HashMap<i32, i32>,
}

impl Default for ClientUiState {
    fn default() -> Self {
        Self {
            cs: vec![String::new(); usize::from(cs::MAX)],
            events: Vec::new(),
            dvars: HashMap::new(),
            hud: Vec::new(),
            objectives: [Objective::default(); MAX_OBJECTIVES],
            server_time: 0,
            scores: Scoreboard::default(),
            scores_partial: Vec::new(),
            stats: HashMap::new(),
        }
    }
}

impl ClientUiState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Events kept for a client that never drains them.
    const MAX_EVENTS: usize = 2048;

    fn push(&mut self, ev: UiEvent) {
        if self.events.len() >= Self::MAX_EVENTS {
            self.events.remove(0);
        }
        self.events.push(ev);
    }

    /// Applies a reliable command.
    pub fn apply(&mut self, cmd: ServerCmd) {
        match cmd {
            ServerCmd::Map { name } => {
                self.scores = Scoreboard::default();
                self.stats.clear();
                self.cs.iter_mut().for_each(String::clear);
                self.hud.clear();
                self.objectives = Default::default();
                self.push(UiEvent::Map { name });
            }
            ServerCmd::ConfigStrings(v) => {
                for (i, s) in v {
                    if let Some(slot) = self.cs.get_mut(usize::from(i)) {
                        *slot = s;
                    }
                }
            }
            ServerCmd::SetDvar { name, value } => {
                self.dvars.insert(name.to_ascii_lowercase(), value.clone());
                self.push(UiEvent::SetDvar { name, value });
            }
            ServerCmd::OpenMenu { name, mouse } => {
                self.push(UiEvent::OpenMenu { name, mouse });
            }
            ServerCmd::CloseMenu { name } => self.push(UiEvent::CloseMenu { name }),
            ServerCmd::CloseIngameMenu => self.push(UiEvent::CloseIngameMenu),
            ServerCmd::Print { kind, text } => self.push(UiEvent::Print { kind, text }),
            ServerCmd::Announce { text } => self.push(UiEvent::Announce { text }),
            ServerCmd::Chat {
                team,
                client,
                tag,
                text,
            } => {
                self.push(UiEvent::Chat {
                    team,
                    client,
                    tag,
                    text,
                });
            }
            ServerCmd::Obituary(o) => self.push(UiEvent::Obituary(o)),
            ServerCmd::Stat { index, value } => {
                self.stats.insert(index, value);
            }
            // The renderer's, taken out of the stream before the interface sees it.
            ServerCmd::Vision { .. } | ServerCmd::ShellShock { .. } => {}
            ServerCmd::Scores {
                axis,
                allies,
                limit,
                start,
                total,
                rows,
            } => {
                if start == 0 {
                    self.scores_partial.clear();
                }
                if usize::from(start) == self.scores_partial.len() {
                    self.scores_partial.extend(rows);
                }
                if self.scores_partial.len() >= usize::from(total) {
                    self.scores = Scoreboard {
                        axis,
                        allies,
                        limit,
                        rows: std::mem::take(&mut self.scores_partial),
                    };
                    self.push(UiEvent::Scores);
                }
            }
        }
    }

    /// The commands that bring a fresh `ClientUiState` to this one's state on level `map`: the level, every
    /// configstring, the stats and the client dvars. A demo begins with them so it can start mid-match.
    pub fn state_commands(&self, map: &str) -> Vec<ServerCmd> {
        let mut out = vec![ServerCmd::Map {
            name: map.to_owned(),
        }];
        let strings: Vec<(u16, String)> = self
            .cs
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.is_empty())
            .map(|(i, s)| (i as u16, s.clone()))
            .collect();
        if !strings.is_empty() {
            out.push(ServerCmd::ConfigStrings(strings));
        }
        let mut stats: Vec<_> = self.stats.iter().collect();
        stats.sort();
        out.extend(
            stats
                .into_iter()
                .map(|(&index, &value)| ServerCmd::Stat { index, value }),
        );
        let mut dvars: Vec<_> = self.dvars.iter().collect();
        dvars.sort();
        out.extend(dvars.into_iter().map(|(n, v)| ServerCmd::SetDvar {
            name: n.clone(),
            value: v.clone(),
        }));
        out
    }

    /// Takes the HUD and objectives of a snapshot (the newest one wins).
    pub fn apply_snapshot(&mut self, snap: &crate::Snapshot) {
        self.server_time = snap.server_time;
        self.hud.clone_from(&snap.hud);
        self.objectives = snap.objectives;
    }

    /// The events since the last call, oldest first. Apply them in order: a `SetDvar` may
    /// precede the `OpenMenu` that reads it.
    pub fn drain_events(&mut self) -> Vec<UiEvent> {
        std::mem::take(&mut self.events)
    }

    pub fn has_events(&self) -> bool {
        !self.events.is_empty()
    }

    /// The newest complete scoreboard (empty until the first [`UiEvent::Scores`]).
    pub fn scoreboard(&self) -> &Scoreboard {
        &self.scores
    }

    /// Every persistent stat the server has said so far, by index.
    pub fn stats(&self) -> &HashMap<i32, i32> {
        &self.stats
    }

    /// Persistent stat `index` as the server last said (0 when never set).
    pub fn stat(&self, index: i32) -> i32 {
        self.stats.get(&index).copied().unwrap_or(0)
    }

    /// The team scores the server keeps in configstrings, `(allies, axis)`; always current,
    /// no request needed.
    pub fn team_scores(&self) -> (i32, i32) {
        let n = |i| self.config(i).parse().unwrap_or(0);
        (n(cs::SCORES_ALLIES), n(cs::SCORES_AXIS))
    }

    /// Server time of the newest snapshot applied.
    pub fn server_time(&self) -> i32 {
        self.server_time
    }

    /// The visible hud elements of the newest snapshot, ascending by id. Draw ordered by
    /// `(sort, id)`; elements with [`hf::FOREGROUND`] after the rest.
    pub fn hud(&self) -> &[HudElem] {
        &self.hud
    }

    pub fn objectives(&self) -> &[Objective; MAX_OBJECTIVES] {
        &self.objectives
    }

    /// The configstring at `index` (empty when unset or out of range).
    pub fn config(&self, index: u16) -> &str {
        self.cs.get(usize::from(index)).map_or("", String::as_str)
    }

    fn ranged(&self, base: u16, count: u16, n: u16) -> &str {
        if n == 0 || n >= count {
            ""
        } else {
            self.config(base + n)
        }
    }

    /// Text of a localized-string index (`HudElem::text`, `HudElem::label`).
    pub fn localized(&self, n: u16) -> &str {
        self.ranged(cs::LOCALIZED, cs::LOCALIZED_COUNT, n)
    }

    /// Name of a material index (`HudElem::material`, `Objective::icon`).
    pub fn material(&self, n: u16) -> &str {
        self.ranged(cs::MATERIALS, cs::MATERIALS_COUNT, n)
    }

    /// Name of a menu index registered by `precachemenu`.
    pub fn menu(&self, n: u16) -> &str {
        self.ranged(cs::SCRIPT_MENUS, cs::SCRIPT_MENUS_COUNT, n)
    }

    /// Name of a model index (`EntityState::model`).
    pub fn model(&self, n: u16) -> &str {
        self.ranged(cs::MODELS, cs::MODELS_COUNT, n)
    }

    /// Every menu the scripts registered.
    pub fn menus(&self) -> impl Iterator<Item = &str> {
        (1..cs::SCRIPT_MENUS_COUNT)
            .map(|n| self.menu(n))
            .filter(|s| !s.is_empty())
    }

    /// Every material the scripts registered, with its index.
    pub fn materials(&self) -> impl Iterator<Item = (u16, &str)> {
        (1..cs::MATERIALS_COUNT)
            .map(|n| (n, self.material(n)))
            .filter(|(_, s)| !s.is_empty())
    }

    /// Who client `n` is (name and team), if that slot is in use.
    pub fn client(&self, n: u16) -> Option<ClientInfo> {
        if n >= cs::CLIENTINFO_COUNT {
            return None;
        }
        client_info(self.config(cs::CLIENTINFO + n))
    }

    /// The latest value the server set with `setclientdvar` (case-insensitive name).
    pub fn dvar(&self, name: &str) -> Option<&str> {
        self.dvars
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Snapshot;

    fn samples() -> Vec<ServerCmd> {
        vec![
            ServerCmd::Vision {
                night: true,
                name: "mp_crash".into(),
                ms: 1500,
            },
            ServerCmd::ShellShock {
                name: "concussion_grenade_mp".into(),
                ms: 4000,
            },
            ServerCmd::ShellShock {
                name: String::new(),
                ms: 0,
            },
            ServerCmd::Map {
                name: "mp_crash".into(),
            },
            ServerCmd::ConfigStrings(vec![(309, "Hello \"you\"".into()), (310, String::new())]),
            ServerCmd::SetDvar {
                name: "ui_xpText".into(),
                value: "1".into(),
            },
            ServerCmd::OpenMenu {
                name: "team_marinesopfor".into(),
                mouse: true,
            },
            ServerCmd::OpenMenu {
                name: "scoreboard".into(),
                mouse: false,
            },
            ServerCmd::CloseMenu {
                name: String::new(),
            },
            ServerCmd::CloseIngameMenu,
            ServerCmd::Print {
                kind: PrintKind::Bold,
                text: "a\\b\nc \"d\"".into(),
            },
            ServerCmd::Announce {
                text: "MP_FIGHT".into(),
            },
            ServerCmd::Chat {
                team: true,
                client: 7,
                tag: chat_tag::DEAD,
                text: "  spaced  out ".into(),
            },
        ]
    }

    #[test]
    fn scoreboard_chunks_obituaries_and_stats_apply() {
        let rows = |r: std::ops::Range<u16>| -> Vec<ScoreRow> {
            r.map(|c| ScoreRow {
                client: c,
                score: i32::from(c) * 10 - 5,
                ping: -1,
                deaths: 2,
                kills: 3,
                assists: 1,
                status_icon: 4,
                team: 2,
                state: pstate::DEAD,
            })
            .collect()
        };
        let mut ui = ClientUiState::new();
        for (start, r) in [(0, rows(0..3)), (3, rows(3..5))] {
            let c = ServerCmd::Scores {
                axis: 7,
                allies: 9,
                limit: 75,
                start,
                total: 5,
                rows: r,
            };
            assert_eq!(ServerCmd::parse(&c.encode()), Some(c.clone()));
            ui.apply(c);
            // Only the last chunk completes it.
            assert_eq!(ui.has_events(), start == 3);
        }
        assert_eq!(ui.drain_events(), [UiEvent::Scores]);
        assert_eq!(ui.scoreboard().rows, rows(0..5));
        assert_eq!((ui.scoreboard().axis, ui.scoreboard().limit), (7, 75));
        let o = Obituary {
            killer: 3,
            victim: 5,
            weapon: "ak47_mp".into(),
            mean: "MOD_HEAD_SHOT".into(),
            headshot: true,
        };
        assert!(!o.suicide());
        ui.apply(ServerCmd::Obituary(o.clone()));
        assert_eq!(ui.drain_events(), [UiEvent::Obituary(o)]);
        ui.apply(ServerCmd::Stat {
            index: 205,
            value: 3,
        });
        assert_eq!((ui.stat(205), ui.stat(206)), (3, 0));
        ui.apply(ServerCmd::ConfigStrings(vec![(
            cs::SCORES_AXIS,
            "12".into(),
        )]));
        assert_eq!(ui.team_scores(), (0, 12));
    }

    #[test]
    fn every_command_round_trips_through_its_string() {
        for c in samples() {
            let s = c.encode();
            assert!(!s.contains('\n'), "{s:?}");
            assert_eq!(ServerCmd::parse(&s), Some(c), "{s}");
        }
    }

    #[test]
    fn strings_that_are_not_ui_commands_are_left_alone() {
        for s in [
            "",
            "cmd 12",
            "map",
            "cs 1",
            "cs x y",
            "print loud x",
            "openmenu \"a",
            "dvar a",
        ] {
            assert_eq!(ServerCmd::parse(s), None, "{s:?}");
        }
    }

    #[test]
    fn state_applies_commands_in_order_and_resolves_indices() {
        let mut ui = ClientUiState::new();
        ui.apply(ServerCmd::ConfigStrings(vec![
            (cs::LOCALIZED + 3, "MP_WAITING".into()),
            (cs::MATERIALS + 2, "white".into()),
            (cs::SCRIPT_MENUS + 1, "team_marinesopfor".into()),
            (
                cs::CLIENTINFO + 5,
                client_info_string(&ClientInfo {
                    name: "Ann".into(),
                    team: 2,
                    score: -3,
                    kills: 4,
                    deaths: 5,
                    rank: 6,
                    prestige: 1,
                }),
            ),
        ]));
        ui.apply(ServerCmd::SetDvar {
            name: "g_scriptMainMenu".into(),
            value: "class".into(),
        });
        ui.apply(ServerCmd::OpenMenu {
            name: "team_marinesopfor".into(),
            mouse: true,
        });
        assert_eq!(ui.localized(3), "MP_WAITING");
        assert_eq!(ui.localized(0), "");
        assert_eq!(ui.material(2), "white");
        assert_eq!(ui.menu(1), "team_marinesopfor");
        assert_eq!(ui.menus().collect::<Vec<_>>(), ["team_marinesopfor"]);
        assert_eq!(ui.dvar("G_SCRIPTMAINMENU"), Some("class"));
        assert_eq!(
            ui.client(5),
            Some(ClientInfo {
                name: "Ann".into(),
                team: 2,
                score: -3,
                kills: 4,
                deaths: 5,
                rank: 6,
                prestige: 1,
            })
        );
        assert_eq!(ui.client(6), None);
        let ev = ui.drain_events();
        assert!(matches!(ev[0], UiEvent::SetDvar { .. }));
        assert!(matches!(ev[1], UiEvent::OpenMenu { .. }));
        assert!(ui.drain_events().is_empty());
        // A new map empties the table but the events of the old one stay for the client.
        ui.apply(ServerCmd::Map {
            name: "mp_x".into(),
        });
        assert_eq!(ui.localized(3), "");
        assert_eq!(
            ui.drain_events(),
            [UiEvent::Map {
                name: "mp_x".into()
            }]
        );
    }

    fn elem(id: u16, f: impl FnOnce(&mut HudElem)) -> HudElem {
        let mut e = HudElem::new(id);
        f(&mut e);
        e
    }

    #[test]
    fn hud_lists_delta_code_adds_changes_and_removals() {
        let a = vec![
            elem(3, |e| {
                e.x = 320.0;
                e.y = -12.5;
                e.text = 9;
                e.color = [10, 20, 30, 255];
            }),
            elem(40, |e| {
                e.kind = he::TIMER_DOWN;
                e.time = 123_456;
            }),
        ];
        let b = vec![
            a[0].clone(),
            elem(41, |e| e.kind = he::MATERIAL),
            elem(900, |e| e.font_scale = 1.6),
        ];
        let c = vec![elem(3, |e| {
            e.text = 10;
            e.move_start = 5;
            e.move_time = 700;
            e.from_x = 0.5;
        })];
        let mut w = BitWriter::new();
        write_hud(&mut w, &[], &a);
        write_hud(&mut w, &a, &b);
        write_hud(&mut w, &b, &c);
        write_hud(&mut w, &c, &c);
        let mut r = BitReader::new(w.as_bytes());
        let ra = read_hud(&mut r, &[]).unwrap();
        assert_eq!(ra, a);
        let rb = read_hud(&mut r, &ra).unwrap();
        assert_eq!(rb, b);
        let rc = read_hud(&mut r, &rb).unwrap();
        assert_eq!(rc, c);
        assert_eq!(read_hud(&mut r, &rc).unwrap(), c);
        // An unchanged list costs a few bits.
        let mut w = BitWriter::new();
        write_hud(&mut w, &c, &c);
        assert_eq!(w.byte_len(), 1);
    }

    #[test]
    fn snapshots_carry_hud_and_objectives_and_the_store_takes_them() {
        let mut s = Snapshot::empty();
        s.hud = vec![elem(1, |e| e.text = 4)];
        s.objectives[2] = Objective {
            state: obj::ACTIVE,
            origin: [100.25, -3.0, 7.0],
            entity: NO_ENTITY,
            icon: 5,
        };
        s.server_time = 777;
        let s = s.canonical();
        let mut w = BitWriter::new();
        crate::snapshot::write_snapshot(&mut w, None, &s);
        let got =
            crate::snapshot::read_snapshot(&mut BitReader::new(w.as_bytes()), |_| None).unwrap();
        assert_eq!(got, s);
        let mut ui = ClientUiState::new();
        ui.apply_snapshot(&got);
        assert_eq!(ui.hud().len(), 1);
        assert!(ui.objectives()[2].visible());
        assert!(!ui.objectives()[3].visible());
        assert_eq!(ui.server_time(), 777);
    }

    #[test]
    fn timed_changes_interpolate_with_server_time() {
        let e = elem(0, |e| {
            e.from_color = [0, 0, 0, 0];
            e.color = [200, 100, 50, 255];
            e.fade_start = 1000;
            e.fade_time = 1000;
            e.from_width = 10;
            e.width = 30;
            e.scale_start = 0;
            e.scale_time = 100;
            e.kind = he::TIMER_DOWN;
            e.time = 5000;
        });
        assert_eq!(e.color_at(500), [0, 0, 0, 0]);
        assert_eq!(e.color_at(1500), [100, 50, 25, 128]);
        assert_eq!(e.color_at(9000), [200, 100, 50, 255]);
        assert_eq!(e.size_at(50).0, 20.0);
        assert!((e.timer_seconds(3500) - 1.5).abs() < 1e-4);
        assert_eq!(e.timer_seconds(9000), 0.0);
        let none = HudElem::new(1);
        assert_eq!(none.color_at(0), [255; 4], "no fade: the color itself");
    }

    #[test]
    fn alignment_bits_match_the_original_packing() {
        let mut e = HudElem::new(0);
        e.set_align_x(2);
        e.set_align_y(1);
        e.set_horz_align(7);
        e.set_vert_align(3);
        assert_eq!((e.align_x(), e.align_y()), (2, 1));
        assert_eq!((e.horz_align(), e.vert_align()), (7, 3));
        assert_eq!(e.align_org, 0b1001);
        assert_eq!(e.align_screen, 0b111_011);
    }

    fn open(name: &str) -> UiEvent {
        UiEvent::OpenMenu {
            name: name.into(),
            mouse: true,
        }
    }

    /// A person on `--listen` is joined quickly, then the server's menus are theirs: Choose Class and Change Team
    /// open, and Escape's close reaches the screen.
    #[test]
    fn auto_join_answers_the_join_menus_then_leaves_the_rest_to_the_player() {
        let mut join = AutoJoin::default();
        let (answers, shown) = join.filter(vec![open("team_marinesopfor")], false);
        assert_eq!(answers.len(), 1, "the team menu is answered");
        assert!(shown.is_empty());
        let (answers, shown) =
            join.filter(vec![open("changeclass"), UiEvent::CloseIngameMenu], false);
        assert_eq!(answers.len(), 1, "the class menu is answered");
        // The join is over once the class is answered: the server's close reaches the (menu-less) screen.
        assert_eq!(shown, vec![UiEvent::CloseIngameMenu]);
        // Mid-round: the player's own menus.
        let mid = vec![
            open("changeclass"),
            open("team_marinesopfor"),
            UiEvent::CloseIngameMenu,
        ];
        let (answers, shown) = join.filter(mid.clone(), false);
        assert!(answers.is_empty());
        assert_eq!(shown, mid);
        // A new level joins again.
        let (answers, shown) = join.filter(
            vec![
                UiEvent::Map {
                    name: "mp_crash".into(),
                },
                open("team_marinesopfor"),
            ],
            false,
        );
        assert_eq!(answers.len(), 1);
        assert_eq!(shown.len(), 1, "only the map change shows");
    }

    /// A scripted player answers every join menu and shows no server menu.
    #[test]
    fn a_scripted_player_answers_every_menu_and_shows_none() {
        let mut join = AutoJoin::default();
        let ev = vec![
            open("changeclass"),
            open("changeclass"),
            UiEvent::CloseIngameMenu,
        ];
        let (answers, shown) = join.filter(ev, true);
        assert_eq!(answers.len(), 2);
        assert!(shown.is_empty());
    }
}
