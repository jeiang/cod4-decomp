// SPDX-License-Identifier: GPL-3.0-only
// Weapon compass ping eligibility follows KisakCOD cgame/cg_weapons.cpp (GPL-3.0; copyright the KisakCOD contributors and Activision).
//! Fills [`GameFacts`] each frame from what the network play knows: the player state, the weapon inventory and the
//! weapon definitions, the server's configstrings and scoreboard, and the other players in the snapshots.

use super::{NetPlay, team_of};
use crate::compass::MapInfo;
use crate::hudstate::{
    self, Actor, CompassVehicle, Counter, HudFacts, OffhandFacts, Stance, WeaponFacts,
};
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
pub(super) fn info_value<'a>(info: &'a str, key: &str) -> Option<&'a str> {
    let mut it = info.split('\\').skip(1);
    while let (Some(k), Some(v)) = (it.next(), it.next()) {
        if k.eq_ignore_ascii_case(key) {
            return Some(v);
        }
    }
    None
}

/// The compass pings of one player at shell time `now`: `pingPlayer` (`eflags::PING`), and an `EV_FIRE_WEAPON` or
/// `EV_FIRE_WEAPON_LASTSHOT` of a weapon that `pings` (unsilenced, not a grenade: `CG_FireWeapon`, KisakCOD
/// cgame/cg_weapons.cpp:2552-2558). Only snapshot fire events ping; non-PVS compass positions carry no events.
fn note_pings(
    a: &mut Actor,
    flags: u32,
    event_seq: u8,
    recent: &[(u8, u8)],
    pings: bool,
    now: i32,
    length: impl Fn(u8) -> Option<u32>,
) -> Pinged {
    let mut out = Pinged::default();
    // A shot seen before its sound played learns the sound's length when it does.
    if let Some(seq) = a.fire_seq {
        a.learn_fire_length(seq, length(seq));
        out.learned = a.fire_seq.is_none();
    }
    // `pingPlayer`: the compass flashes the player, as for a shot, at most every three seconds.
    if flags & eflags::PING != 0 && (a.fire_time == 0 || now - a.fire_time >= 3000) {
        a.ping(now, None, None);
        out.flag = true;
    }
    if event_seq == a.event_seq {
        return out;
    }
    // Every event raised since the last look, not only the newest.
    let fresh = usize::from(event_seq.wrapping_sub(a.event_seq)).min(recent.len());
    a.event_seq = event_seq;
    let fired = recent[recent.len() - fresh..]
        .iter()
        .any(|&(k, _)| matches!(k, ev::FIRE_WEAPON | ev::FIRE_WEAPON_LASTSHOT));
    if fired && pings {
        a.ping(now, Some(event_seq), length(event_seq));
        out.shot = true;
    }
    out
}

/// What [`note_pings`] did: pinged for `pingPlayer`, pinged for a shot, learned a shot's sound length.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Pinged {
    flag: bool,
    shot: bool,
    learned: bool,
}

/// Whether a vehicle owned by `owner` with team flags `eflags` is the viewer's enemy: another team's, or with no
/// teams anyone's but the viewer's own (`CG_CompassDrawVehicles`).
fn vehicle_is_enemy(own_team: u8, own: u16, owner: u16, eflags: u32) -> bool {
    if own_team == 0 {
        return owner != own;
    }
    team_of(eflags).map(|t| if t == Team::Axis { 1 } else { 2 }) != Some(own_team)
}

impl NetPlay {
    /// Updates `g` for the frame at shell time `now` (ms). With no player state yet the HUD is marked not live.
    pub fn fill_game_facts(&mut self, g: &mut GameFacts, now: i32) {
        let (Some(snap), Some(ui), Some((ps, yaw))) =
            (self.net.latest(), self.net.ui_ref(), self.hud_view.as_ref())
        else {
            g.hud.live = false;
            g.hud.forget_view();
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
        // The client info is current every frame; only the ping needs the scoreboard rows.
        if let Some(c) = ui.client(own) {
            (g.score, g.kills, g.deaths) = (c.score, c.kills, c.deaths);
        }
        if let Some(row) = ui.scoreboard().rows.iter().find(|r| r.client == own) {
            g.ping = row.ping.max(0);
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
        h.local_talking = self.talk_down
            && info_value(ui.config(cs::SERVERINFO), "sv_voice").is_some_and(|v| v != "0");
        h.talkers.clear();
        for client in self.hearing.talking(web_time::Instant::now()) {
            if client == own || !self.voice_on || self.muted.contains(&client) {
                continue;
            }
            // A speaker who just left lingers its 300 ms with no client info; stock skips it (`CG_DrawTalkerNum`
            // draws only `infoValid` clients, KisakCOD `cgame_mp/cg_newDraw_mp.cpp:2319-2320`).
            if let Some(info) = ui.client(client) {
                h.talkers
                    .push((client, info.name.clone(), team != 0 && team != info.team));
            }
        }
        h.talkers.sort_unstable_by_key(|t| t.0);
        h.team_known = matches!(team, 1 | 2);
        h.team_spectator = team == 3;
        // Nothing says what team the player is on yet: not even "no team", so nobody is an enemy.
        h.team_valid = snap
            .entity(own)
            .is_some_and(|e| team_of(e.eflags).is_some())
            || ui.client(own).is_some();
        h.pm_dead = dead;
        h.radar_enabled = ps.radar_enabled;
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
        if h.sprint_weapon != self.weapons.name(ps.weapon as u16) {
            h.sprint_weapon = self.weapons.name(ps.weapon as u16).to_owned();
        }
        h.sprint_scale = info.sprint_duration_scale;
        h.sprint_time = self.params.player_sprint_time;
        h.sprint_perk_mult = self.params.perk_sprint_multiplier;
        h.perks = ps.perks;
        // The sprint timers are stamped on the usercmd clock.
        let cmd_time = super::cmd_clock(server_time);
        h.sprint_left = sim::pm::sprint_left_ms(ps, &self.params, cmd_time, h.sprint_max);
        h.sprinting = ps.pm_flags & pmf::SPRINTING != 0;
        h.mantle_hint = ps.mantle_state.flags & 8 != 0;
        if ps.cursor_hint != 0 {
            h.cursor_hint = ps.cursor_hint;
            h.cursor_hint_time = now;
            h.cursor_hint_extra.clear();
            h.cursor_hint_text = if ps.cursor_hint > sim::weapon::pickup::WEAPON_HINT_OFFSET {
                // A weapon on the floor or a live grenade (`CG_GetWeaponUseString`).
                let w = u16::from(ps.cursor_hint - sim::weapon::pickup::WEAPON_HINT_OFFSET);
                let info = self.weapons.info(w);
                let key = if info.inventory_type != sim::weapon::InventoryType::Primary {
                    if info.offhand_class == OffhandClass::Frag {
                        "&PLATFORM_THROWBACKGRENADE"
                    } else {
                        h.cursor_hint_extra = self.weapon_display_name(w);
                        "&PLATFORM_PICKUPNEWWEAPON"
                    }
                } else {
                    h.cursor_hint_extra = self.weapon_display_name(w);
                    if inv.primary_count(&self.weapons) >= 2 {
                        "&PLATFORM_SWAPWEAPONS"
                    } else {
                        "&PLATFORM_PICKUPNEWWEAPON"
                    }
                };
                key.to_owned()
            } else if ps.cursor_hint_string >= 0 {
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
                // `ActionSlotIsActive`: a night vision slot is lit while the player looks through the goggles.
                active: match kind {
                    sim::pm::action_slot::WEAPON => target == ps.weapon as u16,
                    sim::pm::action_slot::NIGHT_VISION => ps.weapon_flags & wf::NIGHTVISION != 0,
                    _ => false,
                },
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
        // The breath hint is about the weapon on show (`BG_GetViewmodelWeaponIndex`).
        let shown = sim::pm::viewmodel_weapon(ps);
        let def = self.lib.content.weapon(self.weapons.name(shown));
        h.breath_hint = ps.weapon_flags & wf::HOLD_BREATH == 0
            && ps.weapon_pos_frac == 1.0
            && def.is_some_and(|d| d.overlay_reticle != 0)
            && self.weapons.info(shown).weap_class != WeaponClass::Item;

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
        h.note_spawn(ps.client_num, ps.spawn_count);
        (h.prev_health, h.prev_stance, h.prev_ammo, h.prev_offhand) =
            (h.health, Some(h.stance), ammo_now, off_now);
        (h.prev_origin, h.prev_weapon, h.prev_sprint_left) = (ps.origin, ammo_now.0, h.sprint_left);
        self.hint_events(&mut g.hud, first, now);

        g.hud.map = MapInfo::parse(ui.config(cs::MINIMAP), ui.config(cs::NORTHYAW));
        let own_team = team;
        self.update_actors(&mut g.hud, server_time, own, own_team, now);
    }

    /// The "no ammo" style hints, from the own player's predicted events not yet shown.
    fn hint_events(&self, h: &mut HudFacts, first: bool, now: i32) {
        if first {
            return;
        }
        for &(event, _) in &self.own_new {
            let key = match event {
                ev::NOAMMO => "WEAPON_NO_AMMO",
                ev::NO_FRAG_GRENADE_HINT => "WEAPON_NO_FRAG_GRENADE",
                ev::NO_SPECIAL_GRENADE_HINT => "WEAPON_NO_SPECIAL_GRENADE",
                _ => continue,
            };
            h.invalid_cmd = Some((key, now));
        }
    }

    /// Other players for the compass: where they are and which way they face, and when they fire.
    fn update_actors(&self, h: &mut HudFacts, server_time: i32, own: u16, own_team: u8, now: i32) {
        let ents = self
            .net
            .snaps
            .interpolate_with_actors(server_time - net::view::INTERP_DELAY_MS, Some(own));
        for e in ents.iter().filter(|e| e.etype == etype::PLAYER) {
            // The dead are not on the radar; they leave the compass at once.
            if e.eflags & eflags::DEAD != 0 {
                h.actors.remove(&e.client);
                continue;
            }
            if self
                .net
                .ui_ref()
                .and_then(|ui| ui.client(e.client))
                .is_some_and(|c| c.team == 3)
            {
                h.actors.remove(&e.client);
                continue;
            }
            let theirs = team_of(e.eflags).map(|t| if t == Team::Axis { 1 } else { 2 });
            let a = h.actors.entry(e.client).or_insert_with(|| Actor {
                event_seq: e.event_seq,
                ..Actor::default()
            });
            a.friendly = own_team != 0 && theirs == Some(own_team);
            a.prev_pos = std::mem::replace(&mut a.pos, [e.origin[0], e.origin[1]]);
            if a.last_update == 0 {
                a.prev_pos = a.pos;
            }
            a.perks = e.perks;
            a.yaw = e.angles[1];
            a.last_update = now;
            let pinged = note_pings(
                a,
                e.eflags,
                e.event_seq,
                &e.recent_events(),
                self.weapons
                    .get(e.weapon)
                    .is_some_and(WeaponInfo::pings_compass),
                now,
                |seq| self.sound.fire_length_ms(e.number, seq),
            );
            if !a.friendly {
                h.enemy_flag_pings += u32::from(pinged.flag);
                h.enemy_fire_pings += u32::from(pinged.shot);
                if pinged.shot {
                    h.last_fire_actor = e.client;
                    (h.last_fire_mark_ms, h.last_fire_gone_ms) = (0, 0);
                }
                if pinged.shot || pinged.learned {
                    // Read the audio observation independently of Actor's hold calculation.
                    h.last_fire_played_ms = self
                        .sound
                        .fire_length_ms(e.number, e.event_seq)
                        .unwrap_or(0);
                }
                if pinged.shot || pinged.learned {
                    h.last_fire_ping_ms = a.fire_duration;
                }
            }
        }
        h.actors.retain(|_, a| now - a.last_update < ACTOR_KEEP_MS);
        h.vehicles.clear();
        for e in ents
            .iter()
            .filter(|e| matches!(e.etype, etype::VEHICLE | etype::PLANE))
        {
            let enemy = vehicle_is_enemy(own_team, own, e.client, e.eflags);
            h.vehicles.push(CompassVehicle {
                plane: e.etype == etype::PLANE,
                pos: [e.origin[0], e.origin[1]],
                yaw: e.angles[1],
                enemy,
            });
        }
        h.vehicles_max = h.vehicles_max.max(h.vehicles.len() as u32);
        h.enemy_vehicles_max = h
            .enemy_vehicles_max
            .max(h.vehicles.iter().filter(|v| v.enemy).count() as u32);
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

    /// The weapon's display name as a localize reference (`&WEAPON_AK47`), empty when it has none.
    fn weapon_display_name(&self, index: u16) -> String {
        self.lib
            .content
            .weapon(self.weapons.name(index))
            .and_then(|d| d.display_name.as_deref().map(|n| format!("&{n}")))
            .unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vehicle_is_the_enemys_by_team_or_in_free_for_all_by_owner() {
        // Allies (2) viewing: an axis helicopter is an enemy, an allied one is not.
        assert!(vehicle_is_enemy(2, 0, 5, eflags::TEAM_AXIS));
        assert!(!vehicle_is_enemy(2, 0, 5, eflags::TEAM_ALLIES));
        // A vehicle with no team is nobody's friend in a team match.
        assert!(vehicle_is_enemy(1, 0, 5, 0));
        // Free for all: only the viewer's own.
        assert!(vehicle_is_enemy(0, 0, 5, 0));
        assert!(!vehicle_is_enemy(0, 5, 5, 0));
    }

    const SHOT: [(u8, u8); 4] = [(0, 0), (0, 0), (0, 0), (ev::FIRE_WEAPON, 0)];

    #[test]
    fn only_an_unsilenced_non_grenade_shot_pings_for_its_sound_length() {
        let mut a = Actor::default();
        // A silenced shot or a grenade: no ping.
        note_pings(&mut a, 0, 1, &SHOT, false, 1000, |_| Some(400));
        assert_eq!(a.fire_time, 0);
        // An unsilenced shot: as long as its fire sound.
        note_pings(&mut a, 0, 2, &SHOT, true, 2000, |s| (s == 2).then_some(400));
        assert_eq!((a.fire_time, a.fire_duration), (2000, 400));
        assert_eq!(a.fire_alpha(2400, 2000.0), Some(1.0));
        assert_eq!(a.fire_alpha(3400, 2000.0), Some(0.5));
        assert!(a.fire_alpha(4400, 2000.0).is_some() && a.fire_alpha(4401, 2000.0).is_none());
        // The same events looked at again do not ping again.
        note_pings(&mut a, 0, 2, &SHOT, true, 2500, |_| Some(400));
        assert_eq!(a.fire_time, 2000);
    }

    #[test]
    fn a_shot_heard_later_learns_its_sound_length_and_unknown_means_zero_hold() {
        let mut a = Actor::default();
        note_pings(&mut a, 0, 3, &SHOT, true, 1000, |_| None);
        assert_eq!(a.fire_duration, 0);
        note_pings(&mut a, 0, 3, &SHOT, true, 1016, |s| (s == 3).then_some(650));
        assert_eq!((a.fire_time, a.fire_duration), (1000, 650));
        // Never heard: fade immediately, without a hold.
        let mut b = Actor::default();
        note_pings(&mut b, 0, 1, &SHOT, true, 1000, |_| None);
        note_pings(&mut b, 0, 1, &SHOT, true, 1016, |_| None);
        assert_eq!(b.fire_duration, 0);
        assert_eq!(b.fire_alpha(2000, 2000.0), Some(0.5));
        assert!(b.fire_alpha(3001, 2000.0).is_none());
    }

    #[test]
    fn ping_player_still_pings_at_most_every_three_seconds() {
        let mut a = Actor::default();
        note_pings(&mut a, eflags::PING, 0, &SHOT, false, 1000, |_| None);
        assert_eq!((a.fire_time, a.fire_duration), (1000, 0));
        note_pings(&mut a, eflags::PING, 0, &SHOT, false, 3999, |_| None);
        assert_eq!(a.fire_time, 1000);
        note_pings(&mut a, eflags::PING, 0, &SHOT, false, 4000, |_| None);
        assert_eq!(a.fire_time, 4000);
    }
}
