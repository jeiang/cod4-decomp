// SPDX-License-Identifier: GPL-3.0-only
//! The view model's animation as the weapon code drives it, on the stock weapons: the real state machine steps a
//! player over a flat floor and the real view model follows. Run by the viewmodel tour (harness `client-viewmodel`)
//! and by a test; both report what is wrong, nothing when the weapons behave.
//!
//! - An off-hand grenade throw shows the grenade (`BG_GetViewmodelWeaponIndex`) with its pin-pull and throw
//!   animations while the rifle stays the weapon in hand, and the rifle comes back up after.
//! - The last round of a magazine plays the last-shot animation, and the reload that follows the empty-reload one.

use crate::viewmodel::{ViewModel, slot};
use server::content::Content;
use sim::cm::{Collide, ENTITYNUM_WORLD, Trace};
use sim::pm::{Params, PlayerState, Pmove, UserCmd, button, pmove, viewmodel_weapon, wf};
use sim::weapon::{PlayerWeapons, WeaponCtx, WeaponTable};

const RIFLE: &str = "ak47_mp";
const GRENADE: &str = "frag_grenade_mp";

/// A floor at height 0.
struct Floor;

impl Collide for Floor {
    fn trace(
        &self,
        start: [f32; 3],
        end: [f32; 3],
        mins: [f32; 3],
        _maxs: [f32; 3],
        _pass: u16,
        _mask: i32,
    ) -> Trace {
        let (a, b) = (start[2] + mins[2], end[2] + mins[2]);
        if a >= 0.0 && b < 0.0 {
            Trace {
                fraction: a / (a - b),
                normal: [0.0, 0.0, 1.0],
                contents: sim::contents::SOLID,
                hit_id: ENTITYNUM_WORLD,
                walkable: true,
                ..Trace::MISS
            }
        } else {
            Trace::MISS
        }
    }

    fn point_contents(&self, _p: [f32; 3], _pass: u16, _mask: i32) -> i32 {
        0
    }
}

struct Player<'a> {
    table: &'a WeaponTable,
    inv: PlayerWeapons,
    ps: PlayerState,
    params: Params,
    sent: UserCmd,
    want: u16,
    offhand: u16,
}

impl Player<'_> {
    fn step(&mut self, buttons: i32) {
        let mut pm = Pmove::new(std::mem::take(&mut self.ps), &self.params);
        pm.weapons = Some(WeaponCtx::new(self.table, &mut self.inv));
        pm.cmd = UserCmd {
            buttons,
            weapon: self.want as u8,
            offhand_index: self.offhand as u8,
            server_time: pm.ps.command_time + 10,
            ..UserCmd::default()
        };
        pm.oldcmd = self.sent;
        self.sent = pm.cmd;
        pmove(&mut pm, &Floor);
        self.ps = pm.ps;
    }
}

fn player<'a>(table: &'a WeaponTable, rifle: u16, grenade: u16) -> Result<Player<'a>, String> {
    let mut ps = PlayerState {
        command_time: 10_000,
        ground_entity_num: ENTITYNUM_WORLD,
        ..PlayerState::default()
    };
    let mut inv = PlayerWeapons::new();
    for w in [rifle, grenade] {
        if !inv.give(table, &mut ps, w, 0) {
            return Err(format!("{} cannot be given", table.name(w)));
        }
    }
    let mut p = Player {
        table,
        inv,
        ps,
        params: Params::default(),
        sent: UserCmd::default(),
        want: rifle,
        offhand: grenade,
    };
    for _ in 0..(table.info(rifle).first_raise_time / 10 + 5) {
        p.step(0);
    }
    Ok(p)
}

fn view(content: &Content, name: &str) -> Result<ViewModel, String> {
    let def = content
        .weapon(name)
        .cloned()
        .ok_or_else(|| format!("{name} not loaded"))?;
    ViewModel::new(content, &def, None)
}

/// What is wrong with the animation of the stock rifle and frag grenade; empty when nothing.
pub fn check(content: &Content) -> Vec<String> {
    match run(content) {
        Ok(bad) => bad,
        Err(e) => vec![format!("animation check: {e}")],
    }
}

fn run(content: &Content) -> Result<Vec<String>, String> {
    let defs = content.weapons();
    let table = WeaponTable::new(&defs).map_err(|e| format!("{e:?}"))?;
    let (rifle, grenade) = (table.index(RIFLE), table.index(GRENADE));
    if rifle == 0 || grenade == 0 {
        return Err(format!("{RIFLE} or {GRENADE} missing from the table"));
    }
    let mut vms = [view(content, RIFLE)?, view(content, GRENADE)?];
    let mut bad = Vec::new();

    // A grenade throw: hold the button, release, wait for the rifle.
    let mut p = player(&table, rifle, grenade)?;
    let mut shown = Vec::new();
    let mut slots: Vec<(u16, usize)> = Vec::new();
    for i in 0..700 {
        p.step(if i < 120 { button::FRAG } else { 0 });
        let on_show = viewmodel_weapon(&p.ps);
        if p.ps.weapon as u16 != rifle {
            bad.push("the weapon in hand changed during the throw".to_owned());
            break;
        }
        let vm = &mut vms[usize::from(on_show == grenade)];
        vm.update(&p.ps, 0.01);
        shown.push(on_show);
        slots.push((on_show, vm.playing().slot));
    }
    let seen = |w: u16, s: usize| slots.contains(&(w, s));
    if p.ps.weapon_flags & wf::USING_OFFHAND != 0 || shown.last() != Some(&rifle) {
        bad.push("the rifle is not back on show after the throw".to_owned());
    }
    for (what, s) in [("pin pull", slot::HOLD_FIRE), ("throw", slot::FIRE)] {
        if !seen(grenade, s) {
            bad.push(format!("the grenade's {what} animation never played"));
        }
    }
    if seen(rifle, slot::HOLD_FIRE) {
        bad.push("the rifle played a grenade animation".to_owned());
    }
    if !seen(rifle, slot::QUICK_RAISE) {
        bad.push("the rifle was not raised again after the throw".to_owned());
    }
    if !shown.contains(&grenade) {
        bad.push("the grenade was never on show".to_owned());
    }

    // The last round, then the reload from empty.
    let mut p = player(&table, rifle, grenade)?;
    let clip = |p: &Player<'_>| p.inv.clip(p.table, rifle);
    let mut last_slot = None;
    let mut reload = None;
    for _ in 0..1500 {
        p.step(button::ATTACK);
        let vm = &mut vms[0];
        vm.set_clip_empty(clip(&p) == 0);
        vm.update(&p.ps, 0.01);
        if clip(&p) == 0 && last_slot.is_none() {
            last_slot = Some(vm.playing().slot);
        }
        if last_slot.is_some() && p.ps.weapon_state == sim::pm::weapon_state::RELOADING {
            reload = Some(vm.playing().slot);
            break;
        }
    }
    if last_slot != Some(slot::LASTSHOT) {
        bad.push(format!(
            "the last round played slot {last_slot:?}, not the last-shot animation"
        ));
    }
    if reload != Some(slot::RELOAD_EMPTY) {
        bad.push(format!(
            "the reload from empty played slot {reload:?}, not the empty-reload animation"
        ));
    }
    Ok(bad)
}

#[cfg(test)]
mod tests {
    use super::*;
    use server::content::Install;

    #[test]
    fn the_stock_weapons_show_the_grenade_and_the_last_shot_and_the_empty_reload() {
        let Some(root) = std::env::var_os("COD4_PATH") else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let install = Install::open(std::path::Path::new(&root)).expect("install");
        let mut content = Content::for_client();
        content.load_boot(&install).expect("boot zones");
        content.load_map(&install, "mp_backlot").expect("map");
        assert_eq!(check(&content), Vec::<String>::new());
    }
}
