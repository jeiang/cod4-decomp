// SPDX-License-Identifier: GPL-3.0-only
//! Runtime weapons: the table of weapon definitions, the per-player inventory, the events the
//! weapon state machine hands to the game, and the pure firing and damage helpers.
//!
//! The weapon state machine itself (`PM_Weapon`) runs inside [`pmove`](crate::pm::pmove) when the
//! [`Pmove`](crate::pm::Pmove) carries a [`WeaponCtx`]; it is shared by the server and client
//! prediction. Everything here is plain data and pure functions, builds for `wasm32`, and does
//! not allocate per tick (tables allocate once, at build time).
//!
//! Fact source: `iw3mp.exe` 1.7 (`bg_weapons.cpp`, `g_weapon.cpp`, `bullet.cpp`,
//! `g_client_script_cmd_mp.cpp`, `g_items.cpp`). Known ceiling: animation script events are not
//! modelled; they do not change when, whether or how a shot, reload, switch or throw happens.

pub mod damage;
pub mod fire;
#[cfg(test)]
pub(crate) mod fixtures;
pub mod gun;
mod info;
mod inventory;
mod params;
pub mod pickup;
mod table;

pub use info::{
    FireType, HITLOC_COUNT, ImpactType, InventoryType, OffhandClass, PenetrateType, ProjExplosion,
    SURFACE_TYPES, WeaponClass, WeaponInfo, WeaponType,
};
pub use inventory::PlayerWeapons;
pub use params::{WeaponParams, perk};
pub use table::{MAX_AMMO, MAX_CLIPS, MAX_WEAPONS, TableError, WeaponTable};

/// The weapon half of a [`Pmove`](crate::pm::Pmove): what the state machine reads and the
/// inventory it changes. Build it per command; a client predicting runs it with the same inputs
/// on the same `PlayerWeapons` and `PlayerState` copies.
#[derive(Debug)]
pub struct WeaponCtx<'a> {
    pub table: &'a WeaponTable,
    pub inv: &'a mut PlayerWeapons,
    pub params: WeaponParams,
}

impl<'a> WeaponCtx<'a> {
    pub fn new(table: &'a WeaponTable, inv: &'a mut PlayerWeapons) -> Self {
        Self {
            table,
            inv,
            params: WeaponParams::default(),
        }
    }
}

/// Something the weapon state machine did that the game must act on. Each is also raised as the
/// original's predictable player event where the original has one, but the four-slot event ring
/// can overflow within one command; this list is the reliable record.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum WeaponEvent {
    #[default]
    None,
    /// A shot left the weapon (`EV_FIRE_WEAPON`, `EV_FIRE_WEAPON_LASTSHOT`). The server fires the
    /// bullets, projectile or thrown grenade from the player's view. `shot` is the number of the
    /// shot in the current trigger pull or burst (1-based, saturating at 4; 0 for full auto).
    Fire {
        weapon: u16,
        shot: u8,
        /// First shot of the trigger pull (the weapon was not already firing).
        first: bool,
        /// Fully aimed down sights.
        ads: bool,
        /// The weapon fires fixed bursts.
        burst: bool,
        /// The magazine is empty after this shot.
        last_round: bool,
    },
    /// The melee swing connects now (`EV_FIRE_MELEE`).
    Melee { weapon: u16 },
    /// A grenade is thrown from the off-hand (`EV_USE_OFFHAND`). `fuse_left` is the fuse
    /// remaining in ms (`ps.grenade_time_left`) and `cooked` the time it was held primed (the
    /// weapon's fuse time less `fuse_left`; 0 for grenades that do not cook).
    OffhandThrow {
        weapon: u16,
        fuse_left: i32,
        cooked: i32,
    },
    /// The fuse of a held grenade ran out in the player's hand (`EV_GRENADE_SUICIDE`).
    GrenadeSuicide { weapon: u16 },
    /// A detonator weapon was triggered (`EV_DETONATE`).
    Detonate { weapon: u16 },
    /// Ammunition moved from stock into the magazine (`EV_RELOAD_ADDAMMO`).
    ReloadAmmoAdded { weapon: u16, amount: i32 },
    /// A reload finished and the weapon is ready again.
    ReloadComplete { weapon: u16 },
    /// The player started lowering `from` to raise `to` (0 = nothing).
    SwitchBegin { from: u16, to: u16 },
    /// The raise of `to` began: the current weapon is now `to`.
    SwitchComplete { from: u16, to: u16 },
    /// A weapon was fired or reloaded with nothing left (`EV_NOAMMO`).
    NoAmmo { weapon: u16 },
    /// A grenade button was pressed with none available (`EV_EMPTY_OFFHAND`).
    EmptyOffhand,
    /// Night vision toggled (`EV_NIGHTVISION_WEAR`, `EV_NIGHTVISION_REMOVE`).
    NightVision { on: bool },
    /// The weapon was dropped from the inventory by the state machine (an emptied clip-only
    /// weapon, e.g. a thrown last grenade).
    Dropped { weapon: u16 },
}

/// Most events one `pmove` call records.
pub const MAX_WEAPON_EVENTS: usize = 16;

/// The events a `pmove` call raised, in order. Fixed size; the `pmove` call clears it first.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeaponOut {
    events: [WeaponEvent; MAX_WEAPON_EVENTS],
    len: usize,
    /// More events happened than fit; the oldest were kept.
    pub overflowed: bool,
}

impl Default for WeaponOut {
    fn default() -> Self {
        Self {
            events: [WeaponEvent::None; MAX_WEAPON_EVENTS],
            len: 0,
            overflowed: false,
        }
    }
}

impl WeaponOut {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn push(&mut self, event: WeaponEvent) {
        if self.len == MAX_WEAPON_EVENTS {
            self.overflowed = true;
            return;
        }
        self.events[self.len] = event;
        self.len += 1;
    }

    pub fn events(&self) -> &[WeaponEvent] {
        &self.events[..self.len]
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(test)]
mod tests;
