// SPDX-License-Identifier: GPL-3.0-or-later
//! The original install as the server sees it: the VFS and the dedicated-server zone set.
//!
//! A dedicated server loads `code_post_gfx_mp`, `localized_code_post_gfx_mp`, `common_mp` and
//! `localized_common_mp` at boot (no `ui_mp`, no `<map>_load`), then one map zone per map
//! load. Zones are decoded with the [`Consumer::Server`] filter and only the assets the
//! simulation reads are retained. An asset name defined by two zones resolves to the zone with
//! the higher tag, the later load winning ties (boot-load research section 6.1); the map zone
//! (tag 8) is always the highest and is dropped when the next map loads.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use assets::vfs::{LANGUAGES, Vfs};
use assets::zone::clipmap::Clipmap;
use assets::zone::text::StringTable;
use assets::zone::weapon::WeaponDef;
use assets::zone::xanim::XAnimParts;
use assets::zone::xmodel::XModel;
use assets::zone::{Asset, Consumer, Zone};

/// Zone load order and tags of a dedicated server (`code_post_gfx_mp` first).
pub const BOOT_ZONES: [(&str, u8); 4] = [
    ("code_post_gfx_mp", 2),
    ("localized_code_post_gfx_mp", 0),
    ("common_mp", 4),
    ("localized_common_mp", 1),
];
pub const MAP_TAG: u8 = 8;

pub struct Install {
    pub root: PathBuf,
    pub vfs: Vfs,
    pub language: &'static str,
}

impl Install {
    /// Opens the install: language from line 1 of `localization.txt` (default `english`) and
    /// the stock search path.
    pub fn open(root: &Path) -> io::Result<Self> {
        let language = std::fs::read_to_string(root.join("localization.txt"))
            .ok()
            .and_then(|t| t.lines().next().map(|l| l.trim().to_ascii_lowercase()))
            .and_then(|l| LANGUAGES.iter().position(|n| *n == l))
            .unwrap_or(0);
        Ok(Self {
            root: root.to_owned(),
            vfs: Vfs::open_stock(root, language)?,
            language: LANGUAGES[language],
        })
    }

    /// `zone/<language>/<name>.ff`, matched case-insensitively.
    pub fn zone_path(&self, name: &str) -> Option<PathBuf> {
        let mut dir = self.root.clone();
        for part in ["zone", self.language] {
            dir = find_ci(&dir, part)?;
        }
        find_ci(&dir, &format!("{name}.ff"))
    }

    pub fn map_exists(&self, map: &str) -> bool {
        self.zone_path(map).is_some()
    }
}

fn find_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.exists() {
        return Some(direct);
    }
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        .map(|e| e.path())
}

/// Content of one layer: the boot zones, or the current map's zone.
#[derive(Default)]
struct Layer {
    /// Lowercase name to (tag, asset).
    raw: HashMap<String, (u8, Arc<[u8]>)>,
    tables: HashMap<String, (u8, Arc<StringTable>)>,
    weapons: HashMap<String, (u8, Arc<WeaponDef>)>,
    models: HashMap<String, (u8, Arc<XModel>)>,
    anims: HashMap<String, (u8, Arc<XAnimParts>)>,
    localize: HashMap<String, (u8, Arc<str>)>,
    clipmap: Option<(u8, Arc<Clipmap>)>,
}

fn put<T>(m: &mut HashMap<String, (u8, T)>, tag: u8, name: &str, v: T) {
    let k = name.to_ascii_lowercase();
    if m.get(&k).is_none_or(|(t, _)| *t <= tag) {
        m.insert(k, (tag, v));
    }
}

fn get<'a, T>(m: &'a HashMap<String, (u8, T)>, name: &str) -> Option<&'a T> {
    m.get(&name.to_ascii_lowercase()).map(|(_, v)| v)
}

#[derive(Default)]
pub struct Content {
    base: Layer,
    map: Layer,
    pub map_name: Option<String>,
    /// Per-zone decode time, for the boot log.
    pub timings: Vec<(String, Duration)>,
}

impl Content {
    /// Decodes `<zone>.ff` into the base layer (`tag`) or, when `tag` is [`MAP_TAG`], into the
    /// map layer.
    pub fn load_zone(&mut self, install: &Install, zone: &str, tag: u8) -> Result<(), String> {
        let path = install
            .zone_path(zone)
            .ok_or_else(|| format!("Could not find zone '{zone}'"))?;
        let t = Instant::now();
        let file = File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let z = Zone::open(BufReader::with_capacity(1 << 16, file))
            .map_err(|e| format!("{zone}: {e}"))?;
        let layer = if tag == MAP_TAG {
            &mut self.map
        } else {
            &mut self.base
        };
        z.decode(&Consumer::Server, |a| match a {
            Asset::RawFile(r) => {
                if let Some(n) = &r.name {
                    // Drop the trailing NUL stored after the text.
                    let text = r.data.strip_suffix(&[0]).unwrap_or(&r.data);
                    put(&mut layer.raw, tag, n, Arc::from(text));
                }
            }
            Asset::StringTable(t) => {
                if let Some(n) = t.name.clone() {
                    put(&mut layer.tables, tag, &n, t);
                }
            }
            Asset::Weapon(w) => {
                if let Some(n) = w.internal_name.clone() {
                    put(&mut layer.weapons, tag, &n, w);
                }
            }
            Asset::XModel(m) => {
                if let Some(n) = m.name.clone() {
                    put(&mut layer.models, tag, &n, m);
                }
            }
            Asset::XAnimParts(x) => {
                if let Some(n) = x.name.clone() {
                    put(&mut layer.anims, tag, &n, x);
                }
            }
            Asset::Localize(l) => {
                if let (Some(n), Some(v)) = (&l.name, &l.value) {
                    put(&mut layer.localize, tag, n, v.clone());
                }
            }
            Asset::Clipmap(c) if layer.clipmap.as_ref().is_none_or(|(t, _)| *t <= tag) => {
                layer.clipmap = Some((tag, c));
            }
            _ => {}
        })
        .map_err(|e| format!("{zone}: {e}"))?;
        self.timings.push((zone.to_owned(), t.elapsed()));
        Ok(())
    }

    /// Loads the boot zone set.
    pub fn load_boot(&mut self, install: &Install) -> Result<(), String> {
        for (zone, tag) in BOOT_ZONES {
            self.load_zone(install, zone, tag)?;
        }
        Ok(())
    }

    /// Replaces the map layer with `map`'s zone (`{map, alloc 8, free 8}`).
    pub fn load_map(&mut self, install: &Install, map: &str) -> Result<(), String> {
        self.map = Layer::default();
        self.map_name = None;
        self.load_zone(install, map, MAP_TAG)?;
        self.map_name = Some(map.to_owned());
        Ok(())
    }

    /// Every rawfile (name, text) the zones define, each name once, the winning zone's copy.
    pub fn rawfiles(&self) -> impl Iterator<Item = (&str, &[u8])> {
        let base = self
            .base
            .raw
            .iter()
            .filter(|(k, _)| !self.map.raw.contains_key(*k));
        self.map
            .raw
            .iter()
            .chain(base)
            .map(|(k, (_, v))| (k.as_str(), &v[..]))
    }

    pub fn rawfile(&self, name: &str) -> Option<&[u8]> {
        get(&self.map.raw, name)
            .or_else(|| get(&self.base.raw, name))
            .map(|v| &v[..])
    }

    pub fn string_table(&self, name: &str) -> Option<&Arc<StringTable>> {
        get(&self.map.tables, name).or_else(|| get(&self.base.tables, name))
    }

    pub fn weapon(&self, name: &str) -> Option<&Arc<WeaponDef>> {
        get(&self.map.weapons, name).or_else(|| get(&self.base.weapons, name))
    }

    pub fn model(&self, name: &str) -> Option<&Arc<XModel>> {
        get(&self.map.models, name).or_else(|| get(&self.base.models, name))
    }

    pub fn anim(&self, name: &str) -> Option<&Arc<XAnimParts>> {
        get(&self.map.anims, name).or_else(|| get(&self.base.anims, name))
    }

    pub fn localize(&self, name: &str) -> Option<&Arc<str>> {
        get(&self.map.localize, name).or_else(|| get(&self.base.localize, name))
    }

    pub fn clipmap(&self) -> Option<&Arc<Clipmap>> {
        self.map
            .clipmap
            .as_ref()
            .or(self.base.clipmap.as_ref())
            .map(|(_, c)| c)
    }
}
