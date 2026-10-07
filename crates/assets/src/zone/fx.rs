// SPDX-License-Identifier: GPL-3.0-or-later
//! FxEffectDef and FxImpactTable.

use super::asset_type::XAssetType;
use super::error::{Result, ZoneError};
use super::gfx::Name;
use super::stream::{Ptr, Stream};
use std::sync::Arc;

#[derive(Debug)]
pub struct FxEffectDef {
    pub name: Name,
}

pub(super) fn load(_: &mut Stream, _: Ptr) -> Result<Option<Arc<FxEffectDef>>> {
    Err(ZoneError::Unsupported(XAssetType::Fx))
}

#[derive(Debug)]
pub struct FxImpactTable {
    pub name: Name,
}

pub(super) fn load_impact(_: &mut Stream, _: Ptr) -> Result<Option<Arc<FxImpactTable>>> {
    Err(ZoneError::Unsupported(XAssetType::ImpactFx))
}
