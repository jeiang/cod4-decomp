// SPDX-License-Identifier: GPL-3.0-only
// Flashbang, the flinch direction, radius damage and CanDamage, G_Damage and the player damage and death paths translated in part from KisakCOD (game_mp/g_combat_mp.cpp, game_mp/g_client_script_cmd_mp.cpp, game_mp/g_scr_main_mp.cpp, game/g_missile.cpp; GPL-3.0, copyright the KisakCOD contributors and Activision).
//! Damage: `G_Damage`, the player damage and death paths, radius damage and damage volumes.
//!
//! Scripts own the rules. The engine's part is the order of events: damage reaches
//! `CodeCallback_PlayerDamage`, the script calls `finishPlayerDamage` to apply it (health,
//! knockback, view flinch), and a player at zero health dies: `death` is notified and
//! `CodeCallback_PlayerKilled` runs. Callbacks are queued as [`ScriptCall`]s; the host runs them
//! right after the builtin or frame step that raised them.

use gsc::{Value, Vm};
use sim::Vec3;
use sim::cm::{ENTITYNUM_NONE, ENTITYNUM_WORLD};
use sim::contents;
use sim::pm::damage::{DAMAGE_COUNT_MS, direction_bytes};
use sim::pm::{PmType, pmf};

use crate::bullet::{dot, length, sub};
use crate::client::{Session, Team};
use crate::game::{Ent, EntKind, Game, ScriptCall};

/// How long a player the Last Stand perk saved is immune to damage.
const LAST_STAND_GRACE_MS: i32 = 500;
/// The death animation length when the body animations are not loaded.
const DEFAULT_DEATH_ANIM_MS: i32 = 1200;

/// `meansOfDeath_t` in the original's order.
pub const MODS: [&str; 16] = [
    "MOD_UNKNOWN",
    "MOD_PISTOL_BULLET",
    "MOD_RIFLE_BULLET",
    "MOD_GRENADE",
    "MOD_GRENADE_SPLASH",
    "MOD_PROJECTILE",
    "MOD_PROJECTILE_SPLASH",
    "MOD_MELEE",
    "MOD_HEAD_SHOT",
    "MOD_CRUSH",
    "MOD_TELEFRAG",
    "MOD_FALLING",
    "MOD_SUICIDE",
    "MOD_TRIGGER_HURT",
    "MOD_EXPLOSIVE",
    "MOD_IMPACT",
];

pub const MOD_UNKNOWN: u8 = 0;
pub const MOD_PISTOL_BULLET: u8 = 1;
pub const MOD_RIFLE_BULLET: u8 = 2;
pub const MOD_GRENADE: u8 = 3;
pub const MOD_GRENADE_SPLASH: u8 = 4;
pub const MOD_PROJECTILE: u8 = 5;
pub const MOD_PROJECTILE_SPLASH: u8 = 6;
pub const MOD_MELEE: u8 = 7;
pub const MOD_HEAD_SHOT: u8 = 8;
pub const MOD_FALLING: u8 = 11;
pub const MOD_SUICIDE: u8 = 12;
pub const MOD_TRIGGER_HURT: u8 = 13;
pub const MOD_EXPLOSIVE: u8 = 14;

/// `DAMAGE_*` flags scripts see as `iDFlags`.
pub mod dflags {
    pub const RADIUS: i32 = 1;
    pub const NO_ARMOR: i32 = 2;
    pub const NO_KNOCKBACK: i32 = 4;
    pub const PENETRATION: i32 = 8;
}

pub fn mod_from_name(s: &str) -> Option<u8> {
    MODS.iter().position(|m| *m == s).map(|i| i as u8)
}

/// Hit location names and their indices (`hitLocation_t`).
pub const HITLOC_NONE: u8 = 0;
pub const HITLOC_HEAD: u8 = 2;
pub const HITLOCS: [&str; 19] = [
    "none",
    "helmet",
    "head",
    "neck",
    "torso_upper",
    "torso_lower",
    "right_arm_upper",
    "left_arm_upper",
    "right_arm_lower",
    "left_arm_lower",
    "right_hand",
    "left_hand",
    "right_leg_upper",
    "left_leg_upper",
    "right_leg_lower",
    "left_leg_lower",
    "right_foot",
    "left_foot",
    "gun",
];

pub fn hitloc_from_name(s: &str) -> Option<u8> {
    HITLOCS.iter().position(|m| *m == s).map(|i| i as u8)
}

/// One explosion (`G_RadiusDamage` arguments): damage falls linearly from `inner` at the centre
/// to `outer` at `radius`.
#[derive(Debug, Clone)]
pub struct Blast {
    pub origin: Vec3,
    pub radius: f32,
    pub inner: f32,
    pub outer: f32,
    pub attacker: Option<u16>,
    pub inflictor: Option<u16>,
    /// Only what lies inside the cone `(cosine of the half angle, axis)` is hurt.
    pub cone: Option<(f32, Vec3)>,
    /// An entity the blast skips, e.g. the one a rocket hit directly.
    pub ignore: Option<u16>,
    /// Players take no damage (`setplayerignoreradiusdamage`); other entities still do.
    pub skip_clients: bool,
    pub mean: u8,
    pub weapon: u32,
}

/// One damage event (`G_Damage` arguments).
#[derive(Debug, Clone)]
pub struct Damage {
    pub inflictor: Option<u16>,
    pub attacker: Option<u16>,
    pub dir: Option<Vec3>,
    pub point: Option<Vec3>,
    pub damage: i32,
    pub flags: i32,
    pub mean: u8,
    /// Weapon index; 0 for none.
    pub weapon: u32,
    pub hitloc: u8,
    pub time_offset: i32,
}

impl Damage {
    pub fn new(damage: i32, mean: u8) -> Self {
        Self {
            inflictor: None,
            attacker: None,
            dir: None,
            point: None,
            damage,
            flags: 0,
            mean,
            weapon: 0,
            hitloc: HITLOC_NONE,
            time_offset: 0,
        }
    }
}

fn vec_or_zero(v: Option<Vec3>) -> Value {
    Value::Vector(v.unwrap_or([0.0; 3]))
}

fn normalize(v: Vec3) -> Vec3 {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l == 0.0 {
        [0.0; 3]
    } else {
        [v[0] / l, v[1] / l, v[2] / l]
    }
}

/// `vectoyaw`.
fn vec_to_yaw(v: Vec3) -> f32 {
    if v[0] == 0.0 && v[1] == 0.0 {
        return 0.0;
    }
    let y = v[1].atan2(v[0]).to_degrees();
    if y < 0.0 { y + 360.0 } else { y }
}

/// The two doses of a flashbang (`FlashbangBlastEnt`): how near the blast is (1 within `radius_min`, falling to
/// 0 at `radius_max`) and how squarely the player looks at it (1 facing it, 0 turned away).
pub fn flashbang_percents(
    dist: f32,
    radius_min: f32,
    radius_max: f32,
    viewangles: Vec3,
    eye: Vec3,
    blast: Vec3,
) -> (f32, f32) {
    let distance = if radius_min < dist {
        1.0 - (dist - radius_min) / (radius_max - radius_min)
    } else {
        1.0
    };
    let forward = sim::pm::math::angle_vectors(&viewangles).0;
    let to_blast = normalize(sub(blast, eye));
    (distance, (dot(forward, to_blast) + 1.0) * 0.5)
}

/// What a blast is stopped by (`0x802011`): the map, glass, shot clips and vehicles.
pub(crate) const RADIUS_DAMAGE_MASK: i32 =
    contents::SOLID | contents::GLASS | contents::CLIPSHOT | contents::VEHICLE;

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// The points `CanDamage` samples on a player: the middle of the body and four more, half a body height above and
/// below it and 15 units to either side of it across the line from the blast.
fn client_sample_points(origin: Vec3, eye: Vec3, centre: Vec3) -> [Vec3; 5] {
    let half = (eye[2] - origin[2]) * 0.5;
    let ahead = normalize([centre[0] - origin[0], centre[1] - origin[1], 0.0]);
    let right = [-ahead[1], ahead[0]];
    let mid = [0, 1, 2].map(|i| (eye[i] + origin[i]) * 0.5);
    let at = |side: f32, up: f32| {
        [
            mid[0] + 15.0 * side * right[0],
            mid[1] + 15.0 * side * right[1],
            mid[2] + up * half,
        ]
    };
    [
        mid,
        at(1.0, 1.0),
        at(1.0, -1.0),
        at(-1.0, 1.0),
        at(-1.0, -1.0),
    ]
}

/// The points `CanDamage` samples on anything else: the middle of its box and the four corners of the box's
/// outline as seen from the blast.
fn box_sample_points(mins: Vec3, maxs: Vec3, centre: Vec3) -> [Vec3; 5] {
    let mid = [0, 1, 2].map(|i| (mins[i] + maxs[i]) * 0.5);
    let toward = normalize(sub(centre, mid));
    let side = normalize([-toward[1], toward[0], 0.0]);
    let up = cross(toward, side);
    let corner = sub(maxs, mid);
    let reach_side = (corner[0] * side[0]).abs() + (corner[1] * side[1]).abs();
    let reach_up =
        (corner[0] * up[0]).abs() + (corner[1] * up[1]).abs() + (corner[2] * up[2]).abs();
    let at = |s: f32, u: f32| {
        [0, 1, 2].map(|i| mid[i] + s * reach_side * side[i] + u * reach_up * up[i])
    };
    [
        mid,
        at(1.0, 1.0),
        at(-1.0, 1.0),
        at(1.0, -1.0),
        at(-1.0, -1.0),
    ]
}

/// How much of a target `seen` of its five sample points cover: a player a third per point (all of it from four),
/// anything else wholly from the first.
fn coverage_fraction(player: bool, seen: usize) -> f32 {
    match seen {
        0 => 0.0,
        s if !player || s > 3 => 1.0,
        s => s as f32 / 3.0,
    }
}

/// `G_GetRadiusDamageDistanceSquared`: from the blast to a brush model's box, to anything else's origin.
fn radius_distance_squared(e: &Ent, blast: Vec3) -> f32 {
    let v: Vec3 = if e.brush_model.is_some() {
        [0, 1, 2].map(|i| {
            let (lo, hi) = (e.origin[i] + e.mins[i], e.origin[i] + e.maxs[i]);
            if lo > blast[i] {
                lo - blast[i]
            } else if hi < blast[i] {
                blast[i] - hi
            } else {
                0.0
            }
        })
    } else {
        sub(e.origin, blast)
    };
    dot(v, v)
}

impl Game {
    fn ent_obj(&self, vm: &mut Vm, n: Option<u16>) -> Value {
        match n {
            Some(n) if self.ent(n).is_some() => self.entity_value(vm, n),
            _ => Value::Undefined,
        }
    }

    /// `G_Damage`.
    pub fn g_damage(&mut self, vm: &mut Vm, target: u16, d: Damage) {
        let Some(t) = self.ent(target) else { return };
        if t.kind == EntKind::Client {
            if d.attacker
                .and_then(|a| self.ent(a))
                .is_some_and(|a| a.veh.is_some())
            {
                self.stats.heli_hits += 1;
            }
            self.damage_client(vm, target, d);
            return;
        }
        // Damage reaches nothing that is not damageable or that cannot be hurt (`FL_GODMODE`).
        if !t.takedamage || t.flags & 1 != 0 {
            return;
        }
        let mut damage = d.damage.max(1);
        let t = self.ent_mut(target).expect("checked above");
        if t.flags & 2 != 0 && t.health - damage <= 0 {
            damage = t.health - 1;
        }
        t.health -= damage;
        let health = t.health;
        let attacker = self.ent_obj(vm, d.attacker.or(Some(ENTITYNUM_WORLD)));
        // `waittill("damage", amount, attacker, direction, point, type, model, tag, part, flags)`; the direction is
        // the one the damage was dealt with, unnormalised. Model, tag and part name what a trace hit of an attached
        // model or body part; nothing here traces those, so they are empty, as for any hit without one.
        vm.notify_entity(
            target,
            "damage",
            &[
                Value::Int(damage),
                attacker.clone(),
                vec_or_zero(d.dir),
                vec_or_zero(d.point),
                Value::str(MODS[usize::from(d.mean)]),
                Value::str(""),
                Value::str(""),
                Value::str(""),
                Value::Int(d.flags),
            ],
        );
        if health <= 0 {
            if let Some(t) = self.ent_mut(target) {
                t.health = t.health.max(-999);
            }
            vm.notify_entity(target, "death", &[attacker]);
        }
        if self
            .ent(target)
            .is_some_and(|e| &*e.classname == "trigger_damage")
        {
            let by = d.attacker.unwrap_or(ENTITYNUM_WORLD);
            self.trigger_damage_hit(vm, target, by, damage, d.mean);
        }
    }

    /// `G_DamageClient`: scale by the hit location and hand to the script.
    fn damage_client(&mut self, vm: &mut Vm, target: u16, d: Damage) {
        let Some(c) = self.client(target) else { return };
        let takes = self.ent(target).is_some_and(|e| e.takedamage);
        if !takes
            || d.damage <= 0
            || c.noclip
            || c.ufo
            || !c.connected()
            || c.ps.pm_type == PmType::Dead
        {
            return;
        }
        let weapon = if d.weapon != 0 {
            d.weapon
        } else {
            d.inflictor
                .or(d.attacker)
                .and_then(|e| self.client(e))
                .map_or(0, |c| c.ps.weapon)
        };
        let mut damage = d.damage;
        if d.mean != MOD_MELEE {
            damage = (self.hitloc_multiplier(weapon, d.hitloc) * damage as f32) as i32;
        }
        if damage <= 0 {
            damage = 1;
        }
        let Some(func) = self.callbacks.player_damage else {
            return;
        };
        let args = vec![
            self.ent_obj(vm, d.inflictor),
            self.ent_obj(vm, d.attacker),
            Value::Int(damage),
            Value::Int(d.flags),
            Value::str(MODS[usize::from(d.mean)]),
            Value::str(self.weapon_name(weapon)),
            d.point.map_or(Value::Undefined, Value::Vector),
            d.dir.map_or(Value::Undefined, Value::Vector),
            Value::str(HITLOCS[usize::from(d.hitloc)]),
            Value::Int(d.time_offset),
        ];
        self.calls.push(ScriptCall {
            func,
            this: Some(target),
            args,
        });
    }

    /// Falling: no weapon, no attacker.
    pub fn damage_fall(&mut self, vm: &mut Vm, n: u16, damage: i32) {
        self.g_damage(vm, n, Damage::new(damage, MOD_FALLING));
    }

    /// `PlayerCmd_finishPlayerDamage`: applies one damage event the script accepted.
    pub fn finish_player_damage(
        &mut self,
        vm: &mut Vm,
        target: u16,
        d: Damage,
    ) -> Result<(), String> {
        let Some(c) = self.client(target) else {
            return Err(format!("entity {target} is not a player"));
        };
        // A player the Last Stand perk just saved takes nothing at all for a moment.
        if c.last_stand && c.last_stand_time > self.level.time {
            return Ok(());
        }
        let mut damage = d.damage;
        if damage <= 0 {
            return Ok(());
        }
        if c.ps.pm_type == PmType::Dead {
            // The original runs each damage callback to completion before the next hit, so
            // a dead target cannot be reached; here pellets and blasts queue several
            // callbacks first, and the ones after the killing blow have nothing to do.
            return Ok(());
        }
        let dir = d.dir.map_or([0.0; 3], normalize);
        let god = self.ent(target).is_some_and(|e| e.flags & 8 != 0);
        if !god && d.flags & dflags::NO_KNOCKBACK == 0 {
            let c = self.client_mut(target).expect("checked above");
            let mod_ = if c.ps.pm_flags & pmf::PRONE != 0 {
                0.02
            } else if c.ps.pm_flags & pmf::DUCKED != 0 {
                0.15
            } else {
                0.3
            };
            let knockback = ((damage as f32 * mod_) as i32).min(60);
            if knockback != 0 {
                let scale = knockback as f32 * self.cvars.float("g_knockback") / 250.0;
                let c = self.client_mut(target).expect("checked above");
                for (v, k) in c.ps.velocity.iter_mut().zip(dir) {
                    *v += k * scale;
                }
                if c.ps.pm_time == 0 {
                    c.ps.pm_time = (2 * knockback).clamp(50, 200);
                    c.ps.pm_flags |= pmf::TIME_KNOCKBACK;
                }
            }
        }
        let invulnerable = self.ent(target).is_some_and(|e| e.flags & 1 != 0);
        if invulnerable {
            return Ok(());
        }
        let per_point = self.cvars.float("player_dmgtimer_timePerPoint");
        let max_time = self.cvars.float("player_dmgtimer_maxTime");
        let now = self.level.time;
        let c = self.client_mut(target).expect("checked above");
        c.ps.damage_timer += (damage as f32 * per_point) as i32;
        c.ps.damage_timer = c.ps.damage_timer.min(max_time as i32);
        c.ps.damage_duration = c.ps.damage_timer;
        // `flinchYawAnim`: the blow's direction relative to the way the victim faces.
        c.ps.flinch_yaw_anim = d.dir.map_or(0, |v| {
            let yaw = sim::pm::math::vec_to_yaw(&v);
            let facing = c.ps.viewangles[1].rem_euclid(360.0).trunc();
            flinch_yaw_anim(yaw - facing)
        });
        // What the end frame shows the player: how much, and which way it came (`damage_blood`, `damage_from`).
        c.damage_blood += damage;
        c.damage_from_world = d.dir.is_none();
        c.damage_from = dir;
        let e = self.ent_mut(target).expect("client entity");
        if e.flags & 2 != 0 && e.health - damage <= 0 {
            damage = e.health - 1;
        }
        e.health -= damage;
        let health = e.health;
        let attacker = self.ent_obj(vm, d.attacker);
        // `waittill("damage", amount, attacker)`.
        vm.notify_entity(target, "damage", &[Value::Int(damage), attacker]);
        let at = self.ent(target).map_or([0.0; 3], |e| e.origin);
        self.tempev.add(now, crate::tempev::ev::PLAYER_PAIN, |s| {
            s.origin = at;
            s.client = target;
            s.event_parm = damage.clamp(0, 255) as u8;
        });
        if health > 0 {
            return Ok(());
        }
        let last_stand_perk = self
            .client(target)
            .is_some_and(|c| !c.last_stand && c.ps.perks & 0x80 != 0);
        if last_stand_perk {
            let until = self.level.time + LAST_STAND_GRACE_MS;
            let c = self.client_mut(target).expect("checked above");
            c.last_stand = true;
            c.last_stand_time = until;
            if let Some(func) = self.callbacks.player_last_stand {
                let args = self.damage_callback_args(vm, &d, damage);
                self.calls.push(ScriptCall {
                    func,
                    this: Some(target),
                    args,
                });
            }
            return Ok(());
        }
        if let Some(e) = self.ent_mut(target) {
            e.health = e.health.max(-999);
        }
        self.player_die(vm, target, &d, damage);
        Ok(())
    }

    /// `P_DamageFeedback`, each end frame: the health the player lost since the last one becomes a wider aim spread
    /// and a hit for the client to show (`damage_event`, `damage_count`, `damage_yaw`, `damage_pitch`), and the
    /// count of the last hit expires after half a second.
    pub(crate) fn damage_feedback(&mut self, n: u16) {
        let now = self.level.time;
        let Some(c) = self.client_mut(n) else { return };
        if c.ps.pm_type >= PmType::Dead {
            return;
        }
        if now - c.damage_time > DAMAGE_COUNT_MS {
            c.ps.damage_count = 0;
        }
        if c.damage_blood <= 0 || c.max_health <= 0 {
            return;
        }
        let percent = (100 * c.damage_blood / c.max_health).min(127);
        c.ps.aim_spread_scale = (c.ps.aim_spread_scale + percent as f32).min(255.0);
        // The direction bytes name the way the blow travelled; a blow from nowhere is the pair 255, 255.
        let from = (!c.damage_from_world).then_some(c.damage_from);
        let kick = sim::pm::damage::view_kick(percent);
        c.v_dmg = match from {
            None => [-kick, 0.0],
            Some(d) => {
                let (fwd, right, _) = sim::pm::math::angle_vectors(&c.ps.viewangles);
                let dot = |v: [f32; 3]| d[0] * v[0] + d[1] * v[1] + d[2] * v[2];
                [dot(fwd) * kick, dot(right) * -kick]
            }
        };
        (c.ps.damage_pitch, c.ps.damage_yaw) = direction_bytes(from);
        c.damage_from_world = false;
        c.ps.damage_event = c.ps.damage_event.wrapping_add(1);
        c.ps.damage_count = percent;
        c.damage_time = now - 20;
        c.damage_blood = 0;
    }

    fn damage_callback_args(&mut self, vm: &mut Vm, d: &Damage, damage: i32) -> Vec<Value> {
        vec![
            self.ent_obj(vm, d.inflictor),
            self.ent_obj(vm, d.attacker),
            Value::Int(damage),
            Value::str(MODS[usize::from(d.mean)]),
            Value::str(self.weapon_name(d.weapon)),
            d.dir.map_or(Value::Undefined, Value::Vector),
            Value::str(HITLOCS[usize::from(d.hitloc)]),
            Value::Int(d.time_offset),
            Value::Int(0),
        ]
    }

    /// `BG_AnimScriptEvent(ANIM_ET_DEATH)`: the length in milliseconds of the death animation of `n`; a server
    /// without the body animations keeps a corpse pose of [`DEFAULT_DEATH_ANIM_MS`].
    fn death_anim_ms(&mut self, n: u16) -> i32 {
        self.ensure_player_anims();
        let anims = self.player_anims.as_deref();
        anims
            .zip(self.client(n))
            .and_then(|(a, c)| c.pose.death_duration_ms(a))
            .unwrap_or(DEFAULT_DEATH_ANIM_MS)
    }

    /// `player_die`: notify, switch to the dead movement type and run the killed callback.
    pub fn player_die(&mut self, vm: &mut Vm, n: u16, d: &Damage, damage: i32) {
        if self.alive_for_death(n) {
            self.death_grenade_drop(vm, n, d.mean == MOD_SUICIDE);
        }
        let Some(c) = self.client_mut(n) else { return };
        if c.ps.pm_type >= PmType::Noclip && c.ps.pm_type != PmType::LastStand {
            return;
        }
        c.ps.pm_type = PmType::Dead;
        c.ps.weapon_state = 0;
        self.stats.deaths += 1;
        let (at, push) = (
            self.ent(n).map_or([0.0; 3], |e| e.origin),
            d.dir.map_or([0.0; 3], normalize),
        );
        let now = self.level.time;
        self.tempev.add(now, crate::tempev::ev::PLAYER_DEATH, |s| {
            s.origin = at;
            s.client = n;
            s.velocity = [push[0] * 240.0, push[1] * 240.0, push[2] * 240.0];
        });
        if d.attacker.is_some_and(|a| a != n && self.is_client(a)) {
            self.stats.kills += 1;
        }
        let attacker = self.ent_obj(vm, d.attacker);
        vm.notify_entity(n, "death", std::slice::from_ref(&attacker));
        // How long the death animation plays: what the body animation of the player picks to die in.
        let mut args = self.damage_callback_args(vm, d, damage);
        args[8] = Value::Int(self.death_anim_ms(n));
        if let Some(func) = self.callbacks.player_killed {
            self.calls.push(ScriptCall {
                func,
                this: Some(n),
                args,
            });
        }
        let toward = d
            .attacker
            .filter(|a| *a != n && *a != ENTITYNUM_WORLD)
            .or(d.inflictor.filter(|a| *a != n && *a != ENTITYNUM_WORLD))
            .and_then(|a| self.ent(a).map(|e| e.origin));
        let my = self.ent(n).map_or([0.0; 3], |e| e.origin);
        let yaw = toward.map_or(self.client(n).map_or(0.0, |c| c.ps.viewangles[1]), |p| {
            vec_to_yaw([p[0] - my[0], p[1] - my[1], 0.0])
        });
        if let Some(c) = self.client_mut(n) {
            c.ps.dead_yaw = yaw as i32;
            c.ps.viewangles[1] = yaw;
            c.ps.viewangles[2] = 0.0;
        }
        if let Some(e) = self.ent_mut(n) {
            e.takedamage = true;
            e.contents = contents::CORPSE;
            e.maxs[2] = 30.0;
            e.health = 0;
        }
        self.relink(n);
    }

    /// `CanDamage`: how much of `target` a blast at `centre` reaches, from 0 to 1. Five points of the target are
    /// sampled and each counts when a line from `centre` to it is clear of `mask` (and, with a `cone`, lies inside
    /// it: cosine of the half angle and axis). A player is covered a third per point seen, any other entity wholly
    /// by one point.
    pub fn damage_coverage(
        &self,
        target: u16,
        centre: Vec3,
        cone: Option<(f32, Vec3)>,
        inflictor: u16,
        mask: i32,
    ) -> f32 {
        let (Some(w), Some(e)) = (self.world.as_ref(), self.ent(target)) else {
            return 0.0;
        };
        let client = self.client(target);
        let points = match client {
            Some(c) => {
                let eye = [
                    c.ps.origin[0],
                    c.ps.origin[1],
                    c.ps.origin[2] + c.ps.view_height_current,
                ];
                client_sample_points(e.origin, eye, centre)
            }
            None => box_sample_points(
                [0, 1, 2].map(|i| e.origin[i] + e.mins[i]),
                [0, 1, 2].map(|i| e.origin[i] + e.maxs[i]),
                centre,
            ),
        };
        use sim::cm::Collide;
        let seen = points
            .iter()
            .filter(|p| {
                if let Some((cos, axis)) = cone
                    && cos > dot(normalize(sub(**p, centre)), axis)
                {
                    return false;
                }
                let t = w.trace(centre, **p, [0.0; 3], [0.0; 3], inflictor, mask);
                t.fraction >= 1.0 || t.hit_id == target
            })
            .count();
        coverage_fraction(client.is_some(), seen)
    }

    /// `G_RadiusDamage`: everything in range that the blast reaches takes damage falling from `inner` at the centre
    /// to `outer` at `radius`, by distance from the blast to the entity's origin (to its box for a brush model)
    /// and scaled by how much of it the blast reaches. Returns whether it hurt an enemy player.
    pub fn blast(&mut self, vm: &mut Vm, b: &Blast) -> bool {
        // The original does nothing for a blast without an attacker.
        let Some(attacker) = b.attacker else {
            return false;
        };
        let radius = b.radius.max(1.0);
        let mut hit = false;
        let targets: Vec<u16> = self
            .in_use()
            .filter(|(n, e)| e.takedamage && Some(*n) != b.ignore && *n < ENTITYNUM_WORLD)
            .map(|(n, _)| n)
            .collect();
        for t in targets {
            let Some(e) = self.ent(t) else { continue };
            if b.skip_clients && e.kind == EntKind::Client {
                continue;
            }
            let dist2 = radius_distance_squared(e, b.origin);
            if dist2 >= radius * radius {
                continue;
            }
            let dist = dist2.sqrt();
            let mut dir = sub(e.origin, b.origin);
            dir[2] += 24.0;
            if let Some(c) = self.client(t)
                && (!c.connected() || c.session != Session::Playing)
            {
                continue;
            }
            let reach = self.damage_coverage(
                t,
                b.origin,
                b.cone,
                b.inflictor.unwrap_or(ENTITYNUM_NONE),
                RADIUS_DAMAGE_MASK,
            );
            if reach <= 0.0 {
                continue;
            }
            hit |= self.is_accurate_hit(t, attacker);
            let points = ((b.inner - b.outer) * (1.0 - dist / radius) + b.outer) * reach;
            let mut dmg = Damage::new(points as i32, b.mean);
            dmg.attacker = b.attacker;
            dmg.inflictor = b.inflictor;
            dmg.dir = Some(dir);
            dmg.point = Some(b.origin);
            dmg.flags = dflags::RADIUS | dflags::NO_KNOCKBACK;
            dmg.weapon = b.weapon;
            dmg.hitloc = HITLOC_NONE;
            self.g_damage(vm, t, dmg);
        }
        hit
    }

    /// `G_FlashbangBlast`: every player within `radius_max` that the blast can see hears of it through
    /// `self waittill("flashbang", distance, angle, attacker, team)`; the stock script turns that into the
    /// shell shock. `radius_min` and below is a full dose.
    #[allow(clippy::too_many_arguments)]
    pub fn flashbang_blast(
        &mut self,
        vm: &mut Vm,
        origin: Vec3,
        radius_max: f32,
        radius_min: f32,
        attacker: Option<u16>,
        team: Team,
    ) {
        let radius_min = radius_min.max(1.0);
        let radius_max = radius_max.max(radius_min);
        let targets: Vec<u16> = self
            .in_use()
            .filter(|(_, e)| e.kind == EntKind::Client && e.takedamage && e.health > 0)
            .map(|(n, _)| n)
            .collect();
        for n in targets {
            let Some(c) = self.client(n) else { continue };
            if !c.connected() || c.session != Session::Playing {
                continue;
            }
            let Some(e) = self.ent(n) else { continue };
            let dist = length(sub(e.origin, origin));
            // The thrower is not in the way, and sky blocks the flash as well as walls (mask 2049).
            let ignore = attacker.unwrap_or(ENTITYNUM_NONE);
            let mask = contents::SOLID | contents::SKY;
            if dist > radius_max || self.damage_coverage(n, origin, None, ignore, mask) <= 0.0 {
                continue;
            }
            let eye = [
                c.ps.origin[0],
                c.ps.origin[1],
                c.ps.origin[2] + c.ps.view_height_current,
            ];
            let (distance, angle) =
                flashbang_percents(dist, radius_min, radius_max, c.ps.viewangles, eye, origin);
            let args = [
                Value::Float(distance),
                Value::Float(angle),
                self.ent_obj(vm, attacker),
                // `AddScrTeamName`: the free team is "free" here, not "none" as `team` reads.
                Value::str(if team == Team::Free {
                    "free"
                } else {
                    team.name()
                }),
            ];
            vm.notify_entity(n, "flashbang", &args);
        }
    }

    /// `G_GetWeaponHitLocationMultiplier`.
    pub fn hitloc_multiplier(&self, weapon: u32, hitloc: u8) -> f32 {
        sim::weapon::damage::weapon_hit_location_multiplier(
            self.weapons.get(weapon as u16),
            usize::from(hitloc),
            &self.hitloc_table,
        )
    }

    pub fn weapon_name(&self, weapon: u32) -> &str {
        self.weapons.name(weapon as u16)
    }

    pub fn weapon_index(&self, name: &str) -> u32 {
        u32::from(self.weapons.index(name))
    }
}

/// `flinchYawAnim` from the blow's yaw relative to the victim's facing, degrees: 0 forward (pushed along the view),
/// 1 back, 2 left, 3 right.
fn flinch_yaw_anim(relative: f32) -> u8 {
    let r = relative.rem_euclid(360.0);
    if !(45.0..315.0).contains(&r) {
        0
    } else if (135.0..225.0).contains(&r) {
        1
    } else if r < 135.0 {
        2
    } else {
        3
    }
}

#[cfg(test)]
mod tests {
    use super::{
        box_sample_points, client_sample_points, coverage_fraction, flashbang_percents,
        flinch_yaw_anim,
    };

    #[test]
    fn a_flashbang_dose_falls_with_distance_and_with_looking_away() {
        let eye = [0.0; 3];
        // Looking along +x at a blast on the +x axis, then at one behind.
        let (near, face) =
            flashbang_percents(100.0, 200.0, 600.0, [0.0; 3], eye, [100.0, 0.0, 0.0]);
        assert_eq!((near, face), (1.0, 1.0));
        let (mid, away) =
            flashbang_percents(400.0, 200.0, 600.0, [0.0; 3], eye, [-400.0, 0.0, 0.0]);
        assert!((mid - 0.5).abs() < 1e-6 && away.abs() < 1e-6);
        let (edge, side) =
            flashbang_percents(600.0, 200.0, 600.0, [0.0; 3], eye, [0.0, 400.0, 0.0]);
        assert!(edge.abs() < 1e-6 && (side - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_hit_flinches_toward_where_it_came_from() {
        assert_eq!(flinch_yaw_anim(0.0), 0);
        assert_eq!(flinch_yaw_anim(90.0), 2);
        assert_eq!(flinch_yaw_anim(180.0), 1);
        assert_eq!(flinch_yaw_anim(-90.0), 3);
    }

    #[test]
    fn a_player_is_covered_a_third_per_point_seen_and_anything_else_by_one() {
        let player: Vec<f32> = (0..=5).map(|n| coverage_fraction(true, n)).collect();
        assert_eq!(player, [0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0, 1.0, 1.0]);
        let other: Vec<f32> = (0..=5).map(|n| coverage_fraction(false, n)).collect();
        assert_eq!(other, [0.0, 1.0, 1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn the_sample_points_of_a_player_straddle_the_body_across_the_line_from_the_blast() {
        // Feet at the origin, eyes 60 up, the blast along +x: the points spread along y and z.
        let p = client_sample_points([0.0; 3], [0.0, 0.0, 60.0], [100.0, 0.0, 0.0]);
        assert_eq!(p[0], [0.0, 0.0, 30.0]);
        let ys: Vec<f32> = p.iter().map(|p| p[1]).collect();
        let zs: Vec<f32> = p.iter().map(|p| p[2]).collect();
        assert_eq!(ys, [0.0, 15.0, 15.0, -15.0, -15.0]);
        assert_eq!(zs, [30.0, 60.0, 0.0, 60.0, 0.0]);
    }

    #[test]
    fn the_sample_points_of_a_box_are_its_outline_as_the_blast_sees_it() {
        let p = box_sample_points([-10.0, -20.0, 0.0], [10.0, 20.0, 40.0], [100.0, 0.0, 20.0]);
        assert_eq!(p[0], [0.0, 0.0, 20.0]);
        // Seen along x: half the width in y, half the height in z.
        for q in &p[1..] {
            assert!((q[1].abs() - 20.0).abs() < 1e-3 && ((q[2] - 20.0).abs() - 20.0).abs() < 1e-3);
        }
        assert!(p[1][2] > 20.0 && p[3][2] < 20.0);
    }
}
