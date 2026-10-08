// SPDX-License-Identifier: GPL-3.0-or-later
//! Grenades and rockets (`ET_MISSILE`): `G_FireGrenade`, `G_FireRocket`, `G_RunMissile`,
//! `MissileImpact`, `BounceMissile` and `G_ExplodeMissile`, plus the dropped weapons that fall
//! to the floor.
//!
//! A missile flies on a [`Trajectory`] sampled at level time. Each frame it is traced from where
//! it was to where the trajectory puts it; a hit either bounces it (grenades), stops it (sticky
//! weapons), hurts and explodes it (rockets) or, at the end of the fuse or `proj_lifetime`,
//! `G_ExplodeMissile` runs: `explode` and `death` reach the scripts, the radius damage is dealt
//! and the entity is freed.
//!
//! Ceilings: guided missiles (javelin steering, attractors and repulsors only steer those),
//! rocket destabilisation, water splashes, `trigger_damage` volumes touched by grenades, glass
//! entities and flashbang blinding are not simulated; impact and explosion effects and sounds
//! belong to the clients.

use gsc::{Value, Vm};
use sim::Vec3;
use sim::cm::{Collide, ENTITYNUM_NONE, ENTITYNUM_WORLD};
use sim::contents;
use sim::pm::math;
use sim::traj::{TrType, Trajectory};
use sim::weapon::{OffhandClass, ProjExplosion, WeaponInfo, WeaponType};

use crate::bullet::{
    SURF_SKY, SURF_TYPE_FLESH, ShotTrace, dot, length, lerp, mad, normalized, sub, surface_type,
};
use crate::client::Team;
use crate::combat::{
    Blast, Damage, MOD_GRENADE, MOD_GRENADE_SPLASH, MOD_PROJECTILE, MOD_PROJECTILE_SPLASH,
};
use crate::game::{Ent, EntKind, Game};

/// What grenades and rockets collide with (`0x2806891`): the map, glass, missile clips, sky, shot
/// clips, actors, vehicles and players.
pub const MISSILE_CLIPMASK: i32 = 0x0280_6891;
/// The contents of a grenade: shootable and a pickup item.
const GRENADE_CONTENTS: i32 = 0x2100;
/// `FL_GRENADE_TOUCH_DAMAGE`.
pub const FL_GRENADE_TOUCH_DAMAGE: i32 = 0x4000;
/// `MOD_EXPLOSIVE`, the splash of a heavy explosive on the grenade handler.
const MOD_EXPLOSIVE: u8 = 14;
const MOD_IMPACT: u8 = 15;

/// `WEAPSTICKINESS_*`.
const STICKY_ALL: i32 = 1;
const STICKY_GROUND: i32 = 2;
const STICKY_GROUND_WITH_YAW: i32 = 3;

/// Which entity handler the missile has (`ENT_HANDLER_GRENADE`, `ENT_HANDLER_ROCKET`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissileKind {
    Grenade,
    Rocket,
}

impl MissileKind {
    fn mean(self) -> u8 {
        match self {
            Self::Grenade => MOD_GRENADE,
            Self::Rocket => MOD_PROJECTILE,
        }
    }

    fn splash_mean(self, info: &WeaponInfo) -> u8 {
        match self {
            Self::Grenade if info.proj_explosion == ProjExplosion::Heavy => MOD_EXPLOSIVE,
            Self::Grenade => MOD_GRENADE_SPLASH,
            Self::Rocket => MOD_PROJECTILE_SPLASH,
        }
    }
}

/// The flight state of a missile entity (`gentity_s::missile` and the trajectories).
#[derive(Debug, Clone)]
pub struct Missile {
    pub kind: MissileKind,
    pub weapon: u16,
    pub info: Box<WeaponInfo>,
    /// `parent`: who gets the credit. Scripts can change it (`detonate(attacker)`).
    pub parent: Option<u16>,
    pub team: Team,
    pub pos: Trajectory,
    pub apos: Trajectory,
    /// Grenades bounce and stick (`eFlags` 0x1000000); rockets do not.
    pub bounces: bool,
    /// Level time of the next think, or 0.
    pub next_think: i32,
    pub launch_time: i32,
    pub travel_dist: f32,
    pub surface_normal: Vec3,
    /// `groundEntityNum`: what the missile rests on.
    pub ground: Option<u16>,
    pub clipmask: i32,
    pub curvature: Vec3,
    pub spawn_time: i32,
    /// `time_to_accelerate` of the weapon, `rotate` of the grenade.
    pub time_to_accelerate: f32,
    pub target: Option<u16>,
    pub target_offset: Vec3,
}

/// `Missile_CreateAttractorEnt` and friends: slots scripts fill for guided missiles.
#[derive(Debug, Clone, Default)]
pub struct Attractors {
    slots: [Option<Attractor>; Attractors::MAX],
}

#[derive(Debug, Clone, Copy)]
pub struct Attractor {
    pub attractor: bool,
    /// The entity it follows, or its fixed `origin`.
    pub entity: Option<u16>,
    pub origin: Vec3,
    pub strength: f32,
    pub max_dist: f32,
}

impl Attractors {
    pub const MAX: usize = 32;

    /// Fills the first free slot; `None` when all are taken.
    pub fn add(&mut self, a: Attractor) -> Option<usize> {
        let i = self.slots.iter().position(Option::is_none)?;
        self.slots[i] = Some(a);
        Some(i)
    }

    pub fn remove(&mut self, i: usize) {
        if let Some(s) = self.slots.get_mut(i) {
            *s = None;
        }
    }

    /// `Missile_FreeAttractorRefs`: an entity that goes away takes its slots with it.
    pub fn free_entity(&mut self, n: u16) {
        for s in &mut self.slots {
            if s.is_some_and(|a| a.entity == Some(n)) {
                *s = None;
            }
        }
    }

    pub fn get(&self, i: usize) -> Option<&Attractor> {
        self.slots.get(i)?.as_ref()
    }
}

fn truncated(v: Vec3) -> Vec3 {
    v.map(f32::trunc)
}

/// `CalcMissileNoDrawTime`.
fn no_draw_time(speed: f32) -> i32 {
    ((speed * -35.0 / 600.0 + 85.0) as i32).clamp(20, 50)
}

impl Game {
    /// A random float in `[0, 1)` (`G_random`).
    pub fn random_f32(&mut self) -> f32 {
        (self.rand() & 0x7FFF) as f32 / 32768.0
    }

    fn flrand(&mut self, min: f32, max: f32) -> f32 {
        (self.rand() & 0x7FFF) as f32 * (max - min) / 32768.0 + min
    }

    /// `G_FireGrenade`: spawns a grenade of `weapon` thrown by `parent` from `start` with
    /// velocity `toss`, then adds `inherit` (the thrower's own motion). `fuse_left` is the fuse
    /// remaining in a primed grenade (0 when it was not cooked). Returns the entity.
    #[allow(clippy::too_many_arguments)]
    pub fn launch_grenade(
        &mut self,
        vm: &mut Vm,
        parent: u16,
        weapon: u16,
        start: Vec3,
        toss: Vec3,
        inherit: Vec3,
        rotate: bool,
        fuse_left: i32,
    ) -> Result<u16, String> {
        let info = self
            .weapons
            .get(weapon)
            .ok_or_else(|| format!("unknown weapon {weapon}"))?
            .clone();
        let def = self.content.weapon(&info.name).cloned();
        let now = self.level.time;
        let mut e = Ent::new(EntKind::Plain, "grenade");
        if let Some(m) = def.as_ref().and_then(|d| d.projectile_model.as_ref()) {
            e.model = m.name.as_deref().unwrap_or("").into();
        }
        e.contents = GRENADE_CONTENTS;
        e.mins = [-1.5; 3];
        e.maxs = [1.5; 3];
        if info.offhand_class == OffhandClass::Frag {
            // `G_MakeMissilePickupItem`: a live frag can be picked up and thrown back.
            e.mins = [-1.0; 3];
            e.maxs = [1.0; 3];
            e.contents |= contents::USE;
        }
        e.origin = start;
        let client = self.client(parent);
        let throw_back = client.filter(|c| {
            c.ps.grenade_time_left < 0 && c.ps.throw_back_grenade_owner != ENTITYNUM_NONE
        });
        let owner = throw_back
            .map(|c| c.ps.throw_back_grenade_owner)
            .filter(|o| self.ent(*o).is_some())
            .unwrap_or(parent);
        let owner_defined = throw_back.is_none() || self.ent(owner).is_some();
        e.owner = owner_defined.then_some(owner);
        let team = client.map_or(Team::Free, |c| c.team);

        let mut vel = truncated(toss);
        let angles = [math::vec_to_pitch(&vel), math::vec_to_yaw(&vel), 0.0];
        e.angles = angles;
        let mut apos = Trajectory::stationary(angles);
        if rotate && def.as_ref().is_some_and(|d| d.rotate != 0) {
            let pitch = math::angle_normalize_360(angles[0] - 120.0);
            let spin_a = self.flrand(320.0, 800.0) * (2 * (self.rand() % 2) as i32 - 1) as f32;
            let spin_b = self.flrand(180.0, 540.0) * (2 * (self.rand() % 2) as i32 - 1) as f32;
            apos = Trajectory {
                kind: TrType::Linear,
                time: now,
                duration: 0,
                base: [pitch, angles[1], angles[2]],
                delta: [spin_a, 0.0, spin_b],
            };
            e.angles = apos.base;
        }
        vel = mad(vel, 1.0, inherit);
        let speed = length(toss);
        let mut next_think = 0;
        let activate = info.projectile_activate_dist;
        if (activate <= 0 || fuse_left > 0) && info.timed_detonation {
            next_think = now
                + if fuse_left > 0 {
                    fuse_left
                } else {
                    info.fuse_time
                };
        }
        if next_think == 0 {
            next_think = now + 30_000;
        }
        next_think = next_think.min(now + 60_000);
        let missile = Missile {
            kind: MissileKind::Grenade,
            weapon,
            parent: owner_defined.then_some(owner),
            team,
            pos: Trajectory {
                kind: TrType::Gravity,
                time: now,
                duration: 0,
                base: start,
                delta: vel,
            },
            apos,
            bounces: true,
            next_think,
            launch_time: now + no_draw_time(speed),
            travel_dist: 0.0,
            surface_normal: [0.0; 3],
            ground: None,
            clipmask: MISSILE_CLIPMASK,
            curvature: [0.0; 3],
            spawn_time: now,
            time_to_accelerate: 0.0,
            target: None,
            target_offset: [0.0; 3],
            info: Box::new(info),
        };
        let announce = missile.info.projectile_activate_dist <= 0;
        let name = missile.info.name.clone();
        e.missile = Some(Box::new(missile));
        let n = self.spawn(e)?;
        self.relink(n);
        if announce {
            let g = self.entity_value(vm, n);
            vm.notify_entity(parent, "grenade_fire", &[g, Value::str(&name)]);
        }
        Ok(n)
    }

    /// `G_FireRocket`: a rocket of `weapon` leaves `start` along the unit vector `dir`.
    pub fn launch_rocket(
        &mut self,
        parent: u16,
        weapon: u16,
        start: Vec3,
        dir: Vec3,
    ) -> Result<u16, String> {
        let info = self
            .weapons
            .get(weapon)
            .ok_or_else(|| format!("unknown weapon {weapon}"))?
            .clone();
        let def = self.content.weapon(&info.name).cloned();
        let now = self.level.time;
        let dir = normalized(dir);
        let speed = info.projectile_speed as f32;
        let tta = def.as_ref().map_or(0.0, |d| d.time_to_accelerate);
        let curvature_max = def.as_ref().map_or(0.0, |d| d.projectile_curvature);
        let angles = [math::vec_to_pitch(&dir), math::vec_to_yaw(&dir), 0.0];
        let mut e = Ent::new(EntKind::Plain, "rocket");
        if let Some(m) = def.as_ref().and_then(|d| d.projectile_model.as_ref()) {
            e.model = m.name.as_deref().unwrap_or("").into();
        }
        e.origin = start;
        e.angles = angles;
        e.owner = Some(parent);
        let pos = if tta <= 0.0 {
            Trajectory {
                kind: TrType::Linear,
                time: now,
                duration: 0,
                base: start,
                delta: truncated(mad([0.0; 3], speed, dir)),
            }
        } else {
            Trajectory {
                kind: TrType::Interpolate,
                time: now,
                duration: 0,
                base: start,
                delta: [0.0; 3],
            }
        };
        let mut curvature = [0.0; 3];
        let mut pos = pos;
        if curvature_max > 0.0 {
            pos.kind = TrType::Interpolate;
            let (_, right, up) = math::angle_vectors(&angles);
            let theta = self.random_f32() * 360.0;
            let r = self.random_f32() * curvature_max;
            let (s, c) = math::sincos_deg(theta);
            curvature = mad(mad([0.0; 3], r * c, right), r * s, up);
        }
        let life = ((info.proj_lifetime * 1000.0) as i32).min(60_000);
        // A vehicle's missiles belong to the team of the player it was called in for.
        let credit = self
            .ent(parent)
            .and_then(|p| p.veh.as_ref())
            .map_or(parent, |v| v.owner);
        let team = self.client(credit).map_or(Team::Free, |c| c.team);
        e.missile = Some(Box::new(Missile {
            kind: MissileKind::Rocket,
            weapon,
            parent: Some(parent),
            team,
            pos,
            apos: Trajectory::stationary(angles),
            bounces: false,
            next_think: now + life,
            launch_time: now + no_draw_time(speed),
            travel_dist: 0.0,
            surface_normal: [0.0; 3],
            ground: None,
            clipmask: MISSILE_CLIPMASK,
            curvature,
            spawn_time: now,
            time_to_accelerate: tta,
            target: None,
            target_offset: [0.0; 3],
            info: Box::new(info),
        }));
        let n = self.spawn(e)?;
        self.relink(n);
        Ok(n)
    }

    /// `G_RunFrameForEntity`: what one entity does each frame.
    pub fn run_entity(&mut self, vm: &mut Vm, n: u16) {
        let Some(e) = self.ent(n) else { return };
        if e.missile.is_some() {
            self.run_missile(vm, n);
        } else if e.kind == EntKind::Item && e.mv.pos.tr.kind == TrType::Gravity {
            self.run_item(n);
        } else if e.veh.is_some() {
            self.run_vehicle(vm, n);
        } else {
            self.run_mover(vm, n);
        }
    }

    /// Runs `f` on the missile of entity `n`, which is out of the entity while it runs so the
    /// game can be used freely. Returns `None` when `n` is no missile.
    pub fn with_missile<R>(
        &mut self,
        n: u16,
        f: impl FnOnce(&mut Game, &mut Missile) -> R,
    ) -> Option<R> {
        let mut m = self.ent_mut(n)?.missile.take()?;
        let r = f(self, &mut m);
        if let Some(e) = self.ent_mut(n)
            && e.missile.is_none()
        {
            e.missile = Some(m);
        }
        Some(r)
    }

    /// A trace the way missiles fly: a point, ignoring the thrower and what it owns.
    fn missile_trace(&self, m: &Missile, owner: u16, start: Vec3, end: Vec3) -> ShotTrace {
        let mut t = self.shot_trace(start, end, owner, ENTITYNUM_NONE, m.clipmask, false);
        if t.start_solid {
            t.fraction = 0.0;
            t.normal = normalized(sub(start, end));
        }
        t
    }

    /// `MissileTrajectory`: where the missile wants to be this frame.
    fn missile_next_origin(&self, n: u16, m: &mut Missile) -> Vec3 {
        let now = self.level.time;
        if now > m.launch_time && m.pos.kind != TrType::Linear && m.kind == MissileKind::Rocket {
            let dt = self.level.frametime as f32 * 0.001;
            let angles = self.ent(n).map_or([0.0; 3], |e| e.angles);
            let (dir, _, _) = math::angle_vectors(&angles);
            let speed = m.info.projectile_speed as f32;
            if m.time_to_accelerate > 0.0 {
                let forward = dot(dir, m.pos.delta);
                if forward < speed {
                    m.pos.delta = mad(m.pos.delta, speed / m.time_to_accelerate * dt, dir);
                } else {
                    m.pos.delta = mad(m.pos.delta, speed - forward, dir);
                }
            }
            if m.curvature != [0.0; 3] {
                m.pos.delta = mad(m.pos.delta, dt, m.curvature);
            }
            m.pos.base = mad(m.pos.base, dt, m.pos.delta);
            m.pos.base
        } else {
            m.pos.evaluate(now)
        }
    }

    /// `G_SetOrigin` and `G_SetAngle` of a missile that stopped or was moved.
    fn missile_set_pose(&mut self, n: u16, origin: Vec3, angles: Option<Vec3>) {
        if let Some(e) = self.ent_mut(n) {
            e.origin = origin;
            if let Some(a) = angles {
                e.angles = a;
            }
        }
    }

    /// `G_RunMissile`.
    pub fn run_missile(&mut self, vm: &mut Vm, n: u16) {
        self.with_missile(n, |g, m| g.step_missile(vm, n, m));
    }

    fn step_missile(&mut self, vm: &mut Vm, n: u16, m: &mut Missile) {
        let now = self.level.time;
        let Some(e) = self.ent(n) else { return };
        let owner = e.owner.unwrap_or(ENTITYNUM_NONE);
        let mut origin = e.origin;
        if m.apos.kind != TrType::Stationary {
            let a = m.apos.evaluate(now);
            self.missile_set_pose(n, origin, Some(a));
        }
        // Resting on something that is not the map: still held up?
        if m.pos.kind == TrType::Stationary && m.ground != Some(ENTITYNUM_WORLD) {
            let probe = mad(origin, -1.635, m.surface_normal);
            if self.missile_trace(m, owner, origin, probe).fraction == 1.0 {
                m.pos = Trajectory {
                    kind: TrType::Gravity,
                    time: now,
                    duration: 0,
                    base: origin,
                    delta: [0.0; 3],
                };
            }
        }
        let old = origin;
        let next = self.missile_next_origin(n, m);
        let dir = normalized(sub(next, origin));
        if length(sub(next, origin)) < 0.001 {
            self.missile_think(vm, n, m);
            return;
        }
        let mut tr = self.missile_trace(m, owner, origin, next);
        let mut endpos = lerp(origin, next, tr.fraction);
        origin = endpos;
        if m.bounces {
            let info = &m.info;
            let sticky_ground =
                info.stickiness == STICKY_GROUND || info.stickiness == STICKY_GROUND_WITH_YAW;
            if info.stickiness == STICKY_ALL || (sticky_ground && tr.normal[2] > 0.7) {
                if tr.fraction < 1.0 {
                    let start = mad(origin, 0.135, tr.normal);
                    let end = mad(origin, -1.5, tr.normal);
                    let down = self.missile_trace(m, owner, start, end);
                    if down.fraction != 1.0 {
                        tr = down;
                        endpos = lerp(start, end, tr.fraction);
                        let offset = sub(endpos, end);
                        m.pos.base = mad(m.pos.base, 1.0, offset);
                        origin = mad(endpos, 1.0, offset);
                    }
                }
            } else if tr.fraction == 1.0 || (tr.fraction < 1.0 && tr.normal[2] > 0.7) {
                let start = [origin[0], origin[1], origin[2] + 0.135];
                let end = [origin[0], origin[1], origin[2] - 1.5];
                let down = self.missile_trace(m, owner, start, end);
                if down.fraction != 1.0 {
                    tr = down;
                    endpos = lerp(start, end, tr.fraction);
                    m.pos.base[2] += endpos[2] + 1.5 - origin[2];
                    origin = [endpos[0], endpos[1], endpos[2] + 1.5];
                }
            }
        }
        self.missile_set_pose(n, origin, None);
        self.relink(n);
        if m.info.projectile_activate_dist > 0 {
            m.travel_dist += length(sub(endpos, old));
        }
        if tr.fraction == 1.0 {
            if length(m.pos.delta) != 0.0 {
                m.ground = None;
            }
            self.missile_think(vm, n, m);
            return;
        }
        if tr.surface_flags & SURF_SKY != 0 {
            vm.notify_entity(n, "death", &[]);
            self.free_entity(vm, n);
            return;
        }
        self.missile_impact(vm, n, m, &tr, dir, endpos);
        if self.ent(n).is_some() {
            self.missile_think(vm, n, m);
        }
    }

    /// `G_RunThink`: the fuse or lifetime ran out.
    fn missile_think(&mut self, vm: &mut Vm, n: u16, m: &mut Missile) {
        if m.next_think > 0 && m.next_think <= self.level.time && self.ent(n).is_some() {
            m.next_think = 0;
            self.explode_missile(vm, n, m);
        }
    }

    /// `G_ExplodeMissile`, for a script that detonates a grenade (`detonate`).
    pub fn detonate_missile(&mut self, vm: &mut Vm, n: u16) {
        self.with_missile(n, |g, m| {
            m.next_think = 0;
            g.explode_missile(vm, n, m);
        });
    }

    /// `G_ExplodeMissile`: the missile goes off where it is.
    pub fn explode_missile(&mut self, vm: &mut Vm, n: u16, m: &mut Missile) {
        let now = self.level.time;
        if m.info.offhand_class == OffhandClass::Smoke && m.ground.is_none() {
            // A smoke grenade only goes off once it has come to rest, or after a minute.
            if now - m.spawn_time <= 60_000 {
                m.next_think = now;
            } else {
                vm.notify_entity(n, "death", &[]);
                self.free_entity(vm, n);
            }
            return;
        }
        let origin = m.pos.evaluate(now);
        let angles = self.ent(n).map_or([0.0; 3], |e| e.angles);
        self.missile_set_pose(n, origin, None);
        vm.notify_entity(n, "explode", &[Value::Vector(origin)]);
        let normal = if m.surface_normal == [0.0; 3] {
            [0.0, 0.0, 1.0]
        } else {
            m.surface_normal
        };
        let weapon = m.weapon;
        let owner = m.parent.unwrap_or(1023);
        self.tempev.add(now, crate::tempev::ev::EXPLOSION, |s| {
            s.origin = origin;
            s.angles = crate::tempev::dir_to_angles(normal);
            s.weapon = weapon;
            s.client = owner;
        });
        if m.info.explosion_inner_damage != 0 {
            self.missile_blast(vm, n, m, origin, math::angle_vectors(&angles).0, None);
        }
        vm.notify_entity(n, "death", &[]);
        self.free_entity(vm, n);
    }

    /// The radius damage of a missile's explosion (`G_RadiusDamage` with the weapon's values),
    /// credited to its parent; nothing happens when the parent is gone.
    fn missile_blast(
        &mut self,
        vm: &mut Vm,
        n: u16,
        m: &Missile,
        origin: Vec3,
        axis: Vec3,
        ignore: Option<u16>,
    ) -> bool {
        let info = &m.info;
        let Some(parent) = m.parent.filter(|p| self.ent(*p).is_some()) else {
            return false;
        };
        let angle = info.damage_cone_angle;
        let cone = (angle - 180.0 < 0.0).then(|| (math::sincos_deg(angle).1, axis));
        self.blast(
            vm,
            &Blast {
                origin,
                radius: info.explosion_radius as f32,
                inner: info.explosion_inner_damage as f32,
                outer: info.explosion_outer_damage as f32,
                attacker: Some(parent),
                inflictor: Some(n),
                cone,
                ignore,
                mean: m.kind.splash_mean(info),
                weapon: u32::from(m.weapon),
            },
        )
    }

    /// `CheckCrumpleMissile`: a fast projectile hitting flesh or a surface nearly square on
    /// does not bounce.
    fn crumples(&self, m: &Missile, tr: &ShotTrace) -> bool {
        if m.info.weap_type != WeaponType::Projectile {
            return false;
        }
        if tr.surface_flags == SURF_TYPE_FLESH {
            return true;
        }
        let v = m.pos.evaluate_delta(self.hit_time(tr.fraction));
        let speed = length(v);
        if speed < 500.0 {
            return false;
        }
        dot(mad([0.0; 3], -1.0 / speed, v), tr.normal) > 0.707
    }

    /// The time within this frame at which a trace fraction was reached.
    fn hit_time(&self, fraction: f32) -> i32 {
        let prev = self.level.time - self.level.frametime;
        prev + (f64::from(self.level.time - prev) * f64::from(fraction)) as i32
    }

    /// `MissileImpact`.
    fn missile_impact(
        &mut self,
        vm: &mut Vm,
        n: u16,
        m: &mut Missile,
        tr: &ShotTrace,
        dir: Vec3,
        endpos: Vec3,
    ) {
        let other = tr.hit;
        let other_takes = self.ent(other).is_some_and(|e| e.takedamage);
        let mut explode_on_impact = m.info.proj_impact_explode;
        let damage = m.info.damage;
        let owner = self.ent(n).and_then(|e| e.owner);
        let dud = m.info.projectile_activate_dist > 0
            && m.travel_dist < m.info.projectile_activate_dist as f32;
        let mean = if dud {
            explode_on_impact = false;
            m.travel_dist = -1.0e10;
            MOD_IMPACT
        } else if explode_on_impact {
            m.kind.mean()
        } else {
            MOD_IMPACT
        };
        let hitloc = if mean == MOD_IMPACT { tr.hitloc } else { 0 };
        let mut tr = *tr;
        if !other_takes && m.bounces && !explode_on_impact && !self.crumples(m, &tr) {
            self.bounce_missile(vm, n, m, &tr);
            if m.info.projectile_activate_dist > 0 && m.pos.kind == TrType::Stationary {
                self.free_entity(vm, n);
            }
            return;
        }
        if other_takes {
            if damage != 0 {
                let attacker = owner.or(Some(ENTITYNUM_WORLD));
                if let Some(a) = owner
                    && self.is_accurate_hit(other, a)
                {
                    self.stats.hits += 1;
                }
                let mut v = m.pos.evaluate_delta(self.level.time);
                let speed = length(v);
                if speed == 0.0 {
                    v[2] = 1.0;
                }
                let min_speed = self.cvars.float("g_minGrenadeDamageSpeed");
                if m.info.weap_type != WeaponType::Grenade || min_speed < speed {
                    let origin = self.ent(n).map_or(endpos, |e| e.origin);
                    let mut d = Damage::new(damage, mean);
                    d.inflictor = Some(n);
                    d.attacker = attacker;
                    d.dir = Some(v);
                    d.point = Some(origin);
                    d.weapon = u32::from(m.weapon);
                    d.hitloc = hitloc;
                    self.g_damage(vm, other, d);
                }
            }
            if !explode_on_impact {
                if self.is_client(other) && tr.surface_flags == 0 {
                    tr.surface_flags = SURF_TYPE_FLESH;
                }
                if !self.crumples(m, &tr) {
                    self.bounce_missile(vm, n, m, &tr);
                    return;
                }
            }
        }
        self.missile_set_pose(n, endpos, None);
        if explode_on_impact
            && m.info.explosion_inner_damage != 0
            && let Some(parent) = m.parent.filter(|p| self.ent(*p).is_some())
        {
            self.missile_blast(vm, n, m, endpos, dir, Some(other));
            let args = [
                Value::str(&m.info.name),
                Value::Vector(endpos),
                Value::Int(m.info.explosion_radius),
            ];
            vm.notify_entity(parent, "projectile_impact", &args);
        }
        vm.notify_entity(n, "death", &[]);
        self.free_entity(vm, n);
    }

    /// `BounceMissile`: reflects the velocity off the surface, scaled by the weapon's bounce
    /// factors for the surface type; a grenade slow enough, or on sticky ground, stops.
    fn bounce_missile(&mut self, vm: &mut Vm, n: u16, m: &mut Missile, tr: &ShotTrace) {
        let now = self.level.time;
        let surf = surface_type(tr.surface_flags).min(28);
        let hit_time = self.hit_time(tr.fraction);
        let velocity = m.pos.evaluate_delta(hit_time);
        let mut d = dot(velocity, tr.normal);
        m.pos.delta = mad(velocity, d * -2.0, tr.normal);
        let info = &m.info;
        let hit_is_missile = self.ent(tr.hit).is_some_and(|e| e.missile.is_some());
        let may_stop = (info.stickiness == 0 || tr.hit >= 64) && !hit_is_missile;
        if may_stop && (info.stickiness == STICKY_ALL || tr.normal[2] > 0.7) {
            m.ground = Some(tr.hit);
        }
        let origin = self.ent(n).map_or([0.0; 3], |e| e.origin);
        let (weapon, owner) = (m.weapon, m.parent.unwrap_or(1023));
        self.tempev
            .add(now, crate::tempev::ev::MISSILE_BOUNCE, |s| {
                s.origin = origin;
                s.angles = crate::tempev::dir_to_angles(tr.normal);
                s.event_parm = surf as u8;
                s.weapon = weapon;
                s.client = owner;
            });
        if m.bounces {
            let speed = length(velocity);
            if speed > 0.0 && d <= 0.0 {
                d /= -speed;
                let par = info.parallel_bounce[surf];
                let factor = (info.perpendicular_bounce[surf] - par) * d + par;
                m.pos.delta = mad([0.0; 3], factor, m.pos.delta);
            }
            let slow = length(m.pos.delta) < 20.0;
            if may_stop
                && (info.stickiness == STICKY_ALL
                    || (tr.normal[2] > 0.7
                        && (info.stickiness == STICKY_GROUND
                            || info.stickiness == STICKY_GROUND_WITH_YAW
                            || slow)))
            {
                let angles = self.land_angles(m, tr, hit_time);
                m.pos = Trajectory::stationary(origin);
                m.apos = Trajectory::stationary(angles);
                self.missile_set_pose(n, origin, Some(angles));
                m.surface_normal = tr.normal;
                if !m.info.timed_detonation {
                    m.next_think = 0;
                }
                self.grenade_danger(vm, n, m);
                return;
            }
        }
        let mut nudge = mad([0.0; 3], 0.1, tr.normal);
        if nudge[2] > 0.0 {
            nudge[2] = 0.0;
        }
        let moved = mad(origin, 1.0, nudge);
        self.missile_set_pose(n, moved, None);
        m.pos.base = moved;
        m.pos.time = now;
        m.apos.base = self.land_angles_free(m, tr, hit_time, false);
        m.apos.time = now;
    }

    /// The angles the missile lands in, by how it is allowed to stick.
    fn land_angles(&self, m: &Missile, tr: &ShotTrace, hit_time: i32) -> Vec3 {
        let mut angles = m.apos.evaluate(hit_time);
        match m.info.stickiness {
            STICKY_GROUND_WITH_YAW => {
                angles[0] = math::pitch_for_yaw_on_normal(angles[1], &tr.normal);
                angles[2] = 0.0;
                angles[2] = roll_on_normal(&angles, &tr.normal);
            }
            STICKY_ALL | STICKY_GROUND => {
                let fwd = math::angle_vectors(&angles).0;
                let projected = math::project_point_on_plane(&fwd, &tr.normal);
                angles = [
                    math::vec_to_pitch(&projected),
                    math::vec_to_yaw(&projected),
                    0.0,
                ];
                angles[2] = roll_on_normal(&angles, &tr.normal);
            }
            _ => angles = self.land_angles_free(m, tr, hit_time, true),
        }
        angles
    }

    /// `MissileLandAngles`: how a tumbling grenade turns to the surface it hit.
    fn land_angles_free(&self, m: &Missile, tr: &ShotTrace, hit_time: i32, force: bool) -> Vec3 {
        let mut angles = m.apos.evaluate(hit_time);
        if tr.normal[2] <= 0.1 {
            return angles;
        }
        let surface_pitch = math::pitch_for_yaw_on_normal(angles[1], &tr.normal);
        let delta = math::angle_delta(surface_pitch, angles[0]);
        let pitch = {
            let v = angles[0] / 360.0;
            (v - (v + 0.5).floor()) * 360.0
        };
        angles[0] = if force || delta.abs() < 45.0 {
            if pitch.abs() <= 90.0 {
                math::angle_normalize_360(surface_pitch)
            } else {
                math::angle_normalize_360(surface_pitch + 180.0)
            }
        } else if delta.abs() >= 80.0 {
            math::angle_normalize_360(pitch)
        } else {
            math::angle_normalize_360(delta * 0.25 + pitch)
        };
        angles
    }

    /// `CheckGrenadeDanger`: players within the blast radius of a grenade that came to rest
    /// are told (`grenadedanger`: grenade, its thrower, the weapon).
    fn grenade_danger(&mut self, vm: &mut Vm, n: u16, m: &Missile) {
        let origin = self.ent(n).map_or([0.0; 3], |e| e.origin);
        let radius = m.info.explosion_radius as f32;
        let near: Vec<u16> = self
            .connected_clients()
            .filter_map(|(c, _)| {
                let e = self.ent(c)?;
                (length(sub(e.origin, origin)) < radius).then_some(c)
            })
            .collect();
        if near.is_empty() {
            return;
        }
        let grenade = self.entity_value(vm, n);
        let parent = m
            .parent
            .filter(|p| self.ent(*p).is_some())
            .map_or(Value::Undefined, |p| self.entity_value(vm, p));
        for c in near {
            vm.notify_entity(
                c,
                "grenadedanger",
                &[grenade.clone(), parent.clone(), Value::str(&m.info.name)],
            );
        }
    }

    /// A dropped weapon falls until it lands.
    fn run_item(&mut self, n: u16) {
        let now = self.level.time;
        let Some(e) = self.ent(n) else { return };
        let (tr, origin, mins, maxs) = (e.mv.pos.tr, e.origin, e.mins, e.maxs);
        let next = tr.evaluate(now);
        let Some(world) = self.world.as_ref() else {
            return;
        };
        let t = world.trace(origin, next, mins, maxs, n, contents::MASK_SOLID);
        let at = if t.fraction < 1.0 {
            lerp(origin, next, t.fraction)
        } else {
            next
        };
        if let Some(e) = self.ent_mut(n) {
            e.origin = at;
            if t.fraction < 1.0 {
                e.mv.pos.tr = Trajectory::stationary(at);
            }
        }
        self.relink(n);
    }
}

/// The roll that lays the missile's up axis against the surface (`MissileLandAnglesFlat`).
fn roll_on_normal(angles: &Vec3, normal: &Vec3) -> f32 {
    let (_, right, up) = math::angle_vectors(angles);
    dot(right, *normal).atan2(dot(up, *normal)).to_degrees()
}
