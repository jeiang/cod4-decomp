// SPDX-License-Identifier: GPL-3.0-or-later
//! `WeaponTable`: every weapon of the loaded zones, numbered the way scripts and usercmds refer
//! to them.
//!
//! Weapons are sorted by lower-case internal name and numbered from 1 (0 is "none"). The
//! original numbers weapons in registration order; this table is built once from whatever the
//! zones hold, so the order is the sorted one instead and `getweaponslist` is alphabetical.
//! Ammunition indices follow `BG_SetupAmmoIndexes` / `BG_SetupClipIndexes` /
//! `BG_SetupSharedAmmoIndexes`: weapons naming the same ammo (or clip) share one counter in the
//! player's inventory; an empty name is index 0, the "none" counter.

use super::info::WeaponInfo;
use assets::zone::weapon::WeaponDef;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// Most weapons a table holds: `usercmd_t.weapon` is one byte and 0 means none.
pub const MAX_WEAPONS: usize = 255;
/// Most distinct ammunition types (the original's `ammo[128]`).
pub const MAX_AMMO: usize = 128;
/// Most distinct clip types (the original's `ammoclip[128]`).
pub const MAX_CLIPS: usize = 128;

/// Why a table could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableError {
    TooManyWeapons(usize),
    TooManyAmmoTypes(usize),
    TooManyClipTypes(usize),
}

impl fmt::Display for TableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyWeapons(n) => write!(f, "{n} weapons (limit {MAX_WEAPONS})"),
            Self::TooManyAmmoTypes(n) => write!(f, "{n} ammunition types (limit {MAX_AMMO})"),
            Self::TooManyClipTypes(n) => write!(f, "{n} clip types (limit {MAX_CLIPS})"),
        }
    }
}

impl std::error::Error for TableError {}

#[derive(Debug)]
pub struct WeaponTable {
    none: WeaponInfo,
    /// `weapons[i]` has index `i + 1`.
    weapons: Vec<WeaponInfo>,
    /// Lower-case names parallel to `weapons`, the sort key.
    keys: Vec<Box<str>>,
    ammo_types: usize,
    clip_types: usize,
    shared_caps: Vec<i32>,
    /// `(weapon, alt weapon name)` pairs whose alternate is not in the table.
    unresolved_alts: Vec<(Box<str>, Box<str>)>,
}

/// Case-insensitive ASCII ordering without allocating.
fn cmp_ci(a: &str, b: &str) -> Ordering {
    a.bytes()
        .map(|c| c.to_ascii_lowercase())
        .cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

impl WeaponTable {
    /// Builds the table from decoded definitions. Definitions with the same name (a weapon
    /// present in both the boot zone and a map zone) count once, the first winning.
    pub fn new(defs: &[Arc<WeaponDef>]) -> Result<Self, TableError> {
        Self::from_infos(defs.iter().map(|d| WeaponInfo::from_def(d)).collect())
    }

    /// Builds the table from already-extracted weapons; see [`WeaponTable::new`].
    pub fn from_infos(mut infos: Vec<WeaponInfo>) -> Result<Self, TableError> {
        infos.retain(|w| !w.name.is_empty());
        infos.sort_by(|a, b| cmp_ci(&a.name, &b.name));
        infos.dedup_by(|b, a| cmp_ci(&a.name, &b.name) == Ordering::Equal);
        if infos.len() > MAX_WEAPONS {
            return Err(TableError::TooManyWeapons(infos.len()));
        }

        let keys: Vec<Box<str>> = infos
            .iter()
            .map(|w| w.name.to_ascii_lowercase().into_boxed_str())
            .collect();
        let mut ammo: HashMap<&str, u16> = HashMap::new();
        let mut clips: HashMap<&str, u16> = HashMap::new();
        let mut caps: HashMap<Box<str>, u16> = HashMap::new();
        let mut shared_caps = Vec::new();
        let mut assigned = Vec::with_capacity(infos.len());
        for w in &infos {
            let next = ammo.len() as u16 + 1;
            let a = if w.ammo_name.is_empty() {
                0
            } else {
                *ammo.entry(&w.ammo_name).or_insert(next)
            };
            let next = clips.len() as u16 + 1;
            let c = if w.clip_name.is_empty() {
                0
            } else {
                *clips.entry(&w.clip_name).or_insert(next)
            };
            let cap = if w.shared_ammo_cap_name.is_empty() {
                None
            } else {
                let key = w.shared_ammo_cap_name.to_ascii_lowercase().into_boxed_str();
                let next = caps.len() as u16;
                let i = *caps.entry(key).or_insert(next);
                if usize::from(i) == shared_caps.len() {
                    shared_caps.push(w.shared_ammo_cap);
                }
                Some(i)
            };
            assigned.push((a, c, cap));
        }
        let (ammo_types, clip_types) = (ammo.len() + 1, clips.len() + 1);
        if ammo_types > MAX_AMMO {
            return Err(TableError::TooManyAmmoTypes(ammo_types));
        }
        if clip_types > MAX_CLIPS {
            return Err(TableError::TooManyClipTypes(clip_types));
        }

        for (i, (w, (a, c, cap))) in infos.iter_mut().zip(assigned).enumerate() {
            w.index = i as u16 + 1;
            w.ammo_index = a;
            w.clip_index = c;
            w.shared_ammo_cap_index = cap;
        }
        let mut unresolved_alts = Vec::new();
        let alts: Vec<u16> = infos
            .iter()
            .map(|w| {
                if w.alt_weapon_name.is_empty() {
                    return 0;
                }
                match keys.binary_search_by(|k| cmp_ci(k, &w.alt_weapon_name)) {
                    Ok(i) => i as u16 + 1,
                    Err(_) => {
                        unresolved_alts.push((w.name.clone(), w.alt_weapon_name.clone()));
                        0
                    }
                }
            })
            .collect();
        for (w, alt) in infos.iter_mut().zip(alts) {
            w.alt_weapon = alt;
        }
        Ok(Self {
            none: WeaponInfo::default(),
            weapons: infos,
            keys,
            ammo_types,
            clip_types,
            shared_caps,
            unresolved_alts,
        })
    }

    /// Number of weapons (indices are `1..=len()`).
    pub fn len(&self) -> usize {
        self.weapons.len()
    }

    pub fn is_empty(&self) -> bool {
        self.weapons.is_empty()
    }

    /// The index of a weapon by internal name (`ak47_mp`), ignoring case; 0 when the name is
    /// empty, `none` or unknown (`BG_FindWeaponIndexForName`).
    pub fn index(&self, name: &str) -> u16 {
        if name.is_empty() || name.eq_ignore_ascii_case("none") {
            return 0;
        }
        self.keys
            .binary_search_by(|k| cmp_ci(k, name))
            .map_or(0, |i| i as u16 + 1)
    }

    /// The weapon with this index; `None` for 0 and out-of-range values.
    pub fn get(&self, index: u16) -> Option<&WeaponInfo> {
        usize::from(index)
            .checked_sub(1)
            .and_then(|i| self.weapons.get(i))
    }

    /// Like [`get`](Self::get), but index 0 and invalid indices give the empty "no weapon"
    /// definition, the way `BG_GetWeaponDef(0)` gives the default weapon.
    pub fn info(&self, index: u16) -> &WeaponInfo {
        self.get(index).unwrap_or(&self.none)
    }

    /// The weapon's internal name, or `none`.
    pub fn name(&self, index: u16) -> &str {
        self.get(index).map_or("none", |w| &w.name)
    }

    /// All weapons in index order.
    pub fn iter(&self) -> impl Iterator<Item = &WeaponInfo> {
        self.weapons.iter()
    }

    /// Number of ammunition counters a player needs (including the "none" counter 0).
    pub fn ammo_types(&self) -> usize {
        self.ammo_types
    }

    /// Number of clip counters a player needs (including the "none" counter 0).
    pub fn clip_types(&self) -> usize {
        self.clip_types
    }

    /// `BG_GetSharedAmmoCapSize`.
    pub fn shared_ammo_cap_size(&self, cap: u16) -> i32 {
        self.shared_caps.get(usize::from(cap)).copied().unwrap_or(0)
    }

    /// Weapons whose `altWeapon` names something that is not in the table.
    pub fn unresolved_alts(&self) -> &[(Box<str>, Box<str>)] {
        &self.unresolved_alts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weapon::fixtures::{grenade, rifle};
    use crate::weapon::info::InventoryType;

    #[test]
    fn indices_are_one_based_and_sorted_by_lowercase_name() {
        let t = WeaponTable::from_infos(vec![
            rifle("m4_mp", "ar"),
            rifle("AK47_mp", "ar"),
            rifle("beretta_mp", "pistol"),
        ])
        .unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(t.index("ak47_mp"), 1);
        assert_eq!(t.index("AK47_MP"), 1);
        assert_eq!(t.index("beretta_mp"), 2);
        assert_eq!(t.index("m4_mp"), 3);
        assert_eq!(t.index("none"), 0);
        assert_eq!(t.index(""), 0);
        assert_eq!(t.index("ak47"), 0);
        assert_eq!(t.get(1).unwrap().name.as_ref(), "AK47_mp");
        assert!(t.get(0).is_none());
        assert!(t.get(4).is_none());
        assert_eq!(t.name(0), "none");
        assert_eq!(t.info(99).clip_size, 0);
    }

    #[test]
    fn same_ammo_name_shares_one_index_and_clips_stay_separate() {
        let t = WeaponTable::from_infos(vec![
            rifle("ak47_mp", "ar"),
            rifle("m4_mp", "ar"),
            rifle("deserteagle_mp", "pistol"),
        ])
        .unwrap();
        let ak = t.get(t.index("ak47_mp")).unwrap();
        let m4 = t.get(t.index("m4_mp")).unwrap();
        let de = t.get(t.index("deserteagle_mp")).unwrap();
        assert_eq!(ak.ammo_index, m4.ammo_index);
        assert_ne!(ak.ammo_index, de.ammo_index);
        assert_ne!(ak.clip_index, m4.clip_index);
        assert!(ak.ammo_index > 0 && de.ammo_index > 0);
        assert_eq!(t.ammo_types(), 3);
        assert_eq!(t.clip_types(), 4);
    }

    #[test]
    fn same_clip_name_shares_a_clip_and_empty_names_are_index_zero() {
        let mut a = rifle("a_mp", "x");
        let mut b = rifle("b_mp", "y");
        a.clip_name = "mag".into();
        b.clip_name = "mag".into();
        let mut none = rifle("c_mp", "");
        none.clip_name = "".into();
        let t = WeaponTable::from_infos(vec![a, b, none]).unwrap();
        assert_eq!(t.info(1).clip_index, t.info(2).clip_index);
        assert_eq!(t.info(3).ammo_index, 0);
        assert_eq!(t.info(3).clip_index, 0);
    }

    #[test]
    fn alt_weapons_resolve_to_table_indices() {
        let mut gl = rifle("gl_mp", "gl");
        gl.inventory_type = InventoryType::AltMode;
        let mut m4 = rifle("m4_gl_mp", "ar");
        m4.alt_weapon_name = "GL_MP".into();
        let mut orphan = rifle("orphan_mp", "ar");
        orphan.alt_weapon_name = "missing_mp".into();
        let t = WeaponTable::from_infos(vec![m4, gl, orphan]).unwrap();
        assert_eq!(t.info(t.index("m4_gl_mp")).alt_weapon, t.index("gl_mp"));
        assert_eq!(t.info(t.index("orphan_mp")).alt_weapon, 0);
        assert_eq!(t.unresolved_alts().len(), 1);
    }

    #[test]
    fn duplicate_names_count_once() {
        let t =
            WeaponTable::from_infos(vec![rifle("ak47_mp", "ar"), rifle("AK47_MP", "ar")]).unwrap();
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn shared_ammo_caps_share_an_index_and_size() {
        let mut a = grenade("frag_mp");
        let mut b = grenade("smoke_mp");
        a.shared_ammo_cap_name = "grenades".into();
        a.shared_ammo_cap = 4;
        b.shared_ammo_cap_name = "Grenades".into();
        b.shared_ammo_cap = 4;
        let t = WeaponTable::from_infos(vec![a, b]).unwrap();
        let ca = t.info(1).shared_ammo_cap_index.unwrap();
        assert_eq!(Some(ca), t.info(2).shared_ammo_cap_index);
        assert_eq!(t.shared_ammo_cap_size(ca), 4);
    }

    #[test]
    fn limits_are_errors() {
        let many: Vec<_> = (0..=MAX_WEAPONS)
            .map(|i| rifle(&format!("w{i:03}_mp"), "ar"))
            .collect();
        assert_eq!(
            WeaponTable::from_infos(many).unwrap_err(),
            TableError::TooManyWeapons(MAX_WEAPONS + 1)
        );
        let ammo: Vec<_> = (0..MAX_AMMO)
            .map(|i| rifle(&format!("w{i:03}_mp"), &format!("a{i}")))
            .collect();
        assert_eq!(
            WeaponTable::from_infos(ammo).unwrap_err(),
            TableError::TooManyAmmoTypes(MAX_AMMO + 1)
        );
    }
}
