// SPDX-License-Identifier: GPL-3.0-only
// Translated from KisakCOD (https://github.com/Kisak-COD/KisakCOD, GPL-3.0): `bgame/bg_misc.cpp`,
// `bgame/bg_weapons.cpp`, `game/g_items.cpp`, `game_mp/player_use_mp.cpp`; the code is the
// original game's, copyright Activision Publishing and the KisakCOD contributors.
//! Dropped weapons and live grenades: which a player may pick up, what a pickup does to the
//! inventory, and what a dropped weapon holds.
//!
//! A dropped weapon carries the ammunition of up to two weapons ([`ItemAmmo`]): the weapon and
//! its alternate fire mode. A player standing over one takes ammunition from it for a weapon
//! already owned ([`leech_from_item`]); using it takes the weapon and its ammunition
//! ([`add_ammo_for_new_weapon`]). Everything here is pure rules over the inventory; the server
//! moves the entity and raises the events.

use super::info::{InventoryType, OffhandClass};
use super::inventory::PlayerWeapons;
use super::table::WeaponTable;
use crate::pm::{PlayerState, pmf, wf};

/// A dropped weapon holds this many weapons' ammunition (`item[2]`).
pub const ITEM_SLOTS: usize = 2;

/// The cursor hint of a weapon is the weapon index plus this (`WEAPON_HINT_OFFSET`, which is
/// `HINT_FRIENDLY`): the hint numbers below it are the fixed icons.
pub const WEAPON_HINT_OFFSET: u8 = 4;

/// `playerState_t.offhandSecondary` values.
pub const OFFHAND_SECONDARY_SMOKE: u8 = 0;
pub const OFFHAND_SECONDARY_FLASH: u8 = 1;

/// One weapon's share of a dropped weapon (`item_ent_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemAmmo {
    /// The weapon, 0 for an unused slot.
    pub weapon: u16,
    /// Rounds in the magazine; -1 when unspecified, so the pickup fills the magazine from stock.
    pub clip: i32,
    /// Rounds in reserve.
    pub stock: i32,
}

impl ItemAmmo {
    pub const NONE: Self = Self {
        weapon: 0,
        clip: -1,
        stock: 0,
    };
}

impl Default for ItemAmmo {
    fn default() -> Self {
        Self::NONE
    }
}

/// What a leech did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Leech {
    /// Some ammunition changed hands.
    pub took_any: bool,
    /// The weapons ammunition was taken for (for the pickup message).
    pub weapons: [u16; ITEM_SLOTS],
    /// The player held the exact weapon and took from it: the dropped weapon is used up.
    pub used_up: bool,
}

/// What stands in the world: a weapon dropped there or a live grenade in the air.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pickup {
    Dropped,
    LiveGrenade,
}

/// `BG_PlayerTouchesItem`: the player's origin within 36 units either way across and from 88
/// below to 18 above the item.
pub fn player_touches_item(player: [f32; 3], item: [f32; 3]) -> bool {
    let d = [
        player[0] - item[0],
        player[1] - item[1],
        player[2] - item[2],
    ];
    (-36.0..=36.0).contains(&d[0])
        && (-36.0..=36.0).contains(&d[1])
        && (-88.0..=18.0).contains(&d[2])
}

/// `BG_PlayerCanPickUpWeaponType`: flash and smoke grenades only go to a player who carries that
/// kind of special grenade.
pub fn can_pick_up_type(table: &WeaponTable, ps: &PlayerState, weapon: u16) -> bool {
    match table.info(weapon).offhand_class {
        OffhandClass::Flash => ps.offhand_secondary == OFFHAND_SECONDARY_FLASH,
        OffhandClass::Smoke => ps.offhand_secondary == OFFHAND_SECONDARY_SMOKE,
        _ => true,
    }
}

/// `HaveRoomForAmmo`: some weapon sharing this one's ammunition can still take rounds.
fn have_room_for_ammo(inv: &PlayerWeapons, table: &WeaponTable, weapon: u16) -> bool {
    let w = table.info(weapon);
    if w.ammo_name.is_empty() {
        return true;
    }
    table
        .iter()
        .any(|o| o.ammo_index == w.ammo_index && inv.max_pickupable(table, o.index) > 0)
}

/// `WeaponEntCanBeGrabbed`. `touched` is walking over it, not pressing use.
fn weapon_ent_can_be_grabbed(
    inv: &PlayerWeapons,
    table: &WeaponTable,
    ps: &PlayerState,
    kind: Pickup,
    touched: bool,
    weapon: u16,
) -> bool {
    if !can_pick_up_type(table, ps, weapon) {
        return false;
    }
    if kind == Pickup::LiveGrenade && table.info(weapon).offhand_class == OffhandClass::Frag {
        return true;
    }
    if touched {
        (inv.has(weapon) || inv.has_compatible(table, weapon))
            && have_room_for_ammo(inv, table, weapon)
    } else {
        !inv.has(weapon)
    }
}

/// `BG_CanItemBeGrabbed`: whether the player may take the weapon lying (or flying) in the world.
/// `dropper` is the player who dropped it a moment ago, who may not take it back yet.
pub fn can_item_be_grabbed(
    inv: &PlayerWeapons,
    table: &WeaponTable,
    ps: &PlayerState,
    kind: Pickup,
    weapon: u16,
    dropper: Option<u16>,
    touched: bool,
) -> bool {
    if ps.weapon_flags & wf::DISABLED != 0
        || dropper == Some(ps.client_num)
        || ps.pm_flags & pmf::VEHICLE_ATTACHED != 0
    {
        return false;
    }
    if weapon_ent_can_be_grabbed(inv, table, ps, kind, touched, weapon) {
        return true;
    }
    let alt = table.info(weapon).alt_weapon;
    alt != 0 && weapon_ent_can_be_grabbed(inv, table, ps, kind, touched, alt)
}

/// `Player_GetItemCursorHint`: the hint shown for a dropped weapon the player looks at, 0 for
/// none. A weapon already owned shows nothing; with both primary slots full and a non-primary in
/// hand, a primary shows nothing.
pub fn item_cursor_hint(inv: &PlayerWeapons, table: &WeaponTable, held: u16, weapon: u16) -> u8 {
    if inv.has(weapon) {
        return 0;
    }
    let held_type = table.info(held).inventory_type;
    let item_type = table.info(weapon).inventory_type;
    if held_type == InventoryType::Primary
        || held_type == InventoryType::AltMode
        || (item_type != InventoryType::Primary && item_type != InventoryType::AltMode)
        || inv.primary_count(table) < 2
    {
        weapon as u8 + WEAPON_HINT_OFFSET
    } else {
        0
    }
}

/// `CurrentPrimaryWeapon`: the primary weapon in hand (the base of an alternate fire mode), 0
/// when the hand holds something else.
pub fn current_primary(inv: &PlayerWeapons, table: &WeaponTable, held: u16) -> u16 {
    if held == 0 {
        return 0;
    }
    let mut w = held;
    if table.info(w).inventory_type == InventoryType::AltMode {
        w = table.info(w).alt_weapon;
    }
    if !inv.has(w) || table.info(w).inventory_type != InventoryType::Primary {
        return 0;
    }
    w
}

/// `GetNonClipAmmoToTransferToWeaponEntity`: the reserve a dropped weapon takes with it, which is
/// what the player's other weapons could not hold.
fn stock_to_transfer(inv: &PlayerWeapons, table: &WeaponTable, weapon: u16) -> i32 {
    (inv.stock(table, weapon) - inv.ammo_player_max(table, weapon, weapon)).max(0)
}

/// `PlayerHasAnyAmmoToTransferToWeapon`: dropping the weapon leaves something on the floor.
pub fn has_ammo_to_transfer(inv: &PlayerWeapons, table: &WeaponTable, weapon: u16) -> bool {
    inv.clip(table, weapon) > 0 || stock_to_transfer(inv, table, weapon) > 0
}

/// `TransferPlayerAmmoToWeaponEntity`: the ammunition a player's weapon (and its alternate fire
/// mode) leaves on the floor when dropped. Take the weapon from the player after.
pub fn transfer_from_player(
    inv: &PlayerWeapons,
    table: &WeaponTable,
    weapon: u16,
) -> [ItemAmmo; ITEM_SLOTS] {
    let mut out = [ItemAmmo::NONE; ITEM_SLOTS];
    let mut w = weapon;
    for slot in &mut out {
        if w == 0 {
            break;
        }
        *slot = ItemAmmo {
            weapon: w,
            clip: inv.clip(table, w),
            stock: stock_to_transfer(inv, table, w),
        };
        w = table.info(w).alt_weapon;
    }
    out
}

/// `TransferRandomAmmoToWeaponEntity`: what a weapon nobody owned holds, between its drop limits.
pub fn transfer_random(
    table: &WeaponTable,
    weapon: u16,
    mut rand: impl FnMut() -> i32,
) -> [ItemAmmo; ITEM_SLOTS] {
    let mut out = [ItemAmmo::NONE; ITEM_SLOTS];
    let mut w = weapon;
    for (i, slot) in out.iter_mut().enumerate() {
        if w == 0 {
            break;
        }
        let info = table.info(w);
        if info.shared_ammo_cap_index.is_some() && i > 0 {
            break;
        }
        let (lo, hi) = if info.drop_ammo_max < info.drop_ammo_min {
            (info.drop_ammo_max, info.drop_ammo_min)
        } else {
            (info.drop_ammo_min, info.drop_ammo_max)
        };
        let (mut clip, mut stock) = (0, 0);
        if hi >= 0 {
            let total = lo + rand().rem_euclid(hi - lo + 1);
            if total > 0 {
                clip = if info.clip_size == 1 {
                    1
                } else {
                    rand().rem_euclid(info.clip_size + 1)
                };
                if clip < total {
                    stock = total - clip;
                } else {
                    clip = total;
                }
            }
        }
        *slot = ItemAmmo {
            weapon: w,
            clip,
            stock,
        };
        w = info.alt_weapon;
    }
    out
}

/// `WeaponPickup_LeechFromWeaponEnt`: the player takes ammunition from the dropped weapon for the
/// weapons they own (or one sharing their ammunition). What is taken leaves the item;
/// `have_exact` also moves the magazine's rounds.
pub fn leech_from_item(
    inv: &mut PlayerWeapons,
    table: &WeaponTable,
    ps: &mut PlayerState,
    slots: &mut [ItemAmmo; ITEM_SLOTS],
    model_of: impl Fn(u16) -> u8,
    have_exact: bool,
) -> Leech {
    let mut out = Leech::default();
    for (i, slot) in slots.iter_mut().enumerate() {
        if slot.weapon == 0 {
            continue;
        }
        let mut available = slot.stock;
        if have_exact {
            available += slot.clip;
        }
        let taken = inv.add_ammo(
            table,
            ps,
            slot.weapon,
            model_of(slot.weapon),
            available,
            false,
        );
        if taken == 0 {
            continue;
        }
        out.took_any = true;
        out.weapons[i] = slot.weapon;
        slot.stock -= taken;
        if slot.stock < 0 {
            slot.clip = (slot.clip + slot.stock).max(0);
            slot.stock = 0;
        }
    }
    out.used_up = have_exact && out.took_any;
    out
}

/// `WeaponPickup_AddAmmoForNewWeapon`: a weapon just taken from the floor arrives with the
/// magazine and reserve it held; a magazine of -1 is filled from the reserve.
pub fn add_ammo_for_new_weapon(
    inv: &mut PlayerWeapons,
    table: &WeaponTable,
    ps: &mut PlayerState,
    slots: &[ItemAmmo; ITEM_SLOTS],
    model_of: impl Fn(u16) -> u8,
) {
    for slot in slots.iter().filter(|s| s.weapon != 0) {
        if slot.clip >= 0 {
            inv.set_clip(table, slot.weapon, slot.clip);
        }
        inv.add_ammo(
            table,
            ps,
            slot.weapon,
            model_of(slot.weapon),
            slot.stock,
            slot.clip == -1,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weapon::WeaponInfo;
    use crate::weapon::fixtures::{grenade, rifle};

    struct Setup {
        table: WeaponTable,
        inv: PlayerWeapons,
        ps: PlayerState,
    }

    fn setup(infos: Vec<WeaponInfo>) -> Setup {
        Setup {
            table: WeaponTable::from_infos(infos).expect("table"),
            inv: PlayerWeapons::new(),
            ps: PlayerState::default(),
        }
    }

    impl Setup {
        fn idx(&self, name: &str) -> u16 {
            self.table.index(name)
        }

        fn give(&mut self, name: &str) {
            let i = self.idx(name);
            assert!(self.inv.give(&self.table, &mut self.ps, i, 0));
        }

        fn grab(&self, name: &str, touched: bool) -> bool {
            can_item_be_grabbed(
                &self.inv,
                &self.table,
                &self.ps,
                Pickup::Dropped,
                self.idx(name),
                None,
                touched,
            )
        }
    }

    #[test]
    fn use_takes_only_a_weapon_the_player_lacks_and_walking_over_only_ammo_for_one_owned() {
        let mut s = setup(vec![rifle("ak_mp", "ak"), rifle("m4_mp", "m4")]);
        s.give("ak_mp");
        assert!(s.grab("m4_mp", false));
        assert!(!s.grab("m4_mp", true));
        let ak = s.idx("ak_mp");
        s.inv.set_stock(&s.table, ak, 10);
        assert!(s.grab("ak_mp", true));
        assert!(!s.grab("ak_mp", false));
    }

    #[test]
    fn walking_over_a_weapon_with_full_ammunition_takes_nothing() {
        let mut s = setup(vec![rifle("ak_mp", "ak")]);
        s.give("ak_mp");
        let ak = s.idx("ak_mp");
        let max = s.inv.ammo_player_max(&s.table, ak, 0);
        s.inv.set_stock(&s.table, ak, max);
        assert!(!s.grab("ak_mp", true), "no room for more rounds");
        s.inv.set_stock(&s.table, ak, max - 1);
        assert!(s.grab("ak_mp", true));
    }

    #[test]
    fn the_dropper_cannot_take_it_back_and_nobody_can_with_weapons_off() {
        let mut s = setup(vec![rifle("m4_mp", "m4")]);
        let m4 = s.idx("m4_mp");
        let may = |s: &Setup, dropper| {
            can_item_be_grabbed(&s.inv, &s.table, &s.ps, Pickup::Dropped, m4, dropper, false)
        };
        s.ps.client_num = 3;
        assert!(may(&s, None));
        assert!(!may(&s, Some(3)));
        assert!(may(&s, Some(4)));
        s.ps.weapon_flags |= wf::DISABLED;
        assert!(!may(&s, None));
    }

    #[test]
    fn a_live_frag_is_always_grabbable_and_smoke_and_flash_follow_the_special_grenade() {
        let mut smoke = grenade("smoke_mp");
        smoke.offhand_class = OffhandClass::Smoke;
        let mut flash = grenade("flash_mp");
        flash.offhand_class = OffhandClass::Flash;
        let mut s = setup(vec![grenade("frag_mp"), smoke, flash]);
        let (frag, smoke, flash) = (s.idx("frag_mp"), s.idx("smoke_mp"), s.idx("flash_mp"));
        s.give("frag_mp");
        assert!(can_item_be_grabbed(
            &s.inv,
            &s.table,
            &s.ps,
            Pickup::LiveGrenade,
            frag,
            None,
            false
        ));
        s.ps.offhand_secondary = OFFHAND_SECONDARY_SMOKE;
        assert!(can_pick_up_type(&s.table, &s.ps, smoke));
        assert!(!can_pick_up_type(&s.table, &s.ps, flash));
        s.ps.offhand_secondary = OFFHAND_SECONDARY_FLASH;
        assert!(!can_pick_up_type(&s.table, &s.ps, smoke));
        assert!(can_pick_up_type(&s.table, &s.ps, flash));
    }

    #[test]
    fn leeching_stops_at_the_ammunition_limit() {
        let mut s = setup(vec![rifle("ak_mp", "ak")]);
        s.give("ak_mp");
        let ak = s.idx("ak_mp");
        s.inv.set_stock(&s.table, ak, 100);
        let max = s.inv.ammo_player_max(&s.table, ak, 0);
        let mut slots = [ItemAmmo::NONE; 2];
        slots[0] = ItemAmmo {
            weapon: ak,
            clip: 20,
            stock: 60,
        };
        let got = leech_from_item(&mut s.inv, &s.table, &mut s.ps, &mut slots, |_| 0, true);
        assert!(got.took_any && got.used_up);
        assert_eq!(s.inv.stock(&s.table, ak), max);
        assert_eq!(slots[0].stock + slots[0].clip, 60 + 20 - (max - 100));
        assert!(slots[0].stock >= 0 && slots[0].clip >= 0);
    }

    #[test]
    fn a_dropped_weapon_takes_the_stock_the_other_weapons_could_not_hold() {
        let mut s = setup(vec![rifle("ak_mp", "ak")]);
        s.give("ak_mp");
        let ak = s.idx("ak_mp");
        s.inv.set_clip(&s.table, ak, 12);
        s.inv.set_stock(&s.table, ak, 55);
        let slots = transfer_from_player(&s.inv, &s.table, ak);
        assert_eq!(slots[0].weapon, ak);
        assert_eq!(slots[0].clip, 12);
        assert_eq!(slots[1], ItemAmmo::NONE);
        assert!(has_ammo_to_transfer(&s.inv, &s.table, ak));
        s.inv.set_clip(&s.table, ak, 0);
        s.inv.set_stock(&s.table, ak, 0);
        assert!(!has_ammo_to_transfer(&s.inv, &s.table, ak));
    }

    #[test]
    fn taking_a_dropped_weapon_restores_its_clip_and_stock() {
        let mut s = setup(vec![rifle("ak_mp", "ak")]);
        let ak = s.idx("ak_mp");
        assert!(s.inv.give_raw(&s.table, &mut s.ps, ak, 0));
        let slots = [
            ItemAmmo {
                weapon: ak,
                clip: 7,
                stock: 33,
            },
            ItemAmmo::NONE,
        ];
        add_ammo_for_new_weapon(&mut s.inv, &s.table, &mut s.ps, &slots, |_| 0);
        assert_eq!(s.inv.clip(&s.table, ak), 7);
        assert_eq!(s.inv.stock(&s.table, ak), 33);
    }

    #[test]
    fn the_hint_is_for_a_weapon_the_player_lacks() {
        let mut s = setup(vec![rifle("ak_mp", "ak"), rifle("g3_mp", "g3")]);
        s.give("ak_mp");
        let (ak, g3) = (s.idx("ak_mp"), s.idx("g3_mp"));
        assert_eq!(
            item_cursor_hint(&s.inv, &s.table, ak, g3),
            g3 as u8 + WEAPON_HINT_OFFSET
        );
        assert_eq!(item_cursor_hint(&s.inv, &s.table, ak, ak), 0);
    }

    #[test]
    fn walking_distance_to_an_item_is_a_box_around_the_origin() {
        let at = [100.0, 100.0, 100.0];
        assert!(player_touches_item([136.0, 64.0, 12.0], at));
        assert!(player_touches_item([100.0, 100.0, 118.0], at));
        assert!(!player_touches_item([137.0, 100.0, 100.0], at));
        assert!(!player_touches_item([100.0, 100.0, 119.0], at));
        assert!(!player_touches_item([100.0, 100.0, 11.0], at));
    }

    #[test]
    fn a_weapon_nobody_owned_holds_between_its_drop_limits() {
        let mut w = rifle("ak_mp", "ak");
        w.drop_ammo_min = 10;
        w.drop_ammo_max = 40;
        let s = setup(vec![w]);
        let ak = s.idx("ak_mp");
        let mut seed = 12345u32;
        let mut rand = || {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            ((seed >> 16) & 0x7fff) as i32
        };
        for _ in 0..50 {
            let [a, b] = transfer_random(&s.table, ak, &mut rand);
            assert!((10..=40).contains(&(a.clip + a.stock)));
            assert!(a.clip <= 30);
            assert_eq!(b, ItemAmmo::NONE);
        }
    }
}
