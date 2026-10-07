// SPDX-License-Identifier: GPL-3.0-or-later
//! Hud element allocation. Field values live on the script object; replication to clients
//! belongs to M6.

use gsc::{EntClass, EntRef, Value, Vm};

use super::Args;
use crate::game::{Game, HUDELEM_BASE, HudElem, MAX_HUDELEMS};

fn alloc(g: &mut Game, vm: &mut Vm) -> Result<Value, String> {
    let idx = match g.hudelems.iter().position(|h| !h.inuse) {
        Some(i) => i,
        None if g.hudelems.len() < MAX_HUDELEMS => {
            g.hudelems.push(HudElem::default());
            g.hudelems.len() - 1
        }
        None => return Err("exceeded maximum number of hudelems".into()),
    };
    g.hudelems[idx].inuse = true;
    Ok(Value::Object(
        vm.entity(HUDELEM_BASE + idx as u16, EntClass::HudElem),
    ))
}

pub fn new_hud_elem(g: &mut Game, vm: &mut Vm, _: Args) -> Result<Value, String> {
    alloc(g, vm)
}

pub fn new_client_hud_elem(g: &mut Game, vm: &mut Vm, a: Args) -> Result<Value, String> {
    a.entity(0)?;
    alloc(g, vm)
}

pub fn new_team_hud_elem(g: &mut Game, vm: &mut Vm, a: Args) -> Result<Value, String> {
    a.string(0)?;
    alloc(g, vm)
}

/// `destroy`: frees the element; its script object dies at the next tick.
pub fn destroy(g: &mut Game, vm: &mut Vm, e: EntRef, _: Args) -> Result<Value, String> {
    let idx = usize::from(e.num - HUDELEM_BASE);
    if let Some(h) = g.hudelems.get_mut(idx) {
        h.inuse = false;
    }
    // `Scr_FreeHudElem` tells the element's threads (`endon("death")`) before it goes.
    vm.notify_entity(e.num, "death", &[]);
    vm.free_entity(e.num);
    Ok(Value::Undefined)
}
