// SPDX-License-Identifier: GPL-3.0-only
// Flashbang translated in part from KisakCOD (game_mp/g_combat_mp.cpp, game/g_missile.cpp; GPL-3.0, copyright the KisakCOD contributors and Activision).
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
use crate::game::{EntKind, Game, ScriptCall};

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
        if !t.takedamage {
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
        let dir = normalize(d.dir.unwrap_or([0.0; 3]));
        // `waittill("damage", amount, attacker, direction, point, type, ...)`.
        vm.notify_entity(
            target,
            "damage",
            &[
                Value::Int(damage),
                attacker.clone(),
                Value::Vector(dir),
                vec_or_zero(d.point),
                Value::str(MODS[usize::from(d.mean)]),
                Value::str(""),
                Value::str(""),
                Value::str(""),
                Value::Int(d.flags),
            ],
        );
        if health <= 0 {
            vm.notify_entity(target, "death", &[attacker]);
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

    /// `hurt_touch`: a damage volume hurts whoever stands in it.
    pub fn hurt_touch(&mut self, vm: &mut Vm, trigger: u16, who: u16) {
        let Some(t) = self.ent(trigger) else { return };
        // Spawnflag 1 keeps the volume off until a script triggers it.
        if t.spawnflags & 1 != 0 {
            return;
        }
        let dmg = if t.dmg > 0 { t.dmg } else { 5 };
        let mut d = Damage::new(dmg, MOD_TRIGGER_HURT);
        d.attacker = Some(ENTITYNUM_WORLD);
        d.inflictor = Some(ENTITYNUM_WORLD);
        let dmg_flags = t.spawnflags;
        // `dmg` of a volume that kills outright (spawnflag 0x100: instant kill).
        if dmg_flags & 0x100 != 0 {
            d.damage = 100_000;
        }
        self.g_damage(vm, who, d);
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
        if c.last_stand && c.ps.damage_timer > 0 {
            // Last-stand grace: the original skips damage until the stand timer ends.
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
        vm.notify_entity(target, "damage", &[attacker, Value::Int(damage)]);
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
            let c = self.client_mut(target).expect("checked above");
            c.last_stand = true;
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
        let killer = d
            .attacker
            .filter(|a| self.is_client(*a))
            .unwrap_or(net::ui::NO_ENTITY);
        let mean = MODS[usize::from(d.mean)];
        self.send(
            crate::ui::Dest::All,
            net::ui::ServerCmd::Obituary(net::ui::Obituary {
                killer,
                victim: n,
                weapon: self.weapon_name(d.weapon).to_owned(),
                mean: mean.to_owned(),
                headshot: d.mean == MOD_HEAD_SHOT || d.hitloc == HITLOC_HEAD,
            }),
        );
        let attacker = self.ent_obj(vm, d.attacker);
        vm.notify_entity(n, "death", std::slice::from_ref(&attacker));
        // Death animation length: the original asks the animation script; the server's
        // corpse pose lasts 1.2 s.
        let mut args = self.damage_callback_args(vm, d, damage);
        args[8] = Value::Int(1200);
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

    /// `CanDamage`: the blast reaches `target` when a line from `origin` to the entity's
    /// centre (or one of its offsets) is clear.
    pub fn can_damage(&self, target: u16, origin: Vec3, inflictor: u16) -> Option<Vec3> {
        self.can_damage_through(target, origin, inflictor, contents::MASK_SOLID)
    }

    /// `CanDamage` with the contents the line of sight is blocked by.
    pub fn can_damage_through(
        &self,
        target: u16,
        origin: Vec3,
        inflictor: u16,
        mask: i32,
    ) -> Option<Vec3> {
        let w = self.world.as_ref()?;
        let e = self.ent(target)?;
        let mid = [
            e.origin[0] + (e.mins[0] + e.maxs[0]) * 0.5,
            e.origin[1] + (e.mins[1] + e.maxs[1]) * 0.5,
            e.origin[2] + (e.mins[2] + e.maxs[2]) * 0.5,
        ];
        let clear = |to: Vec3| {
            let t = w.trace(origin, to, [0.0; 3], [0.0; 3], inflictor, mask);
            t.fraction >= 1.0 || t.hit_id == target
        };
        use sim::cm::Collide;
        if clear(mid) {
            return Some(mid);
        }
        for off in [
            [15.0, 15.0, 0.0],
            [-15.0, 15.0, 0.0],
            [15.0, -15.0, 0.0],
            [-15.0, -15.0, 0.0],
        ] {
            let p = [mid[0] + off[0], mid[1] + off[1], mid[2] + 24.0];
            if clear(p) {
                return Some(p);
            }
        }
        None
    }

    /// `G_RadiusDamage` with no damage cone and nothing ignored. Returns whether any entity
    /// was hit.
    #[allow(clippy::too_many_arguments)]
    pub fn radius_damage(
        &mut self,
        vm: &mut Vm,
        origin: Vec3,
        radius: f32,
        inner: i32,
        outer: i32,
        attacker: Option<u16>,
        inflictor: Option<u16>,
        mean: u8,
        weapon: u32,
    ) -> bool {
        self.blast(
            vm,
            &Blast {
                origin,
                radius,
                inner: inner as f32,
                outer: outer as f32,
                attacker,
                inflictor,
                cone: None,
                ignore: None,
                mean,
                weapon,
            },
        )
    }

    /// `G_RadiusDamage`: everything in range that the blast can see takes distance-scaled
    /// damage. Returns whether any entity was hit.
    pub fn blast(&mut self, vm: &mut Vm, b: &Blast) -> bool {
        let (origin, radius) = (b.origin, b.radius);
        if radius < 1.0 {
            return false;
        }
        let mut hit = false;
        let targets: Vec<u16> = self
            .in_use()
            .filter(|(n, e)| {
                (e.takedamage || e.kind == EntKind::Client)
                    && Some(*n) != b.inflictor
                    && Some(*n) != b.ignore
                    && *n < 1022
            })
            .map(|(n, _)| n)
            .collect();
        for t in targets {
            let Some(e) = self.ent(t) else { continue };
            // Distance from the blast to the entity's box.
            let mut d2 = 0.0f32;
            for (i, o) in origin.iter().enumerate() {
                let (lo, hi) = (e.origin[i] + e.mins[i], e.origin[i] + e.maxs[i]);
                let delta = if *o < lo {
                    lo - o
                } else if *o > hi {
                    o - hi
                } else {
                    0.0
                };
                d2 += delta * delta;
            }
            let dist = d2.sqrt();
            if dist >= radius {
                continue;
            }
            if let Some(c) = self.client(t)
                && (!c.connected() || c.session != Session::Playing)
            {
                continue;
            }
            let Some(p) = self.can_damage(t, origin, b.inflictor.unwrap_or(ENTITYNUM_NONE)) else {
                continue;
            };
            let mut dir = [p[0] - origin[0], p[1] - origin[1], p[2] - origin[2]];
            if let Some((cos, axis)) = b.cone {
                let d = normalize(dir);
                if cos > d[0] * axis[0] + d[1] * axis[1] + d[2] * axis[2] {
                    continue;
                }
            }
            let frac = dist / radius;
            let points = ((b.outer - b.inner) * frac + b.inner) as i32;
            let points = points.max(1);
            dir[2] += 24.0;
            let mut dmg = Damage::new(points, b.mean);
            dmg.attacker = b.attacker;
            dmg.inflictor = b.inflictor;
            dmg.dir = Some(normalize(dir));
            dmg.point = Some(p);
            dmg.flags = dflags::RADIUS | dflags::NO_KNOCKBACK;
            dmg.weapon = b.weapon;
            dmg.hitloc = HITLOC_NONE;
            self.g_damage(vm, t, dmg);
            hit = true;
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
            if dist > radius_max || self.can_damage_through(n, origin, ignore, mask).is_none() {
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

#[cfg(test)]
mod tests {
    use super::flashbang_percents;

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
}
