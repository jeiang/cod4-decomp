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
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use web_time::Instant;

use assets::vfs::{LANGUAGES, Vfs};
use assets::zone::clipmap::Clipmap;
use assets::zone::fx::{FxEffectDef, FxImpactTable};
use assets::zone::text::StringTable;
use assets::zone::weapon::WeaponDef;
use assets::zone::xanim::XAnimParts;
use assets::zone::xmodel::XModel;
use assets::zone::{Asset, Consumer, DecodeFilter, XAssetType, Zone};

use crate::delta::RootMotion;
use crate::tags::Skeleton;

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
        let language = assets::fs::read_to_string(root.join("localization.txt"))
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
    if assets::fs::exists(&direct) {
        return Some(direct);
    }
    assets::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        .map(|e| e.path())
}

/// Content of one layer: the boot zones, or the current map's zone.
#[derive(Default)]
/// What the server keeps of an animation: its length and notetracks.
#[derive(Debug)]
pub struct AnimInfo {
    pub looping: bool,
    /// Seconds (`numframes / framerate`).
    pub length: f32,
    /// Notetrack name and normalized time, in file order.
    pub notes: Vec<(Arc<str>, f32)>,
}

impl AnimInfo {
    fn new(x: &XAnimParts, strings: &[Option<Arc<str>>]) -> Self {
        let notes = x
            .notify
            .iter()
            .filter_map(|n| Some((strings.get(usize::from(n.name))?.clone()?, n.time)))
            .collect();
        Self {
            looping: x.looping,
            length: f32::from(x.num_frames) / x.frame_rate,
            notes,
        }
    }
}

/// One resolved name per bone of a model.
pub type BoneNames = Arc<[Arc<str>]>;

/// A player-body animation kept whole for the server skeleton (`pb_*`: the full-body stand,
/// crouch, prone, run and death animations), with its part names resolved to text.
///
/// Script strings are indices into the zone that defined the asset, so the names are
/// resolved here while that zone's table is at hand; a model from another zone is bound to
/// the animation by comparing text (`sim::skel::Rig::bind`).
#[derive(Debug)]
pub struct PlayerAnim {
    pub parts: Arc<XAnimParts>,
    pub part_names: Box<[Arc<str>]>,
}

impl PlayerAnim {
    /// Heap bytes the decoded keyframe data holds (the name table excluded).
    pub fn data_bytes(&self) -> usize {
        use assets::zone::xanim::Indices;
        let a = &*self.parts;
        let idx = |i: &Indices| match i {
            Indices::None => 0,
            Indices::Byte(v) => v.len(),
            Indices::Short(v) => v.len() * 2,
        };
        a.data_byte.len()
            + a.data_short.len() * 2
            + a.data_int.len() * 4
            + a.random_data_byte.len()
            + a.random_data_short.len() * 2
            + a.random_data_int.len() * 4
            + a.names.len() * 2
            + idx(&a.indices)
    }
}

/// True for the animations the server skeleton samples (`pb_*`), and, for a client, the weapon
/// view model animations (`viewmodel_*`).
fn is_player_anim(name: &str, client: bool) -> bool {
    let starts = |p: &str| name.len() > p.len() && name[..p.len()].eq_ignore_ascii_case(p);
    starts("pb_") || (client && starts("viewmodel_"))
}

/// The server's asset selection, with the render payload (vertices, skin weights) kept when the
/// content is for a client.
struct ContentFilter {
    presentation: bool,
}

impl DecodeFilter for ContentFilter {
    fn keep(&self, ty: XAssetType) -> bool {
        // A client draws effects; the server only needs the weapons that name them.
        (self.presentation && matches!(ty, XAssetType::Fx | XAssetType::ImpactFx))
            || Consumer::Server.keep(ty)
    }

    fn keep_presentation(&self) -> bool {
        self.presentation
    }
}

#[derive(Default)]
struct Layer {
    /// Lowercase name to (tag, asset).
    raw: HashMap<String, (u8, Arc<[u8]>)>,
    tables: HashMap<String, (u8, Arc<StringTable>)>,
    weapons: HashMap<String, (u8, Arc<WeaponDef>)>,
    models: HashMap<String, (u8, Arc<XModel>)>,
    skeletons: HashMap<String, (u8, Arc<Skeleton>)>,
    anims: HashMap<String, (u8, Arc<AnimInfo>)>,
    player_anims: HashMap<String, (u8, Arc<PlayerAnim>)>,
    /// Bone names of every model, as text.
    model_bones: HashMap<String, (u8, BoneNames)>,
    /// The tag names each weapon hides on its view model (`hideTags`), as text.
    hide_tags: HashMap<String, (u8, BoneNames)>,
    motions: HashMap<String, (u8, Arc<RootMotion>)>,
    localize: HashMap<String, (u8, Arc<str>)>,
    clipmap: Option<(u8, Arc<Clipmap>)>,
    /// Effects by lowercase name (client content only).
    fx: HashMap<String, (u8, Arc<FxEffectDef>)>,
    impact: Option<(u8, Arc<FxImpactTable>)>,
}

fn resolve(strings: &[Option<Arc<str>>], i: u16) -> Arc<str> {
    strings
        .get(usize::from(i))
        .cloned()
        .flatten()
        .unwrap_or_else(|| Arc::from(""))
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
    /// A client's content: models keep their vertices and skin weights, and the view model
    /// animations are retained with the player animations.
    pub client: bool,
    base: Layer,
    map: Layer,
    pub map_name: Option<String>,
    /// Per-zone decode time, for the boot log.
    pub timings: Vec<(String, Duration)>,
}

impl Content {
    /// Content for a client: as the server's, plus what drawing a model needs.
    pub fn for_client() -> Self {
        Self {
            client: true,
            ..Self::default()
        }
    }

    /// Decodes `<zone>.ff` into the base layer (`tag`) or, when `tag` is [`MAP_TAG`], into the
    /// map layer.
    pub fn load_zone(&mut self, install: &Install, zone: &str, tag: u8) -> Result<(), String> {
        let path = install
            .zone_path(zone)
            .ok_or_else(|| format!("Could not find zone '{zone}'"))?;
        let t = Instant::now();
        let file = assets::fs::buffered(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let z = Zone::open(file).map_err(|e| format!("{zone}: {e}"))?;
        let layer = if tag == MAP_TAG {
            &mut self.map
        } else {
            &mut self.base
        };
        let strings = z.script_strings().to_vec();
        let client = self.client;
        let filter = ContentFilter {
            presentation: client,
        };
        z.decode(&filter, |a| match a {
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
                    let tags: BoneNames = w
                        .hide_tags
                        .iter()
                        .take_while(|t| **t != 0)
                        .map(|t| resolve(&strings, *t))
                        .collect();
                    put(&mut layer.hide_tags, tag, &n, tags);
                    put(&mut layer.weapons, tag, &n, w);
                }
            }
            Asset::XModel(m) => {
                if let Some(n) = m.name.clone() {
                    let bones: Arc<[Arc<str>]> =
                        m.bone_names.iter().map(|b| resolve(&strings, *b)).collect();
                    put(&mut layer.model_bones, tag, &n, bones);
                    let skel = Arc::new(Skeleton::new(&m, &strings));
                    put(&mut layer.skeletons, tag, &n, skel);
                    put(&mut layer.models, tag, &n, m);
                }
            }
            Asset::XAnimParts(x) => {
                if let Some(n) = x.name.clone() {
                    if let Some(m) = RootMotion::new(&x) {
                        put(&mut layer.motions, tag, &n, Arc::new(m));
                    }
                    put(
                        &mut layer.anims,
                        tag,
                        &n,
                        Arc::new(AnimInfo::new(&x, &strings)),
                    );
                    if is_player_anim(&n, client) {
                        let part_names = x.names.iter().map(|p| resolve(&strings, *p)).collect();
                        put(
                            &mut layer.player_anims,
                            tag,
                            &n,
                            Arc::new(PlayerAnim {
                                parts: x,
                                part_names,
                            }),
                        );
                    }
                }
            }
            Asset::Fx(e) => {
                if let Some(n) = e.name.clone() {
                    put(&mut layer.fx, tag, &n, e);
                }
            }
            Asset::ImpactFx(t) if layer.impact.as_ref().is_none_or(|(g, _)| *g <= tag) => {
                layer.impact = Some((tag, t));
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

    /// An effect by name (`fx/...`); client content only.
    pub fn fx(&self, name: &str) -> Option<&Arc<FxEffectDef>> {
        get(&self.map.fx, name).or_else(|| get(&self.base.fx, name))
    }

    /// The bullet and explosion impact effects by weapon impact type and surface.
    pub fn impact_table(&self) -> Option<&Arc<FxImpactTable>> {
        self.map
            .impact
            .as_ref()
            .or(self.base.impact.as_ref())
            .map(|(_, t)| t)
    }

    pub fn weapon(&self, name: &str) -> Option<&Arc<WeaponDef>> {
        get(&self.map.weapons, name).or_else(|| get(&self.base.weapons, name))
    }

    /// Every effect the loaded zones define, each name once (the map's copy wins); client content only.
    pub fn effects(&self) -> Vec<Arc<FxEffectDef>> {
        let base = self
            .base
            .fx
            .iter()
            .filter(|(k, _)| !self.map.fx.contains_key(*k));
        self.map
            .fx
            .iter()
            .chain(base)
            .map(|(_, (_, e))| e.clone())
            .collect()
    }

    /// Every weapon definition the loaded zones define, each name once (the map's copy wins).
    pub fn weapons(&self) -> Vec<Arc<WeaponDef>> {
        let base = self
            .base
            .weapons
            .iter()
            .filter(|(k, _)| !self.map.weapons.contains_key(*k));
        self.map
            .weapons
            .iter()
            .chain(base)
            .map(|(_, (_, w))| w.clone())
            .collect()
    }

    pub fn model(&self, name: &str) -> Option<&Arc<XModel>> {
        get(&self.map.models, name).or_else(|| get(&self.base.models, name))
    }

    pub fn skeleton(&self, name: &str) -> Option<&Arc<Skeleton>> {
        get(&self.map.skeletons, name).or_else(|| get(&self.base.skeletons, name))
    }

    /// Names of the loaded models that start with `prefix`, sorted.
    pub fn model_names(&self, prefix: &str) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .base
            .models
            .keys()
            .chain(self.map.models.keys())
            .map(String::as_str)
            .filter(|n| n.starts_with(prefix))
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Names of the loaded animations that start with `prefix`, sorted.
    pub fn anim_names(&self, prefix: &str) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .base
            .anims
            .keys()
            .chain(self.map.anims.keys())
            .map(String::as_str)
            .filter(|n| n.starts_with(prefix))
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// The root-motion track of an animation, when it has one.
    pub fn root_motion(&self, name: &str) -> Option<&Arc<RootMotion>> {
        get(&self.map.motions, name).or_else(|| get(&self.base.motions, name))
    }

    pub fn anim(&self, name: &str) -> Option<&Arc<AnimInfo>> {
        get(&self.map.anims, name).or_else(|| get(&self.base.anims, name))
    }

    /// A retained `pb_*` player-body animation.
    pub fn player_anim(&self, name: &str) -> Option<&Arc<PlayerAnim>> {
        get(&self.map.player_anims, name).or_else(|| get(&self.base.player_anims, name))
    }

    /// Every retained player-body animation, each name once.
    pub fn player_anims(&self) -> impl Iterator<Item = &Arc<PlayerAnim>> {
        let base = self
            .base
            .player_anims
            .iter()
            .filter(|(k, _)| !self.map.player_anims.contains_key(*k));
        self.map
            .player_anims
            .values()
            .chain(base.map(|(_, v)| v))
            .map(|(_, a)| a)
    }

    /// One bone name per bone of the model, resolved from the zone that defined it.
    pub fn model_bone_names(&self, name: &str) -> Option<&BoneNames> {
        get(&self.map.model_bones, name).or_else(|| get(&self.base.model_bones, name))
    }

    /// The tags `weapon` hides on its view model (`hideTags`): the sights and parts other variants of the model show.
    pub fn weapon_hide_tags(&self, name: &str) -> Option<&BoneNames> {
        get(&self.map.hide_tags, name).or_else(|| get(&self.base.hide_tags, name))
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
