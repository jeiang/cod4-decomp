// SPDX-License-Identifier: GPL-3.0-or-later
//! What the weapon state machine asks the game to do: fire bullets, swing the knife, throw
//! grenades, and the script notifies that follow.

use gsc::Vm;
use sim::weapon::WeaponOut;

use crate::game::Game;

impl Game {
    /// Acts on the events one command's `PM_Weapon` raised.
    pub fn weapon_events(&mut self, _vm: &mut Vm, _n: u16, _out: &WeaponOut) {}
}
