// SPDX-License-Identifier: GPL-3.0-or-later
//! ComWorld, GameWorldMp, GfxLightDef.

use super::asset_type::XAssetType;
use super::error::{Result, ZoneError};
use super::gfx::Name;
use super::stream::{Ptr, Stream};
use std::sync::Arc;

#[derive(Debug)]
pub struct ComWorld {
    pub name: Name,
}

pub(super) fn load_com_world(_: &mut Stream, _: Ptr) -> Result<Option<Arc<ComWorld>>> {
    Err(ZoneError::Unsupported(XAssetType::ComWorld))
}

#[derive(Debug)]
pub struct GameWorldMp {
    pub name: Name,
}

pub(super) fn load_game_world_mp(_: &mut Stream, _: Ptr) -> Result<Option<Arc<GameWorldMp>>> {
    Err(ZoneError::Unsupported(XAssetType::GameWorldMp))
}

#[derive(Debug)]
pub struct LightDef {
    pub name: Name,
}

pub(super) fn load_light_def(_: &mut Stream, _: Ptr) -> Result<Option<Arc<LightDef>>> {
    Err(ZoneError::Unsupported(XAssetType::LightDef))
}
