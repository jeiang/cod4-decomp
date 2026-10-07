// SPDX-License-Identifier: GPL-3.0-or-later
//! Inventory semantics: ownership, ammunition counters, alternate weapons, limits.

use super::fixtures::{grenade, rifle};
use super::*;
use crate::pm::PlayerState;

struct Setup {
    table: WeaponTable,
    inv: PlayerWeapons,
    ps: PlayerState,
}

impl Setup {
    fn new(infos: Vec<WeaponInfo>) -> Self {
        Self {
            table: WeaponTable::from_infos(infos).unwrap(),
            inv: PlayerWeapons::new(),
            ps: PlayerState::default(),
        }
    }

    fn idx(&self, name: &str) -> u16 {
        let i = self.table.index(name);
        assert_ne!(i, 0, "{name} not in the table");
        i
    }

    fn give(&mut self, name: &str) -> bool {
        let i = self.idx(name);
        self.inv.give(&self.table, &mut self.ps, i, 0)
    }

    fn clip(&self, name: &str) -> i32 {
        self.inv.clip(&self.table, self.idx(name))
    }

    fn stock(&self, name: &str) -> i32 {
        self.inv.stock(&self.table, self.idx(name))
    }
}

fn two_rifles() -> Setup {
    Setup::new(vec![
        rifle("ak47_mp", "ar"),
        rifle("m4_mp", "ar"),
        rifle("deserteagle_mp", "pistol"),
    ])
}

#[test]
fn giving_a_weapon_fills_the_clip_from_the_start_ammo() {
    let mut s = two_rifles();
    assert!(s.give("ak47_mp"));
    assert_eq!((s.clip("ak47_mp"), s.stock("ak47_mp")), (30, 60));
    assert!(s.inv.has(s.idx("ak47_mp")));
    assert!(!s.inv.has(s.idx("m4_mp")));
    assert_eq!(s.inv.model(s.idx("ak47_mp")), 0);
}

#[test]
fn giving_an_owned_weapon_changes_nothing() {
    let mut s = two_rifles();
    s.give("ak47_mp");
    let before = s.inv.clone();
    assert!(!s.give("ak47_mp"), "duplicate give must report false");
    assert_eq!(s.inv, before);
}

#[test]
fn weapons_sharing_ammunition_share_the_stock() {
    let mut s = two_rifles();
    s.give("ak47_mp");
    s.give("m4_mp");
    assert_eq!((s.clip("ak47_mp"), s.clip("m4_mp")), (30, 30));
    // Both name the same ammunition: one stock counter, topped up by the second weapon's start
    // ammunition less what the first one already accounts for.
    assert_eq!(s.stock("ak47_mp"), s.stock("m4_mp"));
    assert_eq!(s.stock("m4_mp"), 120);
}

#[test]
fn list_is_in_index_order_and_primaries_filter() {
    let mut s = Setup::new(vec![
        rifle("m4_mp", "ar"),
        grenade("frag_grenade_mp"),
        rifle("ak47_mp", "ar"),
    ]);
    s.give("m4_mp");
    s.give("frag_grenade_mp");
    s.give("ak47_mp");
    let names = |v: Vec<u16>| -> Vec<String> {
        v.into_iter().map(|i| s.table.name(i).to_owned()).collect()
    };
    assert_eq!(
        names(s.inv.list(&s.table).collect()),
        ["ak47_mp", "frag_grenade_mp", "m4_mp"]
    );
    assert_eq!(
        names(s.inv.list_primaries(&s.table).collect()),
        ["ak47_mp", "m4_mp"]
    );
    assert!(s.inv.primaries_full(&s.table));
}

#[test]
fn alt_weapons_are_owned_and_taken_together() {
    let mut gl = rifle("gl_mp", "gl");
    gl.inventory_type = InventoryType::AltMode;
    gl.clip_size = 1;
    gl.start_ammo = 3;
    gl.max_ammo = 3;
    let mut m4 = rifle("m4_gl_mp", "ar");
    m4.alt_weapon_name = "gl_mp".into();
    let mut s = Setup::new(vec![m4, gl]);
    assert!(s.give("m4_gl_mp"));
    let (m4i, gli) = (s.idx("m4_gl_mp"), s.idx("gl_mp"));
    assert!(s.inv.has(gli), "the alternate fire mode comes with the weapon");
    assert_eq!((s.clip("gl_mp"), s.stock("gl_mp")), (1, 2), "alt ammunition is initialised too");
    assert_eq!(s.inv.list_primaries(&s.table).count(), 1, "the alt is not a primary");
    assert!(s.inv.any_ammo_for_weapon_modes(&s.table, m4i));

    s.ps.weapon = u32::from(m4i);
    assert!(s.inv.take(&s.table, &mut s.ps, m4i, true));
    assert!(!s.inv.has(m4i) && !s.inv.has(gli));
    assert_eq!(s.ps.weapon, 0, "taking the current weapon clears it");
    assert_eq!(s.clip("m4_gl_mp"), 0);
    assert_eq!(s.clip("gl_mp"), 0);
    assert_eq!(s.stock("gl_mp"), 0);
    assert!(!s.inv.take(&s.table, &mut s.ps, m4i, true), "second take is a no-op");
}

#[test]
fn taking_a_weapon_keeps_stock_the_others_can_hold() {
    let mut s = two_rifles();
    s.give("ak47_mp");
    s.give("m4_mp");
    let (ak, m4) = (s.idx("ak47_mp"), s.idx("m4_mp"));
    s.inv.take(&s.table, &mut s.ps, m4, true);
    assert_eq!(s.stock("ak47_mp"), 120, "the ak can hold 120");
    assert_eq!(s.clip("m4_mp"), 0);
    s.inv.take(&s.table, &mut s.ps, ak, true);
    assert_eq!(s.stock("ak47_mp"), 0, "nothing left to hold the ammunition");
}

#[test]
fn take_all_empties_everything() {
    let mut s = two_rifles();
    s.give("ak47_mp");
    s.give("deserteagle_mp");
    s.ps.weapon = 1;
    s.inv.take_all(&s.table, &mut s.ps);
    assert_eq!(s.inv.list(&s.table).count(), 0);
    assert_eq!(s.ps.weapon, 0);
    assert_eq!(s.stock("ak47_mp") + s.clip("ak47_mp"), 0);
    assert_eq!(s.stock("deserteagle_mp") + s.clip("deserteagle_mp"), 0);
}

#[test]
fn set_clip_and_stock_clamp() {
    let mut s = two_rifles();
    s.give("ak47_mp");
    let ak = s.idx("ak47_mp");
    s.inv.set_clip(&s.table, ak, 99);
    assert_eq!(s.inv.get_clip(&s.table, ak), 30);
    s.inv.set_clip(&s.table, ak, -4);
    assert_eq!(s.inv.get_clip(&s.table, ak), 0);
    s.inv.set_stock(&s.table, ak, 9999);
    assert_eq!(s.inv.get_stock(&s.table, ak), 120, "capped at the player maximum");
    s.inv.set_stock(&s.table, ak, -1);
    assert_eq!(s.inv.get_stock(&s.table, ak), 0);
    s.inv.set_clip(&s.table, 0, 5);
    assert_eq!(s.inv.get_clip(&s.table, 0), 0);
}

#[test]
fn clip_only_weapons_report_the_clip_as_stock() {
    let mut s = Setup::new(vec![grenade("frag_grenade_mp")]);
    assert!(s.give("frag_grenade_mp"));
    let g = s.idx("frag_grenade_mp");
    assert_eq!(s.inv.get_stock(&s.table, g), 1);
    assert_eq!(s.stock("frag_grenade_mp"), 0, "no separate stock behind a clip-only weapon");
    s.inv.set_stock(&s.table, g, 5);
    assert_eq!(s.inv.get_clip(&s.table, g), 1, "limited to the clip size");
}

#[test]
fn max_and_start_ammo() {
    let mut s = two_rifles();
    s.give("ak47_mp");
    let ak = s.idx("ak47_mp");
    s.inv.set_stock(&s.table, ak, 10);
    s.inv.give_max_ammo(&s.table, &mut s.ps, ak);
    assert_eq!(s.stock("ak47_mp"), 120);
    s.inv.set_stock(&s.table, ak, 0);
    s.inv.set_clip(&s.table, ak, 0);
    s.inv.give_start_ammo(&s.table, &mut s.ps, ak);
    assert_eq!((s.clip("ak47_mp"), s.stock("ak47_mp")), (30, 60));
}

#[test]
fn ammo_fractions() {
    let mut s = two_rifles();
    let ak = s.idx("ak47_mp");
    assert_eq!(s.inv.fraction_start_ammo(&s.table, ak), 1.0, "not owned");
    s.give("ak47_mp");
    assert_eq!(s.inv.fraction_start_ammo(&s.table, ak), (60.0f64 / 90.0) as f32);
    assert_eq!(s.inv.fraction_max_ammo(&s.table, ak), 0.5);
    s.inv.set_stock(&s.table, ak, 0);
    assert_eq!(s.inv.fraction_max_ammo(&s.table, ak), 0.0);
}

#[test]
fn any_ammo_counts_the_alt_weapon() {
    let mut alt = rifle("alt_mp", "altammo");
    alt.inventory_type = InventoryType::AltMode;
    let mut main = rifle("main_mp", "ar");
    main.alt_weapon_name = "alt_mp".into();
    let mut s = Setup::new(vec![main, alt]);
    s.give("main_mp");
    let (m, a) = (s.idx("main_mp"), s.idx("alt_mp"));
    for w in [m, a] {
        s.inv.set_clip(&s.table, w, 0);
        s.inv.set_stock(&s.table, w, 0);
    }
    assert!(!s.inv.any_ammo_for_weapon_modes(&s.table, m));
    s.inv.set_stock(&s.table, a, 1);
    assert!(s.inv.any_ammo_for_weapon_modes(&s.table, m));
}

#[test]
fn turrets_and_models_without_a_gun_model_cannot_be_given() {
    let mut turret = rifle("turret_mp", "t");
    turret.weap_class = WeaponClass::Turret;
    let mut s = Setup::new(vec![turret, rifle("ak47_mp", "ar")]);
    assert!(!s.give("turret_mp"));
    let ak = s.idx("ak47_mp");
    assert!(!s.inv.give(&s.table, &mut s.ps, ak, 3), "no gun model 3");
    assert!(s.inv.give(&s.table, &mut s.ps, ak, 0));
}

#[test]
fn the_first_offhand_weapon_is_equipped_and_not_replaced_while_it_has_ammo() {
    let mut smoke = grenade("smoke_grenade_mp");
    smoke.offhand_class = OffhandClass::Smoke;
    let mut s = Setup::new(vec![grenade("frag_grenade_mp"), smoke]);
    s.give("frag_grenade_mp");
    assert_eq!(s.ps.offhand_index, s.idx("frag_grenade_mp"));
    s.give("smoke_grenade_mp");
    assert_eq!(s.ps.offhand_index, s.idx("frag_grenade_mp"));
    let smoke = s.idx("smoke_grenade_mp");
    assert_eq!(
        s.inv.first_available_offhand(&s.table, &s.ps, OffhandClass::Smoke),
        smoke
    );
    assert_eq!(s.inv.first_available_offhand(&s.table, &s.ps, OffhandClass::Flash), 0);
    s.inv.set_clip(&s.table, smoke, 0);
    assert_eq!(s.inv.first_available_offhand(&s.table, &s.ps, OffhandClass::Smoke), 0);
    assert_eq!(
        s.inv.first_equipped_offhand(&s.table, OffhandClass::Smoke),
        smoke,
        "an empty weapon is still equipped"
    );
}

#[test]
fn shared_ammo_cap_limits_what_can_be_carried() {
    let mut frag = grenade("frag_grenade_mp");
    let mut smoke = grenade("smoke_grenade_mp");
    smoke.offhand_class = OffhandClass::Smoke;
    for g in [&mut frag, &mut smoke] {
        g.shared_ammo_cap_name = "grenades".into();
        g.shared_ammo_cap = 1;
    }
    let mut s = Setup::new(vec![frag, smoke]);
    assert!(s.give("frag_grenade_mp"));
    assert!(s.give("smoke_grenade_mp"));
    let (f, sm) = (s.idx("frag_grenade_mp"), s.idx("smoke_grenade_mp"));
    // The cap allows one grenade in total: the second one's rounds are taken back and, with
    // nothing left in its clip, the weapon is removed again.
    assert_eq!(s.inv.max_pickupable(&s.table, f), 0);
    assert!(s.inv.has(f));
    assert!(!s.inv.has(sm));
}

#[test]
fn select_only_accepts_owned_weapons() {
    let mut s = two_rifles();
    s.give("ak47_mp");
    assert!(!s.inv.select(s.idx("m4_mp")));
    assert_eq!(s.inv.selected(), 0);
    assert!(s.inv.select(s.idx("ak47_mp")));
    assert_eq!(s.inv.selected(), s.idx("ak47_mp"));
}

#[test]
fn spawn_weapon_readies_an_owned_weapon() {
    let mut s = two_rifles();
    s.give("ak47_mp");
    s.ps.weapon_state = crate::pm::weapon_state::DROPPING;
    let (m4, ak) = (s.idx("m4_mp"), s.idx("ak47_mp"));
    assert!(!s.inv.spawn_weapon(&mut s.ps, m4));
    assert!(s.inv.spawn_weapon(&mut s.ps, ak));
    assert_eq!(s.ps.weapon, u32::from(ak));
    assert_eq!(s.ps.weapon_state, crate::pm::weapon_state::READY);
}
