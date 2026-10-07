// SPDX-License-Identifier: GPL-3.0-or-later
//! clipMap_t (MP and SP share one loader) and MapEnts.

use super::asset_type::XAssetType;
use super::error::{Result, ZoneError};
use super::gfx::Name;
use super::stream::{Ptr, Stream};
use std::sync::Arc;

#[derive(Debug)]
pub struct Clipmap {
    pub name: Name,
}

pub(super) fn load(_: &mut Stream, _: Ptr) -> Result<Option<Arc<Clipmap>>> {
    Err(ZoneError::Unsupported(XAssetType::Clipmap))
}
