// SPDX-License-Identifier: GPL-3.0-only
//! The sound alias tables of the zones: names to lists of variants, each with its volume, pitch and distance
//! ranges, falloff curve, speaker map, channel and flags, and where its audio is (in the zone, or a file in an
//! IWD). Also `soundaliases/channels.def`.

use crate::channels::{self, ChannelDef};
use crate::curve::Curve;
use crate::mixer::Pcm;
use assets::vfs::Vfs;
use assets::zone::sound::{SoundAlias, SoundAliasList, SoundSource, SpeakerMap};
use assets::zone::{Asset, DecodeFilter, XAssetType, Zone};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use web_time::Instant;

/// Where an alias's audio is.
#[derive(Clone, Debug)]
pub enum Clip {
    /// PCM held in the zone.
    Loaded(Arc<Pcm>),
    /// A file in the search path, decoded as it plays.
    Streamed(String),
    /// A placeholder that makes no sound (`,null.wav`) or whose data the zone does not hold.
    Silent,
}

#[derive(Debug)]
pub struct Alias {
    pub name: Arc<str>,
    pub secondary: Option<Arc<str>>,
    /// The alias that plays where this one ends (`chainAliasName`).
    pub chain: Option<Arc<str>>,
    pub audio: Clip,
    pub volume: (f32, f32),
    pub pitch: (f32, f32),
    pub dist: (f32, f32),
    pub curve: Curve,
    /// Row of `channels.def`.
    pub channel: u8,
    pub looping: bool,
    pub random_looping: bool,
    pub master: bool,
    /// The alias keeps out of the room reverb.
    pub no_wet: bool,
    pub slave: bool,
    pub slave_percentage: f32,
    pub delay_ms: u32,
    pub probability: f32,
    /// The speaker map for two output speakers: `[stereo source][source channel][left, right]`.
    pub speaker: [[[f32; 2]; 2]; 2],
}

struct List {
    aliases: Vec<Arc<Alias>>,
    /// The original's anti-repeat counters, one per alias.
    sequence: Vec<i32>,
}

/// What loading found, for the report.
#[derive(Clone, Debug, Default)]
pub struct LoadStats {
    pub lists: usize,
    pub aliases: usize,
    pub loaded: usize,
    pub streamed: usize,
    pub silent: usize,
    pub pcm_bytes: usize,
    pub channels: usize,
    pub zones: Vec<(String, Duration)>,
}

pub struct Bank {
    pub channels: Vec<ChannelDef>,
    lists: HashMap<String, List>,
    vfs: Arc<Vfs>,
    /// The original's alias picker state (`g_sa.randSeed`).
    seed: i32,
    rng: u32,
    pub stats: LoadStats,
}

struct SoundOnly;

impl DecodeFilter for SoundOnly {
    fn keep(&self, ty: XAssetType) -> bool {
        matches!(ty, XAssetType::Sound | XAssetType::RawFile)
    }
}

const CHANNELS_DEF: &str = "soundaliases/channels.def";

fn volume_pair(a: f32, b: f32) -> (f32, f32) {
    (a.min(b), a.max(b))
}

fn speaker_gains(map: Option<&SpeakerMap>) -> [[[f32; 2]; 2]; 2] {
    let mut g = [[[0.0; 2]; 2]; 2];
    match map {
        Some(m) => {
            for (stereo, g) in g.iter_mut().enumerate() {
                let cm = &m.channel_maps[stereo][0];
                let n = usize::try_from(cm.speaker_count).unwrap_or(0).min(6);
                for s in &cm.speakers[..n] {
                    // Speakers 0 and 1 are front left and right; the rest do not exist on a stereo output.
                    if let Ok(o @ 0..2) = usize::try_from(s.speaker) {
                        for (c, row) in g.iter_mut().enumerate() {
                            row[o] = s.levels[c];
                        }
                    }
                }
            }
        }
        None => {
            g[0][0] = [0.5, 0.5];
            g[1] = [[1.0, 0.0], [0.0, 1.0]];
        }
    }
    g
}

fn pcm_of(l: &assets::zone::sound::LoadedSound) -> Option<Pcm> {
    // Every loaded sound of the MP zones is headerless 16-bit little-endian PCM (format 1).
    if l.format != 1 || l.bits != 16 || l.rate == 0 || l.data.len() < 2 {
        return None;
    }
    let channels = u8::try_from(l.channels)
        .ok()
        .filter(|c| (1..=2).contains(c))?;
    Some(Pcm {
        rate: l.rate,
        channels,
        samples: l
            .data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect(),
    })
}

impl Bank {
    /// Reads the zones in order (a later zone's list replaces an earlier one of the same name).
    pub fn load(vfs: Arc<Vfs>, zones: &[PathBuf]) -> Result<Self, String> {
        let mut bank = Self {
            channels: Vec::new(),
            lists: HashMap::new(),
            vfs,
            seed: 1,
            rng: 0x9e37_79b9,
            stats: LoadStats::default(),
        };
        let mut loaded: HashMap<String, Arc<Pcm>> = HashMap::new();
        let mut channels = None;
        for path in zones {
            let t = Instant::now();
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let file =
                assets::fs::buffered(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let zone = Zone::open(file).map_err(|e| format!("{name}: {e}"))?;
            zone.decode(&SoundOnly, |a| match a {
                Asset::Sound(list) => bank.add_list(&list, &mut loaded),
                Asset::RawFile(r)
                    if r.name
                        .as_deref()
                        .is_some_and(|n| n.eq_ignore_ascii_case(CHANNELS_DEF)) =>
                {
                    let text = r.data.strip_suffix(&[0]).unwrap_or(&r.data);
                    channels = Some(channels::parse(&String::from_utf8_lossy(text)));
                }
                _ => {}
            })
            .map_err(|e| format!("{name}: {e}"))?;
            bank.stats.zones.push((name, t.elapsed()));
        }
        bank.channels = channels.ok_or_else(|| format!("no {CHANNELS_DEF} in the zones"))?;
        bank.stats.channels = bank.channels.len();
        bank.stats.lists = bank.lists.len();
        bank.stats.aliases = bank.lists.values().map(|l| l.aliases.len()).sum();
        Ok(bank)
    }

    fn add_list(&mut self, list: &SoundAliasList, loaded: &mut HashMap<String, Arc<Pcm>>) {
        let Some(name) = &list.name else { return };
        let aliases: Vec<Arc<Alias>> = list
            .aliases
            .iter()
            .filter_map(|a| self.alias(a, loaded))
            .collect();
        let sequence = vec![0; aliases.len()];
        self.lists
            .insert(name.to_ascii_lowercase(), List { aliases, sequence });
    }

    fn alias(
        &mut self,
        a: &SoundAlias,
        loaded: &mut HashMap<String, Arc<Pcm>>,
    ) -> Option<Arc<Alias>> {
        let name = a.name.clone()?;
        let audio = match a.sound_file.as_deref().map(|f| &f.source) {
            Some(SoundSource::Loaded(Some(l))) => {
                let key = l.name.as_deref().unwrap_or("").to_ascii_lowercase();
                match loaded.get(&key) {
                    Some(p) => Clip::Loaded(p.clone()),
                    None => match pcm_of(l) {
                        Some(p) => {
                            let p = Arc::new(p);
                            self.stats.pcm_bytes += p.samples.len() * 2;
                            loaded.insert(key, p.clone());
                            Clip::Loaded(p)
                        }
                        None => Clip::Silent,
                    },
                }
            }
            Some(SoundSource::Streamed {
                dir: Some(dir),
                name: Some(file),
            }) => Clip::Streamed(format!("sound/{dir}/{file}")),
            _ => Clip::Silent,
        };
        match audio {
            Clip::Loaded(_) => self.stats.loaded += 1,
            Clip::Streamed(_) => self.stats.streamed += 1,
            Clip::Silent => self.stats.silent += 1,
        }
        let curve = a
            .volume_falloff_curve
            .as_deref()
            .map_or(Curve::LINEAR, Curve::from_asset);
        Some(Arc::new(Alias {
            name,
            secondary: a.secondary_alias_name.clone().filter(|s| !s.is_empty()),
            chain: a.chain_alias_name.clone().filter(|s| !s.is_empty()),
            audio,
            volume: volume_pair(a.vol_min, a.vol_max),
            pitch: volume_pair(a.pitch_min, a.pitch_max),
            dist: (a.dist_min, a.dist_max),
            curve,
            channel: a.channel() as u8,
            looping: a.looping(),
            random_looping: a.random_looping(),
            master: a.master(),
            no_wet: a.no_wet_level(),
            slave: a.slave(),
            slave_percentage: a.slave_percentage,
            delay_ms: u32::try_from(a.start_delay).unwrap_or(0),
            probability: a.probability,
            speaker: speaker_gains(a.speaker_map.as_deref()),
        }))
    }

    pub fn has(&self, name: &str) -> bool {
        self.lists
            .get(&name.to_ascii_lowercase())
            .is_some_and(|l| !l.aliases.is_empty())
    }

    /// Picks a variant of `name` the way the original does: by probability, never the one played last when
    /// there are more than two.
    pub fn pick(&mut self, name: &str) -> Option<Arc<Alias>> {
        let list = self.lists.get_mut(&name.to_ascii_lowercase())?;
        let n = list.aliases.len();
        if n == 0 {
            return None;
        }
        let seed = &mut self.seed;
        let mut roll = |p: f32, cumulative: f32| {
            *seed = seed.wrapping_mul(214_013).wrapping_add(2_531_011);
            p * 32768.0 > ((*seed >> 16) & 0x7fff) as f32 * cumulative
        };
        let mut best = 0;
        let mut cumulative = list.aliases[0].probability;
        let mut max_seq = list.sequence[0];
        for i in 1..n {
            cumulative += list.aliases[i].probability;
            if roll(list.aliases[i].probability, cumulative) {
                best = i;
            }
            max_seq = max_seq.max(list.sequence[i]);
        }
        if n > 2 && list.sequence[best] == max_seq {
            let mut cumulative = 0.0;
            for i in 0..n {
                if list.sequence[i] != max_seq {
                    cumulative += list.aliases[i].probability;
                    if roll(list.aliases[i].probability, cumulative) {
                        best = i;
                    }
                }
            }
        }
        list.sequence[best] = max_seq + 1;
        Some(list.aliases[best].clone())
    }

    /// A uniform number in `[lo, hi]`.
    pub fn between(&mut self, (lo, hi): (f32, f32)) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        lo + (hi - lo) * (self.rng >> 8) as f32 / (1u32 << 24) as f32
    }

    /// The bytes of a streamed alias's file.
    pub fn read_stream(&self, path: &str) -> Result<Arc<[u8]>, String> {
        self.vfs
            .read(path)
            .map_err(|e| format!("{path}: {e}"))?
            .map(Arc::from)
            .ok_or_else(|| format!("{path}: not in the search path"))
    }

    /// Every alias list name, for tests and reports.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.lists.keys().map(String::as_str)
    }

    pub fn aliases_of(&self, name: &str) -> &[Arc<Alias>] {
        self.lists
            .get(&name.to_ascii_lowercase())
            .map_or(&[], |l| &l.aliases)
    }
}

#[cfg(test)]
impl Bank {
    /// A bank of hand-made aliases (a list per name), for tests that need no install.
    pub(crate) fn synthetic(channels: Vec<ChannelDef>, lists: Vec<(&str, Vec<Alias>)>) -> Self {
        let vfs = assets::vfs::Builder::new(std::path::Path::new("."))
            .finish(0)
            .unwrap();
        let mut bank = Self {
            channels,
            lists: HashMap::new(),
            vfs: Arc::new(vfs),
            seed: 1,
            rng: 0x9e37_79b9,
            stats: LoadStats::default(),
        };
        for (name, aliases) in lists {
            let aliases: Vec<Arc<Alias>> = aliases.into_iter().map(Arc::new).collect();
            let sequence = vec![0; aliases.len()];
            bank.lists
                .insert(name.to_ascii_lowercase(), List { aliases, sequence });
        }
        bank
    }
}

#[cfg(test)]
impl Alias {
    /// A plain 2D one-shot of `audio` on channel `channel`.
    pub(crate) fn test(name: &str, channel: u8, audio: Clip) -> Self {
        Self {
            name: name.into(),
            secondary: None,
            chain: None,
            audio,
            volume: (1.0, 1.0),
            pitch: (1.0, 1.0),
            dist: (10.0, 1000.0),
            curve: Curve::LINEAR,
            channel,
            looping: false,
            random_looping: false,
            master: false,
            no_wet: false,
            slave: false,
            slave_percentage: 1.0,
            delay_ms: 0,
            probability: 1.0,
            speaker: speaker_gains(None),
        }
    }
}
