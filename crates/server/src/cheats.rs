// SPDX-License-Identifier: GPL-3.0-only
// Translated from KisakCOD `game_mp/g_cmds_mp.cpp` (GPL-3.0), copyright KisakCOD contributors.
//! Cheat and developer client commands (`ClientCommand`, #370). `CheatsOk` gates `give`, `take`, `god`, `demigod`,
//! `notarget`, `noclip`, `ufo` and `kill` by `sv_cheats` and a living player, with localized game messages on refusal.
//! `setviewpos` requires cheats but not life; `entitycount` requires cheats and prints on the server. Stock
//! `printentities` and the vision-set commands do not use `CheatsOk`.

use net::ui::{PrintKind, ServerCmd};
use sim::weapon::WeaponTable;

use crate::client::{Client, Session};
use crate::game::Game;
use crate::ui::Dest;

/// `ent->flags`: `FL_GODMODE`, `FL_DEMI_GODMODE`, `FL_NOTARGET` (`bg_public.h`).
pub const FL_GODMODE: i32 = 1;
pub const FL_DEMI_GODMODE: i32 = 2;
pub const FL_NOTARGET: i32 = 4;

/// `Add_Ammo`'s stock for `give ammo` without a count.
const GIVE_AMMO_ALL: i32 = 998;

impl Game {
    /// The `e` server command: a localized key the client shows in the game message feed and its console.
    pub fn game_message(&mut self, n: u16, key: &str) {
        self.send(
            Dest::Client(n),
            ServerCmd::Print {
                kind: PrintKind::Normal,
                text: key.to_owned(),
            },
        );
    }

    /// `CheatsOk`: `sv_cheats` is on and the player is alive, else the player is told which is not.
    pub fn cheats_ok(&mut self, n: u16) -> bool {
        if !self.cvars.bool("sv_cheats") {
            self.game_message(n, "GAME_CHEATSNOTENABLED");
            return false;
        }
        if self.ent(n).is_none_or(|e| e.health <= 0) {
            self.game_message(n, "GAME_MUSTBEALIVECOMMAND");
            return false;
        }
        true
    }

    /// `Cmd_God_f`, `Cmd_DemiGod_f`, `Cmd_Notarget_f`: toggles the entity flag and says which way it went.
    pub fn cmd_toggle_flag(&mut self, n: u16, flag: i32, on: &str, off: &str) {
        if !self.cheats_ok(n) {
            return;
        }
        let Some(e) = self.ent_mut(n) else { return };
        e.flags ^= flag;
        let key = if e.flags & flag != 0 { on } else { off };
        self.game_message(n, key);
    }

    /// `Cmd_Noclip_f` and `Cmd_UFO_f`: the client flag `ClientThink` reads for the movement type.
    pub fn cmd_noclip(&mut self, n: u16, ufo: bool) {
        if !self.cheats_ok(n) {
            return;
        }
        let Some(c) = self.client_mut(n) else { return };
        let key = if ufo {
            c.ufo = !c.ufo;
            if c.ufo { "GAME_UFOON" } else { "GAME_UFOOFF" }
        } else {
            c.noclip = !c.noclip;
            if c.noclip {
                "GAME_NOCLIPON"
            } else {
                "GAME_NOCLIPOFF"
            }
        };
        self.set_client_contents(n);
        self.game_message(n, key);
    }

    /// `Cmd_Give_f`: `give all | health [n] | weapons | ammo [n] | allammo <n> | <weapon name>`. The name is every
    /// word after `give` (`ConcatArgs(1)`), the amount the number in the second (`ConcatArgs(2)`, `atoi`).
    pub fn cmd_give(&mut self, n: u16, argv: &[String]) {
        if !self.cheats_ok(n) {
            return;
        }
        let name = argv[1..].join(" ");
        if name.is_empty() {
            return;
        }
        let amount = argv.get(2).map_or(0, |a| crate::cvar::parse_int(a));
        let lower = name.to_ascii_lowercase();
        let all = lower == "all";
        if all || lower.starts_with("health") {
            let max = self.client(n).map_or(100, |c| c.max_health);
            if let Some(e) = self.ent_mut(n) {
                if amount == 0 {
                    e.health = max;
                } else {
                    e.health += amount;
                }
            }
            if !all {
                return;
            }
        }
        if all || lower == "weapons" {
            let weapons = self.weapons.len() as u16;
            let c = &mut self.clients[usize::from(n)];
            for w in 1..=weapons {
                if self.weapons.info(w).has_gun_model(0) {
                    c.inv.take(&self.weapons, &mut c.ps, w, true);
                    c.inv.give_raw(&self.weapons, &mut c.ps, w, 0);
                }
            }
            if !all {
                return;
            }
        }
        if all || lower.starts_with("ammo") {
            let weapons = self.weapons.len() as u16;
            let c = &mut self.clients[usize::from(n)];
            if amount != 0 {
                let w = c.ps.weapon as u16;
                if w != 0 {
                    let model = c.inv.model(w);
                    c.inv
                        .add_ammo(&self.weapons, &mut c.ps, w, model, amount, true);
                }
            } else {
                for w in 1..=weapons {
                    c.inv
                        .add_ammo(&self.weapons, &mut c.ps, w, 0, GIVE_AMMO_ALL, true);
                }
            }
            if !all {
                return;
            }
        }
        if all || lower.starts_with("allammo") {
            if amount != 0 {
                let weapons = self.weapons.len() as u16;
                let c = &mut self.clients[usize::from(n)];
                for w in 1..=weapons {
                    c.inv.add_ammo(&self.weapons, &mut c.ps, w, 0, amount, true);
                }
            }
            if !all {
                return;
            }
        }
        if all {
            return;
        }
        // `G_FindItem` then `Touch_Item`: a weapon by its name is picked up as a dropped one would be.
        let w = self.weapons.index(&name);
        if w == 0 {
            return;
        }
        let c = &mut self.clients[usize::from(n)];
        c.inv.give(&self.weapons, &mut c.ps, w, 0);
        if c.ps.weapon == 0 {
            c.inv.select(w);
        }
    }

    /// `Cmd_Take_f`: `take all | health [n] | weapons | ammo [n] | allammo <n>`.
    pub fn cmd_take(&mut self, n: u16, argv: &[String]) {
        if !self.cheats_ok(n) {
            return;
        }
        let name = argv[1..].join(" ");
        if name.is_empty() {
            return;
        }
        let amount = argv.get(2).map_or(0, |a| crate::cvar::parse_int(a));
        let lower = name.to_ascii_lowercase();
        let all = lower == "all";
        if all || lower.starts_with("health") {
            if let Some(e) = self.ent_mut(n) {
                e.health = if amount != 0 {
                    (e.health - amount).max(1)
                } else {
                    1
                };
            }
            if !all {
                return;
            }
        }
        if all || lower == "weapons" {
            let c = &mut self.clients[usize::from(n)];
            c.inv.take_all(&self.weapons, &mut c.ps);
            if !all {
                return;
            }
        }
        if all || lower.starts_with("ammo") {
            let weapons = self.weapons.len() as u16;
            let c = &mut self.clients[usize::from(n)];
            if amount != 0 {
                let w = c.ps.weapon as u16;
                if w != 0 {
                    take_ammo(c, &self.weapons, w, amount);
                }
            } else {
                for w in 1..=weapons {
                    c.inv.set_stock(&self.weapons, w, 0);
                    c.inv.set_clip(&self.weapons, w, 0);
                }
            }
            if !all {
                return;
            }
        }
        if (all || lower.starts_with("allammo")) && amount != 0 {
            let weapons = self.weapons.len() as u16;
            let c = &mut self.clients[usize::from(n)];
            for w in 1..=weapons {
                take_ammo(c, &self.weapons, w, amount);
            }
        }
    }

    /// `Cmd_SetViewpos_f`: `setviewpos x y z [yaw] [pitch]` puts the player's eye at the point (`TeleportPlayer`
    /// adds one unit of height so the player does not start in the floor). Only `sv_cheats` gates it; a dead player
    /// may use it.
    pub fn cmd_set_viewpos(&mut self, n: u16, argv: &[String]) {
        if !self.cvars.bool("sv_cheats") {
            self.game_message(n, "GAME_CHEATSNOTENABLED");
            return;
        }
        if argv.len() < 4 || argv.len() > 6 {
            self.game_message(n, "GAME_USAGE\x15: setviewpos x y z [yaw] [pitch]");
            return;
        }
        let num = |i: usize| crate::cvar::parse_float(&argv[i]);
        let view_height = self.client(n).map_or(0.0, |c| c.ps.view_height_current);
        let origin = [num(1), num(2), num(3) - view_height + 1.0];
        let yaw = if argv.len() >= 5 { num(4) } else { 0.0 };
        let pitch = if argv.len() == 6 { num(5) } else { 0.0 };
        self.teleport(n, origin);
        self.set_client_view_angle(n, [pitch, yaw, 0.0]);
        let [x, y, z] = origin;
        self.print(format!(
            "Cmd_SetViewpos_f: origin=({x} {y} {z}) angles=(pitch={pitch} yaw={yaw} roll=0)\n"
        ));
    }

    /// `Cmd_EntityCount_f`: the entity count, on the server's console, with cheats on.
    pub fn cmd_entity_count(&mut self) {
        if self.cvars.bool("sv_cheats") {
            let n = self.level.num_entities;
            self.print(format!("entity count = {n}\n"));
        }
    }

    /// `Cmd_PrintEntities_f` / `G_PrintEntities`: every entity's number, classname and origin, on the server's
    /// console.
    pub fn cmd_print_entities(&mut self) {
        let lines: Vec<String> = self
            .ents
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let e = e.as_ref()?;
                let [x, y, z] = e.origin;
                Some(format!("{i:4}: '{}', origin: {x} {y} {z}\n", e.classname))
            })
            .collect();
        for l in lines {
            self.print(l);
        }
    }

    /// `visionsetnaked` / `visionsetnight` (the script builtins and `Cmd_VisionSet*_f`): every client blends to the
    /// named vision file over `ms`.
    pub fn vision_set(&mut self, name: &str, ms: i32, night: bool) {
        self.vision[usize::from(night)] = Some(name.to_owned());
        self.send(
            Dest::All,
            ServerCmd::Vision {
                night,
                name: name.to_owned(),
                ms,
            },
        );
    }

    /// `Cmd_VisionSetNaked_f` / `Cmd_VisionSetNight_f`: `visionsetnaked <name> [seconds]` typed by a player; the
    /// original gates it by nothing.
    pub fn cmd_vision_set(&mut self, argv: &[String], night: bool) {
        let (Some(name), true) = (argv.get(1), argv.len() <= 3) else {
            return;
        };
        let ms = argv.get(2).map_or(1000, |s| {
            (crate::cvar::parse_float(s) * 1000.0).round() as i32
        });
        let name = name.clone();
        self.vision_set(&name, ms, night);
    }

    /// `Cmd_Kill_f`: a playing client with cheats dies by its own hand.
    pub fn cmd_kill(&mut self, vm: &mut gsc::Vm, n: u16) {
        if self
            .client(n)
            .is_some_and(|c| c.session == Session::Playing)
            && self.cheats_ok(n)
        {
            self.die_by_own_hand(vm, n);
        }
    }
}

/// `Cmd_Take_f`'s ammo arithmetic: the stock first, then what is left comes out of the magazine.
fn take_ammo(c: &mut Client, table: &WeaponTable, w: u16, amount: i32) {
    let stock = c.inv.stock(table, w) - amount;
    if stock < 0 {
        let clip = c.inv.clip(table, w) + stock;
        c.inv.set_clip(table, w, clip.max(0));
    }
    c.inv.set_stock(table, w, stock.max(0));
}
