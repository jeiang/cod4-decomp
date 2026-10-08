// SPDX-License-Identifier: GPL-3.0-only
//! Sound alias lists, curves, loaded sounds, SndDriverGlobals.

use super::error::{Result, ZoneError};
use super::gfx::Name;
use super::stream::{Addr, Block, Fields, Ptr, Stream};
use std::sync::Arc;

/// A named group of alternative sounds.
#[derive(Debug)]
pub struct SoundAliasList {
    pub name: Name,
    pub aliases: Arc<[SoundAlias]>,
}

#[derive(Debug)]
pub struct SoundAlias {
    pub name: Name,
    pub subtitle: Name,
    pub secondary_alias_name: Name,
    pub chain_alias_name: Name,
    pub sound_file: Option<Arc<SoundFile>>,
    pub sequence: i32,
    pub vol_min: f32,
    pub vol_max: f32,
    pub pitch_min: f32,
    pub pitch_max: f32,
    pub dist_min: f32,
    pub dist_max: f32,
    pub flags: i32,
    pub slave_percentage: f32,
    pub probability: f32,
    pub lfe_percentage: f32,
    pub center_percentage: f32,
    pub start_delay: i32,
    pub volume_falloff_curve: Option<Arc<SndCurve>>,
    pub envelop_min: f32,
    pub envelop_max: f32,
    pub envelop_percentage: f32,
    pub speaker_map: Option<Arc<SpeakerMap>>,
}

/// `SoundAlias::flags`, verified against every alias of the MP zones (type bits agree with the
/// sound file's kind in all of them; channel indices match `soundaliases/channels.def` rows).
///
/// Bit 0 loops, 1 master, 2 slave, 3 full dry level, 4 no wet level, 5 random looping (needs
/// bit 0), 6-7 the file kind (1 loaded, 2 streamed), 8-13 the entity channel.
impl SoundAlias {
    pub fn looping(&self) -> bool {
        self.flags & 0x01 != 0
    }

    /// Ducks the slaves while it plays.
    pub fn master(&self) -> bool {
        self.flags & 0x02 != 0
    }

    /// Ducked by masters, by `slave_percentage`.
    pub fn slave(&self) -> bool {
        self.flags & 0x04 != 0
    }

    pub fn full_dry_level(&self) -> bool {
        self.flags & 0x08 != 0
    }

    pub fn no_wet_level(&self) -> bool {
        self.flags & 0x10 != 0
    }

    /// A looping alias that starts at a random point.
    pub fn random_looping(&self) -> bool {
        self.flags & 0x20 != 0
    }

    /// Index into the rows of `soundaliases/channels.def`.
    pub fn channel(&self) -> usize {
        ((self.flags >> 8) & 0x3f) as usize
    }
}

#[derive(Debug)]
pub struct SoundFile {
    /// Whether the sound file is present (`exists`).
    pub exists: bool,
    pub source: SoundSource,
}

#[derive(Debug)]
pub enum SoundSource {
    /// Type 0 or any value without data in the stream.
    Unknown,
    Loaded(Option<Arc<LoadedSound>>),
    Streamed {
        dir: Name,
        name: Name,
    },
}

#[derive(Debug)]
pub struct SpeakerLevels {
    pub speaker: i32,
    pub level_count: i32,
    pub levels: [f32; 2],
}

#[derive(Debug)]
pub struct ChannelMap {
    pub speaker_count: i32,
    pub speakers: [SpeakerLevels; 6],
}

#[derive(Debug)]
pub struct SpeakerMap {
    pub is_default: bool,
    pub name: Name,
    pub channel_maps: [[ChannelMap; 2]; 2],
}

const SOUND_ALIAS_LIST_SIZE: u32 = 12;
const SOUND_ALIAS_SIZE: u32 = 92;
const SOUND_FILE_SIZE: u32 = 12;
const SPEAKER_MAP_SIZE: u32 = 408;
const SAT_LOADED: u8 = 1;
const SAT_STREAMED: u8 = 2;

pub(super) fn load_alias_list(s: &mut Stream, p: Ptr) -> Result<Option<Arc<SoundAliasList>>> {
    s.temp_asset(p, 4, SOUND_ALIAS_LIST_SIZE, |s, h| {
        let mut f = Fields::new(h);
        let (name, head, count) = (f.ptr()?, f.ptr()?, f.i32());
        let count =
            u32::try_from(count).map_err(|_| ZoneError::Invalid("negative sound alias count"))?;

        let name = s.string(name)?;
        let aliases = s.array(head, count, 4, SOUND_ALIAS_SIZE, alias)?;
        Ok(SoundAliasList { name, aliases })
    })
}

fn alias(s: &mut Stream, f: &mut Fields) -> Result<SoundAlias> {
    let strings = [f.ptr()?, f.ptr()?, f.ptr()?, f.ptr()?];
    let sound_file = f.ptr()?;
    let sequence = f.i32();
    let [vol_min, vol_max, pitch_min, pitch_max, dist_min, dist_max] = [(); 6].map(|_| f.f32());
    let flags = f.i32();
    let [
        slave_percentage,
        probability,
        lfe_percentage,
        center_percentage,
    ] = [(); 4].map(|_| f.f32());
    let start_delay = f.i32();
    let curve_slot = f.slot();
    let curve = f.ptr()?;
    let [envelop_min, envelop_max, envelop_percentage] = [(); 3].map(|_| f.f32());
    let speaker_map = f.ptr()?;

    let [name, subtitle, secondary_alias_name, chain_alias_name] = strings;
    let name = s.string(name)?;
    let subtitle = s.string(subtitle)?;
    let secondary_alias_name = s.string(secondary_alias_name)?;
    let chain_alias_name = s.string(chain_alias_name)?;
    let sound_file = load_sound_file(s, sound_file)?;
    let volume_falloff_curve = curve_at(s, curve_slot, curve)?;
    let speaker_map = s.shared(speaker_map, 4, SPEAKER_MAP_SIZE, speaker_map_body)?;
    Ok(SoundAlias {
        name,
        subtitle,
        secondary_alias_name,
        chain_alias_name,
        sound_file,
        sequence,
        vol_min,
        vol_max,
        pitch_min,
        pitch_max,
        dist_min,
        dist_max,
        flags,
        slave_percentage,
        probability,
        lfe_percentage,
        center_percentage,
        start_delay,
        volume_falloff_curve,
        envelop_min,
        envelop_max,
        envelop_percentage,
        speaker_map,
    })
}

fn load_sound_file(s: &mut Stream, p: Ptr) -> Result<Option<Arc<SoundFile>>> {
    match p {
        Ptr::Follow => {
            let (at, bytes) = s.load(4, SOUND_FILE_SIZE)?;
            let v = Arc::new(sound_file_body(s, &mut Fields::at(&bytes, at))?);
            s.register(at, v.clone());
            Ok(Some(v))
        }
        p => s.shared(p, 4, SOUND_FILE_SIZE, |_, _| {
            Err(ZoneError::Invalid("sound file"))
        }),
    }
}

fn sound_file_body(s: &mut Stream, f: &mut Fields) -> Result<SoundFile> {
    let kind = f.u8();
    let exists = f.u8() != 0;
    f.skip(2);
    let source = match kind {
        SAT_LOADED => {
            let slot = f.slot();
            SoundSource::Loaded(loaded_at(s, slot, f.ptr()?)?)
        }
        SAT_STREAMED => {
            let (dir, name) = (f.ptr()?, f.ptr()?);
            SoundSource::Streamed {
                dir: s.string(dir)?,
                name: s.string(name)?,
            }
        }
        _ => SoundSource::Unknown,
    };
    Ok(SoundFile { exists, source })
}

fn channel_map(f: &mut Fields) -> ChannelMap {
    let speaker_count = f.i32();
    let speakers = [(); 6].map(|_| SpeakerLevels {
        speaker: f.i32(),
        level_count: f.i32(),
        levels: [f.f32(), f.f32()],
    });
    ChannelMap {
        speaker_count,
        speakers,
    }
}

fn speaker_map_body(s: &mut Stream, h: &[u8]) -> Result<SpeakerMap> {
    let mut f = Fields::new(h);
    let is_default = f.u8() != 0;
    f.skip(3);
    let name = f.ptr()?;
    let channel_maps = [(); 2].map(|_| [(); 2].map(|_| channel_map(&mut f)));
    Ok(SpeakerMap {
        is_default,
        name: s.string(name)?,
        channel_maps,
    })
}

/// A volume falloff curve of up to eight (distance, volume) knots.
#[derive(Debug)]
pub struct SndCurve {
    pub name: Name,
    pub knot_count: i32,
    pub knots: [[f32; 2]; 8],
}

pub(super) fn load_curve(s: &mut Stream, p: Ptr) -> Result<Option<Arc<SndCurve>>> {
    curve_at(s, None, p)
}

fn curve_at(s: &mut Stream, slot: Option<Addr>, p: Ptr) -> Result<Option<Arc<SndCurve>>> {
    s.temp_asset_at(slot, p, 4, 72, |s, h| {
        let mut f = Fields::new(h);
        let name = f.ptr()?;
        let knot_count = f.i32();
        let knots = [(); 8].map(|_| [f.f32(), f.f32()]);
        Ok(SndCurve {
            name: s.string(name)?,
            knot_count,
            knots,
        })
    })
}

/// A PCM/ADPCM sound held in the zone.
#[derive(Debug)]
pub struct LoadedSound {
    pub name: Name,
    pub format: i32,
    pub rate: u32,
    pub bits: i32,
    pub channels: i32,
    pub samples: u32,
    pub block_size: u32,
    /// Sample bytes; empty when the consumer drops presentation data.
    pub data: Arc<[u8]>,
}

pub(super) fn load_loaded(s: &mut Stream, p: Ptr) -> Result<Option<Arc<LoadedSound>>> {
    loaded_at(s, None, p)
}

fn loaded_at(s: &mut Stream, slot: Option<Addr>, p: Ptr) -> Result<Option<Arc<LoadedSound>>> {
    s.temp_asset_at(slot, p, 4, 44, |s, h| {
        let mut f = Fields::new(h);
        let name = f.ptr()?;
        let format = f.i32();
        f.skip(4); // data pointer (runtime)
        let data_len = f.u32();
        let rate = f.u32();
        let bits = f.i32();
        let channels = f.i32();
        let samples = f.u32();
        let block_size = f.u32();
        f.skip(4); // initial pointer (runtime)
        let data = f.ptr()?;
        let name = s.string(name)?;
        let data = match data {
            Ptr::Follow | Ptr::Insert => {
                s.push(Block::Temp);
                let slot = if data == Ptr::Insert {
                    Some(s.insert_slot()?)
                } else {
                    None
                };
                let d: Arc<[u8]> = s.load_presentation(1, data_len)?.1.into();
                if let Some(slot) = slot {
                    s.register(slot, d.clone());
                }
                s.pop()?;
                d
            }
            Ptr::Null => Arc::from(Vec::new()),
            Ptr::Offset(a) => s.lookup::<Arc<[u8]>>(a)?,
        };
        Ok(LoadedSound {
            name,
            format,
            rate,
            bits,
            channels,
            samples,
            block_size,
            data,
        })
    })
}

/// The asset-list entry carries no stream data, so there is nothing to decode.
#[derive(Debug)]
pub struct SndDriverGlobals {
    pub name: Name,
}

pub(super) fn load_driver_globals(_: &mut Stream) -> Result<Option<Arc<SndDriverGlobals>>> {
    Ok(None)
}
