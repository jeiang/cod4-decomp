// SPDX-License-Identifier: GPL-3.0-or-later
//! XAnimParts.

use super::asset_type::XAssetType;
use super::error::{Result, ZoneError};
use super::gfx::Name;
use super::stream::{Ptr, Stream};
use std::sync::Arc;

#[derive(Debug)]
pub struct XAnimParts {
    pub name: Name,
}

pub(super) fn load(_: &mut Stream, _: Ptr) -> Result<Option<Arc<XAnimParts>>> {
    Err(ZoneError::Unsupported(XAssetType::XAnimParts))
}
