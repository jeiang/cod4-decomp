// SPDX-License-Identifier: GPL-3.0-only
//! Cross-zone references. A zone stores an asset another zone defines as a stub: the name behind a comma
//! (`,gfx_smk_white_atlas`) and nothing else. The original's loader passes every asset it reads, nested or top-level,
//! through `DB_AddXAsset` → `DB_LinkXAssetEntry` (KisakCOD `database/db_registry.cpp`): a stub is looked up by type and
//! name, case-insensitively, among the assets loaded so far and the existing one is used in its place; a definition
//! joins that table, replacing an earlier zone's when its zone flags rank at least as high (`DB_OverrideAsset`, new
//! wins ties). Unloading a zone takes its definitions out.
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

/// Lowercase name to (zone tag, asset).
type Table<T> = HashMap<String, (u8, Arc<T>)>;

/// An asset type that zones reference by name across each other.
pub trait Linkable: Any + Send + Sync + Sized {
    const TYPE: XAssetType;
    fn name(&self) -> Option<&str>;
    fn table(db: &mut Db) -> &mut Table<Self>;
}

macro_rules! linkable {
    ($($ty:ty => $variant:ident, $field:ident, $name:ident;)*) => {
        /// The assets of the zones decoded so far, by type and name (`db_hashTable`).
        #[derive(Default)]
        pub struct Db {
            $($field: Table<$ty>,)*
        }

        $(impl Linkable for $ty {
            const TYPE: XAssetType = XAssetType::$variant;
            fn name(&self) -> Option<&str> {
                self.$name.as_deref()
            }
            fn table(db: &mut Db) -> &mut Table<Self> {
                &mut db.$field
            }
        })*

        impl Db {
            /// Drops the definitions of the zone tagged `tag` (`DB_UnloadXZone`).
            pub fn unload(&mut self, tag: u8) {
                $(self.$field.retain(|_, (t, _)| *t != tag);)*
            }
        }
    };
}

linkable! {
    Material => Material, materials, name;
    TechniqueSet => TechniqueSet, techsets, name;
    GfxImage => Image, images, name;
    XModel => XModel, models, name;
    FxEffectDef => Fx, effects, name;
    PhysPreset => PhysPreset, phys_presets, name;
    SoundAliasList => Sound, sounds, name;
    SndCurve => SoundCurve, curves, name;
    LoadedSound => LoadedSound, loaded_sounds, name;
}

/// Whether `name` is a stub: a reference to another zone's asset of the name behind the comma.
pub fn is_stub(name: &str) -> bool {
    name.starts_with(',')
}

impl Db {
    /// The definition of `name` (a leading comma ignored), when a zone decoded so far has one.
    pub fn get<T: Linkable>(&mut self, name: &str) -> Option<&Arc<T>> {
        T::table(self)
            .get(&name.trim_start_matches(',').to_ascii_lowercase())
            .map(|(_, v)| v)
    }

    /// `DB_LinkXAssetEntry` for one decoded asset of the zone tagged `tag`: a stub becomes the definition an earlier
    /// zone gave (or stays the stub, when none did); a definition is filed, over an earlier zone's of a lower or equal
    /// tag.
    pub(super) fn link<T: Linkable>(&mut self, tag: u8, v: Arc<T>) -> Arc<T> {
        let Some(name) = v.name() else { return v };
        let key = name.trim_start_matches(',').to_ascii_lowercase();
        if is_stub(name) {
            return match T::table(self).get(&key) {
                Some((_, found)) => found.clone(),
                None => {
                    tracing::debug!(
                        ty = T::TYPE.name(),
                        name = &name[1..],
                        "asset reference names what no zone loaded so far defines"
                    );
                    v
                }
            };
        }
        let table = T::table(self);
        if table.get(&key).is_none_or(|(t, _)| *t <= tag) {
            table.insert(key, (tag, v.clone()));
        }
        v
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
        let common = db.link(4, Arc::new(material("gfx_smk")));
        let stub = db.link(8, Arc::new(material(",GFX_SMK")));
        assert!(Arc::ptr_eq(&common, &stub), "the stub is the definition");
        // A tag below the definition's does not replace it; an equal or higher one does.
        let low = db.link(2, Arc::new(material("gfx_smk")));
        assert!(Arc::ptr_eq(db.get::<Material>("gfx_smk").unwrap(), &common));
        assert!(!Arc::ptr_eq(db.get::<Material>("gfx_smk").unwrap(), &low));
        let modded = db.link(16, Arc::new(material("gfx_smk")));
        assert!(Arc::ptr_eq(
            db.get::<Material>(",gfx_smk").unwrap(),
            &modded
        ));
        db.unload(16);
        assert!(db.get::<Material>("gfx_smk").is_none());
    }

    #[test]
    fn an_unresolved_stub_stays_a_stub() {
        let mut db = Db::default();
        let stub = Arc::new(material(",gfx_mist"));
        let out = db.link(8, stub.clone());
        assert!(Arc::ptr_eq(&stub, &out));
        assert!(
            db.get::<Material>("gfx_mist").is_none(),
            "a stub is not filed"
        );
    }
}
