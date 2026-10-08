// SPDX-License-Identifier: GPL-3.0-only
//! Hand-built weapons for tests.

use super::info::{InventoryType, OffhandClass, WeaponClass, WeaponInfo, WeaponType};

pub fn rifle(name: &str, ammo: &str) -> WeaponInfo {
    WeaponInfo {
        name: name.into(),
        ammo_name: ammo.into(),
        clip_name: name.into(),
        start_ammo: 90,
        max_ammo: 120,
        clip_size: 30,
        fire_time: 100,
        reload_time: 2000,
        raise_time: 500,
        drop_time: 400,
        quick_raise_time: 300,
        quick_drop_time: 250,
        first_raise_time: 800,
        empty_raise_time: 600,
        empty_drop_time: 350,
        damage: 40,
        min_damage: 20,
        max_damage_range: 500.0,
        min_damage_range: 1000.0,
        ..WeaponInfo::default()
    }
}

pub fn grenade(name: &str) -> WeaponInfo {
    WeaponInfo {
        name: name.into(),
        ammo_name: name.into(),
        clip_name: name.into(),
        weap_type: WeaponType::Grenade,
        weap_class: WeaponClass::Grenade,
        inventory_type: InventoryType::Offhand,
        offhand_class: OffhandClass::Frag,
        start_ammo: 1,
        max_ammo: 1,
        clip_size: 1,
        clip_only: true,
        fuse_time: 3500,
        hold_fire_time: 200,
        fire_time: 400,
        fire_delay: 100,
        ..WeaponInfo::default()
    }
}
