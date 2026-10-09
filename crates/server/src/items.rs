// SPDX-License-Identifier: GPL-3.0-only
// Translated from KisakCOD (https://github.com/Kisak-COD/KisakCOD, GPL-3.0): `game/g_items.cpp`,
// `game_mp/g_active_mp.cpp`, `game_mp/g_combat_mp.cpp`; the code is the original game's,
// copyright Activision Publishing and the KisakCOD contributors.
//! Dropped weapons and live grenades on the server: dropping, the cap on how many lie about,
//! walking over or using one, throwing a live grenade back, and the grenades a dying player
//! leaves. The rules over the inventory are in [`sim::weapon::pickup`].

use crate::client::Session;
use crate::game::{Ent, EntKind, Game, TRIGGER_HURT_CONTENTS};
use gsc::{Value, Vm};
use net::ui::{PrintKind, ServerCmd};
use sim::Vec3;
use sim::cm::{ENTITYNUM_NONE, ENTITYNUM_WORLD};
use sim::contents;
use sim::pm::{PmType, ev, wf};
use sim::traj::{TrType, Trajectory};
use sim::weapon::pickup::{self, ITEM_SLOTS, ItemAmmo, Pickup};
use sim::weapon::{InventoryType, OffhandClass, WeaponType};

/// How long the player who dropped a weapon cannot take it back (`DroppedItemClearOwner`).
const DROPPER_LOCK_MS: i32 = 1000;
/// `G_ItemClipMask`: what a dropped weapon lands on.
const ITEM_CLIPMASK: i32 = 1169;

/// What a weapon lying in the world holds (`gentity_s::item[2]`, `s.index`, `s.clientNum`).
#[derive(Debug, Clone)]
pub struct DroppedItem {
    pub weapon: u16,
    /// The model variant (`weaponmodels`).
    pub model: u8,
    pub ammo: [ItemAmmo; ITEM_SLOTS],
    /// Who dropped it a moment ago, who may not take it back yet.
    pub dropper: Option<u16>,
    /// Level time the dropper is let off.
    pub dropper_clear: i32,
    /// `FL_WEAPON_BEING_GRABBED`: not the one freed to make room.
    pub being_grabbed: bool,
}

fn dist2(a: Vec3, b: Vec3) -> f32 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum()
}

impl Game {
    fn crandom(&mut self) -> f32 {
        self.random_f32() * 2.0 - 1.0
    }

    fn weapon_world_model(&self, weapon: u16, model: u8) -> Option<String> {
        let def = self.content.weapon(self.weapons.name(weapon))?;
        let m = def
            .world_models
            .get(usize::from(model))
            .cloned()
            .flatten()
            .or_else(|| def.world_models.first().cloned().flatten())?;
        m.name.as_deref().map(str::to_owned)
    }

    /// `GetFreeDropCueIdx`: with `g_maxDroppedWeapons` on the floor, the one farthest from every
    /// player in the match goes.
    fn make_room_for_drop(&mut self, vm: &mut Vm) {
        let cap = self.cvars.int("g_maxDroppedWeapons").clamp(1, 32) as usize;
        let ents = &self.ents;
        self.dropped.retain(|&d| {
            ents.get(usize::from(d))
                .and_then(Option::as_ref)
                .is_some_and(|e| e.item.is_some())
        });
        while self.dropped.len() >= cap {
            let players: Vec<Vec3> = self
                .connected_clients()
                .filter(|(_, c)| c.session == Session::Playing)
                .filter_map(|(n, _)| self.ent(n).map(|e| e.origin))
                .collect();
            let mut best: Option<(usize, f32)> = None;
            for (i, &d) in self.dropped.iter().enumerate() {
                let Some(e) = self.ent(d) else { continue };
                let Some(item) = e.item.as_ref() else {
                    continue;
                };
                if item.being_grabbed || self.weapons.info(item.weapon).avoid_drop_cleanup {
                    continue;
                }
                let near = players
                    .iter()
                    .map(|&p| dist2(e.origin, p))
                    .fold(9.999_98e11_f32, f32::min);
                if best.is_none_or(|(_, b)| near > b) {
                    best = Some((i, near));
                }
            }
            let i = best.map_or_else(
                || {
                    self.print(format!(
                        "Could not find a suitable weapon entity to free out of {cap} possible.  Using index zero.\n"
                    ));
                    0
                },
                |(i, _)| i,
            );
            let d = self.dropped.remove(i);
            self.free_entity(vm, d);
        }
    }

    /// `LaunchItem`: a weapon thrown into the world at `origin` with `velocity`.
    #[allow(clippy::too_many_arguments)]
    pub fn launch_item(
        &mut self,
        vm: &mut Vm,
        weapon: u16,
        model: u8,
        ammo: [ItemAmmo; ITEM_SLOTS],
        origin: Vec3,
        angles: Vec3,
        velocity: Vec3,
        dropper: Option<u16>,
    ) -> Result<u16, String> {
        self.make_room_for_drop(vm);
        let mut e = Ent::new(
            EntKind::Item,
            &format!("weapon_{}", self.weapons.name(weapon)),
        );
        if let Some(m) = self.weapon_world_model(weapon, model) {
            e.model = m.into();
        }
        e.origin = origin;
        e.angles = angles;
        e.mins = [-1.0; 3];
        e.maxs = [1.0; 3];
        e.contents = contents::ITEM | TRIGGER_HURT_CONTENTS | contents::USE;
        e.mv.pos.tr = Trajectory {
            kind: TrType::Gravity,
            time: self.level.time,
            duration: 0,
            base: origin,
            delta: velocity,
        };
        e.item = Some(Box::new(DroppedItem {
            weapon,
            model,
            ammo,
            dropper,
            dropper_clear: self.level.time + DROPPER_LOCK_MS,
            being_grabbed: false,
        }));
        let n = self.spawn(e)?;
        self.dropped.push(n);
        self.relink(n);
        Ok(n)
    }

    /// `Drop_Weapon` for a player: the weapon (the base weapon of an alternate fire mode) leaves
    /// the inventory with its ammunition for the floor. Nothing is left on the floor when the
    /// player has no ammunition to leave; the weapon is taken either way.
    pub fn drop_weapon(&mut self, vm: &mut Vm, n: u16, weapon: u16, model: u8) -> Option<u16> {
        let mut weapon = weapon;
        let info = self.weapons.info(weapon);
        if info.inventory_type == InventoryType::AltMode {
            let alt = info.alt_weapon;
            if alt == 0 {
                self.print(format!(
                    "Drop_Weapon(): Trying to drop alt-type weapon, \"{}\", but it has no corresponding altWeapon set.\n",
                    self.weapons.name(weapon)
                ));
                return None;
            }
            weapon = alt;
        }
        let c = self.clients.get(usize::from(n))?;
        let clip_only_empty =
            self.weapons.info(weapon).clip_only && c.inv.clip(&self.weapons, weapon) == 0;
        let leaves_something = c.inv.has(weapon)
            && pickup::has_ammo_to_transfer(&c.inv, &self.weapons, weapon)
            && !clip_only_empty;
        let ammo = pickup::transfer_from_player(&c.inv, &self.weapons, weapon);
        let take = |g: &mut Game| {
            let c = &mut g.clients[usize::from(n)];
            c.inv.take(&g.weapons, &mut c.ps, weapon, true);
        };
        if !leaves_something {
            take(self);
            return None;
        }
        let player = self.ent(n)?;
        let (po, yaw, height) = (
            player.origin,
            player.angles[1],
            player.maxs[2] - player.mins[2],
        );
        let origin = [po[0], po[1], po[2] + height * 0.5];
        let (fwd, up) = (
            self.cvars.float("g_dropForwardSpeed"),
            self.cvars.float("g_dropUpSpeedBase"),
        );
        let (up_rand, horz) = (
            self.cvars.float("g_dropUpSpeedRand"),
            self.cvars.float("g_dropHorzSpeedRand"),
        );
        let (s, c) = yaw.to_radians().sin_cos();
        let velocity = [
            self.crandom() * horz + c * fwd,
            self.crandom() * horz + s * fwd,
            self.crandom() * up_rand + up,
        ];
        let item = self
            .launch_item(
                vm,
                weapon,
                model,
                ammo,
                origin,
                [0.0, yaw, 0.0],
                velocity,
                Some(n),
            )
            .ok();
        take(self);
        item
    }

    /// `G_SpawnItem`: a `weapon_*` entity of the map or of `spawn()` holds what such a weapon
    /// holds when nobody owned it.
    pub fn init_item(&mut self, n: u16) {
        let Some(e) = self.ent(n) else { return };
        let weapon = self
            .weapons
            .index(e.classname.strip_prefix("weapon_").unwrap_or(""));
        if weapon == 0 {
            return;
        }
        let rolls: [i32; 4] = std::array::from_fn(|_| (self.rand() & 0x7FFF) as i32);
        let mut rolls = rolls.into_iter();
        let ammo = pickup::transfer_random(&self.weapons, weapon, || rolls.next().unwrap_or(0));
        let model = self.weapon_world_model(weapon, 0);
        let Some(e) = self.ent_mut(n) else { return };
        if let Some(m) = model {
            e.model = m.into();
        }
        e.mins = [-1.0; 3];
        e.maxs = [1.0; 3];
        e.contents = contents::ITEM | TRIGGER_HURT_CONTENTS | contents::USE;
        e.item = Some(Box::new(DroppedItem {
            weapon,
            model: 0,
            ammo,
            dropper: None,
            dropper_clear: 0,
            being_grabbed: false,
        }));
    }

    /// The part of `G_RunItem` that is not falling: the dropper is let off after a second.
    pub fn item_think(&mut self, n: u16) {
        let now = self.level.time;
        if let Some(item) = self.ent_mut(n).and_then(|e| e.item.as_mut())
            && item.dropper.is_some()
            && now >= item.dropper_clear
        {
            item.dropper = None;
        }
    }

    /// The mask a dropped weapon falls against.
    pub const fn item_clipmask() -> i32 {
        ITEM_CLIPMASK
    }

    fn pickup_message(&mut self, n: u16, key: &str, weapon: u16) {
        let name = self
            .content
            .weapon(self.weapons.name(weapon))
            .and_then(|d| d.display_name.as_deref().map(str::to_owned))
            .unwrap_or_default();
        let text = if name.is_empty() {
            key.to_owned()
        } else {
            format!("{key}\x15{name}")
        };
        self.send(
            crate::ui::Dest::Client(n),
            ServerCmd::Print {
                kind: PrintKind::Normal,
                text,
            },
        );
    }

    fn notify_pickup(&mut self, vm: &mut Vm, item: u16, player: u16, dropped: Option<u16>) {
        let who = self.entity_value(vm, player);
        let other = dropped.map_or(Value::Undefined, |d| self.entity_value(vm, d));
        vm.notify_entity(item, "trigger", &[who, other]);
    }

    /// `Touch_Item`: the player walked over (`touched`) or used the weapon or live grenade `t`.
    pub fn touch_item(&mut self, vm: &mut Vm, n: u16, t: u16, touched: bool) {
        let Some(c) = self.client(n) else { return };
        if self.ent(n).is_none_or(|e| e.health < 1) {
            return;
        }
        let Some(te) = self.ent(t) else { return };
        let (kind, weapon, dropper) = if let Some(item) = te.item.as_ref() {
            (Pickup::Dropped, item.weapon, item.dropper)
        } else if let Some(m) = te.missile.as_ref()
            && m.info.offhand_class == OffhandClass::Frag
        {
            (Pickup::LiveGrenade, m.weapon, None)
        } else {
            return;
        };
        if !pickup::can_item_be_grabbed(
            &c.inv,
            &self.weapons,
            &c.ps,
            kind,
            weapon,
            dropper,
            touched,
        ) {
            if !touched && dropper != Some(n) {
                let owned = c.inv.has(weapon);
                let key = if owned {
                    "GAME_PICKUP_CANTCARRYMOREAMMO"
                } else {
                    "GAME_CANT_GET_PRIMARY_WEAP_MESSAGE"
                };
                self.pickup_message(n, key, if owned { weapon } else { 0 });
            }
            return;
        }
        let (picked, event) = match kind {
            Pickup::LiveGrenade => {
                self.notify_pickup(vm, t, n, None);
                (true, ev::AMMO_PICKUP)
            }
            Pickup::Dropped if touched => self.pickup_touch(vm, n, t, weapon),
            Pickup::Dropped => self.pickup_grab(vm, n, t, weapon),
        };
        if event != ev::NONE
            && let Some(c) = self.client_mut(n)
        {
            c.ps.add_event(event, u32::from(weapon));
        }
        if picked {
            if kind == Pickup::LiveGrenade {
                vm.notify_entity(t, "death", &[]);
            }
            self.free_entity(vm, t);
        }
    }

    /// `WeaponPickup_Touch`: walking over a weapon feeds the ones the player has.
    fn pickup_touch(&mut self, vm: &mut Vm, n: u16, t: u16, weapon: u16) -> (bool, u8) {
        let Some(mut item) = self.ent(t).and_then(|e| e.item.clone()) else {
            return (false, ev::NONE);
        };
        let c = &mut self.clients[usize::from(n)];
        let exact = c.inv.has(weapon);
        if !exact && !c.inv.has_compatible(&self.weapons, weapon) {
            return (false, ev::NONE);
        }
        let model = item.model;
        let got = pickup::leech_from_item(
            &mut c.inv,
            &self.weapons,
            &mut c.ps,
            &mut item.ammo,
            |_| model,
            exact,
        );
        if let Some(e) = self.ent_mut(t) {
            e.item = Some(item);
        }
        if !got.took_any {
            return (false, ev::NONE);
        }
        if self.cvars.bool("pickupPrints") {
            for w in got.weapons.into_iter().filter(|&w| w != 0) {
                let key = if self.weapons.info(w).clip_only {
                    "GAME_PICKUP_CLIPONLY_AMMO"
                } else {
                    "GAME_PICKUP_AMMO"
                };
                self.pickup_message(n, key, w);
            }
        }
        self.notify_pickup(vm, t, n, None);
        (got.used_up, ev::ITEM_PICKUP)
    }

    /// `WeaponPickup_Grab` for a weapon on the floor: the player takes it, swapping out the
    /// primary in hand when both primary slots are full.
    fn pickup_grab(&mut self, vm: &mut Vm, n: u16, t: u16, weapon: u16) -> (bool, u8) {
        let Some(item) = self.ent(t).and_then(|e| e.item.clone()) else {
            return (false, ev::NONE);
        };
        let info = self.weapons.info(weapon);
        let (primary, grenade_like) = (
            info.inventory_type == InventoryType::Primary,
            info.weap_type == WeaponType::Grenade && info.offhand_class != OffhandClass::None,
        );
        let held = self.clients[usize::from(n)].ps.weapon as u16;
        let mut dropped = None;
        if let Some(e) = self.ent_mut(t)
            && let Some(i) = e.item.as_mut()
        {
            i.being_grabbed = true;
        }
        if primary {
            let c = &self.clients[usize::from(n)];
            if held != 0 && !c.inv.has(held) {
                return self.grab_done(t, false);
            }
            if c.inv.primaries_full(&self.weapons) {
                let cur = pickup::current_primary(&c.inv, &self.weapons, held);
                if cur == 0 {
                    self.pickup_message(n, "GAME_CANT_GET_PRIMARY_WEAP_MESSAGE", 0);
                    return self.grab_done(t, false);
                }
                let model = c.inv.model(cur);
                dropped = self.drop_weapon(vm, n, cur, model);
                if let Some(d) = dropped {
                    // The old weapon lies where the new one did.
                    let (at, angles, flags) = self.ent(t).map_or(([0.0; 3], [0.0; 3], 0), |e| {
                        (e.origin, e.angles, e.spawnflags)
                    });
                    if let Some(de) = self.ent_mut(d) {
                        de.origin = at;
                        de.angles = angles;
                        de.spawnflags = flags & !1;
                        de.mv.pos.tr = Trajectory::stationary(at);
                    }
                    self.relink(d);
                }
            }
        }
        let model = item.model;
        let c = &mut self.clients[usize::from(n)];
        c.inv.give_raw(&self.weapons, &mut c.ps, weapon, model);
        pickup::add_ammo_for_new_weapon(&mut c.inv, &self.weapons, &mut c.ps, &item.ammo, |_| {
            model
        });
        if let Some(d) = dropped
            && let Some(mut di) = self.ent(d).and_then(|e| e.item.clone())
        {
            let c = &mut self.clients[usize::from(n)];
            pickup::leech_from_item(
                &mut c.inv,
                &self.weapons,
                &mut c.ps,
                &mut di.ammo,
                |_| model,
                false,
            );
            if let Some(e) = self.ent_mut(d) {
                e.item = Some(di);
            }
        }
        self.notify_pickup(vm, t, n, dropped);
        if !grenade_like {
            self.clients[usize::from(n)].inv.select(weapon);
        }
        self.grab_done(t, true)
    }

    fn grab_done(&mut self, t: u16, taken: bool) -> (bool, u8) {
        if let Some(i) = self.ent_mut(t).and_then(|e| e.item.as_mut()) {
            i.being_grabbed = false;
        }
        (taken, if taken { ev::ITEM_PICKUP } else { ev::NONE })
    }

    /// `AttemptLiveGrenadePickup`: the grenade key with a live grenade under the crosshair takes
    /// the grenade (with the fuse it has left) to throw it back.
    pub fn attempt_live_grenade_pickup(&mut self, vm: &mut Vm, n: u16) {
        let Some(c) = self.client(n) else { return };
        let g = c.ps.cursor_hint_ent_index;
        if g == ENTITYNUM_NONE
            || !self
                .ent(g)
                .and_then(|e| e.missile.as_ref())
                .is_some_and(|m| m.info.offhand_class == OffhandClass::Frag)
        {
            return;
        }
        let left = c.ps.throw_back_grenade_time_left;
        if left == 0 {
            return;
        }
        let parent = self
            .ent(g)
            .and_then(|e| e.missile.as_ref())
            .and_then(|m| m.parent)
            .unwrap_or(ENTITYNUM_WORLD);
        if let Some(c) = self.client_mut(n) {
            c.ps.throw_back_grenade_owner = parent;
            c.ps.grenade_time_left = left;
        }
        self.touch_item(vm, n, g, false);
    }

    /// `DeathGrenadeDrop`: a grenade being cooked goes off where its holder falls, and the
    /// martyrdom perk leaves another one.
    pub fn death_grenade_drop(&mut self, vm: &mut Vm, n: u16, suicide: bool) {
        let Some(c) = self.client(n) else { return };
        let (left, offhand, held, perks) = (
            c.ps.grenade_time_left,
            c.ps.weapon_flags & wf::USING_OFFHAND != 0,
            c.ps.weapon as u16,
            c.ps.perks,
        );
        let origin = self.ent(n).map_or([0.0; 3], |e| e.origin);
        let spot = [origin[0], origin[1], origin[2] + 40.0];
        if left != 0 {
            let weapon = if offhand {
                self.clients[usize::from(n)].ps.offhand_index
            } else {
                held
            };
            let toss = [
                self.crandom() * 160.0,
                self.crandom() * 160.0,
                self.crandom() * 160.0,
            ];
            if let Err(e) = self.launch_grenade(vm, n, weapon, spot, toss, [0.0; 3], true, left) {
                self.print(format!("G_FireGrenade: {e}\n"));
            }
            self.clients[usize::from(n)].ps.grenade_time_left = 0;
        }
        if !suicide && perks & 0x40 != 0 {
            let name = self.cvars.string("perk_grenadeDeath").to_owned();
            let weapon = self.weapons.index(&name);
            if weapon == 0 {
                self.print(format!("Unknown perk_grenadeDeath grenade: {name}\n"));
                return;
            }
            let fuse = self.weapons.info(weapon).fuse_time;
            let toss = [
                self.crandom() * 160.0,
                self.crandom() * 160.0,
                self.crandom() * 160.0,
            ];
            if let Err(e) = self.launch_grenade(vm, n, weapon, spot, toss, [0.0; 3], true, fuse) {
                self.print(format!("G_FireGrenade: {e}\n"));
            }
        }
    }

    /// `ItemWeaponSetAmmo`.
    pub fn item_set_ammo(
        &mut self,
        n: u16,
        clip: i32,
        stock: i32,
        alt: usize,
    ) -> Result<(), String> {
        let weapons = &self.weapons;
        let e = self.ents.get_mut(usize::from(n)).and_then(Option::as_mut);
        let Some(item) = e
            .filter(|e| e.kind == EntKind::Item)
            .and_then(|e| e.item.as_mut())
        else {
            return Err("Entity is not an item.".into());
        };
        let slot = &mut item.ammo[alt];
        if slot.weapon != 0 {
            slot.clip = clip.min(weapons.info(slot.weapon).clip_size);
            slot.stock = stock;
        }
        Ok(())
    }

    /// The death state of a player, for [`Game::player_die`]: not already dead.
    pub fn alive_for_death(&self, n: u16) -> bool {
        self.client(n)
            .is_some_and(|c| !(c.ps.pm_type >= PmType::Noclip && c.ps.pm_type != PmType::LastStand))
    }
}
