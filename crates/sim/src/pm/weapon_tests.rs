// SPDX-License-Identifier: GPL-3.0-or-later
//! The weapon state machine inside `pmove`, against a hand-built weapon table on a flat floor.
//! Timings are exact: the harness steps in whole milliseconds and asserts the state after the
//! step a timer expires on.

use super::state::weapon_state as ws;
use super::test_world::TestWorld;
use super::*;
use crate::weapon::fixtures::{grenade, rifle};
use crate::weapon::{
    FireType, InventoryType, OffhandClass, PlayerWeapons, WeaponClass, WeaponCtx, WeaponEvent,
    WeaponInfo, WeaponTable,
};

/// A player, their weapons and the commands they send.
struct Rig {
    table: WeaponTable,
    inv: PlayerWeapons,
    ps: PlayerState,
    params: Params,
    world: TestWorld,
    sent: UserCmd,
    log: Vec<WeaponEvent>,
    /// Weapon asked for (`cmd.weapon`).
    want: u16,
    want_offhand: u16,
    angles: [i32; 3],
}

impl Rig {
    fn new(infos: Vec<WeaponInfo>) -> Self {
        let ps = PlayerState {
            command_time: 10_000,
            ground_entity_num: ENTITYNUM_WORLD,
            ..PlayerState::default()
        };
        Self {
            table: WeaponTable::from_infos(infos).unwrap(),
            inv: PlayerWeapons::new(),
            ps,
            params: Params::default(),
            world: TestWorld::floor(),
            sent: UserCmd::default(),
            log: Vec::new(),
            want: 0,
            want_offhand: 0,
            angles: [0; 3],
        }
    }

    fn idx(&self, name: &str) -> u16 {
        let i = self.table.index(name);
        assert_ne!(i, 0, "{name}");
        i
    }

    fn give(&mut self, name: &str) -> u16 {
        let i = self.idx(name);
        assert!(self.inv.give(&self.table, &mut self.ps, i, 0), "{name}");
        i
    }

    /// Gives the weapon and holds it ready, as if raised long ago.
    fn hold(&mut self, name: &str) -> u16 {
        let i = self.give(name);
        self.inv.mark_raised(i);
        assert!(self.inv.spawn_weapon(&mut self.ps, i));
        self.want = i;
        i
    }

    /// Aim spread starts wherever the last event left it; tests that look at it set it first.
    fn step(&mut self, buttons: i32, fwd: i8, dt: i32) {
        let mut pm = Pmove::new(std::mem::take(&mut self.ps), &self.params);
        pm.weapons = Some(WeaponCtx::new(&self.table, &mut self.inv));
        pm.cmd = UserCmd {
            buttons,
            forwardmove: fwd,
            weapon: self.want as u8,
            offhand_index: self.want_offhand as u8,
            angles: self.angles,
            server_time: pm.ps.command_time + dt,
            ..UserCmd::default()
        };
        pm.oldcmd = self.sent;
        self.sent = pm.cmd;
        pmove(&mut pm, &self.world);
        self.log.extend_from_slice(pm.weapon_out.events());
        self.ps = pm.ps;
    }

    /// `n` steps of 10 ms.
    fn run(&mut self, buttons: i32, n: usize) {
        for _ in 0..n {
            self.step(buttons, 0, 10);
        }
    }

    fn state(&self) -> u8 {
        self.ps.weapon_state
    }

    fn count(&self, f: impl Fn(&WeaponEvent) -> bool) -> usize {
        self.log.iter().filter(|e| f(e)).count()
    }

    fn shots(&self) -> usize {
        self.count(|e| matches!(e, WeaponEvent::Fire { .. }))
    }

    fn clip(&self, i: u16) -> i32 {
        self.inv.clip(&self.table, i)
    }

    fn stock(&self, i: u16) -> i32 {
        self.inv.stock(&self.table, i)
    }
}

const ATTACK: i32 = button::ATTACK;

fn ak() -> WeaponInfo {
    WeaponInfo {
        reload_empty_time: 2400,
        ..rifle("ak47_mp", "ar")
    }
}

#[test]
fn raising_a_first_weapon_takes_the_first_raise_time() {
    let mut r = Rig::new(vec![ak()]);
    let a = r.give("ak47_mp");
    r.want = a;
    r.run(0, 1);
    assert_eq!(r.ps.weapon, u32::from(a), "the empty hands pick it up at once");
    assert_eq!((r.state(), r.ps.weapon_time), (ws::RAISING, 800));
    assert_eq!(r.ps.aim_spread_scale, 255.0, "a fresh raise starts at full spread");
    r.run(0, 79);
    assert_eq!((r.state(), r.ps.weapon_time), (ws::RAISING, 10), "1 ms short");
    r.run(0, 1);
    assert_eq!(r.state(), ws::READY);
    assert!(r.log.contains(&WeaponEvent::SwitchComplete { from: 0, to: a }));
}

#[test]
fn full_auto_fires_once_per_fire_time() {
    let mut r = Rig::new(vec![ak()]);
    let a = r.hold("ak47_mp");
    r.run(ATTACK, 1);
    assert_eq!(r.shots(), 1, "the first round leaves on the first command");
    assert_eq!(r.state(), ws::FIRING);
    assert_eq!(r.clip(a), 29);
    r.run(ATTACK, 99);
    assert_eq!(r.shots(), 10, "100 ms apart: rounds 1, 11, ..., 91");
    r.run(ATTACK, 1);
    assert_eq!(r.shots(), 11, "the 101st ms fires the eleventh round");
    assert_eq!(r.clip(a), 19);
    match r.log[0] {
        WeaponEvent::Fire {
            weapon,
            first,
            ads,
            burst,
            last_round,
            ..
        } => {
            assert_eq!(weapon, a);
            assert!(first && !ads && !burst && !last_round);
        }
        e => panic!("{e:?}"),
    }
    assert!(matches!(r.log[1], WeaponEvent::Fire { first: false, .. }));
    // The predictable event ring got the fire event too.
    let n = usize::from(r.ps.event_sequence).min(4);
    assert!(
        (0..n).any(|i| r.ps.events[(usize::from(r.ps.event_sequence) - 1 - i) & 3] == ev::FIRE_WEAPON)
    );
}

#[test]
fn a_command_longer_than_a_step_is_split_and_fires_the_same_shots() {
    let mut r = Rig::new(vec![ak()]);
    r.hold("ak47_mp");
    r.step(ATTACK, 0, 50);
    r.step(ATTACK, 0, 50);
    r.step(ATTACK, 0, 50);
    // 150 ms in 50 ms commands: fire_time 100 elapses on the second command's end (>= 100).
    assert_eq!(r.shots(), 2);
}

#[test]
fn a_bullet_weapon_with_a_fire_delay_never_gets_past_it() {
    // `PM_Weapon_StartFiring` re-arms the delay on the delayed call too: only weapons whose
    // delay is computed (ads_fire_only) can fire with one.
    let mut w = ak();
    w.fire_delay = 50;
    let mut r = Rig::new(vec![w]);
    let a = r.hold("ak47_mp");
    r.run(ATTACK, 100);
    assert_eq!(r.shots(), 0);
    assert_eq!(r.clip(a), 30);
}

#[test]
fn semi_auto_needs_the_trigger_released() {
    let mut w = ak();
    w.fire_type = FireType::SingleShot;
    let mut r = Rig::new(vec![w]);
    r.hold("ak47_mp");
    r.run(ATTACK, 100);
    assert_eq!(r.shots(), 1, "holding the trigger fires one round");
    r.run(0, 1);
    assert_eq!(r.shots(), 1);
    assert_eq!(r.state(), ws::READY);
    r.run(ATTACK, 1);
    assert_eq!(r.shots(), 2, "a new pull fires again");
    assert!(matches!(r.log[1], WeaponEvent::Fire { shot: 1, .. }));
}

#[test]
fn semi_auto_only_notices_a_release_when_the_fire_time_ends() {
    let mut w = ak();
    w.fire_type = FireType::SingleShot;
    let mut r = Rig::new(vec![w]);
    r.hold("ak47_mp");
    r.run(ATTACK, 1);
    r.run(0, 3);
    r.run(ATTACK, 20);
    assert_eq!(r.shots(), 1, "the trigger was down again when the 100 ms ran out");
    r.run(0, 1);
    r.run(ATTACK, 2);
    assert_eq!(r.shots(), 2, "released at the end: the next pull fires");
}

#[test]
fn a_burst_fires_its_rounds_then_cools_down() {
    let mut w = ak();
    w.fire_type = FireType::Burst3;
    let mut r = Rig::new(vec![w]);
    r.hold("ak47_mp");
    r.run(ATTACK, 200);
    assert_eq!(r.shots(), 3, "one burst per trigger pull");
    let shots: Vec<u8> = r
        .log
        .iter()
        .filter_map(|e| match e {
            WeaponEvent::Fire { shot, burst: true, .. } => Some(*shot),
            _ => None,
        })
        .collect();
    assert_eq!(shots, [1, 2, 3]);
    r.run(0, 1);
    r.run(ATTACK, 40);
    assert_eq!(r.shots(), 6, "the next pull fires the next burst");
}

#[test]
fn the_burst_cooldown_blocks_an_immediate_second_burst() {
    let mut w = ak();
    w.fire_type = FireType::Burst3;
    let mut r = Rig::new(vec![w]);
    r.hold("ak47_mp");
    r.run(ATTACK, 31);
    assert_eq!(r.shots(), 3);
    assert_eq!((r.state(), r.ps.weapon_time), (ws::READY, 200), "default 0.2 s cooldown");
    r.run(0, 1);
    r.run(ATTACK, 5);
    assert_eq!(r.shots(), 3, "pulled again 60 ms into the cooldown");
    r.run(ATTACK, 15);
    assert_eq!(r.shots(), 4, "the new burst starts when the cooldown is over");
}

#[test]
fn the_last_round_is_flagged_and_firing_on_empty_reloads_automatically() {
    let mut r = Rig::new(vec![ak()]);
    let a = r.hold("ak47_mp");
    r.inv.set_clip(&r.table, a, 2);
    r.run(ATTACK, 11);
    assert_eq!(r.shots(), 2);
    assert!(matches!(r.log[1], WeaponEvent::Fire { last_round: true, .. }));
    assert_eq!(r.clip(a), 0);
    r.run(ATTACK, 10);
    assert_eq!(r.shots(), 2, "nothing left to fire");
    assert_eq!(r.state(), ws::RELOADING, "the empty magazine reloads on the next pull");
    assert_eq!(r.ps.weapon_time, 2400, "an empty reload uses the empty time");
    r.run(ATTACK, 239);
    assert_eq!((r.state(), r.clip(a)), (ws::RELOADING, 0), "1 ms before the end");
    r.run(ATTACK, 1);
    assert_eq!(r.state(), ws::READY);
    assert_eq!((r.clip(a), r.stock(a)), (30, 30));
    assert!(r.log.contains(&WeaponEvent::ReloadAmmoAdded { weapon: a, amount: 30 }));
    assert!(r.log.contains(&WeaponEvent::ReloadComplete { weapon: a }));
}

#[test]
fn the_reload_button_tops_up_a_partial_magazine() {
    let mut r = Rig::new(vec![ak()]);
    let a = r.hold("ak47_mp");
    r.inv.set_clip(&r.table, a, 10);
    r.run(button::RELOAD, 1);
    assert_eq!((r.state(), r.ps.weapon_time), (ws::RELOADING, 2000));
    r.run(0, 199);
    assert_eq!((r.state(), r.clip(a)), (ws::RELOADING, 10));
    r.run(0, 1);
    assert_eq!((r.state(), r.clip(a), r.stock(a)), (ws::READY, 30, 40));
    r.run(button::RELOAD, 5);
    assert_eq!(r.state(), ws::READY, "a full magazine does not reload");
}

#[test]
fn no_partial_reload_refuses_to_top_up() {
    let mut w = ak();
    w.no_partial_reload = true;
    let mut r = Rig::new(vec![w]);
    let a = r.hold("ak47_mp");
    r.inv.set_clip(&r.table, a, 10);
    r.run(button::RELOAD, 5);
    assert_eq!(r.state(), ws::READY);
    r.inv.set_clip(&r.table, a, 0);
    r.run(button::RELOAD, 1);
    assert_eq!(r.state(), ws::RELOADING);
}

#[test]
fn reload_stops_at_the_stock() {
    let mut r = Rig::new(vec![ak()]);
    let a = r.hold("ak47_mp");
    r.inv.set_clip(&r.table, a, 0);
    r.inv.set_stock(&r.table, a, 12);
    r.run(button::RELOAD, 1);
    r.run(0, 240);
    assert_eq!((r.state(), r.clip(a), r.stock(a)), (ws::READY, 12, 0));
}

#[test]
fn segmented_reload_adds_a_round_per_segment() {
    let mut w = rifle("shotgun_mp", "shells");
    w.segmented_reload = true;
    w.clip_size = 6;
    w.reload_start_time = 300;
    w.reload_start_add_time = 200;
    w.reload_start_add = 1;
    w.reload_time = 400;
    w.reload_add_time = 250;
    w.reload_ammo_add = 1;
    w.reload_end_time = 350;
    let mut r = Rig::new(vec![w]);
    let a = r.hold("shotgun_mp");
    r.inv.set_clip(&r.table, a, 3);
    r.run(button::RELOAD, 1);
    assert_eq!(r.state(), ws::RELOAD_START);
    r.run(0, 20);
    assert_eq!(r.clip(a), 4, "the start segment loads its round after 200 ms");
    r.run(0, 10);
    assert_eq!(r.state(), ws::RELOADING, "then the loop");
    // Two more rounds fill the magazine: 6 total.
    r.run(0, 25 + 15 + 25 + 15);
    assert_eq!(r.clip(a), 6);
    assert_eq!(r.state(), ws::RELOAD_END);
    r.run(0, 35);
    assert_eq!(r.state(), ws::READY);
}

#[test]
fn segmented_reload_is_interrupted_by_the_trigger() {
    let mut w = rifle("shotgun_mp", "shells");
    w.segmented_reload = true;
    w.clip_size = 6;
    w.reload_start_time = 100;
    w.reload_time = 400;
    w.reload_add_time = 250;
    w.reload_ammo_add = 1;
    w.reload_end_time = 100;
    let mut r = Rig::new(vec![w]);
    let a = r.hold("shotgun_mp");
    r.inv.set_clip(&r.table, a, 3);
    r.run(button::RELOAD, 1);
    r.run(0, 10);
    assert_eq!(r.state(), ws::RELOADING);
    r.run(ATTACK, 1);
    assert_eq!(r.state(), ws::RELOADING_INTERUPT);
    r.run(ATTACK, 44);
    assert_eq!(r.state(), ws::RELOAD_END, "interrupted at the end of the current shell");
    r.run(0, 10);
    assert_eq!(r.state(), ws::READY);
    assert!(r.clip(a) >= 4);
}

#[test]
fn switching_drops_then_raises() {
    let mut r = Rig::new(vec![ak(), rifle("m4_mp", "ar")]);
    let a = r.hold("ak47_mp");
    let m = r.give("m4_mp");
    r.inv.mark_raised(m);
    r.want = m;
    r.run(0, 1);
    assert_eq!((r.state(), r.ps.weapon_time, r.ps.weapon), (ws::DROPPING, 400, u32::from(a)));
    assert!(r.log.contains(&WeaponEvent::SwitchBegin { from: a, to: m }));
    r.run(0, 39);
    assert_eq!((r.state(), r.ps.weapon), (ws::DROPPING, u32::from(a)), "1 ms short of 400");
    r.run(0, 1);
    assert_eq!((r.state(), r.ps.weapon, r.ps.weapon_time), (ws::RAISING, u32::from(m), 500));
    r.run(0, 49);
    assert_eq!(r.state(), ws::RAISING);
    r.run(0, 1);
    assert_eq!(r.state(), ws::READY);
    assert!(r.log.contains(&WeaponEvent::SwitchComplete { from: a, to: m }));
}

#[test]
fn switching_to_a_pistol_or_with_an_empty_magazine_uses_the_quick_and_empty_times() {
    let mut pistol = rifle("deserteagle_mp", "pistol");
    pistol.weap_class = WeaponClass::Pistol;
    let mut r = Rig::new(vec![ak(), pistol]);
    let a = r.hold("ak47_mp");
    let p = r.give("deserteagle_mp");
    r.inv.mark_raised(p);
    r.want = p;
    r.run(0, 1);
    assert_eq!((r.state(), r.ps.weapon_time), (ws::DROPPING_QUICK, 250), "pistols switch quickly");
    r.run(0, 25);
    assert_eq!((r.state(), r.ps.weapon_time), (ws::RAISING, 300), "quick raise");
    r.run(0, 30);
    assert_eq!(r.state(), ws::READY);
    // Back to the rifle with the pistol's magazine empty: the empty drop time applies.
    r.inv.mark_raised(a);
    r.inv.set_clip(&r.table, p, 0);
    r.inv.set_stock(&r.table, p, 0);
    r.want = a;
    r.run(0, 1);
    assert_eq!((r.state(), r.ps.weapon_time), (ws::DROPPING, 350), "empty drop");
}

#[test]
fn a_weapon_not_owned_is_never_raised() {
    let mut r = Rig::new(vec![ak(), rifle("m4_mp", "ar")]);
    let a = r.hold("ak47_mp");
    r.want = r.idx("m4_mp");
    r.run(0, 30);
    assert_eq!((r.ps.weapon, r.state()), (u32::from(a), ws::READY));
}

#[test]
fn script_switch_overrides_the_command_until_done() {
    let mut r = Rig::new(vec![ak(), rifle("m4_mp", "ar")]);
    let a = r.hold("ak47_mp");
    let m = r.give("m4_mp");
    r.inv.mark_raised(m);
    assert!(r.inv.select(m));
    // The player keeps asking for the ak.
    r.want = a;
    r.run(0, 41);
    assert_eq!(r.ps.weapon, u32::from(m));
    assert_eq!(r.inv.selected(), 0, "the request is done once the weapon is raised");
    r.run(0, 95);
    assert_eq!((r.ps.weapon, r.state()), (u32::from(a), ws::READY), "the command rules again");
}

#[test]
fn disabled_weapons_are_lowered_and_come_back() {
    let mut r = Rig::new(vec![ak()]);
    let a = r.hold("ak47_mp");
    r.ps.weapon_flags |= wf::DISABLED;
    r.run(0, 41);
    assert_eq!(r.ps.weapon, 0);
    r.run(ATTACK, 10);
    assert_eq!(r.shots(), 0);
    r.ps.weapon_flags &= !wf::DISABLED;
    r.run(0, 3);
    assert_eq!(r.ps.weapon, u32::from(a));
}

#[test]
fn dead_players_hold_nothing() {
    let mut r = Rig::new(vec![ak()]);
    r.hold("ak47_mp");
    r.ps.pm_type = PmType::Dead;
    r.run(ATTACK, 3);
    assert_eq!(r.ps.weapon, 0);
    assert_eq!(r.shots(), 0);
}

#[test]
fn melee_swings_and_connects_after_the_delay() {
    let mut w = ak();
    w.melee_damage = 135;
    w.melee_time = 600;
    w.melee_delay = 200;
    let mut r = Rig::new(vec![w]);
    let a = r.hold("ak47_mp");
    r.run(button::MELEE, 1);
    assert_eq!((r.state(), r.ps.weapon_time, r.ps.weapon_delay), (ws::MELEE_INIT, 600, 200));
    assert_eq!(r.count(|e| matches!(e, WeaponEvent::Melee { .. })), 0);
    r.run(button::MELEE, 19);
    assert_eq!(r.count(|e| matches!(e, WeaponEvent::Melee { .. })), 0, "1 ms short");
    assert_eq!(r.state(), ws::MELEE_INIT);
    r.run(0, 1);
    assert_eq!(r.log.last(), Some(&WeaponEvent::Melee { weapon: a }));
    assert_eq!(r.state(), ws::MELEE_FIRE);
    r.run(0, 39);
    assert_eq!(r.state(), ws::MELEE_FIRE);
    r.run(0, 1);
    assert_eq!(r.state(), ws::READY);
}

#[test]
fn melee_needs_a_fresh_button_press() {
    let mut w = ak();
    w.melee_damage = 135;
    w.melee_time = 100;
    w.melee_delay = 50;
    let mut r = Rig::new(vec![w]);
    r.hold("ak47_mp");
    r.run(button::MELEE, 50);
    assert_eq!(r.count(|e| matches!(e, WeaponEvent::Melee { .. })), 1);
}

#[test]
fn weapons_without_melee_damage_cannot_melee() {
    let mut r = Rig::new(vec![ak()]);
    r.hold("ak47_mp");
    r.run(button::MELEE, 5);
    assert_eq!(r.state(), ws::READY);
}

fn frag() -> WeaponInfo {
    WeaponInfo {
        cook_off_hold: true,
        ..grenade("frag_grenade_mp")
    }
}

fn grenade_rig() -> (Rig, u16, u16) {
    let mut r = Rig::new(vec![ak(), frag()]);
    let a = r.hold("ak47_mp");
    let g = r.give("frag_grenade_mp");
    r.want_offhand = g;
    (r, a, g)
}

#[test]
fn an_offhand_grenade_lowers_the_weapon_primes_and_throws_on_release() {
    let (mut r, a, g) = grenade_rig();
    r.run(button::FRAG, 1);
    assert_eq!((r.state(), r.ps.weapon_time), (ws::OFFHAND_INIT, 250), "weapon lowers first");
    assert!(r.ps.events.contains(&ev::SWITCH_OFFHAND));
    r.run(button::FRAG, 24);
    assert_eq!(r.state(), ws::OFFHAND_INIT);
    r.run(button::FRAG, 1);
    assert_eq!((r.state(), r.ps.weapon_time), (ws::OFFHAND_PREPARE, 200));
    assert_ne!(r.ps.weapon_flags & wf::USING_OFFHAND, 0);
    r.run(button::FRAG, 20);
    assert_eq!(r.state(), ws::OFFHAND_START, "primed and held");
    assert_eq!(r.ps.grenade_time_left, 3500);
    // Hold (cook) the grenade for a second.
    r.run(button::FRAG, 100);
    assert_eq!(r.state(), ws::OFFHAND_START);
    assert_eq!(r.shots(), 0, "a throw is not a weapon shot");
    assert_eq!(r.count(|e| matches!(e, WeaponEvent::OffhandThrow { .. })), 0);
    assert!(r.ps.grenade_time_left <= 2510 && r.ps.grenade_time_left >= 2490, "{}", r.ps.grenade_time_left);
    // Release: the grenade leaves after the fire delay (100 ms).
    r.run(0, 1);
    assert_eq!((r.state(), r.ps.weapon_time, r.ps.weapon_delay), (ws::OFFHAND_HOLD, 400, 100));
    r.run(0, 9);
    assert_eq!(r.count(|e| matches!(e, WeaponEvent::OffhandThrow { .. })), 0);
    r.run(0, 1);
    let throw = r
        .log
        .iter()
        .find_map(|e| match e {
            WeaponEvent::OffhandThrow { weapon, fuse_left } => Some((*weapon, *fuse_left)),
            _ => None,
        })
        .expect("thrown");
    assert_eq!(throw.0, g);
    assert!(throw.1 < 3500 && throw.1 > 2000, "cooked: {}", throw.1);
    assert_eq!(r.state(), ws::OFFHAND);
    assert_eq!(r.clip(g), 0, "the grenade is used up");
    // The weapon comes back up.
    r.run(0, 30);
    assert_eq!(r.state(), ws::OFFHAND_END);
    assert_eq!(r.ps.weapon_time, 300);
    assert_eq!(r.ps.weapon_flags & wf::USING_OFFHAND, 0);
    r.run(0, 30);
    assert_eq!((r.state(), r.ps.weapon), (ws::READY, u32::from(a)));
}

#[test]
fn a_grenade_cooked_too_long_goes_off_in_the_hand() {
    let (mut r, _, g) = grenade_rig();
    r.run(button::FRAG, 46 + 351);
    assert_eq!(
        r.count(|e| matches!(e, WeaponEvent::GrenadeSuicide { weapon } if *weapon == g)),
        1
    );
    assert_eq!(r.ps.grenade_time_left, -1);
    assert_eq!(r.clip(g), 0);
}

#[test]
fn grenades_without_cook_keep_their_full_fuse() {
    let mut w = frag();
    w.cook_off_hold = false;
    let mut r = Rig::new(vec![ak(), w]);
    r.hold("ak47_mp");
    let g = r.give("frag_grenade_mp");
    r.want_offhand = g;
    r.run(button::FRAG, 46 + 100);
    assert_eq!(r.ps.grenade_time_left, 3500);
    r.run(0, 11);
    assert!(r.log.contains(&WeaponEvent::OffhandThrow { weapon: g, fuse_left: 3500 }));
}

#[test]
fn no_grenades_left_reports_it() {
    let (mut r, _, g) = grenade_rig();
    r.inv.set_clip(&r.table, g, 0);
    r.run(button::FRAG, 1);
    assert_eq!(r.state(), ws::READY);
    assert!(r.log.contains(&WeaponEvent::EmptyOffhand));
}

#[test]
fn a_smoke_button_picks_the_secondary_class() {
    let mut flash = grenade("flash_grenade_mp");
    flash.offhand_class = OffhandClass::Flash;
    let mut smoke = grenade("smoke_grenade_mp");
    smoke.offhand_class = OffhandClass::Smoke;
    let mut r = Rig::new(vec![ak(), flash, smoke]);
    r.hold("ak47_mp");
    r.give("flash_grenade_mp");
    let s = r.give("smoke_grenade_mp");
    r.run(button::SMOKE, 1);
    assert_eq!(r.ps.offhand_index, s);
    r.run(0, 200);
    r.ps.offhand_secondary = 1;
    r.run(button::SMOKE, 1);
    assert_eq!(r.ps.offhand_index, r.idx("flash_grenade_mp"));
}

#[test]
fn primary_grenades_fire_after_the_hold_fire_time() {
    let mut g = frag();
    g.inventory_type = InventoryType::Primary;
    g.offhand_class = OffhandClass::None;
    g.hold_fire_time = 150;
    g.fire_time = 500;
    g.fire_delay = 0;
    g.clip_only = false;
    g.clip_size = 2;
    g.start_ammo = 4;
    g.max_ammo = 4;
    let mut r = Rig::new(vec![g]);
    let w = r.hold("frag_grenade_mp");
    // The button is held: the grenade stays primed (the fuse starts) and flies on release.
    r.run(ATTACK, 14);
    assert_eq!(r.shots(), 0);
    assert_eq!(r.ps.grenade_time_left, 3500 - 13 * 10, "pulled the pin, the fuse cooks");
    r.run(ATTACK, 1);
    assert_eq!(r.shots(), 0, "still held");
    r.run(0, 1);
    assert_eq!(r.shots(), 1, "thrown on release");
    assert!(matches!(r.log[0], WeaponEvent::Fire { weapon, .. } if weapon == w));
    assert_eq!(r.clip(w), 1);
}

#[test]
fn night_vision_toggles_on_the_button_press() {
    let mut r = Rig::new(vec![ak()]);
    r.hold("ak47_mp");
    r.run(button::NIGHTVISION, 3);
    assert_eq!(r.count(|e| matches!(e, WeaponEvent::NightVision { on: true })), 1);
    assert_ne!(r.ps.weapon_flags & wf::NIGHTVISION, 0);
    r.run(0, 1);
    r.run(button::NIGHTVISION, 1);
    assert!(r.log.contains(&WeaponEvent::NightVision { on: false }));
    assert_eq!(r.ps.weapon_flags & wf::NIGHTVISION, 0);
}

#[test]
fn detonators_trigger_and_idle() {
    let mut d = grenade("c4_mp");
    d.has_detonator = true;
    d.inventory_type = InventoryType::Primary;
    d.offhand_class = OffhandClass::None;
    d.clip_only = false;
    d.detonate_time = 300;
    d.detonate_delay = 100;
    let mut r = Rig::new(vec![d]);
    let w = r.hold("c4_mp");
    r.run(ATTACK, 1);
    assert_eq!((r.state(), r.ps.weapon_time, r.ps.weapon_delay), (ws::DETONATING, 300, 100));
    r.run(0, 9);
    assert_eq!(r.count(|e| matches!(e, WeaponEvent::Detonate { .. })), 0);
    r.run(0, 1);
    assert!(r.log.contains(&WeaponEvent::Detonate { weapon: w }));
    r.run(0, 20);
    assert_eq!(r.state(), ws::READY);
}

fn bolt() -> WeaponInfo {
    WeaponInfo {
        bolt_action: true,
        fire_time: 300,
        rechamber_time: 1000,
        aim_down_sight: true,
        ads_trans_in_time: 100,
        ads_trans_out_time: 100,
        oo_pos_anim_length: [0.01, 0.01],
        clip_size: 5,
        ..rifle("remington700_mp", "bolt")
    }
}

#[test]
fn a_bolt_action_works_the_bolt_after_each_shot() {
    let mut r = Rig::new(vec![bolt()]);
    let w = r.hold("remington700_mp");
    r.run(ATTACK, 1);
    assert_eq!(r.shots(), 1);
    assert!(r.inv.needs_rechamber(w));
    r.run(0, 30);
    assert_eq!(r.state(), ws::READY, "fire time over");
    assert!(r.ps.events.contains(&ev::FIRE_WEAPON));
    r.run(0, 2);
    assert_eq!(r.state(), ws::RECHAMBERING);
    assert!(!r.inv.needs_rechamber(w), "the case is ejected right away");
    r.run(0, 98);
    assert_eq!(r.state(), ws::RECHAMBERING);
    r.run(0, 5);
    assert_eq!(r.state(), ws::READY);
}

#[test]
fn ads_blends_in_and_out_at_the_weapon_rate() {
    let mut r = Rig::new(vec![bolt()]);
    r.hold("remington700_mp");
    r.run(button::ADS, 5);
    assert!((r.ps.weapon_pos_frac - 0.5).abs() < 1e-5, "{}", r.ps.weapon_pos_frac);
    r.run(button::ADS, 20);
    assert_eq!(r.ps.weapon_pos_frac, 1.0);
    r.run(0, 5);
    assert!((r.ps.weapon_pos_frac - 0.5).abs() < 1e-5);
    r.run(0, 10);
    assert_eq!(r.ps.weapon_pos_frac, 0.0);
}

#[test]
fn rechambering_drops_ads_unless_the_weapon_allows_it() {
    for allowed in [false, true] {
        let mut w = bolt();
        w.rechamber_while_ads = allowed;
        let mut r = Rig::new(vec![w]);
        r.hold("remington700_mp");
        r.run(button::ADS, 15);
        assert_eq!(r.ps.weapon_pos_frac, 1.0);
        r.run(ATTACK | button::ADS, 1);
        r.run(button::ADS, 60);
        assert_eq!(r.state(), ws::RECHAMBERING);
        if allowed {
            assert_eq!(r.ps.weapon_pos_frac, 1.0);
        } else {
            assert!(r.ps.weapon_pos_frac < 1.0, "{}", r.ps.weapon_pos_frac);
        }
    }
}

#[test]
fn no_ads_with_an_empty_magazine_when_the_weapon_says_so() {
    let mut w = bolt();
    w.no_ads_when_mag_empty = true;
    let mut r = Rig::new(vec![w]);
    let a = r.hold("remington700_mp");
    r.inv.set_clip(&r.table, a, 0);
    r.inv.set_stock(&r.table, a, 0);
    r.run(button::ADS, 20);
    assert_eq!(r.ps.weapon_pos_frac, 0.0);
    r.inv.set_clip(&r.table, a, 3);
    r.run(button::ADS, 20);
    assert_eq!(r.ps.weapon_pos_frac, 1.0);
}

#[test]
fn ads_fire_only_weapons_wait_for_the_sights() {
    let mut w = bolt();
    w.ads_fire_only = true;
    w.bolt_action = false;
    let mut r = Rig::new(vec![w]);
    r.hold("remington700_mp");
    r.run(ATTACK | button::ADS, 1);
    assert_eq!(r.shots(), 0, "waits for the sights to come up");
    r.run(ATTACK | button::ADS, 12);
    assert_eq!(r.shots(), 1);
    assert!(matches!(r.log[0], WeaponEvent::Fire { ads: true, .. }));
}

fn spread_weapon() -> WeaponInfo {
    WeaponInfo {
        hip_spread_decay_rate: 1.0,
        hip_spread_fire_add: 0.2,
        hip_spread_move_add: 2.0,
        hip_spread_turn_add: 1.0,
        hip_spread_stand_min: 1.0,
        hip_spread_stand_max: 5.0,
        ..rifle("ak47_mp", "ar")
    }
}

#[test]
fn spread_decays_at_the_weapon_rate() {
    let mut r = Rig::new(vec![spread_weapon()]);
    r.hold("ak47_mp");
    r.ps.aim_spread_scale = 255.0;
    r.run(0, 50);
    // 1.0 per second of the 255 range, 10 ms at a time.
    assert!((r.ps.aim_spread_scale - 127.5).abs() < 0.1, "{}", r.ps.aim_spread_scale);
    r.run(0, 100);
    assert_eq!(r.ps.aim_spread_scale, 0.0, "clamped at zero");
}

#[test]
fn firing_widens_the_spread_by_the_fire_add() {
    let mut r = Rig::new(vec![spread_weapon()]);
    r.hold("ak47_mp");
    r.ps.aim_spread_scale = 0.0;
    r.run(ATTACK, 1);
    assert!((r.ps.aim_spread_scale - 51.0).abs() < 1e-3, "{}", r.ps.aim_spread_scale);
    r.run(0, 1);
    assert!((r.ps.aim_spread_scale - 48.45).abs() < 1e-2, "decays again: {}", r.ps.aim_spread_scale);
}

#[test]
fn full_auto_fire_climbs_to_the_maximum_spread() {
    let mut w = spread_weapon();
    w.hip_spread_fire_add = 0.5;
    let mut r = Rig::new(vec![w]);
    r.hold("ak47_mp");
    r.ps.aim_spread_scale = 0.0;
    r.run(ATTACK, 25);
    assert!(r.ps.aim_spread_scale > 200.0 && r.ps.aim_spread_scale <= 255.0, "{}", r.ps.aim_spread_scale);
}

#[test]
fn moving_and_turning_widen_the_spread() {
    let mut r = Rig::new(vec![spread_weapon()]);
    r.hold("ak47_mp");
    r.ps.aim_spread_scale = 0.0;
    r.ps.velocity = [190.0, 0.0, 0.0];
    r.step(0, 127, 10);
    // viewchange = move_add * speed / ps.speed = 2: 2 * 0.01 * 255.
    assert!((r.ps.aim_spread_scale - 5.1).abs() < 0.05, "{}", r.ps.aim_spread_scale);

    let mut r = Rig::new(vec![spread_weapon()]);
    r.hold("ak47_mp");
    r.ps.aim_spread_scale = 0.0;
    r.step(0, 0, 10);
    r.angles[1] += 1820; // ~10 degrees
    r.step(0, 0, 10);
    assert!(r.ps.aim_spread_scale > 24.0 && r.ps.aim_spread_scale < 27.0, "{}", r.ps.aim_spread_scale);
}

#[test]
fn spread_does_not_grow_while_aimed_down_sights() {
    let mut w = spread_weapon();
    w.aim_down_sight = true;
    w.oo_pos_anim_length = [1.0, 1.0];
    let mut r = Rig::new(vec![w]);
    r.hold("ak47_mp");
    r.run(button::ADS, 2);
    assert_eq!(r.ps.weapon_pos_frac, 1.0);
    r.ps.aim_spread_scale = 0.0;
    r.ps.velocity = [190.0, 0.0, 0.0];
    r.step(button::ADS, 127, 10);
    assert_eq!(r.ps.aim_spread_scale, 0.0);
}

#[test]
fn a_jump_in_the_air_keeps_widening_the_spread() {
    let mut r = Rig::new(vec![spread_weapon()]);
    r.hold("ak47_mp");
    r.ps.aim_spread_scale = 0.0;
    r.ps.origin[2] = 200.0;
    r.ps.ground_entity_num = ENTITYNUM_NONE;
    r.step(0, 0, 10);
    // Two axes of 0.01 * 128 per second.
    assert!((r.ps.aim_spread_scale - 6.53).abs() < 0.05, "{}", r.ps.aim_spread_scale);
}

#[test]
fn ammo_use_can_be_sustained() {
    let mut r = Rig::new(vec![ak()]);
    let a = r.hold("ak47_mp");
    let mut params = crate::weapon::WeaponParams::default();
    params.sustain_ammo = true;
    let mut pm = Pmove::new(std::mem::take(&mut r.ps), &r.params);
    pm.weapons = Some(WeaponCtx {
        params,
        ..WeaponCtx::new(&r.table, &mut r.inv)
    });
    pm.cmd = UserCmd {
        buttons: ATTACK,
        weapon: a as u8,
        server_time: pm.ps.command_time + 10,
        ..UserCmd::default()
    };
    pmove(&mut pm, &r.world);
    assert_eq!(pm.weapon_out.events().len(), 1);
    drop(pm);
    assert_eq!(r.clip(a), 30);
}

#[test]
fn pmove_without_weapons_leaves_the_weapon_state_alone() {
    let params = Params::default();
    let world = TestWorld::floor();
    let ps = PlayerState {
        command_time: 10_000,
        weapon: 3,
        weapon_state: ws::FIRING,
        weapon_time: 77,
        ..PlayerState::default()
    };
    let mut pm = Pmove::new(ps, &params);
    pm.cmd = UserCmd {
        buttons: ATTACK,
        weapon: 3,
        server_time: 10_100,
        ..UserCmd::default()
    };
    pmove(&mut pm, &world);
    assert_eq!((pm.ps.weapon_state, pm.ps.weapon_time), (ws::FIRING, 77));
    assert!(pm.weapon_out.is_empty());
}

#[test]
fn outputs_are_cleared_by_the_next_call() {
    let mut r = Rig::new(vec![ak()]);
    r.hold("ak47_mp");
    let mut pm = Pmove::new(std::mem::take(&mut r.ps), &r.params);
    pm.weapons = Some(WeaponCtx::new(&r.table, &mut r.inv));
    pm.cmd = UserCmd {
        buttons: ATTACK,
        weapon: r.want as u8,
        server_time: pm.ps.command_time + 10,
        ..UserCmd::default()
    };
    pmove(&mut pm, &r.world);
    assert_eq!(pm.weapon_out.events().len(), 1);
    pm.cmd.buttons = 0;
    pm.cmd.server_time += 10;
    pmove(&mut pm, &r.world);
    assert!(pm.weapon_out.is_empty());
}
