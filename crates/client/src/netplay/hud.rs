// SPDX-License-Identifier: GPL-3.0-only
//! Fills [`GameFacts`] each frame from what the network play knows: the player state, the weapon inventory and the
//! weapon definitions, the server's configstrings and scoreboard, and the other players in the snapshots.

use super::{NetPlay, team_of};
use crate::compass::MapInfo;
use crate::hudstate::{self, Actor, Counter, HudFacts, OffhandFacts, Stance, WeaponFacts};
use crate::models::Team;
use crate::shell::GameFacts;
use net::entity::etype;
use net::ui::cs;
use server::netsv::eflags;
use sim::pm::{PlayerState, PmType, ev, pmf, weapon_state as ws, wf};
use sim::weapon::{OffhandClass, PlayerWeapons, WeaponClass, WeaponInfo};

/// How long an actor that left the snapshots is kept before the compass forgets it.
const ACTOR_KEEP_MS: i32 = 5000;
/// `ammoCounterClip` value that shows the alternate weapon's magazine.
const COUNTER_ALT: i32 = 6;

/// The `\key\value` pairs of an info string.
fn info_value<'a>(info: &'a str, key: &str) -> Option<&'a str> {
    let mut it = info.split('\\').skip(1);
    while let (Some(k), Some(v)) = (it.next(), it.next()) {
        if k.eq_ignore_ascii_case(key) {
            return Some(v);
        }
    }
    None
}

impl NetPlay {
    /// Updates `g` for the frame at shell time `now` (ms). With no player state yet the HUD is marked not live.
    pub fn fill_game_facts(&mut self, g: &mut GameFacts, now: i32) {
        let (Some(snap), Some(ui), Some((ps, yaw))) =
            (self.net.latest(), self.net.ui_ref(), self.hud_view.as_ref())
        else {
            g.hud.live = false;
            return;
        };
        let own = snap.own();
        let first = !g.hud.live;
        let server_time = self
            .net
            .snaps
            .server_time(self.net.now_ms())
            .unwrap_or(snap.server_time);

        // Team: the player's body in the snapshot knows it before the client info string does.
        let team = snap
            .entity(own)
            .and_then(|e| team_of(e.eflags))
            .map(|t| if t == Team::Axis { 1 } else { 2 })
            .or_else(|| ui.client(own).map(|c| c.team))
            .unwrap_or(0);
        g.team = match team {
            1 => "axis",
            2 => "allies",
            3 => "spectator",
            _ => "free",
        }
        .into();
        let dead = ps.pm_type >= PmType::Dead && ps.pm_type != PmType::Spectator;
        g.dead = dead;
        g.intermission = ps.pm_type == PmType::Intermission;
        g.gametype = info_value(ui.config(cs::SERVERINFO), "g_gametype")
            .unwrap_or_else(|| ui.config(cs::GAMETYPE))
            .to_owned();
        (g.allies_score, g.axis_score) = ui.team_scores();
        let end: i32 = ui.config(cs::GAMEENDTIME).parse().unwrap_or(0);
        g.time_left = if end > 0 {
            ((end - server_time + 999) / 1000).max(0)
        } else {
            0
        };
        if let Some(row) = ui.scoreboard().rows.iter().find(|r| r.client == own) {
            (g.score, g.kills, g.deaths, g.ping) =
                (row.score, row.kills, row.deaths, row.ping.max(0));
        }

        let inv = PlayerWeapons::from_words(&snap.inv);
        let selected = self
            .want_weapon
            .filter(|w| inv.has(*w))
            .unwrap_or(ps.weapon as u16);
        let weapon = (selected != 0).then(|| self.weapon_facts(&inv, selected, ps));
        g.clip_ammo = weapon.as_ref().map_or(0, |w| w.clip.max(0));

        let h = &mut g.hud;
        h.live = true;
        h.now = now;
        h.own_client = own;
        h.team_known = matches!(team, 1 | 2);
        h.pm_dead = dead;
        h.spectator = ps.pm_type == PmType::Spectator;
        h.health = ps.health;
        h.max_health = ps.max_health;
        h.origin = ps.origin;
        h.yaw = *yaw;
        h.weapon_disabled = ps.weapon_flags & wf::DISABLED != 0;
        h.weapon = weapon;
        (h.frag, h.second) = (
            self.offhand(&inv, |c| c == OffhandClass::Frag),
            self.offhand(&inv, |c| {
                c == if ps.offhand_secondary == 0 {
                    OffhandClass::Smoke
                } else {
                    OffhandClass::Flash
                }
            }),
        );
        h.second_is_flash = ps.offhand_secondary != 0;
        h.stance = if ps.pm_flags & pmf::PRONE != 0 {
            Stance::Prone
        } else if ps.pm_flags & pmf::DUCKED != 0 {
            Stance::Crouch
        } else {
            Stance::Stand
        };
        if ps.pm_flags & pmf::NO_PRONE != 0 && h.prone_blocked_end < now {
            h.prone_blocked_end = now + 1500;
        }
        let info = self.weapons.info(ps.weapon as u16);
        h.sprint_max = sim::pm::max_sprint_ms(&self.params, info.sprint_duration_scale, ps.perks);
        h.sprint_left = sim::pm::sprint_left_ms(ps, &self.params, server_time, h.sprint_max);
        h.sprinting = ps.pm_flags & pmf::SPRINTING != 0;
        h.mantle_hint = ps.mantle_state.flags & 8 != 0;
        if ps.cursor_hint != 0 {
            h.cursor_hint = ps.cursor_hint;
            h.cursor_hint_time = now;
            h.cursor_hint_text = if ps.cursor_hint_string >= 0 {
                ui.config(cs::USE_TRIG_STRINGS + ps.cursor_hint_string as u16)
                    .to_owned()
            } else {
                String::new()
            };
        }
        for (i, slot) in h.slots.iter_mut().enumerate() {
            let (kind, param) = (ps.action_slot_type[i], ps.action_slot_param[i]);
            // Alt mode shows the alternate weapon of the one held.
            let target = match kind {
                sim::pm::action_slot::WEAPON => param,
                sim::pm::action_slot::ALT_MODE => self.weapons.info(ps.weapon as u16).alt_weapon,
                _ => 0,
            };
            let usable = match kind {
                sim::pm::action_slot::NIGHT_VISION => true,
                _ => target != 0 && inv.has(target),
            };
            let def = (target != 0 && usable)
                .then(|| self.lib.content.weapon(self.weapons.name(target)))
                .flatten();
            *slot = crate::hudstate::SlotFacts {
                kind,
                usable,
                active: kind == sim::pm::action_slot::WEAPON && target == ps.weapon as u16,
                icon: def
                    .and_then(|d| d.dpad_icon.as_ref())
                    .and_then(|m| m.name.as_deref().map(str::to_owned)),
                icon_ratio: def.map_or(0, |d| d.dpad_icon_ratio),
                ammo: if usable && target != 0 {
                    inv.weapon_ammo(&self.weapons, target)
                } else {
                    0
                },
            };
        }
        h.loc_material = (ps.loc_selection != 0)
            .then(|| ui.material(ps.loc_selection).to_owned())
            .filter(|m| !m.is_empty());
        h.loc_radius = f32::from(ps.loc_radius) / 63.0;
        h.loc_cursor = self.loc_cursor;
        h.selecting_location = ps.e_flags & sim::pm::ef::LOC_SELECTING != 0;
        let def = self.lib.content.weapon(self.weapons.name(ps.weapon as u16));
        h.breath_hint = ps.weapon_flags & wf::HOLD_BREATH == 0
            && ps.weapon_pos_frac == 1.0
            && def.is_some_and(|d| d.overlay_reticle != 0)
            && info.weap_class != WeaponClass::Item;

        // Things that make a piece show up for a while.
        let sprint_changed = h.sprint_left != h.prev_sprint_left && h.sprint_left < h.sprint_max;
        let ammo_now = h
            .weapon
            .as_ref()
            .map_or((0, 0, 0), |w| (w.index, w.clip, w.stock));
        let off_now = (
            h.frag.as_ref().map_or(-1, |o| o.ammo),
            h.second.as_ref().map_or(-1, |o| o.ammo),
        );
        if first || h.health != h.prev_health {
            h.health_fade = now;
        }
        if first || Some(h.stance) != h.prev_stance {
            h.stance_fade = now;
        }
        if first || sprint_changed || h.sprinting {
            h.sprint_fade = now;
        }
        if first || ammo_now != h.prev_ammo {
            h.ammo_fade = now;
        }
        if first || off_now != h.prev_offhand {
            h.offhand_fade = now;
        }
        if first || ps.origin != h.prev_origin {
            h.compass_fade = now;
        }
        if first || ammo_now.0 != h.prev_weapon {
            h.weapon_select_time = now;
        }
        h.note_spawn(own, ps.spawn_count);
        (h.prev_health, h.prev_stance, h.prev_ammo, h.prev_offhand) =
            (h.health, Some(h.stance), ammo_now, off_now);
        (h.prev_origin, h.prev_weapon, h.prev_sprint_left) = (ps.origin, ammo_now.0, h.sprint_left);
        self.hint_events(&mut g.hud, &snap.ps, first, now);

        g.hud.map = MapInfo::parse(ui.config(cs::MINIMAP), ui.config(cs::NORTHYAW));
        let own_team = team;
        self.update_actors(&mut g.hud, server_time, own, own_team, now);
    }

    /// The "no ammo" style hints, from the server's events on the player's state.
    fn hint_events(&self, h: &mut HudFacts, ps: &PlayerState, first: bool, now: i32) {
        let seq = ps.event_sequence;
        if !first {
            let new = usize::from(seq.wrapping_sub(h.prev_event_seq)).min(4);
            for i in 0..new {
                let n = seq.wrapping_sub((new - 1 - i) as u8);
                let key = match ps.events[usize::from(n & 3)] {
                    ev::NOAMMO => "WEAPON_NO_AMMO",
                    ev::NO_FRAG_GRENADE_HINT => "WEAPON_NO_FRAG_GRENADE",
                    ev::NO_SPECIAL_GRENADE_HINT => "WEAPON_NO_SPECIAL_GRENADE",
                    _ => continue,
                };
                h.invalid_cmd = Some((key, now));
            }
        }
        h.prev_event_seq = seq;
    }

    /// Other players for the compass: where they are and which way they face, and when they fire.
    fn update_actors(&self, h: &mut HudFacts, server_time: i32, own: u16, own_team: u8, now: i32) {
        let ents = self
            .net
            .snaps
            .interpolate(server_time - net::view::INTERP_DELAY_MS, Some(own));
        for e in ents.iter().filter(|e| e.etype == etype::PLAYER) {
            if e.eflags & eflags::DEAD != 0 {
                continue;
            }
            let theirs = team_of(e.eflags).map(|t| if t == Team::Axis { 1 } else { 2 });
            let a = h.actors.entry(e.client).or_insert_with(|| Actor {
                event_seq: e.event_seq,
                ..Actor::default()
            });
            a.friendly = own_team != 0 && theirs == Some(own_team);
            a.pos = [e.origin[0], e.origin[1]];
            a.yaw = e.angles[1];
            a.last_update = now;
            if e.event_seq != a.event_seq {
                a.event_seq = e.event_seq;
                if matches!(e.event, ev::FIRE_WEAPON | ev::FIRE_WEAPON_LASTSHOT) {
                    a.fire_time = now;
                    a.fire_pos = a.pos;
                }
            }
        }
        h.actors.retain(|_, a| now - a.last_update < ACTOR_KEEP_MS);
    }

    /// The best grenade of a class to show: one with rounds left, else any carried.
    fn offhand(
        &self,
        inv: &PlayerWeapons,
        class: impl Fn(OffhandClass) -> bool,
    ) -> Option<OffhandFacts> {
        let of_class: Vec<u16> = inv
            .list(&self.weapons)
            .filter(|&i| class(self.weapons.info(i).offhand_class))
            .collect();
        let best = of_class
            .iter()
            .copied()
            .find(|&i| inv.clip(&self.weapons, i) > 0)
            .or_else(|| of_class.first().copied())?;
        let name = self.weapons.name(best);
        Some(OffhandFacts {
            icon: self
                .lib
                .content
                .weapon(name)
                .and_then(|d| d.hud_icon.as_ref())
                .and_then(|m| m.name.as_deref().map(str::to_owned)),
            ammo: of_class.iter().map(|&i| inv.clip(&self.weapons, i)).sum(),
        })
    }

    /// What the weapon displays need to know about weapon `index`.
    fn weapon_facts(&self, inv: &PlayerWeapons, index: u16, ps: &PlayerState) -> WeaponFacts {
        let t = &self.weapons;
        let info: &WeaponInfo = t.info(index);
        let def = self.lib.content.weapon(t.name(index));
        let text = |n: Option<&assets::zone::gfx::Name>| {
            n.and_then(|n| n.as_deref()).unwrap_or("").to_owned()
        };
        // Which weapon's magazine the counter draws: the alternate fire mode's for an attachment weapon.
        let counter_idx = match def.map_or(0, |d| d.ammo_counter_clip) {
            COUNTER_ALT => {
                let alt = info.alt_weapon;
                let alt_style = self
                    .lib
                    .content
                    .weapon(t.name(alt))
                    .map_or(0, |d| d.ammo_counter_clip);
                if alt != 0 && alt_style != COUNTER_ALT {
                    alt
                } else {
                    0
                }
            }
            0 => 0,
            _ => index,
        };
        let counter_def = (counter_idx != 0)
            .then(|| self.lib.content.weapon(t.name(counter_idx)))
            .flatten();
        let reserve = |i: u16| inv.get_stock(t, i);
        let stock = reserve(index);
        let clip = if info.clip_only {
            -1
        } else {
            inv.clip(t, index)
        };
        let threshold = def.map_or(0.0, |d| d.low_ammo_warning_threshold);
        let counter_info = t.info(counter_idx);
        let reloading = matches!(
            ps.weapon_state,
            ws::RELOADING
                | ws::RELOAD_START
                | ws::RELOAD_END
                | ws::RELOAD_START_INTERUPT
                | ws::RELOADING_INTERUPT
        );
        WeaponFacts {
            index,
            display: text(def.map(|d| &d.display_name)),
            mode: text(def.map(|d| &d.mode_name)),
            clip,
            stock,
            low_ammo: hudstate::low_ammo(stock, inv.ammo_player_max(t, index, 0)),
            low_clip: hudstate::low_clip(clip, info.clip_size, threshold),
            counter: counter_def.map_or(Counter::None, |d| Counter::from_raw(d.ammo_counter_clip)),
            counter_clip: if counter_idx != 0 {
                inv.clip(t, counter_idx)
            } else {
                0
            },
            counter_clip_size: if counter_idx != 0 {
                counter_info.clip_size
            } else {
                0
            },
            counter_low: counter_def.is_some_and(|d| {
                hudstate::low_clip(
                    inv.clip(t, counter_idx),
                    counter_info.clip_size,
                    d.low_ammo_warning_threshold,
                )
            }),
            stock_shown: counter_def
                .filter(|d| d.suppress_ammo_reserve_display == 0)
                .map(|_| reserve(counter_idx)),
            stock_low: counter_def.is_some()
                && hudstate::low_ammo(reserve(counter_idx), inv.ammo_player_max(t, counter_idx, 0)),
            icon: def
                .and_then(|d| d.ammo_counter_icon.as_ref())
                .and_then(|m| m.name.as_deref().map(str::to_owned)),
            icon_ratio: def.map_or(0, |d| d.ammo_counter_icon_ratio),
            can_reload: inv.stock(t, index) > 0,
            empty: !info.clip_only && inv.clip(t, index) == 0,
            clip_size: info.clip_size,
            hide_warning: reloading || ps.e_flags & sim::pm::ef::TURRET_ACTIVE != 0,
            blocks_prone: info.blocks_prone,
        }
    }
}
