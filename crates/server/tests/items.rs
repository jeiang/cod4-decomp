// SPDX-License-Identifier: GPL-3.0-only
//! Dropped weapons and live grenades against a hand-built map: dropping, the cap, picking up
//! by walking over and by use, throwing a live grenade back, and what a dying player leaves.

use gsc::{Builtins, Options, Vm, compile};
use server::client::{Session, Team};
use server::combat::{Damage, MOD_RIFLE_BULLET, MOD_SUICIDE};
use server::content::Content;
use server::cvar::Cvars;
use server::game::{EntKind, Game};
use sim::cm::ENTITYNUM_NONE;
use sim::cm::test_support::{BrushSpec, MapSpec};
use sim::contents::{self, SOLID};
use sim::pm::{PLAYER_MAXS, PLAYER_MINS, PmType, UserCmd, button, ev, wf};
use sim::traj::Trajectory;
use sim::weapon::pickup::ItemAmmo;
use sim::weapon::{InventoryType, OffhandClass, WeaponClass, WeaponInfo, WeaponTable, WeaponType};
use sim::world::World;

fn vm() -> Vm {
    let prog = compile(
        &[("t.gsc", "main() {}")],
        &Builtins::stock_mp(),
        Options::default(),
    )
    .unwrap();
    Vm::new(prog).unwrap()
}

fn rifle(name: &str, ammo: &str) -> WeaponInfo {
    WeaponInfo {
        name: name.into(),
        weap_type: WeaponType::Bullet,
        weap_class: WeaponClass::Rifle,
        ammo_name: ammo.into(),
        clip_name: name.into(),
        start_ammo: 90,
        max_ammo: 120,
        clip_size: 30,
        ..WeaponInfo::default()
    }
}

fn frag() -> WeaponInfo {
    WeaponInfo {
        name: "frag_grenade_mp".into(),
        ammo_name: "frag_grenade_mp".into(),
        clip_name: "frag_grenade_mp".into(),
        weap_type: WeaponType::Grenade,
        weap_class: WeaponClass::Grenade,
        inventory_type: InventoryType::Offhand,
        offhand_class: OffhandClass::Frag,
        start_ammo: 1,
        max_ammo: 1,
        clip_size: 1,
        clip_only: true,
        fuse_time: 3500,
        timed_detonation: true,
        ..WeaponInfo::default()
    }
}

fn arena() -> (Game, Vm) {
    let map = MapSpec {
        world: vec![BrushSpec::aabb(
            [-4000.0, -4000.0, -100.0],
            [4000.0, 4000.0, 0.0],
            SOLID,
        )],
        ..Default::default()
    };
    let mut cvars = Cvars::new();
    for (n, d) in [
        ("g_maxDroppedWeapons", "16"),
        ("g_dropForwardSpeed", "10"),
        ("g_dropUpSpeedBase", "10"),
        ("g_dropUpSpeedRand", "5"),
        ("g_dropHorzSpeedRand", "100"),
        ("player_throwbackInnerRadius", "90"),
        ("player_throwbackOuterRadius", "160"),
        ("bg_maxGrenadeIndicatorSpeed", "20"),
        ("perk_grenadeDeath", "frag_grenade_mp"),
    ] {
        cvars.register(n, d, 0);
    }
    let mut g = Game::new(cvars, Content::default());
    g.reset_level(8);
    g.world = Some(World::new(map.build()));
    let mut m16 = rifle("m16_mp", "ar");
    m16.alt_weapon_name = "gl_mp".into();
    let mut gl = rifle("gl_mp", "gl");
    gl.inventory_type = InventoryType::AltMode;
    gl.alt_weapon_name = "m16_mp".into();
    g.weapons = WeaponTable::from_infos(vec![
        m16,
        gl,
        rifle("ak47_mp", "ar"),
        rifle("m4_mp", "ar2"),
        rifle("g3_mp", "ar3"),
        frag(),
    ])
    .unwrap();
    g.level.frametime = 50;
    g.level.time = 1000;
    (g, vm())
}

fn add_player(g: &mut Game, vm: &mut Vm, origin: [f32; 3], team: Team) -> u16 {
    let n = g.connect_client(vm, true, "p").expect("slot");
    g.client_begin(vm, n);
    let c = g.client_mut(n).unwrap();
    c.session = Session::Playing;
    c.team = team;
    c.ps.origin = origin;
    c.ps.viewangles = [0.0, 0.0, 0.0];
    c.ps.pm_type = PmType::Normal;
    c.ps.view_height_current = 60.0;
    c.ps.client_num = n;
    let e = g.ent_mut(n).unwrap();
    e.origin = origin;
    e.health = 100;
    e.takedamage = true;
    e.mins = PLAYER_MINS;
    e.maxs = PLAYER_MAXS;
    g.set_client_contents(n);
    g.relink(n);
    g.calls.clear();
    n
}

fn give(g: &mut Game, n: u16, name: &str) -> u16 {
    let w = g.weapons.index(name);
    let c = &mut g.clients[usize::from(n)];
    assert!(c.inv.give(&g.weapons, &mut c.ps, w, 0));
    w
}

/// Lets a dropped weapon fall and land.
fn land(g: &mut Game, vm: &mut Vm, item: u16) {
    for _ in 0..200 {
        g.level.time += 50;
        g.run_entity(vm, item);
        if g.ent(item)
            .is_none_or(|e| e.mv.pos.tr.kind == sim::traj::TrType::Stationary)
        {
            return;
        }
    }
    panic!("the item never landed");
}

fn items(g: &Game) -> Vec<u16> {
    g.in_use()
        .filter(|(_, e)| e.item.is_some())
        .map(|(n, _)| n)
        .collect()
}

fn missiles(g: &Game) -> Vec<u16> {
    g.in_use()
        .filter(|(_, e)| e.missile.is_some())
        .map(|(n, _)| n)
        .collect()
}

#[test]
fn a_dropped_weapon_is_an_item_everyone_is_told_about_and_it_carries_the_ammunition() {
    let (mut g, mut vm) = arena();
    let a = add_player(&mut g, &mut vm, [0.0; 3], Team::Allies);
    let w = give(&mut g, a, "ak47_mp");
    let ak_clip = g.client(a).unwrap().inv.clip(&g.weapons, w);
    let item = g
        .drop_weapon(&mut vm, a, w, 0)
        .expect("something was dropped");
    assert!(
        !g.client(a).unwrap().inv.has(w),
        "the weapon left the player"
    );
    let e = g.ent(item).unwrap();
    assert_eq!(e.kind, EntKind::Item);
    assert_eq!(
        e.contents,
        contents::ITEM | 0x405C_0008 | contents::USE,
        "an item is touched like a trigger and used"
    );
    assert_eq!(e.item.as_ref().unwrap().ammo[0].clip, ak_clip);
    land(&mut g, &mut vm, item);
    let seen = server::netsv::world_entities(&g);
    assert!(
        seen.iter()
            .any(|s| s.number == item && s.etype == net::entity::etype::ITEM && s.weapon == w),
        "the clients are shown the weapon"
    );
}

#[test]
fn walking_over_a_weapon_feeds_the_one_owned_and_using_it_takes_a_new_one() {
    let (mut g, mut vm) = arena();
    let a = add_player(&mut g, &mut vm, [0.0; 3], Team::Allies);
    let b = add_player(&mut g, &mut vm, [500.0, 0.0, 0.0], Team::Allies);
    let ak = give(&mut g, a, "ak47_mp");
    let item = g.drop_weapon(&mut vm, a, ak, 0).unwrap();
    land(&mut g, &mut vm, item);
    // The item is full of ammunition the dropper had.
    let held = g.ent(item).unwrap().item.as_ref().unwrap().ammo[0];
    assert!(held.clip + held.stock > 0);
    // B has none of it: walking over does nothing, using takes it with its ammunition.
    g.level.time += 2000;
    g.run_entity(&mut vm, item);
    g.touch_item(&mut vm, b, item, true);
    assert!(
        g.ent(item).is_some(),
        "walking over a weapon not owned leaves it"
    );
    g.touch_item(&mut vm, b, item, false);
    assert!(g.ent(item).is_none(), "taking the weapon uses the item up");
    let c = g.client(b).unwrap();
    assert!(c.inv.has(ak));
    assert_eq!(c.inv.clip(&g.weapons, ak), held.clip);
    assert_eq!(c.inv.stock(&g.weapons, ak), held.stock);
    let last = c.ps.events[usize::from(c.ps.event_sequence.wrapping_sub(1) & 3)];
    assert_eq!(
        last,
        ev::ITEM_PICKUP,
        "the pickup is announced to the clients"
    );

    // A second weapon of the same ammunition, dropped for B to run over.
    let item = g
        .launch_item(
            &mut vm,
            ak,
            0,
            [
                ItemAmmo {
                    weapon: ak,
                    clip: 10,
                    stock: 50,
                },
                ItemAmmo::NONE,
            ],
            [500.0, 0.0, 5.0],
            [0.0; 3],
            [0.0; 3],
            None,
        )
        .unwrap();
    g.level.time += 2000;
    let before = g.client(b).unwrap().inv.stock(&g.weapons, ak);
    let room = g.client(b).unwrap().inv.ammo_player_max(&g.weapons, ak, 0) - before;
    g.touch_item(&mut vm, b, item, true);
    let after = g.client(b).unwrap().inv.stock(&g.weapons, ak);
    assert_eq!(after - before, room.min(60).max(0));
}

#[test]
fn the_player_who_dropped_a_weapon_cannot_take_it_back_until_a_second_has_passed() {
    let (mut g, mut vm) = arena();
    let a = add_player(&mut g, &mut vm, [0.0; 3], Team::Allies);
    let ak = give(&mut g, a, "ak47_mp");
    let item = g.drop_weapon(&mut vm, a, ak, 0).unwrap();
    land(&mut g, &mut vm, item);
    g.level.time = 1100;
    g.run_entity(&mut vm, item);
    g.touch_item(&mut vm, a, item, false);
    assert!(g.ent(item).is_some(), "too soon");
    g.level.time = 2200;
    g.run_entity(&mut vm, item);
    g.touch_item(&mut vm, a, item, false);
    assert!(g.ent(item).is_none());
    assert!(g.client(a).unwrap().inv.has(ak));
}

#[test]
fn only_g_maxdroppedweapons_stay_on_the_floor_and_the_farthest_goes() {
    let (mut g, mut vm) = arena();
    g.cvars.set("g_maxDroppedWeapons", "2");
    let a = add_player(&mut g, &mut vm, [0.0; 3], Team::Allies);
    let ak = g.weapons.index("ak47_mp");
    let at = |x: f32| {
        [
            ItemAmmo {
                weapon: ak,
                clip: 5,
                stock: 5,
            },
            ItemAmmo::NONE,
        ]
        .map(|a| (a, x))
    };
    let mut made = Vec::new();
    for x in [3000.0, 100.0, 200.0] {
        let i = g
            .launch_item(
                &mut vm,
                ak,
                0,
                at(x).map(|p| p.0),
                [x, 0.0, 5.0],
                [0.0; 3],
                [0.0; 3],
                None,
            )
            .unwrap();
        made.push(i);
    }
    let _ = a;
    let left = items(&g);
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(
        left.iter().all(|&i| g.ent(i).unwrap().origin[0] < 1000.0),
        "the one farthest from the players was freed"
    );
}

#[test]
fn a_live_frag_at_rest_shows_a_hint_and_the_grenade_key_takes_it_to_throw_back() {
    let (mut g, mut vm) = arena();
    let thrower = add_player(&mut g, &mut vm, [-300.0, 0.0, 0.0], Team::Axis);
    let b = add_player(&mut g, &mut vm, [0.0; 3], Team::Allies);
    let frag_w = g.weapons.index("frag_grenade_mp");
    let nade = g
        .launch_grenade(
            &mut vm,
            thrower,
            frag_w,
            [70.0, 0.0, 10.0],
            [0.0; 3],
            [0.0; 3],
            false,
            3000,
        )
        .unwrap();
    // At rest, as it is a moment after it landed.
    {
        let e = g.ent_mut(nade).unwrap();
        e.origin = [70.0, 0.0, 10.0];
        e.missile.as_mut().unwrap().pos = Trajectory::stationary([70.0, 0.0, 10.0]);
    }
    g.relink(nade);
    g.update_cursor_hints(b);
    let ps = &g.client(b).unwrap().ps;
    assert_eq!(ps.cursor_hint_ent_index, nade);
    assert_eq!(
        u16::from(ps.cursor_hint),
        frag_w + u16::from(sim::weapon::pickup::WEAPON_HINT_OFFSET)
    );
    assert!(ps.throw_back_grenade_time_left > 0);
    let left = ps.throw_back_grenade_time_left;

    g.attempt_live_grenade_pickup(&mut vm, b);
    assert!(g.ent(nade).is_none(), "the grenade is in the player's hand");
    let ps = &g.client(b).unwrap().ps;
    assert_eq!(
        ps.throw_back_grenade_owner, thrower,
        "the credit stays with the first thrower"
    );
    assert_eq!(ps.grenade_time_left, left, "the fuse keeps burning");
}

#[test]
fn a_live_frag_that_is_still_flying_cannot_be_grabbed() {
    let (mut g, mut vm) = arena();
    let thrower = add_player(&mut g, &mut vm, [-300.0, 0.0, 0.0], Team::Axis);
    let b = add_player(&mut g, &mut vm, [0.0; 3], Team::Allies);
    let frag_w = g.weapons.index("frag_grenade_mp");
    g.launch_grenade(
        &mut vm,
        thrower,
        frag_w,
        [70.0, 0.0, 10.0],
        [300.0, 0.0, 0.0],
        [0.0; 3],
        false,
        3000,
    )
    .unwrap();
    g.update_cursor_hints(b);
    assert_eq!(
        g.client(b).unwrap().ps.cursor_hint_ent_index,
        ENTITYNUM_NONE
    );
}

#[test]
fn a_player_killed_while_cooking_a_grenade_drops_it_with_the_fuse_left() {
    let (mut g, mut vm) = arena();
    let a = add_player(&mut g, &mut vm, [0.0; 3], Team::Allies);
    let frag_w = g.weapons.index("frag_grenade_mp");
    {
        let c = g.client_mut(a).unwrap();
        c.ps.grenade_time_left = 1800;
        c.ps.weapon_flags |= wf::USING_OFFHAND;
        c.ps.offhand_index = frag_w;
    }
    g.ent_mut(a).unwrap().health = 0;
    let now = g.level.time;
    g.player_die(&mut vm, a, &Damage::new(200, MOD_RIFLE_BULLET), 200);
    let m = missiles(&g);
    assert_eq!(m.len(), 1, "the cooked grenade is not lost");
    let think = g.ent(m[0]).unwrap().missile.as_ref().unwrap().next_think;
    assert_eq!(think, now + 1800);
}

#[test]
fn martyrdom_leaves_a_grenade_unless_the_player_killed_themselves() {
    let (mut g, mut vm) = arena();
    for suicide in [false, true] {
        let a = add_player(&mut g, &mut vm, [0.0; 3], Team::Allies);
        g.client_mut(a).unwrap().ps.perks |= 0x40;
        g.ent_mut(a).unwrap().health = 0;
        let before = missiles(&g).len();
        let mean = if suicide {
            MOD_SUICIDE
        } else {
            MOD_RIFLE_BULLET
        };
        g.player_die(&mut vm, a, &Damage::new(200, mean), 200);
        let after = missiles(&g).len();
        assert_eq!(after - before, usize::from(!suicide), "suicide {suicide}");
    }
}

fn drop_at(g: &mut Game, vm: &mut Vm, weapon: u16, x: f32, ammo: ItemAmmo) -> u16 {
    g.launch_item(
        vm,
        weapon,
        0,
        [ammo, ItemAmmo::NONE],
        [x, 0.0, 2.0],
        [0.0; 3],
        [0.0; 3],
        None,
    )
    .unwrap()
}

#[test]
fn using_a_weapon_with_both_primaries_full_swaps_it_for_the_one_in_hand_alt_mode_and_all() {
    let (mut g, mut vm) = arena();
    let a = add_player(&mut g, &mut vm, [0.0; 3], Team::Allies);
    let m16 = give(&mut g, a, "m16_mp");
    let ak = give(&mut g, a, "ak47_mp");
    let gl = g.weapons.index("gl_mp");
    let g3 = g.weapons.index("g3_mp");
    assert!(
        g.client(a).unwrap().inv.has(gl),
        "the launcher comes with the rifle"
    );
    // Holding the launcher mode of the rifle.
    g.client_mut(a).unwrap().ps.weapon = u32::from(gl);
    let stock_before = {
        let c = g.client(a).unwrap();
        c.inv.stock(&g.weapons, m16)
    };
    let item = drop_at(
        &mut g,
        &mut vm,
        g3,
        50.0,
        ItemAmmo {
            weapon: g3,
            clip: 10,
            stock: 50,
        },
    );
    g.level.time += 2000;
    g.touch_item(&mut vm, a, item, false);
    assert!(g.ent(item).is_none(), "the weapon on the floor was taken");
    let c = g.client(a).unwrap();
    assert!(c.inv.has(g3) && c.inv.has(ak));
    assert!(
        !c.inv.has(m16) && !c.inv.has(gl),
        "the rifle and its launcher were put down"
    );
    assert_eq!(
        (c.inv.clip(&g.weapons, g3), c.inv.stock(&g.weapons, g3)),
        (10, 50)
    );
    assert_eq!(c.inv.selected(), g3, "the new weapon is raised");
    let swapped = items(&g);
    assert_eq!(swapped.len(), 1, "{swapped:?}");
    let left = g.ent(swapped[0]).unwrap().item.as_ref().unwrap();
    assert_eq!(left.weapon, m16);
    assert_eq!(left.ammo[1].weapon, gl, "the alternate mode goes with it");
    // The ammunition that does not fit the player's other weapons stayed on the dropped one;
    // none was made or lost.
    let c = g.client(a).unwrap();
    assert_eq!(
        c.inv.stock(&g.weapons, ak) + left.ammo[0].stock,
        stock_before,
        "the shared reserve is split between the player and the floor"
    );
}

#[test]
fn walking_over_a_weapon_in_the_frame_feeds_the_owned_one() {
    let (mut g, mut vm) = arena();
    let a = add_player(&mut g, &mut vm, [0.0; 3], Team::Allies);
    let ak = give(&mut g, a, "ak47_mp");
    {
        let c = &mut g.clients[usize::from(a)];
        c.inv.set_stock(&g.weapons, ak, 10);
    }
    let item = drop_at(
        &mut g,
        &mut vm,
        ak,
        10.0,
        ItemAmmo {
            weapon: ak,
            clip: 0,
            stock: 40,
        },
    );
    let cmd = UserCmd {
        server_time: g.level.time + 50,
        ..UserCmd::default()
    };
    g.client_think(&mut vm, a, cmd);
    g.touch_triggers(&mut vm, a);
    let c = g.client(a).unwrap();
    assert_eq!(c.inv.stock(&g.weapons, ak), 50);
    assert!(g.ent(item).is_none(), "the weapon on the floor was used up");
}

#[test]
fn the_use_key_takes_the_weapon_the_hint_names() {
    let (mut g, mut vm) = arena();
    let b = add_player(&mut g, &mut vm, [0.0; 3], Team::Allies);
    let g3 = g.weapons.index("g3_mp");
    let item = drop_at(
        &mut g,
        &mut vm,
        g3,
        50.0,
        ItemAmmo {
            weapon: g3,
            clip: 12,
            stock: 34,
        },
    );
    g.update_cursor_hints(b);
    assert_eq!(g.client(b).unwrap().ps.cursor_hint_ent_index, item);
    let cmd = UserCmd {
        server_time: g.level.time + 50,
        buttons: button::USE,
        ..UserCmd::default()
    };
    g.client_think(&mut vm, b, cmd);
    assert!(g.ent(item).is_none());
    let c = g.client(b).unwrap();
    assert_eq!(
        (c.inv.clip(&g.weapons, g3), c.inv.stock(&g.weapons, g3)),
        (12, 34)
    );
}

#[test]
fn a_freed_item_leaves_the_drop_cue() {
    let (mut g, mut vm) = arena();
    let ak = g.weapons.index("ak47_mp");
    let item = drop_at(&mut g, &mut vm, ak, 50.0, ItemAmmo::NONE);
    assert_eq!(g.dropped, vec![item]);
    g.free_entity(&mut vm, item);
    assert!(g.dropped.is_empty());
}

#[test]
fn a_flying_frag_is_networked_with_its_weapon_and_heading_and_its_blast_with_the_weapon() {
    let (mut g, mut vm) = arena();
    let thrower = add_player(&mut g, &mut vm, [-300.0, 0.0, 0.0], Team::Axis);
    let frag_w = g.weapons.index("frag_grenade_mp");
    let nade = g
        .launch_grenade(
            &mut vm,
            thrower,
            frag_w,
            [0.0, 0.0, 50.0],
            [300.0, 0.0, 0.0],
            [0.0; 3],
            false,
            3000,
        )
        .unwrap();
    let state = server::netsv::world_entities(&g)
        .into_iter()
        .find(|s| s.number == nade)
        .unwrap();
    assert_eq!(state.etype, net::entity::etype::MISSILE);
    assert_eq!(state.weapon, frag_w);
    assert_eq!(
        state.eflags,
        g.ent(nade).unwrap().missile.as_ref().unwrap().launch_time as u32 & 0xff_ffff,
        "the launch time gates drawing"
    );
    assert!(state.velocity[0] > 200.0, "heading: {:?}", state.velocity);

    g.detonate_missile(&mut vm, nade);
    let blasts: Vec<_> = server::netsv::world_entities(&g)
        .into_iter()
        .filter(|s| {
            s.etype == net::entity::etype::EVENT && s.event == server::tempev::ev::EXPLOSION
        })
        .collect();
    assert_eq!(blasts.len(), 1);
    assert_eq!(blasts[0].weapon, frag_w);
}
