// SPDX-License-Identifier: GPL-3.0-or-later
//! MenuList, menuDef, itemDef and expressions.

use super::error::{Result, ZoneError};
use super::gfx::{material_ptr_at, raw_of, Material, Name};
use super::sound::{load_alias_list, SoundAliasList};
use super::stream::{Addr, Fields, Ptr, Stream};
use std::sync::Arc;

#[derive(Debug)]
pub struct MenuList {
    pub name: Name,
    pub menus: Vec<Option<Arc<MenuDef>>>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub horz_align: i32,
    pub vert_align: i32,
}

fn rect(f: &mut Fields) -> Rect {
    Rect {
        x: f.f32(),
        y: f.f32(),
        w: f.f32(),
        h: f.f32(),
        horz_align: f.i32(),
        vert_align: f.i32(),
    }
}

fn string(s: &mut Stream, f: &mut Fields) -> Result<Name> {
    let p = f.ptr()?;
    s.string(p)
}

fn color(f: &mut Fields) -> [f32; 4] {
    [f.f32(), f.f32(), f.f32(), f.f32()]
}

#[derive(Debug)]
pub struct WindowDef {
    pub name: Name,
    pub rect: Rect,
    pub rect_client: Rect,
    pub group: Name,
    pub style: i32,
    pub border: i32,
    pub owner_draw: i32,
    pub owner_draw_flags: u32,
    pub border_size: f32,
    pub static_flags: u32,
    pub dynamic_flags: u32,
    pub next_time: i32,
    pub fore_color: [f32; 4],
    pub back_color: [f32; 4],
    pub border_color: [f32; 4],
    pub outline_color: [f32; 4],
    pub background: Option<Arc<Material>>,
}

fn window(s: &mut Stream, f: &mut Fields) -> Result<WindowDef> {
    let name = string(s, f)?;
    let rect_ = rect(f);
    let rect_client = rect(f);
    let group = string(s, f)?;
    let (style, border, owner_draw, owner_draw_flags) = (f.i32(), f.i32(), f.i32(), f.u32());
    let border_size = f.f32();
    let static_flags = f.u32();
    let dynamic_flags = f.u32();
    let next_time = f.i32();
    let fore_color = color(f);
    let back_color = color(f);
    let border_color = color(f);
    let outline_color = color(f);
    let slot = f.slot();
    let background = f.ptr()?;
    let background = material_ptr_at(s, slot, background)?;
    Ok(WindowDef {
        name,
        rect: rect_,
        rect_client,
        group,
        style,
        border,
        owner_draw,
        owner_draw_flags,
        border_size,
        static_flags,
        dynamic_flags,
        next_time,
        fore_color,
        back_color,
        border_color,
        outline_color,
        background,
    })
}

#[derive(Debug)]
pub struct KeyHandler {
    pub key: i32,
    pub action: Name,
}

/// Loads a linked list of key handlers; each node's pointer is followed in place.
fn key_handlers(s: &mut Stream, mut p: Ptr) -> Result<Vec<KeyHandler>> {
    let mut out = Vec::new();
    while p != Ptr::Null {
        let (_, b) = follow(s, p, 12)?;
        let mut f = Fields::new(&b);
        let key = f.i32();
        let action = f.ptr()?;
        p = f.ptr()?;
        out.push(KeyHandler {
            key,
            action: s.string(action)?,
        });
    }
    Ok(out)
}

/// Loads an optional struct that can only be stored inline.
fn inline<T>(
    s: &mut Stream,
    p: Ptr,
    size: u32,
    f: impl FnOnce(&mut Stream, Fields) -> Result<T>,
) -> Result<Option<Arc<T>>> {
    if p == Ptr::Null {
        return Ok(None);
    }
    let (at, b) = follow(s, p, size)?;
    Ok(Some(Arc::new(f(s, Fields::at(&b, at))?)))
}

/// Loads a struct that can only be stored inline.
fn follow(s: &mut Stream, p: Ptr, size: u32) -> Result<(super::stream::Addr, Vec<u8>)> {
    match p {
        Ptr::Follow => s.load(4, size),
        p => Err(ZoneError::BadPointer(raw_of(p))),
    }
}

#[derive(Debug)]
pub enum Operand {
    Int(i32),
    Float(f32),
    String(Name),
}

#[derive(Debug)]
pub enum ExpressionEntry {
    /// Operator code (`OP_*`).
    Operator(i32),
    Operand(Operand),
}

#[derive(Debug, Default)]
pub struct Statement {
    pub entries: Vec<Arc<ExpressionEntry>>,
}

fn expression_entry(s: &mut Stream, b: &[u8]) -> Result<ExpressionEntry> {
    let mut f = Fields::new(b);
    let ty = f.i32();
    match ty {
        0 => Ok(ExpressionEntry::Operator(f.i32())),
        1 => {
            let data_type = f.i32();
            Ok(ExpressionEntry::Operand(match data_type {
                0 => Operand::Int(f.i32()),
                1 => Operand::Float(f.f32()),
                2 => {
                    let p = f.ptr()?;
                    Operand::String(s.string(p)?)
                }
                _ => return Err(ZoneError::Invalid("menu operand type")),
            }))
        }
        _ => Err(ZoneError::Invalid("menu expression entry type")),
    }
}

fn statement(s: &mut Stream, f: &mut Fields) -> Result<Statement> {
    let count = f.u32();
    let entries = f.ptr()?;
    pointer_array(s, entries, count, |s, p| {
        s.shared(p, 4, 12, expression_entry)?
            .ok_or(ZoneError::Invalid("null expression entry"))
    })
    .map(|entries| Statement { entries })
}

/// A counted array of pointers, each element loaded with `f`.
fn pointer_array<T>(
    s: &mut Stream,
    p: Ptr,
    count: u32,
    mut f: impl FnMut(&mut Stream, Ptr) -> Result<T>,
) -> Result<Vec<T>> {
    if p == Ptr::Null || count == 0 {
        return Ok(Vec::new());
    }
    let ptrs = match p {
        Ptr::Follow => {
            let len = count
                .checked_mul(4)
                .ok_or(ZoneError::Invalid("array too large"))?;
            s.load(4, len)?.1
        }
        p => return Err(ZoneError::BadPointer(raw_of(p))),
    };
    let mut out = Vec::with_capacity(count as usize);
    for c in ptrs.chunks_exact(4) {
        let p = Ptr::from_raw(u32::from_le_bytes(c.try_into().unwrap()))?;
        if p != Ptr::Null {
            out.push(f(s, p)?);
        }
    }
    Ok(out)
}

#[derive(Debug)]
pub struct ColumnInfo {
    pub pos: i32,
    pub width: i32,
    pub max_chars: i32,
    pub alignment: i32,
}

#[derive(Debug)]
pub struct ListBoxDef {
    pub mouse_pos: i32,
    pub start_pos: i32,
    pub end_pos: i32,
    pub draw_padding: i32,
    pub element_width: f32,
    pub element_height: f32,
    pub element_style: i32,
    pub columns: Vec<ColumnInfo>,
    pub on_double_click: Name,
    pub not_selectable: i32,
    pub no_scroll_bars: i32,
    pub use_paging: i32,
    pub select_border: [f32; 4],
    pub disable_color: [f32; 4],
    pub select_icon: Option<Arc<Material>>,
}

fn list_box(s: &mut Stream, mut f: Fields) -> Result<ListBoxDef> {
    let mouse_pos = f.i32();
    let start_pos = f.i32();
    let end_pos = f.i32();
    let draw_padding = f.i32();
    let element_width = f.f32();
    let element_height = f.f32();
    let element_style = f.i32();
    let num_columns = f.i32();
    let mut columns = Vec::new();
    for i in 0..16 {
        let c = ColumnInfo {
            pos: f.i32(),
            width: f.i32(),
            max_chars: f.i32(),
            alignment: f.i32(),
        };
        if i < num_columns {
            columns.push(c);
        }
    }
    let on_double_click = f.ptr()?;
    let not_selectable = f.i32();
    let no_scroll_bars = f.i32();
    let use_paging = f.i32();
    let select_border = color(&mut f);
    let disable_color = color(&mut f);
    let slot = f.slot();
    let select_icon = f.ptr()?;
    let on_double_click = s.string(on_double_click)?;
    let select_icon = material_ptr_at(s, slot, select_icon)?;
    Ok(ListBoxDef {
        mouse_pos,
        start_pos,
        end_pos,
        draw_padding,
        element_width,
        element_height,
        element_style,
        columns,
        on_double_click,
        not_selectable,
        no_scroll_bars,
        use_paging,
        select_border,
        disable_color,
        select_icon,
    })
}

#[derive(Debug)]
pub struct EditFieldDef {
    pub min_val: f32,
    pub max_val: f32,
    pub def_val: f32,
    pub range: f32,
    pub max_chars: i32,
    pub max_chars_goto_next: i32,
    pub max_paint_chars: i32,
    pub paint_offset: i32,
}

fn edit_field(_: &mut Stream, mut f: Fields) -> Result<EditFieldDef> {
    Ok(EditFieldDef {
        min_val: f.f32(),
        max_val: f.f32(),
        def_val: f.f32(),
        range: f.f32(),
        max_chars: f.i32(),
        max_chars_goto_next: f.i32(),
        max_paint_chars: f.i32(),
        paint_offset: f.i32(),
    })
}

#[derive(Debug)]
pub struct MultiDef {
    pub dvar_list: Vec<Name>,
    pub dvar_str: Vec<Name>,
    pub dvar_value: Vec<f32>,
    pub count: i32,
    pub str_def: i32,
}

fn multi(s: &mut Stream, mut f: Fields) -> Result<MultiDef> {
    let list: Vec<Ptr> = (0..32).map(|_| f.ptr()).collect::<Result<_>>()?;
    let strs: Vec<Ptr> = (0..32).map(|_| f.ptr()).collect::<Result<_>>()?;
    let dvar_value = (0..32).map(|_| f.f32()).collect();
    let count = f.i32();
    let str_def = f.i32();
    let dvar_list = list
        .into_iter()
        .map(|p| s.string(p))
        .collect::<Result<_>>()?;
    let dvar_str = strs
        .into_iter()
        .map(|p| s.string(p))
        .collect::<Result<_>>()?;
    Ok(MultiDef {
        dvar_list,
        dvar_str,
        dvar_value,
        count,
        str_def,
    })
}

/// Type-specific item data, selected by the item type.
#[derive(Debug)]
pub enum ItemData {
    None,
    ListBox(Option<Arc<ListBoxDef>>),
    EditField(Option<Arc<EditFieldDef>>),
    Multi(Option<Arc<MultiDef>>),
    EnumDvarName(Name),
}

#[derive(Debug)]
pub struct ItemDef {
    pub window: WindowDef,
    pub text_rect: Rect,
    pub ty: i32,
    pub data_type: i32,
    pub alignment: i32,
    pub font_enum: i32,
    pub text_align_mode: i32,
    pub text_align_x: f32,
    pub text_align_y: f32,
    pub text_scale: f32,
    pub text_style: i32,
    pub game_msg_window_index: i32,
    pub game_msg_window_mode: i32,
    pub text: Name,
    pub item_flags: u32,
    pub mouse_enter_text: Name,
    pub mouse_exit_text: Name,
    pub mouse_enter: Name,
    pub mouse_exit: Name,
    pub action: Name,
    pub on_accept: Name,
    pub on_focus: Name,
    pub leave_focus: Name,
    pub dvar: Name,
    pub dvar_test: Name,
    pub on_key: Vec<KeyHandler>,
    pub enable_dvar: Name,
    pub dvar_flags: i32,
    pub focus_sound: Option<Arc<SoundAliasList>>,
    pub special: f32,
    pub image_track: i32,
    pub data: ItemData,
    pub visible_exp: Statement,
    pub text_exp: Statement,
    pub material_exp: Statement,
    pub rect_x_exp: Statement,
    pub rect_y_exp: Statement,
    pub rect_w_exp: Statement,
    pub rect_h_exp: Statement,
    pub forecolor_a_exp: Statement,
}

const ITEM_SIZE: u32 = 372;

fn item(s: &mut Stream, base: Addr, b: &[u8]) -> Result<ItemDef> {
    let mut f = Fields::at(b, base);
    let window = window(s, &mut f)?;
    let text_rect = rect(&mut f);
    let ty = f.i32();
    let data_type = f.i32();
    let alignment = f.i32();
    let font_enum = f.i32();
    let text_align_mode = f.i32();
    let text_align_x = f.f32();
    let text_align_y = f.f32();
    let text_scale = f.f32();
    let text_style = f.i32();
    let game_msg_window_index = f.i32();
    let game_msg_window_mode = f.i32();
    let text = string(s, &mut f)?;
    let item_flags = f.u32();
    f.skip(4); // parent
    let mouse_enter_text = string(s, &mut f)?;
    let mouse_exit_text = string(s, &mut f)?;
    let mouse_enter = string(s, &mut f)?;
    let mouse_exit = string(s, &mut f)?;
    let action = string(s, &mut f)?;
    let on_accept = string(s, &mut f)?;
    let on_focus = string(s, &mut f)?;
    let leave_focus = string(s, &mut f)?;
    let dvar = string(s, &mut f)?;
    let dvar_test = string(s, &mut f)?;
    let on_key = f.ptr()?;
    let on_key = key_handlers(s, on_key)?;
    let enable_dvar = string(s, &mut f)?;
    let dvar_flags = f.i32();
    let focus_sound = f.ptr()?;
    let focus_sound = load_alias_list(s, focus_sound)?;
    let special = f.f32();
    f.skip(4); // cursorPos
    let type_data = f.ptr()?;
    let data = match ty {
        6 => ItemData::ListBox(inline(s, type_data, 340, list_box)?),
        0 | 4 | 9 | 10 | 11 | 14 | 16 | 17 | 18 => {
            ItemData::EditField(inline(s, type_data, 32, edit_field)?)
        }
        12 => ItemData::Multi(inline(s, type_data, 392, multi)?),
        13 => ItemData::EnumDvarName(s.string(type_data)?),
        _ => ItemData::None,
    };
    let image_track = f.i32();
    let visible_exp = statement(s, &mut f)?;
    let text_exp = statement(s, &mut f)?;
    let material_exp = statement(s, &mut f)?;
    let rect_x_exp = statement(s, &mut f)?;
    let rect_y_exp = statement(s, &mut f)?;
    let rect_w_exp = statement(s, &mut f)?;
    let rect_h_exp = statement(s, &mut f)?;
    let forecolor_a_exp = statement(s, &mut f)?;
    Ok(ItemDef {
        window,
        text_rect,
        ty,
        data_type,
        alignment,
        font_enum,
        text_align_mode,
        text_align_x,
        text_align_y,
        text_scale,
        text_style,
        game_msg_window_index,
        game_msg_window_mode,
        text,
        item_flags,
        mouse_enter_text,
        mouse_exit_text,
        mouse_enter,
        mouse_exit,
        action,
        on_accept,
        on_focus,
        leave_focus,
        dvar,
        dvar_test,
        on_key,
        enable_dvar,
        dvar_flags,
        focus_sound,
        special,
        image_track,
        data,
        visible_exp,
        text_exp,
        material_exp,
        rect_x_exp,
        rect_y_exp,
        rect_w_exp,
        rect_h_exp,
        forecolor_a_exp,
    })
}

#[derive(Debug)]
pub struct MenuDef {
    pub window: WindowDef,
    pub font: Name,
    pub full_screen: i32,
    pub font_index: i32,
    pub fade_cycle: i32,
    pub fade_clamp: f32,
    pub fade_amount: f32,
    pub fade_in_amount: f32,
    pub blur_radius: f32,
    pub on_open: Name,
    pub on_close: Name,
    pub on_esc: Name,
    pub on_key: Vec<KeyHandler>,
    pub visible_exp: Statement,
    pub allowed_binding: Name,
    pub sound_name: Name,
    pub image_track: i32,
    pub focus_color: [f32; 4],
    pub disable_color: [f32; 4],
    pub rect_x_exp: Statement,
    pub rect_y_exp: Statement,
    pub items: Vec<ItemDef>,
}

const MENU_SIZE: u32 = 284;

fn menu_def(s: &mut Stream, b: &[u8]) -> Result<MenuDef> {
    let mut f = Fields::new(b);
    let window = window(s, &mut f)?;
    let font = string(s, &mut f)?;
    let full_screen = f.i32();
    let item_count = f.u32();
    let font_index = f.i32();
    f.skip(4); // cursorItem
    let fade_cycle = f.i32();
    let (fade_clamp, fade_amount, fade_in_amount, blur_radius) =
        (f.f32(), f.f32(), f.f32(), f.f32());
    let on_open = string(s, &mut f)?;
    let on_close = string(s, &mut f)?;
    let on_esc = string(s, &mut f)?;
    let on_key = f.ptr()?;
    let on_key = key_handlers(s, on_key)?;
    let visible_exp = statement(s, &mut f)?;
    let allowed_binding = string(s, &mut f)?;
    let sound_name = string(s, &mut f)?;
    let image_track = f.i32();
    let focus_color = color(&mut f);
    let disable_color = color(&mut f);
    let rect_x_exp = statement(s, &mut f)?;
    let rect_y_exp = statement(s, &mut f)?;
    let items = f.ptr()?;
    let items = pointer_array(s, items, item_count, |s, p| {
        let (base, b) = follow(s, p, ITEM_SIZE)?;
        item(s, base, &b)
    })?;
    Ok(MenuDef {
        window,
        font,
        full_screen,
        font_index,
        fade_cycle,
        fade_clamp,
        fade_amount,
        fade_in_amount,
        blur_radius,
        on_open,
        on_close,
        on_esc,
        on_key,
        visible_exp,
        allowed_binding,
        sound_name,
        image_track,
        focus_color,
        disable_color,
        rect_x_exp,
        rect_y_exp,
        items,
    })
}

pub(super) fn load(s: &mut Stream, p: Ptr) -> Result<Option<Arc<MenuList>>> {
    s.temp_asset(p, 4, 12, |s, h| {
        let mut f = Fields::new(h);
        let name = f.ptr()?;
        let count = f.u32();
        let menus = f.ptr()?;
        let name = s.string(name)?;
        let menus = {
            if menus == Ptr::Null {
                Vec::new()
            } else {
                let (base, ptrs) = match menus {
                    Ptr::Follow => s.load(4, count * 4)?,
                    p => return Err(ZoneError::BadPointer(raw_of(p))),
                };
                let mut out = Vec::new();
                let mut f = Fields::at(&ptrs, base);
                for _ in 0..count {
                    let slot = f.slot();
                    let p = f.ptr()?;
                    out.push(s.temp_ptr_at(slot, p, 4, MENU_SIZE, true, menu_def)?);
                }
                out
            }
        };
        Ok(MenuList { name, menus })
    })
}
