// SPDX-License-Identifier: GPL-3.0-or-later
//! `PlayerWeapons`: what a player owns and how much ammunition each counter holds.
//!
//! The original keeps this in `playerState_t` (`weapons`, `weaponold`, `weaponrechamber`,
//! `ammo[]`, `ammoclip[]`) and the script builtins and `g_weapon.cpp` / `g_items.cpp` helpers
//! act on it: `BG_TakePlayerWeapon`, `G_GivePlayerWeapon`, `G_InitializeAmmo`, `Add_Ammo`,
//! `Fill_Clip` and the ammunition limit queries are ported here with their operation order.
//! Everything is plain fixed-size data: a weapon and its alternate fire mode are two entries
//! of the table, owned together, sharing the ammunition counter their definitions name.
//!
//! Counters are indexed by [`WeaponInfo::ammo_index`] (stock, shared by every weapon that names
//! the same ammunition) and [`WeaponInfo::clip_index`] (rounds in the magazine); index 0 is the
//! "none" counter and is never written by the script helpers.

use super::info::{InventoryType, OffhandClass, WeaponClass};
use super::table::{MAX_AMMO, MAX_CLIPS, MAX_WEAPONS, WeaponTable};
use crate::pm::PlayerState;
use crate::pm::weapon_state;

/// A bit set over weapon indices `0..=255`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct WeaponSet([u64; 4]);

impl WeaponSet {
    pub(crate) fn get(&self, i: u16) -> bool {
        let i = usize::from(i);
        i <= MAX_WEAPONS && self.0[i >> 6] >> (i & 63) & 1 != 0
    }

    pub(crate) fn set(&mut self, i: u16) {
        let i = usize::from(i);
        if i <= MAX_WEAPONS {
            self.0[i >> 6] |= 1 << (i & 63);
        }
    }

    pub(crate) fn clear(&mut self, i: u16) {
        let i = usize::from(i);
        if i <= MAX_WEAPONS {
            self.0[i >> 6] &= !(1 << (i & 63));
        }
    }
}

/// The per-player weapon inventory; see the module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerWeapons {
    owned: WeaponSet,
    /// `weaponold`: weapons that have been raised before (a first raise is slower).
    old: WeaponSet,
    /// `weaponrechamber`: a bolt-action round still has to be worked.
    rechamber: WeaponSet,
    ammo: [i32; MAX_AMMO],
    clip: [i32; MAX_CLIPS],
    /// `weaponmodels`: the model variant (attachments, camouflage) each owned weapon uses.
    models: [u8; MAX_WEAPONS + 1],
    /// A weapon the scripts asked the player to switch to (`switchtoweapon`); it replaces
    /// `cmd.weapon` until the player holds it or no longer owns it. 0 = none.
    selected: u16,
}

impl Default for PlayerWeapons {
    fn default() -> Self {
        Self {
            owned: WeaponSet::default(),
            old: WeaponSet::default(),
            rechamber: WeaponSet::default(),
            ammo: [0; MAX_AMMO],
            clip: [0; MAX_CLIPS],
            models: [0; MAX_WEAPONS + 1],
            selected: 0,
        }
    }
}

impl PlayerWeapons {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- wire form ----------------------------------------------------------------------------

    /// Words in the flattened form of [`to_words`](Self::to_words).
    pub const WORDS: usize = 3 * 8 + MAX_AMMO + MAX_CLIPS + (MAX_WEAPONS + 1).div_ceil(4) + 1;

    /// The whole inventory as fixed-position 32-bit words (for snapshots: a delta against the
    /// previous words is a few changed entries).
    pub fn to_words(&self) -> [i32; Self::WORDS] {
        let mut w = [0i32; Self::WORDS];
        let mut i = 0;
        for set in [&self.owned, &self.old, &self.rechamber] {
            for q in set.0 {
                w[i] = q as u32 as i32;
                w[i + 1] = (q >> 32) as u32 as i32;
                i += 2;
            }
        }
        for &a in self.ammo.iter().chain(self.clip.iter()) {
            w[i] = a;
            i += 1;
        }
        for c in self.models.chunks(4) {
            w[i] = i32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            i += 1;
        }
        w[i] = i32::from(self.selected);
        w
    }

    /// The inverse of [`to_words`](Self::to_words).
    pub fn from_words(w: &[i32; Self::WORDS]) -> Self {
        let mut s = Self::default();
        let mut i = 0;
        for set in [&mut s.owned, &mut s.old, &mut s.rechamber] {
            for q in &mut set.0 {
                *q = u64::from(w[i] as u32) | u64::from(w[i + 1] as u32) << 32;
                i += 2;
            }
        }
        for a in s.ammo.iter_mut().chain(s.clip.iter_mut()) {
            *a = w[i];
            i += 1;
        }
        for c in s.models.chunks_mut(4) {
            c.copy_from_slice(&w[i].to_le_bytes());
            i += 1;
        }
        s.selected = w[i] as u16;
        s
    }

    // ---- ownership -------------------------------------------------------------------------

    /// `Com_BitCheck(ps->weapons, index)` / `hasweapon`.
    pub fn has(&self, index: u16) -> bool {
        index != 0 && self.owned.get(index)
    }

    /// The model variant of an owned weapon.
    pub fn model(&self, index: u16) -> u8 {
        self.models.get(usize::from(index)).copied().unwrap_or(0)
    }

    /// Every owned weapon in index order (`getweaponslist`).
    pub fn list<'a>(&'a self, table: &'a WeaponTable) -> impl Iterator<Item = u16> + 'a {
        (1..=table.len() as u16).filter(|&i| self.owned.get(i))
    }

    /// Every owned primary weapon in index order (`getweaponslistprimaries`).
    pub fn list_primaries<'a>(&'a self, table: &'a WeaponTable) -> impl Iterator<Item = u16> + 'a {
        self.list(table)
            .filter(|&i| table.info(i).inventory_type == InventoryType::Primary)
    }

    /// `BG_PlayerWeaponCountPrimaryTypes`.
    pub fn primary_count(&self, table: &WeaponTable) -> usize {
        self.list_primaries(table).count()
    }

    /// `BG_PlayerWeaponsFull_Primaries`.
    pub fn primaries_full(&self, table: &WeaponTable) -> bool {
        self.primary_count(table) >= 2
    }

    /// `BG_PlayerHasCompatibleWeapon`: some owned weapon uses the same ammunition.
    pub fn has_compatible(&self, table: &WeaponTable, index: u16) -> bool {
        let ammo = table.info(index).ammo_index;
        self.list(table).any(|i| table.info(i).ammo_index == ammo)
    }

    // ---- script switch request ---------------------------------------------------------------

    /// `switchtoweapon`: asks the player to raise `index`. False when it is not owned.
    pub fn select(&mut self, index: u16) -> bool {
        if self.has(index) {
            self.selected = index;
            true
        } else {
            false
        }
    }

    /// The weapon the scripts asked for, if still pending.
    pub fn selected(&self) -> u16 {
        self.selected
    }

    pub(crate) fn clear_selected(&mut self) {
        self.selected = 0;
    }

    // ---- weapon state bits the state machine keeps -----------------------------------------

    pub(crate) fn was_raised(&self, index: u16) -> bool {
        self.old.get(index)
    }

    pub(crate) fn mark_raised(&mut self, index: u16) {
        self.old.set(index);
    }

    pub(crate) fn needs_rechamber(&self, index: u16) -> bool {
        self.rechamber.get(index)
    }

    pub(crate) fn set_rechamber(&mut self, index: u16, on: bool) {
        if on {
            self.rechamber.set(index);
        } else {
            self.rechamber.clear(index);
        }
    }

    // ---- counters ------------------------------------------------------------------------

    /// Rounds in the magazine (`ammoclip[clip index]`).
    pub fn clip(&self, table: &WeaponTable, index: u16) -> i32 {
        self.clip[usize::from(table.info(index).clip_index)]
    }

    /// Stock rounds (`ammo[ammo index]`).
    pub fn stock(&self, table: &WeaponTable, index: u16) -> i32 {
        self.ammo[usize::from(table.info(index).ammo_index)]
    }

    /// The magazine counter of a weapon, for the state machine.
    pub(crate) fn clip_mut(&mut self, table: &WeaponTable, index: u16) -> &mut i32 {
        &mut self.clip[usize::from(table.info(index).clip_index)]
    }

    /// The stock counter of a weapon, for the state machine.
    pub(crate) fn stock_mut(&mut self, table: &WeaponTable, index: u16) -> &mut i32 {
        &mut self.ammo[usize::from(table.info(index).ammo_index)]
    }

    /// `getweaponammoclip`.
    pub fn get_clip(&self, table: &WeaponTable, index: u16) -> i32 {
        if index == 0 {
            0
        } else {
            self.clip(table, index)
        }
    }

    /// `getweaponammostock`: a clip-only weapon reports its magazine.
    pub fn get_stock(&self, table: &WeaponTable, index: u16) -> i32 {
        if index == 0 {
            0
        } else if table.info(index).clip_only {
            self.clip(table, index)
        } else {
            self.stock(table, index)
        }
    }

    /// `setweaponammoclip`: clamped to `0..=clip size`; the "none" clip is not writable.
    pub fn set_clip(&mut self, table: &WeaponTable, index: u16, count: i32) {
        let w = table.info(index);
        if index != 0 && w.clip_index != 0 {
            self.clip[usize::from(w.clip_index)] = count.clamp(0, w.clip_size.max(0));
        }
    }

    /// `setweaponammostock`: clip-only weapons set their magazine, others the stock up to the
    /// player maximum.
    pub fn set_stock(&mut self, table: &WeaponTable, index: u16, count: i32) {
        if index == 0 {
            return;
        }
        let w = table.info(index);
        if w.clip_only {
            if w.clip_index != 0 {
                self.clip[usize::from(w.clip_index)] = count.min(w.clip_size).max(0);
            }
        } else if w.ammo_index != 0 {
            let max = self.ammo_player_max(table, index, 0);
            self.ammo[usize::from(w.ammo_index)] = count.min(max).max(0);
        }
    }

    /// `BG_WeaponAmmo`: magazine plus stock.
    pub fn weapon_ammo(&self, table: &WeaponTable, index: u16) -> i32 {
        self.clip(table, index) + self.stock(table, index)
    }

    /// `anyAmmoForWeaponModes`: the weapon or its alternate has any ammunition.
    pub fn any_ammo_for_weapon_modes(&self, table: &WeaponTable, index: u16) -> bool {
        let mut total = self.weapon_ammo(table, index);
        let alt = table.info(index).alt_weapon;
        if alt != 0 {
            total += self.weapon_ammo(table, alt);
        }
        total != 0
    }

    /// `getfractionstartammo`.
    pub fn fraction_start_ammo(&self, table: &WeaponTable, index: u16) -> f32 {
        let w = table.info(index);
        if self.has(index) && w.start_ammo >= 1 {
            let stock = self.stock(table, index);
            if stock >= 1 {
                return (f64::from(stock) / f64::from(w.start_ammo)) as f32;
            }
            return 0.0;
        }
        1.0
    }

    /// `getfractionmaxammo`.
    pub fn fraction_max_ammo(&self, table: &WeaponTable, index: u16) -> f32 {
        let w = table.info(index);
        if self.has(index) && w.max_ammo >= 1 {
            let stock = self.stock(table, index);
            if stock >= 1 {
                return (f64::from(stock) / f64::from(w.max_ammo)) as f32;
            }
            return 0.0;
        }
        1.0
    }

    /// `BG_GetAmmoPlayerMax`: the most stock the player can hold for this weapon's ammunition,
    /// not counting `skip` (0 = count everything).
    pub fn ammo_player_max(&self, table: &WeaponTable, index: u16, skip: u16) -> i32 {
        let w = table.info(index);
        if let Some(cap) = w.shared_ammo_cap_index {
            return table.shared_ammo_cap_size(cap);
        }
        if w.clip_only {
            return w.clip_size;
        }
        let mut total = 0;
        for other in self.list(table) {
            if other == skip {
                continue;
            }
            let o = table.info(other);
            if o.ammo_index == w.ammo_index {
                if let Some(cap) = o.shared_ammo_cap_index {
                    return table.shared_ammo_cap_size(cap);
                }
                total += o.max_ammo;
            }
        }
        total
    }

    /// `BG_GetMaxPickupableAmmo`: how much more ammunition the player can take; negative when
    /// over the limit.
    pub fn max_pickupable(&self, table: &WeaponTable, index: u16) -> i32 {
        let w = table.info(index);
        if let Some(cap) = w.shared_ammo_cap_index {
            let mut ammo = table.shared_ammo_cap_size(cap);
            let (mut ammo_seen, mut clip_seen) = ([false; MAX_AMMO], [false; MAX_CLIPS]);
            for cur in self.list(table) {
                let c = table.info(cur);
                if c.shared_ammo_cap_index != Some(cap) {
                    continue;
                }
                if c.clip_only {
                    let i = usize::from(c.clip_index);
                    if !clip_seen[i] {
                        clip_seen[i] = true;
                        ammo -= self.clip[i];
                    }
                } else {
                    let i = usize::from(c.ammo_index);
                    if !ammo_seen[i] {
                        ammo_seen[i] = true;
                        ammo -= self.ammo[i];
                    }
                }
            }
            ammo
        } else if w.clip_only {
            w.clip_size - self.clip(table, index)
        } else {
            self.ammo_player_max(table, index, 0) - self.stock(table, index)
        }
    }

    /// `BG_GetTotalAmmoReserve`.
    pub fn total_ammo_reserve(&self, table: &WeaponTable, index: u16) -> i32 {
        let w = table.info(index);
        let Some(cap) = w.shared_ammo_cap_index else {
            return if w.clip_only {
                self.clip(table, index)
            } else {
                self.stock(table, index)
            };
        };
        let mut total = 0;
        let (mut ammo_seen, mut clip_seen) = ([false; MAX_AMMO], [false; MAX_CLIPS]);
        for cur in self.list(table) {
            let c = table.info(cur);
            if c.shared_ammo_cap_index != Some(cap) {
                continue;
            }
            if c.clip_only {
                let i = usize::from(c.clip_index);
                if !clip_seen[i] {
                    clip_seen[i] = true;
                    total += self.clip[i];
                }
            } else {
                let i = usize::from(c.ammo_index);
                if !ammo_seen[i] {
                    ammo_seen[i] = true;
                    total += self.ammo[i];
                }
            }
        }
        total
    }

    /// `Fill_Clip`: moves stock into the magazine.
    pub fn fill_clip(&mut self, table: &WeaponTable, index: u16) {
        if index == 0 || usize::from(index) > table.len() {
            return;
        }
        let w = table.info(index);
        let (a, c) = (usize::from(w.ammo_index), usize::from(w.clip_index));
        let moved = (w.clip_size - self.clip[c]).min(self.ammo[a]);
        if moved != 0 {
            self.ammo[a] -= moved;
            self.clip[c] += moved;
        }
    }

    /// `Add_Ammo`: adds stock for a weapon the player has (or a compatible one), refilling the
    /// magazine when asked, and clamps to the limits. Returns the rounds actually gained.
    pub fn add_ammo(
        &mut self,
        table: &WeaponTable,
        ps: &mut PlayerState,
        index: u16,
        model: u8,
        count: i32,
        fill_clip: bool,
    ) -> i32 {
        if !self.has(index) && !self.has_compatible(table, index) {
            return 0;
        }
        let w = table.info(index);
        let (a, c) = (usize::from(w.ammo_index), usize::from(w.clip_index));
        let (old_ammo, old_clip) = (self.ammo[a], self.clip[c]);
        let max_ammo = self.ammo_player_max(table, index, 0);
        self.ammo[a] += count;
        let clip_only = w.clip_only;
        if clip_only {
            self.give_raw(table, ps, index, model);
        }
        if fill_clip || clip_only {
            self.fill_clip(table, index);
        }
        if clip_only {
            self.ammo[a] = 0;
        } else if self.ammo[a] > max_ammo {
            self.ammo[a] = max_ammo;
        }
        if self.clip[c] > w.clip_size {
            self.clip[c] = w.clip_size;
        }
        if w.shared_ammo_cap_index.is_some() {
            let over = self.max_pickupable(table, index);
            if over < 0 {
                if clip_only {
                    self.clip[c] += over;
                    if self.clip[c] <= 0 {
                        self.take(table, ps, index, true);
                        return 0;
                    }
                } else {
                    self.ammo[a] += over;
                    if self.ammo[a] < 0 {
                        self.ammo[a] = 0;
                    }
                }
            }
        }
        self.clip[c] - old_clip + self.ammo[a] - old_ammo
    }

    // ---- give and take -------------------------------------------------------------------

    /// `G_GivePlayerWeapon`: marks the weapon (and its alternate fire modes) as owned without
    /// giving ammunition. False when it is already owned, cannot be carried (turret and
    /// non-player classes), or `model` has no gun model.
    ///
    /// An off-hand weapon becomes the equipped off-hand when none is equipped or the equipped
    /// one is out of ammunition, in which case `ps.offhand_index` changes.
    pub fn give_raw(
        &mut self,
        table: &WeaponTable,
        ps: &mut PlayerState,
        index: u16,
        model: u8,
    ) -> bool {
        if index == 0 || self.has(index) {
            return false;
        }
        let Some(w) = table.get(index) else {
            return false;
        };
        if matches!(w.weap_class, WeaponClass::Turret | WeaponClass::NonPlayer)
            || !w.has_gun_model(model)
        {
            return false;
        }
        self.owned.set(index);
        self.rechamber.clear(index);
        self.old.clear(index);
        self.models[usize::from(index)] = model;
        if w.weap_class == WeaponClass::Item {
            return true;
        }
        if w.offhand_class != OffhandClass::None {
            if ps.offhand_index != 0 {
                if self.weapon_ammo(table, ps.offhand_index) <= 0 {
                    let class = table.info(ps.offhand_index).offhand_class;
                    let next = self.first_available_offhand(table, ps, class);
                    ps.offhand_index = if next != 0 { next } else { index };
                }
            } else {
                ps.offhand_index = index;
            }
        } else {
            let mut cur = w.alt_weapon;
            while cur != 0 && !self.owned.get(cur) {
                self.owned.set(cur);
                self.rechamber.clear(index);
                self.models[usize::from(cur)] = model;
                cur = table.info(cur).alt_weapon;
            }
        }
        true
    }

    /// `giveweapon`: [`give_raw`](Self::give_raw), then the starting ammunition
    /// ([`initialize_ammo`](Self::initialize_ammo)).
    pub fn give(
        &mut self,
        table: &WeaponTable,
        ps: &mut PlayerState,
        index: u16,
        model: u8,
    ) -> bool {
        let had = self.has(index);
        if !self.give_raw(table, ps, index, model) {
            return false;
        }
        self.initialize_ammo(table, ps, index, model, had);
        true
    }

    /// `G_GetNeededStartAmmo`: the starting stock still missing for this weapon given what other
    /// owned weapons of the same ammunition already account for.
    pub fn needed_start_ammo(&self, table: &WeaponTable, index: u16) -> i32 {
        let w = table.info(index);
        let mut owned_ammo = self.ammo[usize::from(w.ammo_index)];
        for other in self.list(table) {
            let o = table.info(other);
            if o.ammo_index == w.ammo_index && other != index {
                owned_ammo -= o.start_ammo - self.clip[usize::from(o.clip_index)];
            }
        }
        w.start_ammo - owned_ammo.max(0)
    }

    /// `G_InitializeAmmo` (`givestartammo`): starting ammunition for the weapon and its
    /// alternate fire modes.
    pub fn initialize_ammo(
        &mut self,
        table: &WeaponTable,
        ps: &mut PlayerState,
        index: u16,
        model: u8,
        had_weapon: bool,
    ) {
        let start = index;
        let mut cur = index;
        let mut budget = table.len() as i32 + 1;
        loop {
            let give = self.needed_start_ammo(table, cur);
            if give <= 0 {
                if !had_weapon {
                    self.fill_clip(table, cur);
                }
            } else {
                self.add_ammo(table, ps, cur, model, give, !had_weapon);
            }
            cur = table.info(cur).alt_weapon;
            budget -= 1;
            if cur == 0 || cur == start || budget < 0 || !self.has(cur) {
                break;
            }
        }
    }

    /// `givestartammo`: only for an owned weapon.
    pub fn give_start_ammo(&mut self, table: &WeaponTable, ps: &mut PlayerState, index: u16) {
        if self.has(index) {
            let model = self.model(index);
            self.initialize_ammo(table, ps, index, model, false);
        }
    }

    /// `givemaxammo`: tops the stock up to the player maximum.
    pub fn give_max_ammo(&mut self, table: &WeaponTable, ps: &mut PlayerState, index: u16) {
        if self.has(index) {
            let max = self.ammo_player_max(table, index, 0);
            let give = max - self.stock(table, index);
            if give > 0 {
                let model = self.model(index);
                self.add_ammo(table, ps, index, model, give, false);
            }
        }
    }

    /// `BG_TakePlayerWeapon`: removes the weapon and its owned alternate fire modes. With
    /// `take_ammo` the stock drops to what the remaining weapons can hold and the magazine is
    /// emptied. Clears `ps.weapon` when it was the current weapon.
    pub fn take(
        &mut self,
        table: &WeaponTable,
        ps: &mut PlayerState,
        index: u16,
        take_ammo: bool,
    ) -> bool {
        if !self.has(index) {
            return false;
        }
        let w = table.info(index);
        self.owned.clear(index);
        if take_ammo {
            let max_after = self.ammo_player_max(table, index, index);
            let keep = if max_after == 0 {
                0
            } else {
                self.stock(table, index).min(max_after)
            };
            self.ammo[usize::from(w.ammo_index)] = keep;
            self.clip[usize::from(w.clip_index)] = 0;
        }
        let mut cur = w.alt_weapon;
        while cur != 0 && self.owned.get(cur) {
            if take_ammo {
                let a = table.info(cur);
                self.ammo[usize::from(a.ammo_index)] = 0;
                self.clip[usize::from(a.clip_index)] = 0;
            }
            self.owned.clear(cur);
            cur = table.info(cur).alt_weapon;
        }
        if u32::from(index) == ps.weapon {
            ps.weapon = 0;
        }
        true
    }

    /// `takeallweapons`.
    pub fn take_all(&mut self, table: &WeaponTable, ps: &mut PlayerState) {
        ps.weapon = 0;
        for i in 1..=table.len() as u16 {
            self.take(table, ps, i, true);
        }
    }

    /// `BG_TakeClipOnlyWeaponIfEmpty`.
    pub(crate) fn take_clip_only_if_empty(
        &mut self,
        table: &WeaponTable,
        ps: &mut PlayerState,
        index: u16,
    ) {
        if index != 0 {
            let w = table.info(index);
            if self.has(index)
                && w.clip_only
                && self.clip(table, index) == 0
                && self.stock(table, index) == 0
                && !w.has_detonator
            {
                self.take(table, ps, index, false);
            }
        }
    }

    // ---- off-hand ------------------------------------------------------------------------

    /// `BG_GetFirstAvailableOffhand`: the first owned weapon of the class with ammunition (or
    /// while a grenade is being thrown back).
    pub fn first_available_offhand(
        &self,
        table: &WeaponTable,
        ps: &PlayerState,
        class: OffhandClass,
    ) -> u16 {
        self.list(table)
            .find(|&i| {
                table.info(i).offhand_class == class
                    && (ps.throw_back_grenade_time_left > 0 || self.weapon_ammo(table, i) > 0)
            })
            .unwrap_or(0)
    }

    /// `BG_GetFirstEquippedOffhand`: the first owned weapon of the class, with or without
    /// ammunition.
    pub fn first_equipped_offhand(&self, table: &WeaponTable, class: OffhandClass) -> u16 {
        self.list(table)
            .find(|&i| table.info(i).offhand_class == class)
            .unwrap_or(0)
    }

    /// `setspawnweapon`: makes an owned weapon the current one, ready, with no raise.
    pub fn spawn_weapon(&self, ps: &mut PlayerState, index: u16) -> bool {
        if self.has(index) {
            ps.weapon = u32::from(index);
            ps.weapon_state = weapon_state::READY;
            true
        } else {
            false
        }
    }
}
