// SPDX-License-Identifier: GPL-3.0-or-later
//! Localize entries, string tables, fonts.

use super::asset_type::XAssetType;
use super::error::{Result, ZoneError};
use super::gfx::Name;
use super::stream::{Ptr, Stream};
use std::sync::Arc;

#[derive(Debug)]
pub struct LocalizeEntry {
    pub name: Name,
}

pub(super) fn load_localize(_: &mut Stream, _: Ptr) -> Result<Option<Arc<LocalizeEntry>>> {
    Err(ZoneError::Unsupported(XAssetType::Localize))
}

#[derive(Debug)]
pub struct StringTable {
    pub name: Name,
}

pub(super) fn load_string_table(_: &mut Stream, _: Ptr) -> Result<Option<Arc<StringTable>>> {
    Err(ZoneError::Unsupported(XAssetType::StringTable))
}

#[derive(Debug)]
pub struct Font {
    pub name: Name,
}

pub(super) fn load_font(_: &mut Stream, _: Ptr) -> Result<Option<Arc<Font>>> {
    Err(ZoneError::Unsupported(XAssetType::Font))
}
