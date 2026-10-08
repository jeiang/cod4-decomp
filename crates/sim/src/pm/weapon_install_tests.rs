// SPDX-License-Identifier: GPL-3.0-only
//! Weapons from the stock zones. Skipped without `COD4_PATH`.

use super::install_tests::decode;
use super::state::weapon_state as ws;
use super::test_world::TestWorld;
use super::*;
use crate::weapon::{
    FireType, InventoryType, PlayerWeapons, WeaponClass, WeaponCtx, WeaponEvent, WeaponTable,
    WeaponType, damage,
};
use assets::zone::Asset;
use assets::zone::weapon::WeaponDef;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

/// The zones a dedicated server holds: the boot set and the map.
const ZONES: [&str; 5] = [
    "code_post_gfx_mp",
    "localized_code_post_gfx_mp",
    "common_mp",
    "localized_common_mp",
    "mp_crash",
];

fn table() -> Option<&'static WeaponTable> {
    static TABLE: LazyLock<Option<WeaponTable>> = LazyLock::new(|| {
        let Some(root) = std::env::var_os("COD4_PATH") else {
            eprintln!("COD4_PATH not set; skipping");
            return None;
        };
        let root = PathBuf::from(root);
        let mut defs: Vec<Arc<WeaponDef>> = Vec::new();
        for zone in ZONES {
            decode(&root, zone, |a| {
                if let Asset::Weapon(w) = a {
                    defs.push(w);
                }
            })?;
        }
        Some(WeaponTable::new(&defs).expect("the stock weapons fit the table"))
    });
    TABLE.as_ref()
}

#[test]
fn the_stock_weapons_index_and_share_ammunition() {
    let Some(t) = table() else { return };
    eprintln!(
        "{} weapons, {} ammo types, {} clip types",
        t.len(),
        t.ammo_types(),
        t.clip_types()
    );
    for name in [
        "ak47_mp",
        "m4_silencer_mp",
        "deserteagle_mp",
        "frag_grenade_mp",
        "rpg_mp",
        "remington700_mp",
    ] {
        assert_ne!(t.index(name), 0, "{name}");
        assert_eq!(t.index(&name.to_uppercase()), t.index(name));
    }
    assert!(t.len() > 50);
    let sorted: Vec<_> = t.iter().map(|w| w.name.to_ascii_lowercase()).collect();
    assert!(sorted.windows(2).all(|w| w[0] < w[1]), "sorted and unique");
    assert!(t.unresolved_alts().is_empty(), "{:?}", t.unresolved_alts());

    let ak = t.info(t.index("ak47_mp"));
    assert_eq!(ak.weap_type, WeaponType::Bullet);
    assert_eq!(ak.weap_class, WeaponClass::Rifle);
    assert_eq!(ak.inventory_type, InventoryType::Primary);
    assert_eq!(ak.clip_size, 30);
    assert!(ak.fire_time > 0 && ak.raise_time > 0 && ak.reload_time > 0);
    assert!(ak.damage > ak.min_damage && ak.max_damage_range < ak.min_damage_range);
    assert!(ak.aim_down_sight);
    let ak_variant = t.info(t.index("ak47_silencer_mp"));
    let m4 = t.info(t.index("m4_silencer_mp"));
    assert_ne!(ak.ammo_index, 0);
    assert_eq!(
        ak.ammo_index, ak_variant.ammo_index,
        "variants of a rifle share their ammunition"
    );
    assert_ne!(
        ak.clip_index, ak_variant.clip_index,
        "but not their magazine"
    );
    assert_ne!(ak.ammo_index, m4.ammo_index, "different calibres do not");
    assert_eq!(
        t.info(t.index("m4_gl_mp")).alt_weapon,
        t.index("gl_m4_mp"),
        "the underbarrel launcher is the alternate fire mode"
    );
    let frag = t.info(t.index("frag_grenade_mp"));
    assert_eq!(frag.weap_type, WeaponType::Grenade);
    assert_ne!(frag.offhand_class, crate::weapon::OffhandClass::None);
    assert!(frag.fuse_time > 0);
    let rpg = t.info(t.index("rpg_mp"));
    assert_eq!(rpg.weap_type, WeaponType::Projectile);
    assert_eq!(rpg.weap_class, WeaponClass::RocketLauncher);
    assert!(rpg.explosion_radius > 0 && rpg.explosion_inner_damage > rpg.explosion_outer_damage);
    let sniper = t.info(t.index("remington700_mp"));
    assert!(sniper.bolt_action && sniper.overlay_reticle);
    assert!(damage::damage_at_range(ak, 0.0) > damage::damage_at_range(ak, 5000.0));
    assert!(ak.location_damage_multipliers[damage::HITLOC_HEAD] > 1.0);
}

/// Steps one second-long window of commands and reports the events.
struct Shooter {
    inv: PlayerWeapons,
    ps: PlayerState,
    params: Params,
    world: TestWorld,
    sent: UserCmd,
    want: u16,
    log: Vec<WeaponEvent>,
}

impl Shooter {
    fn new(t: &WeaponTable, weapon: &str) -> Self {
        let mut ps = PlayerState {
            command_time: 10_000,
            ground_entity_num: ENTITYNUM_WORLD,
            ..PlayerState::default()
        };
        let mut inv = PlayerWeapons::new();
        let w = t.index(weapon);
        assert!(inv.give(t, &mut ps, w, 0), "{weapon} can be given");
        Self {
            inv,
            ps,
            params: Params::default(),
            world: TestWorld::floor(),
            sent: UserCmd::default(),
            want: w,
            log: Vec::new(),
        }
    }

    fn step(&mut self, t: &WeaponTable, buttons: i32, dt: i32) {
        let mut pm = Pmove::new(std::mem::take(&mut self.ps), &self.params);
        pm.weapons = Some(WeaponCtx::new(t, &mut self.inv));
        pm.cmd = UserCmd {
            buttons,
            weapon: self.want as u8,
            server_time: pm.ps.command_time + dt,
            ..UserCmd::default()
        };
        pm.oldcmd = self.sent;
        self.sent = pm.cmd;
        pmove(&mut pm, &self.world);
        self.log.extend_from_slice(pm.weapon_out.events());
        self.ps = pm.ps;
    }

    fn shots(&self) -> usize {
        self.log
            .iter()
            .filter(|e| matches!(e, WeaponEvent::Fire { .. }))
            .count()
    }
}

/// Server ticks the first shot after the raise, then one round per `fire_time` rounded up to
/// whole commands.
fn expected_shots(fire_time: i32, dt: i32, steps: i32) -> i32 {
    let every = (fire_time + dt - 1) / dt;
    (steps - 1) / every + 1
}

#[test]
fn a_three_second_burst_with_a_real_rifle_fires_by_its_fire_time() {
    let Some(t) = table() else { return };
    for name in ["ak47_mp", "m4_silencer_mp", "rpd_mp"] {
        let w = t.info(t.index(name));
        if w.fire_type != FireType::FullAuto {
            continue;
        }
        for dt in [10, 33] {
            let mut s = Shooter::new(t, name);
            let idx = s.want;
            // Raise.
            let raise_steps = w.first_raise_time / dt + 3;
            for _ in 0..raise_steps {
                s.step(t, 0, dt);
            }
            assert_eq!(s.ps.weapon_state, ws::READY, "{name}: raised");
            assert_eq!(s.ps.weapon, u32::from(idx));
            let clip0 = s.inv.clip(t, idx);
            // Hold the trigger for three seconds.
            let steps = 3000 / dt;
            for _ in 0..steps {
                s.step(t, button::ATTACK, dt);
            }
            let fired = s.shots() as i32;
            let want = expected_shots(w.fire_time, dt, steps).min(clip0);
            assert_eq!(
                fired, want,
                "{name} dt={dt}: fire_time {} ms, clip {clip0}",
                w.fire_time
            );
            let used = clip0 - s.inv.clip(t, idx);
            assert!(
                used == fired || fired == clip0 || s.ps.weapon_state == ws::RELOADING,
                "{name}: {used} rounds used for {fired} shots"
            );
            eprintln!(
                "{name} dt={dt}: {fired} shots in 3 s, fire_time {} ms",
                w.fire_time
            );
        }
    }
}

#[test]
fn a_real_rifle_reloads_with_its_own_times() {
    let Some(t) = table() else { return };
    let w = t.info(t.index("ak47_mp"));
    let mut s = Shooter::new(t, "ak47_mp");
    let idx = s.want;
    for _ in 0..(w.first_raise_time / 10 + 3) {
        s.step(t, 0, 10);
    }
    s.inv.set_clip(t, idx, 5);
    s.step(t, button::RELOAD, 10);
    assert_eq!(s.ps.weapon_state, ws::RELOADING);
    assert_eq!(s.ps.weapon_time, w.reload_time);
    // Ammunition goes in when the magazine is seated (`reload_add_time`, else the whole reload).
    let add = if w.reload_add_time != 0 && w.reload_add_time < w.reload_time {
        w.reload_add_time
    } else {
        w.reload_time
    };
    let ceil = |ms: i32| ((ms + 9) / 10) as usize;
    for _ in 0..ceil(add) - 1 {
        s.step(t, 0, 10);
    }
    assert_eq!(s.inv.clip(t, idx), 5, "not yet");
    s.step(t, 0, 10);
    assert_eq!(s.inv.clip(t, idx), w.clip_size);
    assert_eq!(
        s.ps.weapon_state,
        if add == w.reload_time {
            ws::READY
        } else {
            ws::RELOADING
        }
    );
    for _ in 0..ceil(w.reload_time) - ceil(add) - 1 {
        s.step(t, 0, 10);
    }
    assert_eq!(
        s.ps.weapon_state,
        ws::RELOADING,
        "10 ms short of the full reload"
    );
    s.step(t, 0, 10);
    assert_eq!(s.ps.weapon_state, ws::READY);
}

#[test]
fn real_grenades_prime_cook_and_throw() {
    let Some(t) = table() else { return };
    let mut s = Shooter::new(t, "ak47_mp");
    let frag = t.index("frag_grenade_mp");
    assert!(s.inv.give(t, &mut s.ps, frag, 0));
    assert_eq!(s.ps.offhand_index, frag);
    let ak = t.info(s.want);
    for _ in 0..(ak.first_raise_time / 10 + 3) {
        s.step(t, 0, 10);
    }
    let pm_step = |s: &mut Shooter, buttons: i32, n: usize| {
        for _ in 0..n {
            let mut pm = Pmove::new(std::mem::take(&mut s.ps), &s.params);
            pm.weapons = Some(WeaponCtx::new(t, &mut s.inv));
            pm.cmd = UserCmd {
                buttons,
                weapon: s.want as u8,
                offhand_index: frag as u8,
                server_time: pm.ps.command_time + 10,
                ..UserCmd::default()
            };
            pm.oldcmd = s.sent;
            s.sent = pm.cmd;
            pmove(&mut pm, &s.world);
            s.log.extend_from_slice(pm.weapon_out.events());
            s.ps = pm.ps;
        }
    };
    pm_step(&mut s, button::FRAG, 300);
    assert_eq!(
        s.ps.weapon_state,
        ws::OFFHAND_START,
        "holding a primed grenade"
    );
    pm_step(&mut s, 0, 200);
    let throw = s.log.iter().find_map(|e| match e {
        WeaponEvent::OffhandThrow {
            weapon, fuse_left, ..
        } => Some((*weapon, *fuse_left)),
        _ => None,
    });
    let (weapon, fuse_left) = throw.expect("thrown after the release");
    assert_eq!(weapon, frag);
    let fuse = t.info(frag).fuse_time;
    assert!(fuse_left <= fuse && fuse_left > 0);
    assert_eq!(s.ps.weapon_state, ws::READY);
}
