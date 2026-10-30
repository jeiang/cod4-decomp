// SPDX-License-Identifier: GPL-3.0-only
//! Cross-zone references. A zone stores an asset another zone defines as a stub: the name behind a comma
//! (`,gfx_smk_white_atlas`) and nothing else. The original's loader passes every asset it reads, nested or top-level,
//! through `DB_AddXAsset` → `DB_LinkXAssetEntry` (KisakCOD `database/db_registry.cpp`): a stub is looked up by type and
//! name, case-insensitively, among the assets loaded so far and the existing one is used in its place; a definition
//! joins that table, replacing an earlier zone's when its zone flags rank at least as high (`DB_OverrideAsset`, new
//! wins ties). Unloading a zone takes its definitions out. If no definition exists, `DB_CreateDefaultEntry` clones
//! the type's lowest-ranked default asset under the requested name, silently (material: `$default`). Failure to find
//! that default is a load error, not a skipped draw. See `db_registry.cpp:286-324,1617-1652,1708-1730`.
//!
//! [`Db`] is that table for one consumer: it lives across the zones the consumer decodes with
//! [`Zone::decode_linked`](super::Zone::decode_linked), which links the types the consumer's
//! [`DecodeFilter::link`](super::DecodeFilter::link) names. What the table holds is what the consumer decoded, so a
//! type decoded without its presentation payload links to the payload-less copy.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use super::XAssetType;
use super::fx::FxEffectDef;
use super::gfx::{GfxImage, Material, TechniqueSet};
use super::phys::PhysPreset;
use super::sound::{LoadedSound, SndCurve, SoundAliasList};
use super::xmodel::XModel;
use super::{Result, ZoneError};

/// Lowercase name to the definitions of every zone that gave one, by rising tag (ties in load order): the last is
/// the one in use, the rest its override chain (`XAssetEntry::nextOverride`), which unloading a zone promotes from.
type Table<T> = HashMap<String, Vec<(Option<u8>, Arc<T>)>>;

/// An asset type that zones reference by name across each other.
pub trait Linkable: Any + Send + Sync + Sized {
    const TYPE: XAssetType;
    const DEFAULT_NAME: &'static str;
    fn name(&self) -> Option<&str>;
    fn table(db: &mut Db) -> &mut Table<Self>;
    fn default_entry(&self, name: Arc<str>) -> Self;
}

macro_rules! linkable {
    ($($ty:ty => $variant:ident, $field:ident, $name:ident, $default:literal;)*) => {
        /// The assets of the zones decoded so far, by type and name (`db_hashTable`).
        #[derive(Default)]
        pub struct Db {
            $($field: Table<$ty>,)*
            sounds: Table<SoundAliasList>,
        }

        $(impl Linkable for $ty {
            const TYPE: XAssetType = XAssetType::$variant;
            const DEFAULT_NAME: &'static str = $default;
            fn name(&self) -> Option<&str> {
                self.$name.as_deref()
            }
            fn table(db: &mut Db) -> &mut Table<Self> {
                &mut db.$field
            }
            fn default_entry(&self, name: Arc<str>) -> Self {
                let mut out = self.clone();
                out.$name = Some(name);
                out
            }
        })*

        impl Db {
            /// Drops the definitions of the zone tagged `tag` (`DB_UnloadXZone`).
            pub fn unload(&mut self, tag: u8) {
                $(self.$field.retain(|_, chain| {
                    chain.retain(|(t, _)| *t != Some(tag));
                    !chain.is_empty()
                });)*
                self.sounds.retain(|_, chain| {
                    chain.retain(|(t, _)| *t != Some(tag));
                    !chain.is_empty()
                });
            }
        }
    };
}

linkable! {
    Material => Material, materials, name, "$default";
    TechniqueSet => TechniqueSet, techsets, name, "default";
    GfxImage => Image, images, name, "$white";
    XModel => XModel, models, name, "void";
    FxEffectDef => Fx, effects, name, "misc/missing_fx";
    PhysPreset => PhysPreset, phys_presets, name, "default";
    SndCurve => SoundCurve, curves, name, "default";
    LoadedSound => LoadedSound, loaded_sounds, name, "null.wav";
}

// DB_CreateDefaultEntry clears the sound's alias list instead of playing the default sound.
impl Linkable for SoundAliasList {
    const TYPE: XAssetType = XAssetType::Sound;
    const DEFAULT_NAME: &'static str = "null";

    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn table(db: &mut Db) -> &mut Table<Self> {
        &mut db.sounds
    }

    fn default_entry(&self, name: Arc<str>) -> Self {
        Self {
            name: Some(name),
            aliases: Arc::from([]),
        }
    }
}

/// Whether `name` is a stub: a reference to another zone's asset of the name behind the comma.
pub fn is_stub(name: &str) -> bool {
    name.starts_with(',')
}

impl Db {
    /// The definition of `name` (a leading comma ignored) in use, when a zone decoded so far has one.
    pub fn get<T: Linkable>(&mut self, name: &str) -> Option<&Arc<T>> {
        T::table(self)
            .get(&name.trim_start_matches(',').to_ascii_lowercase())
            .and_then(|chain| chain.last())
            .map(|(_, v)| v)
    }

    /// `DB_LinkXAssetEntry`: a stub uses the definition in use or a default entry (`DB_CreateDefaultEntry`); a
    /// definition joins the override chain in tag/load order. Defaults have no owning zone.
    pub(super) fn link<T: Linkable>(&mut self, tag: u8, v: Arc<T>) -> Result<Arc<T>> {
        let Some(name) = v.name() else { return Ok(v) };
        let plain = name.strip_prefix(',').unwrap_or(name);
        let key = plain.to_ascii_lowercase();
        if is_stub(name) {
            if let Some(found) = self.get::<T>(&key) {
                return Ok(found.clone());
            }
            // The original walks nextOverride to the bottom for the default, not the definition currently in use.
            let default = T::table(self)
                .get(T::DEFAULT_NAME)
                .and_then(|chain| chain.first())
                .ok_or_else(|| ZoneError::MissingDefault {
                    ty: T::TYPE,
                    default: T::DEFAULT_NAME,
                    name: plain.to_owned(),
                })?;
            let out = Arc::new(default.1.default_entry(Arc::from(plain)));
            T::table(self).insert(key, vec![(None, out.clone())]);
            return Ok(out);
        }
        let chain = T::table(self).entry(key).or_default();
        let at = chain.partition_point(|(t, _)| *t <= Some(tag));
        chain.insert(at, (Some(tag), v.clone()));
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn material(name: &str) -> Material {
        Material {
            name: Some(name.into()),
            game_flags: 0,
            sort_key: 0,
            atlas_rows: 0,
            atlas_columns: 0,
            draw_surf: 0,
            surface_type_bits: 0,
            hash_index: 0,
            state_bits_entry: [0; 34],
            state_flags: 0,
            camera_region: 0,
            technique_set: None,
            textures: Arc::from([]),
            constants: Arc::from([]),
            state_bits: Arc::from([]),
        }
    }

    #[test]
    fn a_stub_links_to_the_earlier_definition_and_a_later_zone_overrides() {
        let mut db = Db::default();
        let common = db.link(4, Arc::new(material("gfx_smk"))).unwrap();
        let stub = db.link(8, Arc::new(material(",GFX_SMK"))).unwrap();
        assert!(Arc::ptr_eq(&common, &stub), "the stub is the definition");
        // A tag below the definition's does not replace it; an equal or higher one does.
        let low = db.link(2, Arc::new(material("gfx_smk"))).unwrap();
        assert!(Arc::ptr_eq(db.get::<Material>("gfx_smk").unwrap(), &common));
        assert!(!Arc::ptr_eq(db.get::<Material>("gfx_smk").unwrap(), &low));
        let modded = db.link(16, Arc::new(material("gfx_smk"))).unwrap();
        assert!(Arc::ptr_eq(
            db.get::<Material>(",gfx_smk").unwrap(),
            &modded
        ));
        // Unloading the mod puts the shadowed definition back in use (`DB_UnloadXZone`); unloading that too leaves
        // the lower-ranked one, then nothing.
        db.unload(16);
        assert!(Arc::ptr_eq(db.get::<Material>("gfx_smk").unwrap(), &common));
        db.unload(4);
        assert!(Arc::ptr_eq(db.get::<Material>("gfx_smk").unwrap(), &low));
        db.unload(2);
        assert!(db.get::<Material>("gfx_smk").is_none());
    }

    #[test]
    fn an_unresolved_material_uses_the_stock_default_silently_under_its_own_name() {
        let mut db = Db::default();
        let mut original = material("$default");
        original.technique_set = Some(Arc::new(TechniqueSet {
            name: Some("default".into()),
            world_vert_format: 0,
            has_been_uploaded: false,
            techniques: Vec::new(),
        }));
        original.sort_key = 7;
        let default = db.link(2, Arc::new(original)).unwrap();
        // DB_FindXAssetDefaultHeaderInternal walks to the lowest-ranked default even when a mod overrides it.
        let mut modded = material("$default");
        modded.sort_key = 42;
        db.link(16, Arc::new(modded)).unwrap();
        let (out, events) =
            logging::capture(|| db.link(8, Arc::new(material(",mc/lambert1"))).unwrap());
        assert_eq!(out.name.as_deref(), Some("mc/lambert1"));
        assert_eq!(out.sort_key, 7);
        assert!(Arc::ptr_eq(
            out.technique_set.as_ref().unwrap(),
            default.technique_set.as_ref().unwrap()
        ));
        assert!(
            events.is_empty(),
            "stock creates this default entry without printing"
        );
        let again = db.link(8, Arc::new(material(",MC/LAMBERT1"))).unwrap();
        assert!(Arc::ptr_eq(&out, &again), "the default entry is registered");
        db.unload(8);
        assert!(Arc::ptr_eq(
            db.get::<Material>("mc/lambert1").unwrap(),
            &out
        ));
        let real = db.link(8, Arc::new(material("mc/lambert1"))).unwrap();
        assert!(Arc::ptr_eq(
            db.get::<Material>("mc/lambert1").unwrap(),
            &real
        ));
    }

    #[test]
    fn a_missing_type_default_is_a_load_error() {
        let mut db = Db::default();
        assert!(matches!(
            db.link(8, Arc::new(material(",mc/lambert1"))),
            Err(ZoneError::MissingDefault {
                ty: XAssetType::Material,
                default: "$default",
                ..
            })
        ));
    }
}
