// SPDX-License-Identifier: GPL-3.0-only
//! Hud elements (`g_hudelem.cpp` of the original): allocation, the engine-owned fields scripts
//! read and write (`alpha`, `color`, `alignx`, ...), and the methods that set what an element
//! shows. The state lives in [`crate::ui::ServerUi`]; clients receive the visible set in their
//! snapshots ([`net::ui::HudElem`]).

use gsc::{EntClass, EntRef, Value, Vm};
use net::ui::{self, HudElem, he, hf};

use super::Args;
use crate::client::Team;
use crate::game::{Game, HUDELEM_BASE, MAX_HUDELEMS};
use crate::ui::{HudSlot, Table};

type R = Result<Value, String>;

fn alloc(g: &mut Game, vm: &mut Vm, client: Option<u16>, team: Team) -> R {
    let idx = match g.ui.hud.iter().position(|h| !h.inuse) {
        Some(i) => i,
        None if g.ui.hud.len() < MAX_HUDELEMS => {
            g.ui.hud.push(HudSlot::default());
            g.ui.hud.len() - 1
        }
        None => return Err("out of hudelems".into()),
    };
    g.ui.hud[idx] = HudSlot {
        inuse: true,
        client,
        team,
        e: HudElem::new(idx as u16),
    };
    Ok(Value::Object(
        vm.entity(HUDELEM_BASE + idx as u16, EntClass::HudElem),
    ))
}

pub fn new_hud_elem(g: &mut Game, vm: &mut Vm, _: Args) -> R {
    alloc(g, vm, None, Team::Free)
}

pub fn new_client_hud_elem(g: &mut Game, vm: &mut Vm, a: Args) -> R {
    let e = a.entity(0)?;
    if !g.is_client(e.num) {
        return Err("not a client".into());
    }
    alloc(g, vm, Some(e.num), Team::Free)
}

pub fn new_team_hud_elem(g: &mut Game, vm: &mut Vm, a: Args) -> R {
    let team = match a.string(0)? {
        "allies" => Team::Allies,
        "axis" => Team::Axis,
        "spectator" => Team::Spectator,
        o => {
            return Err(format!(
                "team \"{o}\" should be \"allies\", \"axis\", or \"spectator\""
            ));
        }
    };
    alloc(g, vm, None, team)
}

fn slot(g: &mut Game, e: EntRef) -> Result<&mut HudSlot, String> {
    if e.class != EntClass::HudElem {
        return Err("not a hud element".into());
    }
    g.ui.hud
        .get_mut(usize::from(e.num.wrapping_sub(HUDELEM_BASE)))
        .filter(|h| h.inuse)
        .ok_or_else(|| "not a hud element".into())
}

/// `HudElem_ClearTypeSettings`.
fn clear_type(e: &mut HudElem) {
    e.width = 0;
    e.height = 0;
    e.material = 0;
    e.from_x = 0.0;
    e.from_y = 0.0;
    e.from_align_org = 0;
    e.from_align_screen = 0;
    e.from_width = 0;
    e.from_height = 0;
    e.scale_start = 0;
    e.scale_time = 0;
    e.time = 0;
    e.duration = 0;
    e.value = 0.0;
    e.text = 0;
}

/// `destroy`: frees the element; its script object dies at the next tick.
pub fn destroy(g: &mut Game, vm: &mut Vm, e: EntRef, _: Args) -> R {
    slot(g, e)?.inuse = false;
    free_object(vm, e.num);
    Ok(Value::Undefined)
}

/// `Scr_FreeHudElem` tells the element's threads (`endon("death")`) before it goes.
pub fn free_object(vm: &mut Vm, num: u16) {
    vm.notify_entity(num, "death", &[]);
    vm.free_entity(num);
}

/// `HudElem_ClientDisconnect`: frees the elements that were only for client `n`.
pub fn free_client_elems(g: &mut Game, vm: &mut Vm, n: u16) {
    for (i, h) in g.ui.hud.iter_mut().enumerate() {
        if h.inuse && h.client == Some(n) {
            h.inuse = false;
            free_object(vm, HUDELEM_BASE + i as u16);
        }
    }
}

/// A string argument as the text a hud element shows: a localized reference keeps its `&`.
fn text_of(v: &Value) -> String {
    match v {
        Value::LocStr(s) => format!("&{s}"),
        v => super::args::display(v),
    }
}

fn ms(seconds: f32) -> i32 {
    (seconds * 1000.0 + 0.5) as i32
}

pub fn set_text(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    slot(g, e)?;
    let text = text_of(a.get(0)?);
    let idx = g.localized_index(&text)?;
    let h = slot(g, e)?;
    clear_type(&mut h.e);
    h.e.kind = he::TEXT;
    h.e.text = idx;
    Ok(Value::Undefined)
}

pub fn clear_all_text_after(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let text = slot(g, e)?.e.text;
    if text == 0 {
        return Err("Hud elem doesn't reference any text.  Make sure to call setText before using clearAllTextAfterHudElem.".into());
    }
    g.clear_text_after(text);
    Ok(Value::Undefined)
}

pub fn set_shader(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    slot(g, e)?;
    if a.len() != 1 && a.len() != 3 {
        return Err(
            "USAGE: <hudelem> setShader(\"materialname\"[, optional_width, optional_height]);"
                .into(),
        );
    }
    let material = g.precache(Table::Material, a.string(0)?)?;
    let (mut width, mut height) = (0, 0);
    if a.len() == 3 {
        width = a.int(1)?;
        height = a.int(2)?;
        if width < 0 {
            return Err(format!("width {width} < 0"));
        }
        if height < 0 {
            return Err(format!("height {height} < 0"));
        }
    }
    let h = slot(g, e)?;
    clear_type(&mut h.e);
    h.e.kind = he::MATERIAL;
    h.e.material = material;
    h.e.width = width.min(0xffff) as u16;
    h.e.height = height.min(0xffff) as u16;
    Ok(Value::Undefined)
}

fn timer(g: &mut Game, e: EntRef, a: Args, kind: u8, name: &str) -> R {
    let now = g.level.time;
    let h = slot(g, e)?;
    if a.len() != 1 {
        return Err(format!("USAGE: <hudelem> {name}(time_in_seconds);\n"));
    }
    let time = ms(a.float(0)?);
    if time <= 0 && kind != he::TIMER_UP {
        return Err(format!("time {} should be > 0", time as f32 * 0.001));
    }
    clear_type(&mut h.e);
    h.e.kind = kind;
    h.e.time = time + now;
    Ok(Value::Undefined)
}

pub fn set_timer(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    timer(g, e, a, he::TIMER_DOWN, "setTimer")
}
pub fn set_timer_up(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    timer(g, e, a, he::TIMER_UP, "setTimerUp")
}
pub fn set_tenths_timer(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    timer(g, e, a, he::TENTHS_TIMER_DOWN, "setTenthsTimer")
}
pub fn set_tenths_timer_up(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    timer(g, e, a, he::TENTHS_TIMER_UP, "setTenthsTimerUp")
}

fn clock(g: &mut Game, e: EntRef, a: Args, kind: u8, name: &str) -> R {
    slot(g, e)?;
    if a.len() != 3 && a.len() != 5 {
        return Err(format!(
            "USAGE: <hudelem> {name}(time_in_seconds, total_clock_time_in_seconds, shadername[, width, height]);\n"
        ));
    }
    let time = ms(a.float(0)?);
    if time <= 0 && kind != he::CLOCK_UP {
        return Err(format!("time {} should be > 0", time as f32 * 0.001));
    }
    let duration = ms(a.float(1)?);
    if duration <= 0 {
        return Err(format!(
            "duration {} should be > 0",
            duration as f32 * 0.001
        ));
    }
    let material = g.precache(Table::Material, a.string(2)?)?;
    let (mut width, mut height) = (0, 0);
    if a.len() == 5 {
        width = a.int(3)?;
        height = a.int(4)?;
        if width < 0 || height < 0 {
            return Err(format!("width {width} or height {height} < 0"));
        }
    }
    let now = g.level.time;
    let h = slot(g, e)?;
    clear_type(&mut h.e);
    h.e.kind = kind;
    h.e.time = time + now;
    h.e.duration = duration;
    h.e.material = material;
    h.e.width = width.min(0xffff) as u16;
    h.e.height = height.min(0xffff) as u16;
    Ok(Value::Undefined)
}

pub fn set_clock(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    clock(g, e, a, he::CLOCK_DOWN, "setClock")
}
pub fn set_clock_up(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    clock(g, e, a, he::CLOCK_UP, "setClockUp")
}

pub fn set_value(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let v = a.float(0)?;
    let h = slot(g, e)?;
    clear_type(&mut h.e);
    h.e.kind = he::VALUE;
    h.e.value = v;
    Ok(Value::Undefined)
}

pub fn set_waypoint(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    slot(g, e)?;
    let v = a.int(0)?;
    let off = if a.len() == 1 {
        0
    } else {
        g.precache(Table::Material, a.string(1)?)?
    };
    let h = slot(g, e)?;
    h.e.kind = he::WAYPOINT;
    h.e.value = v as f32;
    h.e.offscreen_material = off;
    Ok(Value::Undefined)
}

pub fn set_target_ent(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = a.entity(0)?.num;
    slot(g, e)?.e.target_ent = n.min(ui::NO_ENTITY);
    Ok(Value::Undefined)
}

pub fn clear_target_ent(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    slot(g, e)?.e.target_ent = ui::NO_ENTITY;
    Ok(Value::Undefined)
}

/// `0 < seconds <= 60`, the range the timed changes accept.
fn change_time(a: Args, what: &str) -> Result<i32, String> {
    let t = a.float(0)?;
    if t <= 0.0 {
        return Err(format!("{what} time {t} <= 0"));
    }
    if t > 60.0 {
        return Err(format!("{what} time {t} > 60"));
    }
    Ok((t * 1000.0).round() as i32)
}

pub fn fade_over_time(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let time = change_time(a, "fade")?;
    let now = g.level.time;
    let h = slot(g, e)?;
    let from = h.e.color_at(now);
    h.e.from_color = from;
    h.e.fade_start = now;
    h.e.fade_time = time;
    Ok(Value::Undefined)
}

pub fn scale_over_time(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    if a.len() != 3 {
        return Err("hudelem scaleOverTime(time_in_seconds, new_width, new_height)".into());
    }
    let time = change_time(a, "scale")?;
    let (w, hgt) = (a.int(1)?, a.int(2)?);
    let now = g.level.time;
    let h = slot(g, e)?;
    h.e.scale_start = now;
    h.e.scale_time = time;
    h.e.from_width = h.e.width;
    h.e.from_height = h.e.height;
    h.e.width = w.clamp(0, 0xffff) as u16;
    h.e.height = hgt.clamp(0, 0xffff) as u16;
    Ok(Value::Undefined)
}

pub fn move_over_time(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let time = change_time(a, "move")?;
    let now = g.level.time;
    let h = slot(g, e)?;
    h.e.move_start = now;
    h.e.move_time = time;
    h.e.from_x = h.e.x;
    h.e.from_y = h.e.y;
    h.e.from_align_org = h.e.align_org;
    h.e.from_align_screen = h.e.align_screen;
    Ok(Value::Undefined)
}

/// `reset`: back to a fresh element, keeping its owner.
pub fn reset(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let h = slot(g, e)?;
    h.e = HudElem::new(h.e.id);
    Ok(Value::Undefined)
}

pub fn set_pulse_fx(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    if a.len() != 3 {
        return Err(
            "USAGE: <hudelem> SetPulseFX( <speed>, <decayStart>, <decayDuration> );\n".into(),
        );
    }
    let mut v = [0; 3];
    for (i, v) in v.iter_mut().enumerate() {
        *v = a.int(i)?;
        if *v <= 0 {
            return Err(format!("value {v} must be > 0"));
        }
    }
    let now = g.level.time;
    let h = slot(g, e)?;
    h.e.fx_birth = now;
    h.e.fx_letter = v[0];
    h.e.fx_decay_start = v[1];
    h.e.fx_decay_duration = v[2];
    Ok(Value::Undefined)
}

pub fn set_player_name_string(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    slot(g, e)?;
    let p = a.entity(0)?;
    if !g.is_client(p.num) {
        g.print("Invalid entity passed to hudelem setplayernamestring(), entity is not a client\n");
        return Ok(Value::Undefined);
    }
    let h = slot(g, e)?;
    clear_type(&mut h.e);
    h.e.kind = he::PLAYERNAME;
    h.e.value = f32::from(p.num);
    Ok(Value::Undefined)
}

pub fn set_game_type_string(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    slot(g, e)?;
    let gt = a.string(0)?.to_owned();
    g.set_configstring(ui::cs::GAMETYPE, &gt);
    let h = slot(g, e)?;
    clear_type(&mut h.e);
    h.e.kind = he::GAMETYPE;
    h.e.value = f32::from(ui::cs::GAMETYPE);
    Ok(Value::Undefined)
}

pub fn set_map_name_string(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    slot(g, e)?;
    let map = a.string(0)?.to_owned();
    if !g.map_exists(&map) {
        g.print("Invalid map name passed to hudelem setmapnamestring(), map not found\n");
        return Ok(Value::Undefined);
    }
    g.set_configstring(ui::cs::MAPNAME, &map);
    let h = slot(g, e)?;
    clear_type(&mut h.e);
    h.e.kind = he::MAPNAME;
    Ok(Value::Undefined)
}

// ---- fields -----------------------------------------------------------------------------------

fn float_of(v: &Value) -> Result<f32, String> {
    match v {
        Value::Float(x) => Ok(*x),
        Value::Int(n) => Ok(*n as f32),
        o => Err(format!("type {} is not a float", o.type_name())),
    }
}

fn int_of(v: &Value) -> Result<i32, String> {
    match v {
        Value::Int(n) => Ok(*n),
        o => Err(format!("type {} is not an int", o.type_name())),
    }
}

fn vector_of(v: &Value) -> Result<[f32; 3], String> {
    match v {
        Value::Vector(v) => Ok(*v),
        o => Err(format!("type {} is not a vector", o.type_name())),
    }
}

fn byte(x: f32) -> u8 {
    (x.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn from_byte(b: u8) -> f32 {
    f32::from(b) * (1.0 / 255.0)
}

fn enum_of(v: &Value, names: &[&str], what: &str) -> Result<u8, String> {
    let Value::Str(s) = v else {
        return Err(format!("type {} is not a string", v.type_name()));
    };
    names
        .iter()
        .position(|n| n.eq_ignore_ascii_case(s))
        .map(|i| i as u8)
        .ok_or_else(|| format!("unknown {what} \"{s}\""))
}

fn flag(e: &mut HudElem, bit: u8, on: bool) {
    if on {
        e.flags |= bit;
    } else {
        e.flags &= !bit;
    }
}

/// Writes an engine-owned field of a hud element; `Ok(false)` leaves the name to the script
/// object (`Scr_SetHudElemField`).
pub fn set_field(g: &mut Game, num: u16, name: &str, v: &Value) -> Result<bool, String> {
    let label = if name == "label" {
        Some(g.localized_index(&text_of(v))?)
    } else {
        None
    };
    let Some(h) =
        g.ui.hud
            .get_mut(usize::from(num.wrapping_sub(HUDELEM_BASE)))
            .filter(|h| h.inuse)
    else {
        return Ok(false);
    };
    let e = &mut h.e;
    match name {
        "x" => e.x = float_of(v)?,
        "y" => e.y = float_of(v)?,
        "z" => e.z = float_of(v)?,
        "fontscale" => {
            let s = float_of(v)?;
            if s <= 0.0 {
                return Err(format!("font scale was {s}; should be > 0"));
            }
            e.font_scale = s;
        }
        "font" => e.font = enum_of(v, &ui::FONTS, "font")?,
        "alignx" => e.set_align_x(enum_of(v, &ui::ALIGN_X, "alignx")?),
        "aligny" => e.set_align_y(enum_of(v, &ui::ALIGN_Y, "aligny")?),
        "horzalign" => e.set_horz_align(enum_of(v, &ui::HORZ_ALIGN, "horzalign")?),
        "vertalign" => e.set_vert_align(enum_of(v, &ui::VERT_ALIGN, "vertalign")?),
        "color" => {
            let c = vector_of(v)?;
            e.color[..3].copy_from_slice(&c.map(byte));
        }
        "alpha" => e.color[3] = byte(float_of(v)?),
        "glowcolor" => {
            let c = vector_of(v)?;
            e.glow_color[..3].copy_from_slice(&c.map(byte));
        }
        "glowalpha" => e.glow_color[3] = byte(float_of(v)?),
        "label" => e.label = label.unwrap_or(0),
        "sort" => e.sort = float_of(v)?,
        "foreground" => flag(e, hf::FOREGROUND, int_of(v)? != 0),
        "hidewhendead" => flag(e, hf::HIDE_WHEN_DEAD, int_of(v)? != 0),
        "hidewheninmenu" => flag(e, hf::HIDE_WHEN_IN_MENU, int_of(v)? != 0),
        "archived" => flag(e, hf::ARCHIVED, int_of(v)? != 0),
        _ => return Ok(false),
    }
    Ok(true)
}

pub fn get_field(g: &Game, num: u16, name: &str) -> Option<Value> {
    let h =
        g.ui.hud
            .get(usize::from(num.wrapping_sub(HUDELEM_BASE)))
            .filter(|h| h.inuse)?;
    let e = &h.e;
    let b = |f: u8| Value::Int(i32::from(e.flags & f != 0));
    Some(match name {
        "x" => Value::Float(e.x),
        "y" => Value::Float(e.y),
        "z" => Value::Float(e.z),
        "fontscale" => Value::Float(e.font_scale),
        "font" => Value::str(ui::FONTS.get(usize::from(e.font))?),
        "alignx" => Value::str(ui::ALIGN_X.get(usize::from(e.align_x()))?),
        "aligny" => Value::str(ui::ALIGN_Y.get(usize::from(e.align_y()))?),
        "horzalign" => Value::str(ui::HORZ_ALIGN[usize::from(e.horz_align())]),
        "vertalign" => Value::str(ui::VERT_ALIGN[usize::from(e.vert_align())]),
        "color" => Value::Vector([
            from_byte(e.color[0]),
            from_byte(e.color[1]),
            from_byte(e.color[2]),
        ]),
        "alpha" => Value::Float(from_byte(e.color[3])),
        "glowcolor" => Value::Vector([
            from_byte(e.glow_color[0]),
            from_byte(e.glow_color[1]),
            from_byte(e.glow_color[2]),
        ]),
        "glowalpha" => Value::Float(from_byte(e.glow_color[3])),
        "label" => Value::Int(i32::from(e.label)),
        "sort" => Value::Float(e.sort),
        "foreground" => b(hf::FOREGROUND),
        "hidewhendead" => b(hf::HIDE_WHEN_DEAD),
        "hidewheninmenu" => b(hf::HIDE_WHEN_IN_MENU),
        "archived" => b(hf::ARCHIVED),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::Content;
    use crate::cvar::Cvars;

    fn game() -> Game {
        Game::new(Cvars::new(), Content::default())
    }

    fn put(g: &mut Game, client: Option<u16>, team: Team) -> u16 {
        let i = g.ui.hud.len();
        g.ui.hud.push(HudSlot {
            inuse: true,
            client,
            team,
            e: HudElem::new(i as u16),
        });
        HUDELEM_BASE + i as u16
    }

    #[test]
    fn fields_round_trip_with_the_originals_quantization_and_names() {
        let mut g = game();
        let n = put(&mut g, None, Team::Free);
        for (name, v) in [
            ("x", Value::Float(320.5)),
            ("fontscale", Value::Float(1.6)),
            ("font", Value::str("objective")),
            ("alignx", Value::str("right")),
            ("aligny", Value::str("middle")),
            ("horzalign", Value::str("center_safearea")),
            ("vertalign", Value::str("bottom")),
            ("color", Value::Vector([1.0, 0.5, 0.0])),
            ("alpha", Value::Float(0.25)),
            ("sort", Value::Int(-3)),
            ("foreground", Value::Int(1)),
            ("hidewhendead", Value::Int(1)),
            ("archived", Value::Int(0)),
        ] {
            assert_eq!(set_field(&mut g, n, name, &v), Ok(true), "{name}");
        }
        let get = |g: &Game, f: &str| get_field(g, n, f).unwrap();
        assert!(matches!(get(&g, "x"), Value::Float(x) if x == 320.5));
        assert!(matches!(get(&g, "font"), Value::Str(s) if &*s == "objective"));
        assert!(matches!(get(&g, "alignx"), Value::Str(s) if &*s == "right"));
        assert!(matches!(get(&g, "aligny"), Value::Str(s) if &*s == "middle"));
        assert!(matches!(get(&g, "horzalign"), Value::Str(s) if &*s == "center_safearea"));
        assert!(matches!(get(&g, "vertalign"), Value::Str(s) if &*s == "bottom"));
        // 0.5 is 128/255 on the wire, and alpha is a byte as well.
        assert!(matches!(get(&g, "color"), Value::Vector(c) if (c[1] - 0.502).abs() < 0.001));
        assert!(matches!(get(&g, "alpha"), Value::Float(a) if (a - 0.251).abs() < 0.001));
        assert!(matches!(get(&g, "foreground"), Value::Int(1)));
        assert!(matches!(get(&g, "archived"), Value::Int(0)));
        let e = &g.ui.hud[0].e;
        assert_eq!((e.align_x(), e.align_y()), (2, 1));
        assert_eq!(e.color, [255, 128, 0, 64]);
        assert_eq!(e.flags & hf::ARCHIVED, 0);
        // Unknown names are the script object's, bad values are errors.
        assert_eq!(set_field(&mut g, n, "mything", &Value::Int(1)), Ok(false));
        assert!(set_field(&mut g, n, "font", &Value::str("comic")).is_err());
        assert!(set_field(&mut g, n, "fontscale", &Value::Float(0.0)).is_err());
        assert!(set_field(&mut g, n, "alpha", &Value::str("x")).is_err());
    }

    #[test]
    fn a_label_is_a_localized_string_the_clients_get_a_configstring_for() {
        let mut g = game();
        let n = put(&mut g, None, Team::Free);
        assert_eq!(
            set_field(&mut g, n, "label", &Value::LocStr("MP_TIME".into())),
            Ok(true)
        );
        let idx = g.ui.hud[0].e.label;
        assert_eq!(idx, 1);
        assert_eq!(
            g.configstrings
                .get(&u32::from(ui::cs::LOCALIZED + idx))
                .map(String::as_str),
            Some("&MP_TIME")
        );
        assert!(g.ui.dirty_cs.contains(&(ui::cs::LOCALIZED + idx)));
        // The same text is the same slot; text differing in case is not.
        assert_eq!(g.localized_index("&MP_TIME"), Ok(idx));
        assert_eq!(g.localized_index("&mp_time"), Ok(idx + 1));
        // `clearalltextafterhudelem` frees what came after.
        g.clear_text_after(idx);
        assert_eq!(g.localized_index("again"), Ok(idx + 1));
    }

    #[test]
    fn the_string_table_has_a_limit_and_a_failed_add_leaves_it_alone() {
        let mut g = game();
        for i in 1..usize::from(ui::cs::LOCALIZED_COUNT) {
            assert_eq!(g.localized_index(&format!("s{i}")), Ok(i as u16));
        }
        assert!(g.localized_index("one too many").is_err());
        assert_eq!(g.localized_index("s5"), Ok(5));
        assert!(g.precache(Table::Menu, "").unwrap() == 0);
    }

    #[test]
    fn each_client_sees_the_global_its_own_and_its_teams_elements_up_to_the_limit() {
        use crate::client::Client;
        let mut g = game();
        g.clients = (0..3)
            .map(|n| Client::new(n, false, format!("p{n}")))
            .collect();
        g.clients[0].team = Team::Allies;
        g.clients[1].team = Team::Axis;
        put(&mut g, None, Team::Free);
        put(&mut g, Some(0), Team::Free);
        put(&mut g, Some(1), Team::Free);
        put(&mut g, None, Team::Axis);
        put(&mut g, None, Team::Allies);
        let ids = |g: &Game, c| g.visible_hud(c).iter().map(|e| e.id).collect::<Vec<_>>();
        assert_eq!(ids(&g, 0), [0, 1, 4]);
        assert_eq!(ids(&g, 1), [0, 2, 3]);
        assert_eq!(ids(&g, 2), [0]);
        // Thirty-one archived and thirty-one not, then no more of either.
        for i in 0..80 {
            let n = put(&mut g, None, Team::Free);
            let off = n - HUDELEM_BASE;
            g.ui.hud[usize::from(off)].e.flags = if i % 2 == 0 { hf::ARCHIVED } else { 0 };
        }
        let v = g.visible_hud(2);
        let archived = v.iter().filter(|e| e.archived()).count();
        assert_eq!(archived, ui::MAX_HUD_PER_GROUP);
        assert_eq!(v.len() - archived, ui::MAX_HUD_PER_GROUP);
        // A freed slot is not sent.
        g.ui.hud[0].inuse = false;
        assert!(!ids(&g, 2).contains(&0));
    }
}
