// SPDX-License-Identifier: GPL-3.0-only
//! Localize entries, string tables, fonts.

use super::error::{Result, ZoneError};
use super::gfx::{Material, Name, material_ptr};
use super::stream::{Fields, Ptr, Stream};
use std::sync::Arc;

#[derive(Debug)]
pub struct LocalizeEntry {
    pub name: Name,
    pub value: Name,
}

pub(super) fn load_localize(s: &mut Stream, p: Ptr) -> Result<Option<Arc<LocalizeEntry>>> {
    s.temp_asset(p, 4, 8, |s, h| {
        let mut f = Fields::new(h);
        let (value, name) = (f.ptr()?, f.ptr()?);
        let value = s.string(value)?;
        let name = s.string(name)?;
        Ok(LocalizeEntry { name, value })
    })
}

#[derive(Debug)]
pub struct StringTable {
    pub name: Name,
    pub column_count: u32,
    pub row_count: u32,
    /// Row-major cells.
    pub values: Arc<[Name]>,
}

pub(super) fn load_string_table(s: &mut Stream, p: Ptr) -> Result<Option<Arc<StringTable>>> {
    s.shared(p, 4, 16, |s, h| {
        let mut f = Fields::new(h);
        let name = f.ptr()?;
        let (columns, rows) = (f.i32(), f.i32());
        let values = f.ptr()?;
        let column_count =
            u32::try_from(columns).map_err(|_| ZoneError::Invalid("negative column count"))?;
        let row_count =
            u32::try_from(rows).map_err(|_| ZoneError::Invalid("negative row count"))?;
        let count = column_count
            .checked_mul(row_count)
            .ok_or(ZoneError::Invalid("string table too large"))?;
        let name = s.string(name)?;
        // An empty cell array still advances the allocation to its alignment.
        if count == 0 && values == Ptr::Follow {
            s.alloc(4, 0)?;
        }
        let values = s.array(values, count, 4, 4, |s, f| {
            let p = f.ptr()?;
            s.string(p)
        })?;
        Ok(StringTable {
            name,
            column_count,
            row_count,
            values,
        })
    })
}

#[derive(Debug)]
pub struct Glyph {
    pub letter: u16,
    pub x0: i8,
    pub y0: i8,
    pub dx: u8,
    pub pixel_width: u8,
    pub pixel_height: u8,
    pub s0: f32,
    pub t0: f32,
    pub s1: f32,
    pub t1: f32,
}

#[derive(Debug)]
pub struct Font {
    pub name: Name,
    pub pixel_height: i32,
    pub material: Option<Arc<Material>>,
    pub glow_material: Option<Arc<Material>>,
    pub glyphs: Arc<[Glyph]>,
}

pub(super) fn load_font(s: &mut Stream, p: Ptr) -> Result<Option<Arc<Font>>> {
    s.temp_asset(p, 4, 24, |s, h| {
        let mut f = Fields::new(h);
        let name = f.ptr()?;
        let pixel_height = f.i32();
        let count =
            u32::try_from(f.i32()).map_err(|_| ZoneError::Invalid("negative glyph count"))?;
        let (material, glow, glyphs) = (f.ptr()?, f.ptr()?, f.ptr()?);
        let name = s.string(name)?;
        let material = material_ptr(s, material)?;
        let glow_material = material_ptr(s, glow)?;
        let glyphs = s.array(glyphs, count, 4, 24, |_, f| {
            let letter = f.u16();
            let [x0, y0] = [f.u8() as i8, f.u8() as i8];
            let [dx, pixel_width, pixel_height] = [f.u8(), f.u8(), f.u8()];
            f.skip(1);
            Ok(Glyph {
                letter,
                x0,
                y0,
                dx,
                pixel_width,
                pixel_height,
                s0: f.f32(),
                t0: f.f32(),
                s1: f.f32(),
                t1: f.f32(),
            })
        })?;
        Ok(Font {
            name,
            pixel_height,
            material,
            glow_material,
            glyphs,
        })
    })
}
