// SPDX-License-Identifier: GPL-3.0-only
// Rocket destabilisation, attractors and repulsors, water splash and explosion depth, and glass penetration translated in part from KisakCOD (game/g_missile.cpp; GPL-3.0, copyright the KisakCOD contributors and Activision).
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
//! Guided missiles (`guidedMissileType` 1 to 3) steer toward the target a script gave them with
//! `missile_settarget`. An unguided rocket that flies far enough destabilises: its heading drifts in random
//! course changes, bent toward the attractors and away from the repulsors scripts placed. A missile crossing
//! water splashes, one blowing up deeper under it than `missileWaterMaxDepth` makes no explosion event, and a
//! damageable glass entity is hurt and flown through. Ceilings: the top-attack flight mode (no multiplayer
//! script can select it) and `trigger_damage` volumes touched by grenades are not simulated; impact and
//! explosion effects and sounds belong to the clients.

use gsc::{Value, Vm};
use sim::Vec3;
use sim::cm::{Collide, ENTITYNUM_NONE, ENTITYNUM_WORLD};
use sim::contents;
use sim::pm::math;
use sim::traj::{TrType, Trajectory};
use sim::weapon::{OffhandClass, ProjExplosion, WeaponInfo, WeaponType};

use crate::bullet::{
    SURF_SKY, SURF_TYPE_FLESH, SURF_TYPE_WATER, ShotTrace, dot, length, lerp, mad, normalized, sub,
    surface_type,
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
/// `FL_MISSILE_DESTABILIZED`: the rocket has begun its random course changes.
const FL_MISSILE_DESTABILIZED: i32 = 0x1_0000;
/// `FL_STABLE_MISSILES`: the rocket never destabilises.
pub const FL_STABLE_MISSILES: i32 = 0x2_0000;
/// `SURF_TYPE_GLASS`.
const SURF_TYPE_GLASS: usize = 9;
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
    /// The surface type the missile last bounced off, for the sound and effect of its explosion.
    pub surface: u8,
    /// `groundEntityNum`: what the missile rests on.
    pub ground: Option<u16>,
    pub clipmask: i32,
    /// `missile.curvature`: a curving rocket's sideways drift, then (once destabilised) its heading change per frame.
    pub curvature: Vec3,
    /// `missile.time`: milliseconds after `pos.time` the rocket destabilises, then the time between course changes.
    pub destabilize_ms: i32,
    pub spawn_time: i32,
    /// `time_to_accelerate` of the weapon, `rotate` of the grenade.
    pub time_to_accelerate: f32,
    /// `missile.stage`: where a javelin is in its flight.
    pub stage: JavelinStage,
    pub target: Option<u16>,
    pub target_offset: Vec3,
}

/// `MISSILESTAGE_*`: a javelin is tossed out (`SoftLaunch`), lights its motor and climbs (`Ascent`), then dives
/// on its target (`Descent`). Other missiles never leave `SoftLaunch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JavelinStage {
    SoftLaunch,
    Ascent,
    Descent,
}

/// `guidedMissileType`: 1 sidewinder, 2 hellfire, 3 javelin.
const GUIDED_HELLFIRE: i32 = 2;
const GUIDED_JAVELIN: i32 = 3;
/// `missileHellfireMaxSlope`, `missileHellfireUpAccel`: stock values of the cheat dvars.
const HELLFIRE_MAX_SLOPE: f32 = 0.5;
const HELLFIRE_UP_ACCEL: f32 = 1000.0;

/// The missile tuning dvars (`missileJav*`, `missileWaterMaxDepth`), with the defaults the multiplayer binary registers.
pub const MISSILE_CVARS: &[(&str, &str)] = &[
    ("missileJavClimbHeightDirect", "10000"),
    ("missileJavClimbAngleDirect", "85"),
    ("missileJavClimbCeilingDirect", "0"),
    ("missileJavTurnRateDirect", "60"),
    ("missileJavAccelClimb", "300"),
    ("missileJavAccelDescend", "3000"),
    ("missileJavSpeedLimitClimb", "1000"),
    ("missileJavSpeedLimitDescend", "6000"),
    ("missileJavTurnDecel", "0.05"),
    ("missileJavClimbToOwner", "700"),
    ("missileWaterMaxDepth", "60"),
];

/// `VecToQuat`: the orientation looking along `dir`, as `[x, y, z, w]`.
fn dir_to_quat(dir: Vec3) -> [f32; 4] {
    let angles = [math::vec_to_pitch(&dir), math::vec_to_yaw(&dir), 0.0];
    let (f, r, u) = math::angle_vectors(&angles);
    // Columns forward, left, up.
    let m = [
        [f[0], -r[0], u[0]],
        [f[1], -r[1], u[1]],
        [f[2], -r[2], u[2]],
    ];
    let trace = m[0][0] + m[1][1] + m[2][2];
    if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        [
            (m[2][1] - m[1][2]) / s,
            (m[0][2] - m[2][0]) / s,
            (m[1][0] - m[0][1]) / s,
            s / 4.0,
        ]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        [
            s / 4.0,
            (m[0][1] + m[1][0]) / s,
            (m[0][2] + m[2][0]) / s,
            (m[2][1] - m[1][2]) / s,
        ]
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        [
            (m[0][1] + m[1][0]) / s,
            s / 4.0,
            (m[1][2] + m[2][1]) / s,
            (m[0][2] - m[2][0]) / s,
        ]
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        [
            (m[0][2] + m[2][0]) / s,
            (m[1][2] + m[2][1]) / s,
            s / 4.0,
            (m[1][0] - m[0][1]) / s,
        ]
    }
}

/// `QuatSlerp` followed by `Vec4Normalize`.
fn quat_slerp(from: [f32; 4], to: [f32; 4], frac: f32) -> [f32; 4] {
    let mut dot: f32 = (0..4).map(|i| from[i] * to[i]).sum();
    let flip = dot < 0.0;
    if flip {
        dot = -dot;
    }
    let (scale_from, scale_to) = if dot <= 0.95 {
        let angle = dot.acos();
        let sin = angle.sin();
        (
            ((1.0 - frac) * angle).sin() / sin,
            (angle * frac).sin() / sin,
        )
    } else {
        (1.0 - frac, frac)
    };
    let sign = if flip { -1.0 } else { 1.0 };
    let mut q = [0.0f32; 4];
    for i in 0..4 {
        q[i] = scale_from * from[i] + scale_to * to[i] * sign;
    }
    let len = q.iter().map(|v| v * v).sum::<f32>().sqrt();
    if len != 0.0 { q.map(|v| v / len) } else { q }
}

/// `UnitQuatToForward`.
fn quat_forward(q: [f32; 4]) -> Vec3 {
    [
        1.0 - (q[1] * q[1] + q[2] * q[2]) * 2.0,
        (q[0] * q[1] + q[2] * q[3]) * 2.0,
        (q[0] * q[2] - q[1] * q[3]) * 2.0,
    ]
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
        if fuse_left < 0 {
            // A grenade that cooked off in the hand goes off at once.
            next_think = now - 1;
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
            surface: 0,
            ground: None,
            clipmask: MISSILE_CLIPMASK,
            curvature: [0.0; 3],
            destabilize_ms: 0,
            spawn_time: now,
            time_to_accelerate: 0.0,
            stage: JavelinStage::SoftLaunch,
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
        let tta = info.time_to_accelerate;
        let curvature_max = info.projectile_curvature;
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
        let mut apos = Trajectory::stationary(angles);
        if info.guided_missile_type != 0 {
            apos.kind = TrType::Interpolate;
            pos.kind = TrType::Interpolate;
            pos.duration = 0;
            if info.guided_missile_type == GUIDED_JAVELIN {
                // Tossed out under gravity until the motor lights.
                pos.kind = TrType::Gravity;
                pos.time = now;
            }
        }
        let life = ((info.proj_lifetime * 1000.0) as i32).min(60_000);
        let destabilize_ms = if speed > 0.0 {
            (info.destabilize_distance as f32 / speed * 1000.0) as i32
        } else {
            0
        };
        // A rocket with no `destabilizationRateTime` is stable, as is one fired by a stable parent.
        let parent_flags = self.ent(parent).map_or(0, |p| p.flags);
        e.flags |= if info.destabilization_rate_time == 0.0 {
            FL_STABLE_MISSILES
        } else {
            parent_flags & FL_STABLE_MISSILES
        };
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
            apos,
            bounces: false,
            next_think: now + life,
            launch_time: now + no_draw_time(speed),
            travel_dist: 0.0,
            surface_normal: [0.0; 3],
            surface: 0,
            ground: None,
            clipmask: MISSILE_CLIPMASK,
            curvature,
            destabilize_ms,
            spawn_time: now,
            time_to_accelerate: tta,
            stage: JavelinStage::SoftLaunch,
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
        if e.free_at.is_some_and(|t| self.level.time >= t) {
            self.free_entity(vm, n);
        } else if e.missile.is_some() {
            self.run_missile(vm, n);
        } else if e.kind == EntKind::Item {
            self.run_item(vm, n);
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
        self.missile_trace_mask(m.clipmask, owner, start, end)
    }

    fn missile_trace_mask(&self, mask: i32, owner: u16, start: Vec3, end: Vec3) -> ShotTrace {
        let mut t = self.shot_trace(start, end, owner, ENTITYNUM_NONE, mask, false);
        if t.start_solid {
            t.fraction = 0.0;
            t.normal = normalized(sub(start, end));
        }
        t
    }

    fn in_water(&self, p: Vec3) -> bool {
        use sim::cm::Collide;
        self.world
            .as_ref()
            .is_some_and(|w| w.point_contents(p, ENTITYNUM_NONE, contents::WATER) != 0)
    }

    /// The trace of one frame of flight. A missile falling or climbing fast splashes where it crosses the surface
    /// of water (the splash is the bounce event on water) and flies on through; one already in the water, or flying
    /// level, does not see it.
    fn flight_trace(&mut self, m: &Missile, owner: u16, start: Vec3, end: Vec3) -> ShotTrace {
        let through = if m.pos.delta[2].abs() <= 30.0 || self.in_water(start) {
            m.clipmask
        } else {
            m.clipmask | contents::WATER
        };
        let tr = self.missile_trace_mask(through, owner, start, end);
        if surface_type(tr.surface_flags) != SURF_TYPE_WATER {
            return tr;
        }
        self.missile_bounce_event(m, start, tr.normal, SURF_TYPE_WATER);
        self.missile_trace(m, owner, start, end)
    }

    /// `EV_GRENADE_BOUNCE`: a bounce, or a splash, for the clients to show.
    fn missile_bounce_event(&mut self, m: &Missile, origin: Vec3, normal: Vec3, surface: usize) {
        let now = self.level.time;
        let (weapon, owner) = (m.weapon, m.parent.unwrap_or(1023));
        self.tempev
            .add(now, crate::tempev::ev::MISSILE_BOUNCE, |s| {
                s.origin = origin;
                s.angles = crate::tempev::dir_to_angles(normal);
                s.event_parm = surface as u8;
                s.weapon = weapon;
                s.client = owner;
            });
    }

    /// `Missile_PenetrateGlass`: a missile that strikes a damageable entity of glass hurts it (`MOD_IMPACT`) and
    /// flies on through it; glass that belongs to the map stops it like any wall.
    fn penetrate_glass(
        &mut self,
        vm: &mut Vm,
        n: u16,
        m: &Missile,
        tr: &mut ShotTrace,
        (owner, start, end): (u16, Vec3, Vec3),
    ) {
        let glass = tr.hit;
        let velocity = m.pos.evaluate_delta(self.level.time);
        if glass >= ENTITYNUM_WORLD
            || !self.ent(glass).is_some_and(|e| e.takedamage)
            || m.info.damage == 0
            || dot(velocity, velocity) < 1.0
        {
            return;
        }
        let mut d = Damage::new(m.info.damage, MOD_IMPACT);
        d.inflictor = Some(n);
        d.attacker = (owner != ENTITYNUM_NONE).then_some(owner);
        d.dir = Some(velocity);
        d.point = self.ent(n).map(|e| e.origin);
        d.weapon = u32::from(m.weapon);
        d.hitloc = tr.hitloc;
        self.g_damage(vm, glass, d);
        // Look again with the pane out of the way.
        let Some(e) = self.ent_mut(glass) else { return };
        let contents = std::mem::take(&mut e.contents);
        self.relink(glass);
        *tr = self.missile_trace(m, owner, start, end);
        if let Some(e) = self.ent_mut(glass) {
            e.contents = contents;
            self.relink(glass);
        }
    }

    /// `RunMissile_Destabilize`: after `destabilize_distance` of flight an unguided rocket's heading starts to drift,
    /// by an angular rate it draws at random every `destabilizationRateTime`, and attractors and repulsors bend it.
    fn destabilize(&mut self, n: u16, m: &mut Missile) {
        let now = self.level.time;
        let Some(e) = self.ent(n) else { return };
        let (origin, started) = (e.origin, e.flags & FL_MISSILE_DESTABILIZED != 0);
        if m.pos.time + m.destabilize_ms >= now {
            if !started {
                return;
            }
        } else {
            let max = m.info.destabilization_curvature_max;
            m.curvature = if started {
                // Later changes keep turning the same way round as the last.
                let mut rate = [0.0; 3].map(|_| self.flrand(0.0, 1.0) * max);
                if m.curvature[1] < 0.0 {
                    rate[0] = -rate[0];
                }
                if m.curvature[0] > 0.0 {
                    rate[1] = -rate[1];
                }
                rate
            } else {
                [0.0; 3].map(|_| self.flrand(-1.0, 1.0) * max)
            };
            m.pos.time = now;
            m.destabilize_ms = (m.info.destabilization_rate_time * 1000.0) as i32;
            if let Some(e) = self.ent_mut(n) {
                e.flags |= FL_MISSILE_DESTABILIZED;
            }
        }
        let angles = mad(m.apos.base, 0.05, m.curvature);
        m.apos.base = angles;
        m.apos.kind = TrType::Interpolate;
        if let Some(e) = self.ent_mut(n) {
            e.angles = angles;
        }
        let heading = math::angle_vectors(&angles).0;
        m.pos.delta = mad([0.0; 3], m.info.projectile_speed as f32, heading);
        m.pos.base = origin;
        m.pos.kind = TrType::Interpolate;
        self.apply_attractors(m, 0.05);
    }

    /// `Missile_ApplyAttractorsRepulsors`: every slot in front of the missile and within its `maxDist` pulls the
    /// heading sideways toward it (an attractor, no more than the sideways speed that would take the missile there)
    /// or pushes it away (a repulsor), more the nearer it is and the more nearly it lies along the flight line.
    fn apply_attractors(&self, m: &mut Missile, dt: f32) {
        let speed = length(m.pos.delta);
        if speed < 1e-5 {
            return;
        }
        let forward = mad([0.0; 3], 1.0 / speed, m.pos.delta);
        let mut push = [0.0f32; 3];
        for i in 0..Attractors::MAX {
            let Some(a) = self.attractors.get(i) else {
                continue;
            };
            let at = a
                .entity
                .and_then(|n| self.ent(n))
                .map_or(a.origin, |e| e.origin);
            let delta = sub(at, m.pos.base);
            let ahead = dot(delta, forward);
            if ahead <= 0.0 {
                continue;
            }
            let aside = mad(delta, -ahead, forward);
            let mut aside_dist = length(aside);
            let mut aside_dir = if aside_dist < 1e-5 {
                [0.0; 3]
            } else {
                mad([0.0; 3], 1.0 / aside_dist, aside)
            };
            if aside_dist < 1e-5 {
                if a.attractor {
                    continue;
                }
                aside_dir = [0.0, 0.0, -1.0];
            }
            if !a.attractor && aside_dir[2] > 0.0 {
                aside_dir = [0.0, 0.0, -1.0];
                aside_dist = 0.0;
            }
            let total = length(delta);
            if a.max_dist < total {
                continue;
            }
            let mut force = (1.0 - total / a.max_dist) * a.strength;
            // Of the way round to facing it: 0 straight ahead, 1 at right angles.
            let angle = (aside_dist.abs() / ahead).atan() * (2.0 / std::f32::consts::PI);
            force *= if a.attractor { angle } else { angle - 1.0 };
            if a.attractor {
                let max_correcting_accel =
                    m.info.projectile_speed as f32 * aside_dist / ahead * 20.0;
                if force - max_correcting_accel >= 0.0 {
                    force = max_correcting_accel;
                }
            }
            push = mad(push, force, aside_dir);
        }
        if push != [0.0; 3] {
            let heading = normalized(mad(m.pos.delta, dt, push));
            m.pos.delta = mad([0.0; 3], m.info.projectile_speed as f32, heading);
            m.apos.base = [
                math::vec_to_pitch(&heading),
                math::vec_to_yaw(&heading),
                0.0,
            ];
        }
    }

    /// Where a missile that goes off at `origin` shows its explosion. Under water it is on the surface, with the
    /// water's normal; deeper under than `missileWaterMaxDepth` there is none (`None`).
    fn explosion_site(
        &self,
        n: u16,
        origin: Vec3,
        normal: Vec3,
        surface: u8,
    ) -> Option<(Vec3, Vec3, u8)> {
        use sim::cm::Collide;
        if !self.in_water(origin) {
            return Some((origin, normal, surface));
        }
        let top = [
            origin[0],
            origin[1],
            origin[2] + self.cvars.float("missileWaterMaxDepth"),
        ];
        let w = self.world.as_ref()?;
        let t = w.trace(top, origin, [0.0; 3], [0.0; 3], n, contents::WATER);
        (!t.start_solid && t.fraction < 1.0).then(|| {
            (
                lerp(top, origin, t.fraction),
                t.normal,
                SURF_TYPE_WATER as u8,
            )
        })
    }

    /// `MissileIsReadyForSteering`: the motor has finished spooling up.
    fn ready_for_steering(m: &Missile, now: i32) -> bool {
        m.time_to_accelerate - (now - m.pos.time) as f32 * 0.001 <= 0.0
    }

    /// `GuidedMissileSteering`: a missile with a target bends its velocity toward it once it is ready. Sidewinder
    /// (`guidedMissileType` 1, the helicopter's rockets) and hellfire (2) steer by acceleration within the weapon's
    /// `maxSteeringAccel`; the javelin (3) rotates its velocity at a capped turn rate through soft launch, climb
    /// and descent.
    fn guided_steering(&self, m: &mut Missile, origin: Vec3, now: i32, dt: f32) {
        let kind = m.info.guided_missile_type;
        if kind == 0 || !Self::ready_for_steering(m, now) {
            return;
        }
        let Some(target) = m.target.and_then(|t| self.ent(t)) else {
            return;
        };
        let target_pos = mad(target.origin, 1.0, m.target_offset);
        if kind == GUIDED_JAVELIN {
            let owner = m
                .parent
                .and_then(|p| self.ent(p))
                .map_or([0.0; 3], |e| e.origin);
            self.javelin_steering(m, origin, now, dt, target_pos, target.origin, owner);
            return;
        }
        let max_accel = m.info.max_steering_accel;
        if max_accel <= 0.0 {
            return;
        }
        let flat = (m.pos.delta[0] * m.pos.delta[0] + m.pos.delta[1] * m.pos.delta[1]).sqrt();
        let dir = if flat == 0.0 {
            [0.0; 2]
        } else {
            [m.pos.delta[0] / flat, m.pos.delta[1] / flat]
        };
        let right = [dir[1], -dir[0]];
        let to = sub(target_pos, m.pos.base);
        let rel = [
            dir[1] * to[1] + dir[0] * to[0],
            right[1] * to[1] + right[0] * to[0],
            to[2],
        ];
        let mut steer = [0.0f32; 3];
        Self::horz_steer(&m.info, right, rel, flat, &mut steer);
        Self::vertical_steer(m, rel, flat, &mut steer);
        m.pos.delta = mad(m.pos.delta, dt, steer);
    }

    /// `MissileHorzSteerToTarget`.
    fn horz_steer(
        info: &WeaponInfo,
        right: [f32; 2],
        rel: Vec3,
        horz_speed: f32,
        steer: &mut Vec3,
    ) {
        let max_accel = info.max_steering_accel;
        let speed = info.projectile_speed as f32;
        let tightest = speed * speed / max_accel;
        let radius = if rel[1] == 0.0 {
            f32::MAX
        } else {
            (rel[1] * rel[1] + rel[0] * rel[0]) / (rel[1] * 2.0)
        };
        let mut side = None;
        if rel[0] <= 0.0 {
            if radius.abs() >= tightest + 60.0 {
                side = Some(if rel[1] <= 0.0 { -max_accel } else { max_accel });
            }
        } else if rel[1] != 0.0 && tightest <= radius.abs() {
            side = Some((horz_speed * 2.0 * horz_speed / radius).clamp(-max_accel, max_accel));
        }
        if let Some(a) = side {
            steer[0] = a * right[0];
            steer[1] = a * right[1];
        }
    }

    /// `MissileVerticalSteering`: sidewinder always steers its climb to the target; hellfire first climbs at
    /// `missileHellfireUpAccel` until it is on its way up, then does the same.
    fn vertical_steer(m: &mut Missile, rel: Vec3, horz_speed: f32, steer: &mut Vec3) {
        let horz = (rel[0] * rel[0] + rel[1] * rel[1]).sqrt();
        steer[2] = 0.0;
        if horz == 0.0 {
            return;
        }
        let max_accel = m.info.max_steering_accel;
        let speed = m.info.projectile_speed as f32;
        if m.info.guided_missile_type != GUIDED_HELLFIRE || m.pos.duration != 0 {
            // `MissileVerticalSteerToTarget`.
            if rel[0] / horz >= 0.0 {
                let wish = (rel[0] / horz * horz_speed).abs() * rel[2] / horz;
                steer[2] = ((wish - m.pos.delta[2]) * 20.0).clamp(-max_accel, max_accel);
            }
            return;
        }
        let min_time = if rel[0] <= 0.0 || horz_speed == 0.0 {
            horz / speed
        } else {
            rel[0] / speed
        };
        let mut max_vert = rel[2] / min_time;
        max_vert = (max_accel * 0.5 * min_time + max_vert) * 0.9;
        max_vert = max_vert.min(horz_speed * HELLFIRE_MAX_SLOPE);
        if m.pos.delta[2] >= max_vert {
            m.pos.duration = 1;
        } else {
            let want = (max_vert - m.pos.delta[2]) * 20.0;
            steer[2] = if -max_accel - want < 0.0 {
                want.min(HELLFIRE_UP_ACCEL)
            } else {
                -max_accel
            };
        }
    }

    /// `JavelinSteering`.
    #[allow(clippy::too_many_arguments)]
    fn javelin_steering(
        &self,
        m: &mut Missile,
        origin: Vec3,
        now: i32,
        dt: f32,
        target_pos: Vec3,
        target_origin: Vec3,
        owner_origin: Vec3,
    ) {
        if m.stage == JavelinStage::SoftLaunch {
            if now - m.launch_time < m.info.proj_ignition_delay {
                return;
            }
            m.stage = JavelinStage::Ascent;
            m.pos.kind = TrType::Interpolate;
            m.pos.base = origin;
        }
        let mut aim = target_pos;
        if m.stage == JavelinStage::Ascent {
            if self.javelin_climb_end(m, aim) {
                m.stage = JavelinStage::Descent;
            } else {
                // `JavelinClimbOffset`, direct-fire mode (every multiplayer javelin): aim high, and a way back
                // toward the owner so the dive comes in over the target.
                aim[2] += self.cvars.float("missileJavClimbHeightDirect");
                let back = [
                    owner_origin[0] - target_origin[0],
                    owner_origin[1] - target_origin[1],
                ];
                let len = (back[0] * back[0] + back[1] * back[1]).sqrt();
                if len != 0.0 {
                    let scale = self.cvars.float("missileJavClimbToOwner") / len;
                    aim[0] += back[0] * scale;
                    aim[1] += back[1] * scale;
                }
            }
        }
        let to = normalized(sub(aim, m.pos.base));
        m.pos.delta = self.javelin_rotate_velocity(m, to, dt);
    }

    /// `JavelinClimbEnd`: above the ceiling, and either the climb angle or the distance says dive.
    fn javelin_climb_end(&self, m: &Missile, target_pos: Vec3) -> bool {
        let height = m.pos.base[2] - target_pos[2] - m.target_offset[2];
        if self.cvars.float("missileJavClimbCeilingDirect") >= height {
            return false;
        }
        // `JavelinClimbExceededAngle`.
        let limit = self.cvars.float("missileJavClimbAngleDirect");
        let flat = (m.pos.delta[0] * m.pos.delta[0] + m.pos.delta[1] * m.pos.delta[1]).sqrt();
        let dir = if flat == 0.0 {
            [0.0; 2]
        } else {
            [m.pos.delta[0] / flat, m.pos.delta[1] / flat]
        };
        let to = sub(target_pos, m.pos.base);
        let along = dir[1] * to[1] + dir[0] * to[0];
        let deg = (along / to[2]).atan().to_degrees().abs();
        if limit > deg || deg.is_nan() {
            return true;
        }
        // `JavelinClimbWithinDistance`.
        let (dx, dy) = (m.pos.base[0] - target_pos[0], m.pos.base[1] - target_pos[1]);
        (dx * dx + dy * dy).sqrt() < 400.0
    }

    /// `JavelinRotateVelocity`: turn toward `target_dir`, speed up while the turn is small, slow with the turn.
    fn javelin_rotate_velocity(&self, m: &Missile, target_dir: Vec3, dt: f32) -> Vec3 {
        let vel = m.pos.delta;
        let mut len = length(vel);
        let (dir, turn_diff) = self.javelin_rotate_dir(normalized(vel), target_dir, dt);
        if turn_diff < 30.0 {
            let (accel, limit) = if m.stage == JavelinStage::Ascent || vel[2] > 0.0 {
                ("missileJavAccelClimb", "missileJavSpeedLimitClimb")
            } else {
                ("missileJavAccelDescend", "missileJavSpeedLimitDescend")
            };
            len = (self.cvars.float(accel) * dt + len).min(self.cvars.float(limit));
        }
        len *= 1.0 - turn_diff / 180.0 * self.cvars.float("missileJavTurnDecel");
        mad([0.0; 3], len, dir)
    }

    /// `JavelinRotateDir`: turns toward `target` by at most `missileJavTurnRateDirect` degrees a second. Returns
    /// the new direction and how much it had to turn (in the engine's scaled units; 0 when it reached the target).
    fn javelin_rotate_dir(&self, current: Vec3, target: Vec3, dt: f32) -> (Vec3, f32) {
        let max_dps = self.cvars.float("missileJavTurnRateDirect");
        let diff = (1.0 - (dot(target, current) + 1.0) * 0.5) * 180.0;
        if diff <= 0.1 {
            return (target, 0.0);
        }
        let wanted_dps = diff / dt;
        if max_dps > wanted_dps {
            return (target, 0.0);
        }
        let q = quat_slerp(
            dir_to_quat(current),
            dir_to_quat(target),
            max_dps / wanted_dps,
        );
        (quat_forward(q), diff)
    }

    /// `MissileTrajectory`: where the missile wants to be this frame.
    fn missile_next_origin(&mut self, n: u16, m: &mut Missile) -> Vec3 {
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
            if m.info.projectile_curvature > 0.0 {
                m.pos.delta = mad(m.pos.delta, dt, m.curvature);
            }
            let origin = self.ent(n).map_or(m.pos.base, |e| e.origin);
            self.guided_steering(m, origin, now, dt);
            let javelin_coasting =
                m.info.guided_missile_type == GUIDED_JAVELIN && m.stage == JavelinStage::SoftLaunch;
            let next = if javelin_coasting {
                m.pos.evaluate(now)
            } else {
                m.pos.base = mad(m.pos.base, dt, m.pos.delta);
                m.pos.base
            };
            if m.apos.kind == TrType::Interpolate
                && Self::ready_for_steering(m, now)
                && length(m.pos.delta) > 1.0
            {
                // A rocket that steers or has destabilised points where it flies.
                let angles = [
                    math::vec_to_pitch(&m.pos.delta),
                    math::vec_to_yaw(&m.pos.delta),
                    0.0,
                ];
                m.apos.base = angles;
                if let Some(e) = self.ent_mut(n) {
                    e.angles = angles;
                }
            }
            next
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
        let mut tr = self.flight_trace(m, owner, origin, next);
        if surface_type(tr.surface_flags) == SURF_TYPE_GLASS {
            self.penetrate_glass(vm, n, m, &mut tr, (owner, origin, next));
        }
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
        if m.kind == MissileKind::Grenade {
            self.grenade_touch_trigger_damage(vm, n, old, origin, m.info.explosion_inner_damage);
        }
        if tr.fraction == 1.0 {
            if length(m.pos.delta) != 0.0 {
                m.ground = None;
                let flags = self.ent(n).map_or(0, |e| e.flags);
                if m.kind == MissileKind::Rocket
                    && m.info.weap_type == WeaponType::Projectile
                    && m.info.guided_missile_type == 0
                    && flags & FL_STABLE_MISSILES == 0
                {
                    self.destabilize(n, m);
                }
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
        // Under water the explosion shows on the surface; too deep under it, not at all.
        if let Some((at, normal, surface)) = self.explosion_site(n, origin, normal, m.surface) {
            self.explosion_event(m, at, normal, surface);
        }
        if m.info.explosion_inner_damage != 0 {
            self.missile_blast(vm, n, m, origin, math::angle_vectors(&angles).0, None);
        }
        self.missile_flash(vm, m, origin);
        vm.notify_entity(n, "death", &[]);
        self.free_entity(vm, n);
    }

    /// The `EV_*_EXPLODE` of a missile going off on a surface of type `surface` (a `proj_explosion` of `dud` also
    /// plays the dud), for the clients' effects and sounds.
    fn explosion_event(&mut self, m: &Missile, origin: Vec3, normal: Vec3, surface: u8) {
        let now = self.level.time;
        let (weapon, owner) = (m.weapon, m.parent.unwrap_or(1023));
        let angles = crate::tempev::dir_to_angles(normal);
        let mut kinds = vec![crate::tempev::ev::EXPLOSION];
        if m.info.proj_explosion == ProjExplosion::Dud {
            kinds.push(crate::tempev::ev::DUD);
        }
        for kind in kinds {
            self.tempev.add(now, kind, |s| {
                s.origin = origin;
                s.angles = angles;
                s.event_parm = surface;
                s.weapon = weapon;
                s.client = owner;
            });
        }
    }

    /// `EV_DUD_IMPACT`: a missile that will never go off hit `normal`'s surface.
    fn dud_event(&mut self, m: &Missile, normal: Vec3, kind: u8) {
        let origin = m.pos.evaluate(self.level.time);
        self.tempev.add(self.level.time, kind, |s| {
            s.origin = origin;
            s.angles = crate::tempev::dir_to_angles(normal);
            s.event_parm = m.surface;
            s.weapon = m.weapon;
            s.client = m.parent.unwrap_or(1023);
        });
    }

    /// The flash of a `WEAPPROJEXP_FLASHBANG` weapon's explosion (`G_FlashbangBlast`), credited to the parent
    /// and its team.
    fn missile_flash(&mut self, vm: &mut Vm, m: &Missile, origin: Vec3) {
        if m.info.proj_explosion == ProjExplosion::Flashbang {
            let parent = m.parent.filter(|p| self.ent(*p).is_some());
            let info = &m.info;
            let (max, min) = (
                info.explosion_radius as f32,
                info.explosion_radius_min as f32,
            );
            self.flashbang_blast(vm, origin, max, min, parent, m.team);
        }
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
                skip_clients: false,
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
        // `GrenadeDud` or `JavelinDud`: one that never armed, or a javelin still being tossed out.
        let dud = (m.info.projectile_activate_dist > 0
            && m.travel_dist < m.info.projectile_activate_dist as f32)
            || (m.info.guided_missile_type == GUIDED_JAVELIN
                && m.stage == JavelinStage::SoftLaunch);
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
                // `EV_CHANGE_TO_DUD`: the grenade came to rest without arming.
                self.dud_event(m, tr.normal, crate::tempev::ev::CHANGE_TO_DUD);
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
        if damage != 0 {
            let origin = self.ent(n).map_or(endpos, |e| e.origin);
            let by = owner.unwrap_or(ENTITYNUM_WORLD);
            self.check_hit_trigger_damage(vm, by, origin, endpos, damage, mean);
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
        // A dud (a grenade not yet armed) does not go off.
        let surface = surface_type(tr.surface_flags).min(28) as u8;
        if dud {
            m.surface = surface;
            self.dud_event(m, tr.normal, crate::tempev::ev::DUD);
        } else {
            self.explosion_event(m, endpos, tr.normal, surface);
            self.missile_flash(vm, m, endpos);
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
        m.surface = surf as u8;
        self.missile_bounce_event(m, origin, tr.normal, surf);
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
    fn run_item(&mut self, vm: &mut Vm, n: u16) {
        self.item_think(n);
        let now = self.level.time;
        let Some(e) = self.ent(n) else { return };
        if e.mv.pos.tr.kind != TrType::Gravity {
            return;
        }
        let (tr, origin, mins, maxs) = (e.mv.pos.tr, e.origin, e.mins, e.maxs);
        let next = tr.evaluate(now);
        let Some(world) = self.world.as_ref() else {
            return;
        };
        let t = world.trace(origin, next, mins, maxs, n, Game::item_clipmask());
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
        // It landed on something it cannot rest on (a wall or ceiling): gone.
        if t.fraction < 1.0 && t.normal[2] <= 0.0 {
            self.free_entity(vm, n);
            return;
        }
        self.relink(n);
    }
}

/// The roll that lays the missile's up axis against the surface (`MissileLandAnglesFlat`).
fn roll_on_normal(angles: &Vec3, normal: &Vec3) -> f32 {
    let (_, right, up) = math::angle_vectors(angles);
    dot(right, *normal).atan2(dot(up, *normal)).to_degrees()
}
