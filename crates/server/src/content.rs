// SPDX-License-Identifier: GPL-3.0-only
//! The original install as the server sees it: the VFS and the dedicated-server zone set.
//!
//! A dedicated server loads `code_post_gfx_mp`, `localized_code_post_gfx_mp`, `common_mp` and
//! `localized_common_mp` at boot (no `ui_mp`, no `<map>_load`), then one map zone per map
//! load. Zones are decoded with the [`Consumer::Server`] filter and only the assets the
//! simulation reads are retained. An asset name defined by two zones resolves to the zone with
//! the higher tag, the later load winning ties (boot-load research section 6.1); the map zone
//! (tag 8) is dropped when the next map loads. A mod's `mod.ff` (tag 16, `DB_ZONE_MOD`) is loaded
//! after the boot zones and outranks them and the map zone.

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
use assets::zone::{Asset, Consumer, Db, DecodeFilter, XAssetType, Zone};

use crate::delta::RootMotion;
use crate::tags::Skeleton;
use sim::pm::{MANTLE_ANIM_NAMES, MantleAnims};

/// Zone load order and tags of a dedicated server (`code_post_gfx_mp` first).
pub const BOOT_ZONES: [(&str, u8); 4] = [
    ("code_post_gfx_mp", 2),
    ("localized_code_post_gfx_mp", 0),
    ("common_mp", 4),
    ("localized_common_mp", 1),
];
pub const MAP_TAG: u8 = 8;
/// `DB_ZONE_MOD`: the mod's `mod.ff` outranks every other zone, the map's included.
pub const MOD_TAG: u8 = 16;

pub struct Install {
    pub root: PathBuf,
    pub vfs: Vfs,
    pub language: &'static str,
    /// `fs_game`: the mod directory (`mods/<name>`) the search path and the fastfile lookups include; empty for stock.
    pub game: String,
    /// The custom map whose `usermaps/<map>` package the search path includes.
    usermap: Option<String>,
}

/// `zone/<language>` of the install at `root`, matched case-insensitively; where a downloaded map zone goes.
pub fn zone_dir(root: &Path) -> Option<PathBuf> {
    assets::paths::zone_dir(root)
}

impl Install {
    /// Opens the stock install: language from line 1 of `localization.txt` (default `english`) and the stock search
    /// path.
    pub fn open(root: &Path) -> io::Result<Self> {
        Self::open_game(root, "")
    }

    /// [`Self::open`] with the mod `game` (`fs_game`) on top of the stock search path.
    pub fn open_game(root: &Path, game: &str) -> io::Result<Self> {
        let language = assets::paths::language_index(root);
        Ok(Self {
            root: root.to_owned(),
            vfs: Vfs::open(root, language, game, None)?,
            language: LANGUAGES[language],
            game: game.to_owned(),
            usermap: None,
        })
    }

    /// Puts the package of `map` (`usermaps/<map>`: its IWDs and loose files) on the search path, or takes the previous
    /// map's off when `map` has none.
    pub fn mount_usermap(&mut self, map: &str) -> io::Result<()> {
        let map = map.strip_suffix("_load").unwrap_or(map);
        let want = assets::paths::is_usermap(&self.root, &self.game, map).then(|| map.to_owned());
        if want != self.usermap {
            let language = LANGUAGES.iter().position(|l| *l == self.language);
            self.vfs = Vfs::open(
                &self.root,
                language.unwrap_or(0),
                &self.game,
                want.as_deref(),
            )?;
            self.usermap = want;
        }
        Ok(())
    }

    /// The fastfile `name`: in the mod directory, then `zone/<language>`, then a custom map's `usermaps/<map>`.
    pub fn zone_path(&self, name: &str) -> Option<PathBuf> {
        assets::paths::zone_file(&self.root, &self.game, name)
    }

    pub fn map_exists(&self, map: &str) -> bool {
        self.zone_path(map).is_some()
    }
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
    /// Whether the last `anim_gunhand` notetrack at or before normalized `time` puts the gun in the left hand
    /// (`CG_ProcessClientNoteTracks`); a clip without one leaves it in the right.
    pub fn gun_hand_left(&self, time: f32) -> bool {
        self.notes
            .iter()
            .rfind(|(n, t)| n.starts_with("anim_gunhand") && *t <= time)
            .is_some_and(|(n, _)| n.contains("left"))
    }

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

/// A weapon's notetrack to sound alias pairs, as text.
pub type NoteSounds = Arc<[(Arc<str>, Arc<str>)]>;

/// A player-body animation kept whole for the server skeleton (`pb_*`: the full-body stand,
/// crouch, prone, run and death animations; `pt_*`: the upper-body fire, reload, melee, throw and flinch partials), with its part names resolved to text.
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

/// True for the animations the server skeleton samples (`pb_*` body animations and `pt_*` upper-body partials for a client), the
/// mantle animations whose root motion moves a climbing player, and, for a client, the weapon view model animations
/// (`viewmodel_*`) and the helicopter's rotors (`bh_rotors`).
fn is_player_anim(name: &str, client: bool) -> bool {
    let starts = |p: &str| name.len() > p.len() && name[..p.len()].eq_ignore_ascii_case(p);
    starts("pb_")
        || (client && starts("pt_"))
        || MANTLE_ANIM_NAMES
            .iter()
            .any(|n| n.eq_ignore_ascii_case(name))
        || (client && (starts("viewmodel_") || name.eq_ignore_ascii_case("bh_rotors")))
}

/// The server's asset selection, with the render payload (vertices, skin weights) kept when the
/// content is for a client.
struct ContentFilter {
    presentation: bool,
    /// Keep the zone's materials, which the effects and models of later zones name (a client).
    materials: bool,
}

impl DecodeFilter for ContentFilter {
    fn keep(&self, ty: XAssetType) -> bool {
        // A client draws effects; the server only needs the weapons that name them.
        (self.presentation && matches!(ty, XAssetType::Fx | XAssetType::ImpactFx))
            || (self.materials && ty == XAssetType::Material)
            // The alias names, for `soundexists`; the samples are presentation data and stay out.
            || ty == XAssetType::Sound
            || Consumer::Server.keep(ty)
    }

    fn keep_presentation(&self) -> bool {
        self.presentation
    }

    /// Sound alias lists are kept for their names only; linking them would retain their samples.
    fn link(&self, ty: XAssetType) -> bool {
        self.keep(ty) && ty != XAssetType::Sound
    }
}

#[derive(Default, Clone)]
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
    /// The sound alias each weapon plays at a notetrack of its view model animations (`notetrackSoundMap`).
    note_sounds: HashMap<String, (u8, NoteSounds)>,
    motions: HashMap<String, (u8, Arc<RootMotion>)>,
    localize: HashMap<String, (u8, Arc<str>)>,
    clipmap: Option<(u8, Arc<Clipmap>)>,
    /// Effects by lowercase name (client content only).
    fx: HashMap<String, (u8, Arc<FxEffectDef>)>,
    impact: Option<(u8, Arc<FxImpactTable>)>,
    /// Lowercase names of the sound alias lists.
    sounds: HashMap<String, (u8, ())>,
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

/// Takes the entries of `src` that outrank (or tie) those of `dst`.
fn merge_map<T: Clone>(dst: &mut HashMap<String, (u8, T)>, src: &HashMap<String, (u8, T)>) {
    for (k, (tag, v)) in src {
        if dst.get(k).is_none_or(|(t, _)| t <= tag) {
            dst.insert(k.clone(), (*tag, v.clone()));
        }
    }
}

impl Layer {
    /// Adds what `other` holds that outranks what is here: a mod's assets over a zone's.
    fn merge(&mut self, other: &Layer) {
        merge_map(&mut self.raw, &other.raw);
        merge_map(&mut self.tables, &other.tables);
        merge_map(&mut self.weapons, &other.weapons);
        merge_map(&mut self.models, &other.models);
        merge_map(&mut self.skeletons, &other.skeletons);
        merge_map(&mut self.anims, &other.anims);
        merge_map(&mut self.player_anims, &other.player_anims);
        merge_map(&mut self.model_bones, &other.model_bones);
        merge_map(&mut self.hide_tags, &other.hide_tags);
        merge_map(&mut self.note_sounds, &other.note_sounds);
        merge_map(&mut self.motions, &other.motions);
        merge_map(&mut self.localize, &other.localize);
        merge_map(&mut self.fx, &other.fx);
        merge_map(&mut self.sounds, &other.sounds);
        if let Some((tag, c)) = &other.clipmap
            && self.clipmap.as_ref().is_none_or(|(t, _)| t <= tag)
        {
            self.clipmap = Some((*tag, c.clone()));
        }
        if let Some((tag, i)) = &other.impact
            && self.impact.as_ref().is_none_or(|(t, _)| t <= tag)
        {
            self.impact = Some((*tag, i.clone()));
        }
    }
}

fn get<'a, T>(m: &'a HashMap<String, (u8, T)>, name: &str) -> Option<&'a T> {
    m.get(&name.to_ascii_lowercase()).map(|(_, v)| v)
}

#[derive(Default)]
pub struct Content {
    /// The assets the zones decoded so far define, by type and name: what a later zone's references by name (the
    /// materials of its effects and models, the models and effects of its weapons and props) link to.
    db: Db,
    /// A client's content: models keep their vertices and skin weights, and the view model
    /// animations are retained with the player animations.
    pub client: bool,
    base: Layer,
    map: Layer,
    /// What `mod.ff` defines, kept apart so each map's layer can take it again.
    modded: Layer,
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
        self.decode_zone(&path, zone, tag)
    }

    /// Loads the mod's `mod.ff` when the install's `fs_game` has one; whether it did.
    pub fn load_mod(&mut self, install: &Install) -> Result<bool, String> {
        let Some(path) = assets::paths::mod_zone(&install.root, &install.game) else {
            return Ok(false);
        };
        self.modded = Layer::default();
        self.db.unload(MOD_TAG);
        self.decode_zone(&path, "mod", MOD_TAG)?;
        self.base.merge(&self.modded);
        if self.map_name.is_some() {
            self.map.merge(&self.modded);
        }
        Ok(true)
    }

    fn decode_zone(&mut self, path: &Path, zone: &str, tag: u8) -> Result<(), String> {
        let t = Instant::now();
        let file = assets::fs::buffered(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let z = Zone::open(file).map_err(|e| format!("{zone}: {e}"))?;
        let layer = match tag {
            MAP_TAG => &mut self.map,
            MOD_TAG => &mut self.modded,
            _ => &mut self.base,
        };
        let strings = z.script_strings().to_vec();
        let client = self.client;
        let filter = ContentFilter {
            presentation: client,
            materials: client,
        };
        z.decode_linked(&filter, &mut self.db, tag, |a| match a {
            // Kept for the link table only: effects and models draw with them through their own references.
            Asset::Material(_) => {}
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
                    // The map ends at the first empty key; a pair with no sound plays nothing.
                    let notes: NoteSounds = w
                        .notetrack_sound_map_keys
                        .iter()
                        .zip(&w.notetrack_sound_map_values)
                        .take_while(|(k, _)| **k != 0)
                        .filter(|(_, v)| **v != 0)
                        .map(|(k, v)| (resolve(&strings, *k), resolve(&strings, *v)))
                        .collect();
                    put(&mut layer.note_sounds, tag, &n, notes);
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
            Asset::Sound(l) => {
                if let Some(n) = &l.name {
                    put(&mut layer.sounds, tag, n, ());
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
        self.load_mod(install)?;
        Ok(())
    }

    /// Replaces the map layer with `map`'s zone (`{map, alloc 8, free 8}`).
    pub fn load_map(&mut self, install: &Install, map: &str) -> Result<(), String> {
        self.map = Layer::default();
        self.map_name = None;
        self.db.unload(MAP_TAG);
        self.load_zone(install, map, MAP_TAG)?;
        self.map.merge(&self.modded);
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

    /// A rawfile made up for a test.
    #[cfg(test)]
    pub fn add_test_rawfile(&mut self, name: &str, text: &str) {
        self.base
            .raw
            .insert(name.to_ascii_lowercase(), (0, text.as_bytes().into()));
    }

    /// A sound alias list made up for a test.
    #[cfg(test)]
    pub fn add_test_sound(&mut self, name: &str) {
        put(&mut self.base.sounds, 0, name, ());
    }

    pub fn rawfile(&self, name: &str) -> Option<&[u8]> {
        get(&self.map.raw, name)
            .or_else(|| get(&self.base.raw, name))
            .map(|v| &v[..])
    }

    /// Whether a sound alias list of this name is in the loaded zones (`soundexists`).
    pub fn sound_exists(&self, name: &str) -> bool {
        get(&self.map.sounds, name)
            .or_else(|| get(&self.base.sounds, name))
            .is_some()
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

    /// The mantle animation set movement climbs ledges with; `None` when an animation is
    /// missing. Server and client both build it here so their prediction agrees.
    pub fn mantle_anims(&self) -> Option<MantleAnims> {
        MantleAnims::from_xanims(|name| self.player_anim(name).map(|a| &*a.parts))
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

    /// The sound aliases `weapon` plays at the notetracks of its view model animations (`notetrackSoundMap`): note name
    /// and alias.
    pub fn weapon_note_sounds(&self, name: &str) -> Option<&NoteSounds> {
        get(&self.map.note_sounds, name).or_else(|| get(&self.base.note_sounds, name))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mod_ff_rawfiles_override_the_stock_scripts_whatever_the_map() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod.ff");
        std::fs::write(
            &path,
            assets::zone::testing::rawfile_zone(&[
                ("maps/mp/gametypes/dm.gsc", "mod dm"),
                ("mod/new.gsc", "new"),
            ]),
        )
        .unwrap();
        let mut c = Content::default();
        c.add_test_rawfile("maps/mp/gametypes/dm.gsc", "stock dm");
        c.add_test_rawfile("maps/mp/gametypes/war.gsc", "stock war");
        c.map.raw.insert(
            "maps/mp/gametypes/dm.gsc".into(),
            (MAP_TAG, Arc::from(&b"map dm"[..])),
        );
        c.map_name = Some("mp_x".into());
        c.decode_zone(&path, "mod", MOD_TAG).unwrap();
        c.base.merge(&c.modded);
        c.map.merge(&c.modded);
        assert_eq!(c.rawfile("maps/mp/gametypes/dm.gsc").unwrap(), b"mod dm");
        assert_eq!(
            c.rawfile("maps/mp/gametypes/war.gsc").unwrap(),
            b"stock war"
        );
        assert_eq!(c.rawfile("mod/new.gsc").unwrap(), b"new");
    }

    /// A mod's rawfiles are readable as soon as `mod.ff` is loaded, before any map: the boot `exec` of a config the mod
    /// carries and the `radiant/keys.txt` lookups happen then.
    #[test]
    fn mod_ff_rawfiles_are_readable_right_after_load_mod_before_a_map() {
        let dir = tempfile::tempdir().unwrap();
        let m = dir.path().join("mods/t");
        std::fs::create_dir_all(&m).unwrap();
        std::fs::write(
            m.join("mod.ff"),
            assets::zone::testing::rawfile_zone(&[("mod/boot.cfg", "set x 1")]),
        )
        .unwrap();
        let install = Install::open_game(dir.path(), "mods/t").unwrap();
        let mut c = Content::default();
        assert!(c.load_mod(&install).unwrap());
        assert_eq!(c.map_name, None);
        assert_eq!(c.rawfile("mod/boot.cfg").unwrap(), b"set x 1");
        assert!(c.rawfiles().any(|(n, _)| n == "mod/boot.cfg"));
    }

    #[test]
    fn a_mods_assets_outrank_the_maps_and_the_stock_zones() {
        let mut stock = Layer::default();
        put(&mut stock.raw, 4, "maps/mp/a.gsc", Arc::from(&b"stock"[..]));
        put(&mut stock.raw, 4, "maps/mp/b.gsc", Arc::from(&b"stock"[..]));
        let mut map = Layer::default();
        put(
            &mut map.raw,
            MAP_TAG,
            "maps/mp/a.gsc",
            Arc::from(&b"map"[..]),
        );
        let mut modded = Layer::default();
        put(
            &mut modded.raw,
            MOD_TAG,
            "maps/mp/A.gsc",
            Arc::from(&b"mod"[..]),
        );
        stock.merge(&modded);
        map.merge(&modded);
        let text = |l: &Layer, n: &str| get(&l.raw, n).map(|b| b.to_vec());
        assert_eq!(text(&map, "maps/mp/a.gsc").unwrap(), b"mod");
        assert_eq!(text(&stock, "maps/mp/a.gsc").unwrap(), b"mod");
        assert_eq!(text(&stock, "maps/mp/b.gsc").unwrap(), b"stock");
    }

    #[test]
    fn the_gun_changes_hands_at_the_gunhand_notetracks() {
        let note = |n: &str, t: f32| (Arc::from(n), t);
        let reload = AnimInfo {
            looping: false,
            length: 2.0,
            notes: vec![
                note("anim_gunhand = \"left\"", 0.0),
                note("reload_clipout", 0.4),
                note("anim_gunhand = \"right\"", 0.9),
            ],
        };
        assert!(reload.gun_hand_left(0.0));
        assert!(reload.gun_hand_left(0.89));
        assert!(!reload.gun_hand_left(0.9));
        let pullout = AnimInfo {
            notes: vec![note("anim_gunhand = \"right\"", 0.85)],
            ..reload
        };
        assert!(!pullout.gun_hand_left(0.5), "no note yet: the right hand");
    }
}
