// SPDX-License-Identifier: GPL-3.0-or-later
//! Sound alias lists, curves, loaded sounds, SndDriverGlobals.

use super::asset_type::XAssetType;
use super::error::{Result, ZoneError};
use super::gfx::Name;
use super::stream::{Ptr, Stream};
use std::sync::Arc;

#[derive(Debug)]
pub struct SoundAliasList {
    pub name: Name,
}

pub(super) fn load_alias_list(_: &mut Stream, _: Ptr) -> Result<Option<Arc<SoundAliasList>>> {
    Err(ZoneError::Unsupported(XAssetType::Sound))
}

#[derive(Debug)]
pub struct SndCurve {
    pub name: Name,
}

pub(super) fn load_curve(_: &mut Stream, _: Ptr) -> Result<Option<Arc<SndCurve>>> {
    Err(ZoneError::Unsupported(XAssetType::SoundCurve))
}

#[derive(Debug)]
pub struct LoadedSound {
    pub name: Name,
}

pub(super) fn load_loaded(_: &mut Stream, _: Ptr) -> Result<Option<Arc<LoadedSound>>> {
    Err(ZoneError::Unsupported(XAssetType::LoadedSound))
}

#[derive(Debug)]
pub struct SndDriverGlobals {
    pub name: Name,
}

pub(super) fn load_driver_globals(_: &mut Stream) -> Result<Option<Arc<SndDriverGlobals>>> {
    Ok(None)
}
