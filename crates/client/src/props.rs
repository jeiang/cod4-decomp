// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (GPL-3.0, KisakCOD contributors and Activision): `DynEntity/DynEntity_client.cpp`
// (`DynEntCl_ExplosionEvent`, `DynEntCl_DynEntImpactEvent`, `DynEntCl_Damage`) and `DynEntity/DynEntity_pieces.cpp`.
//! The map's loose props (the clipmap's clutter and destructible dynamic entities: crates, barrels, bottles). The
//! server does not simulate them, so each client draws them where the map puts them and, when a shot, a blast or a
//! physics explosion reaches one, lets it fall as a rigid body with its PhysPreset (mass, bounce, friction, and the
//! model's physics geometry and mass distribution; see [`fx::Body`]) that collides with the map. What a client does to
//! a prop is not shared; it does not hurt anyone.
//!
//! A destructible prop (`kind` 2) has health: shots and blasts hurt it, and when it is spent it vanishes, plays its
//! destroy effect and throws its destroy pieces as rigid bodies (`DynEntCl_Damage`). Bodies that hit hard enough are
//! heard ([`Collision`]).

use crate::effects::Tracer;
use crate::events::ClientEvent;
use assets::zone::clipmap::{DynEntityDef, XModelPieces};
use assets::zone::fx::FxEffectDef;
use assets::zone::phys::PhysPreset;
use assets::zone::weapon::WeaponDef;
use assets::zone::xmodel::XModel;
use fx::{Body, Frame, Impact, Mass, Shape};
use glam::Vec3;
use render::{ModelInstance, ModelKind};
use server::tempev::Physics;
use sim::cm::Collide;
use std::collections::HashMap;
use std::sync::Arc;

const STEP: f32 = 1.0 / 60.0;
/// A prop that has not settled this long is put to sleep where it is.
const LIFE: f32 = 12.0;
/// `dynEnt_bulletForce`: the speed of the bullet that strikes a prop.
const BULLET_FORCE: f32 = 1000.0;
/// `dynEnt_explodeForce`: the momentum a blast gives a prop at its centre, times the preset's explosive force scale.
const EXPLODE_FORCE: f32 = 12500.0;
/// `dynEnt_explodeUpbias` and `dynEnt_explodeSpinScale` (the random offset of the push from the centre of mass).
const EXPLODE_UP_BIAS: f32 = 0.5;
const EXPLODE_SPIN_SCALE: f32 = 3.0;
/// The upward speed a jitter of one unit gives, units per second.
const JITTER_KICK: f32 = 120.0;
/// `dynEnt_explodeMinForce`: a blast gives a prop less than this momentum does not even wake it.
const EXPLODE_MIN_FORCE: f32 = 40.0;
/// `dynEnt_explodeMaxEnts`: the most props one blast wakes (the nearest).
const EXPLODE_MAX_ENTS: usize = 20;
/// `dynEntPieces_impactForce`, and the most pieces alive.
const PIECES_FORCE: f32 = 1000.0;
const MAX_PIECES: usize = 100;
/// The most collision sounds one frame starts (`SND_MAX_PHYSICS`).
pub const MAX_COLLISION_SOUNDS: usize = 32;

/// Corner `i` of a box is at `mins` where bit `k` is clear and at `maxs` where it is set.
fn corner(i: usize, mins: Vec3, maxs: Vec3) -> Vec3 {
    Vec3::new(
        if i & 1 == 0 { mins.x } else { maxs.x },
        if i & 2 == 0 { mins.y } else { maxs.y },
        if i & 4 == 0 { mins.z } else { maxs.z },
    )
}

/// The engine's `angles` (pitch, yaw, roll degrees) of a rotation whose forward, left and up axes are `axes`.
pub fn angles_of(axes: [Vec3; 3]) -> [f32; 3] {
    let [f, _, u] = axes;
    let yaw = f.y.atan2(f.x);
    let pitch = -f.z.clamp(-1.0, 1.0).asin();
    // The roll is how far up has turned from the up of a body with this pitch and yaw and no roll.
    let left0 = Vec3::Z.cross(f).normalize_or(Vec3::Y);
    let up0 = f.cross(left0);
    let roll = u.dot(-left0).atan2(u.dot(up0));
    [pitch.to_degrees(), yaw.to_degrees(), roll.to_degrees()]
}

struct Prop {
    model: Arc<XModel>,
    /// What the map says it is made of; a prop without a PhysPreset cannot move.
    preset: Option<Arc<PhysPreset>>,
    shape: Shape,
    origin: Vec3,
    /// Forward, left and up when placed.
    axes: [Vec3; 3],
    /// Made when something first strikes it, and kept at rest in the map's pose after it settles.
    body: Option<Body>,
    carry: f32,
    age: f32,
    /// A destructible prop's health, and what it does when it breaks.
    destructible: bool,
    health: i32,
    destroy_fx: Option<Arc<FxEffectDef>>,
    destroy_pieces: Option<Arc<XModelPieces>>,
    gone: bool,
    /// The surface type (`SURF_TYPEINDEX`) its shots land on.
    surface: u8,
}

impl Prop {
    fn axes(&self) -> [Vec3; 3] {
        self.body.as_ref().map_or(self.axes, Body::axes)
    }

    fn origin(&self) -> Vec3 {
        self.body.as_ref().map_or(self.origin, Body::origin)
    }

    /// World-space bounds, from the corners of the model's bounds where they are.
    fn bounds(&self) -> (Vec3, Vec3) {
        let (mins, maxs) = (Vec3::from(self.model.mins), Vec3::from(self.model.maxs));
        let (a, o) = (self.axes(), self.origin());
        (0..8).map(|i| corner(i, mins, maxs)).fold(
            (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
            |(lo, hi), c| {
                // The model's Y points away from the left axis.
                let w = o + a[0] * c.x - a[1] * c.y + a[2] * c.z;
                (lo.min(w), hi.max(w))
            },
        )
    }

    /// The body, made on first use; `None` for a prop that cannot move.
    fn body(&mut self) -> Option<&mut Body> {
        if self.body.is_none() {
            let preset = self.preset.as_ref()?;
            let mut b = Body::new(preset, &self.shape, self.origin, self.axes, Vec3::ZERO);
            b.sleep();
            self.body = Some(b);
        }
        self.body.as_mut()
    }

    /// Strikes the prop with `f` and wakes it; `false` when it cannot move.
    fn strike(&mut self, f: impl FnOnce(&mut Body, &PhysPreset)) -> bool {
        let Some(preset) = self.preset.clone() else {
            return false;
        };
        let Some(b) = self.body() else {
            return false;
        };
        b.wake();
        f(b, &preset);
        self.age = 0.0;
        true
    }

    /// Steps the body to the end of `dt`; the first loud hit of any step is returned with the sound prefix.
    fn advance(&mut self, dt: f32, world: &dyn fx::World) -> Option<(Arc<str>, Impact)> {
        let b = self.body.as_mut()?;
        if b.asleep() {
            return None;
        }
        let mut loud = None;
        self.carry += dt.min(0.1);
        while self.carry >= STEP {
            self.carry -= STEP;
            b.step(STEP, world);
            if loud.is_none()
                && let (Some(hit), Some(prefix)) = (b.take_impact(), b.sound_prefix())
            {
                loud = Some((prefix.clone(), hit));
            }
            self.age += STEP;
            if self.age >= LIFE {
                b.sleep();
            }
        }
        loud
    }

    /// Hurts a destructible prop; `true` when this breaks it.
    fn hurt(&mut self, damage: i32) -> bool {
        if !self.destructible || self.gone || damage <= 0 {
            return false;
        }
        self.health -= damage;
        self.gone = self.health <= 0;
        if self.gone {
            self.body = None;
        }
        self.gone
    }

    fn instance(&self) -> ModelInstance {
        let origin = self.origin().to_array();
        let mut m = ModelInstance::new(self.model.clone(), ModelKind::World);
        m.origin = origin;
        m.angles = angles_of(self.axes());
        m.light_origin = origin;
        m
    }
}

/// Every prop of the map.
pub struct Props {
    props: Vec<Prop>,
    /// Props that started to fall, for the report.
    pub woken: u64,
    /// Where the next random spin offset of a blast comes from.
    seed: u32,
    /// What broke props threw.
    pieces: Vec<Piece>,
    out: Happened,
}

/// What a frame of prop interaction asks the rest of the client to play.
#[derive(Default)]
pub struct Happened {
    /// Bullets that struck a prop, for the impact effect and sound of their weapon.
    pub impacts: Vec<ClientEvent>,
    /// Destroy effects of broken props.
    pub fx: Vec<(Arc<FxEffectDef>, Frame)>,
    /// Bodies that hit hard enough to be heard.
    pub collisions: Vec<Collision>,
}

/// A body hit something hard enough to be heard: the preset's sound prefix, where, and the surface hit.
#[derive(Clone, Debug, PartialEq)]
pub struct Collision {
    pub prefix: Arc<str>,
    pub origin: [f32; 3],
    pub surface: u8,
}

/// The surface type under a hit (`SURF_TYPEINDEX` of the contact), found by looking through the surface.
pub fn heard(world: &dyn Collide, prefix: Arc<str>, hit: &Impact) -> Collision {
    let t = world.trace(
        (hit.at + hit.normal * 2.0).to_array(),
        (hit.at - hit.normal * 2.0).to_array(),
        [0.0; 3],
        [0.0; 3],
        sim::cm::ENTITYNUM_NONE,
        sim::contents::SOLID,
    );
    Collision {
        prefix,
        origin: hit.at.to_array(),
        surface: surface_type(t.surface_flags),
    }
}

/// `SURF_TYPEINDEX`.
fn surface_type(flags: i32) -> u8 {
    ((flags & 0x01F0_0000) >> 20) as u8
}

/// A piece a broken prop threw.
struct Piece {
    model: Arc<XModel>,
    body: Body,
    carry: f32,
}

impl Piece {
    fn advance(&mut self, dt: f32, world: &dyn fx::World) -> Option<Impact> {
        if self.body.asleep() {
            return None;
        }
        self.carry += dt.min(0.1);
        let mut loud = None;
        while self.carry >= STEP {
            self.carry -= STEP;
            self.body.step(STEP, world);
            loud = loud.or(self.body.take_impact());
        }
        loud
    }
}

/// `DynEntPieces_SpawnPieces`: each piece with a PhysPreset falls from where it sat in the broken prop, struck at
/// the hit along a direction spread by the preset's `piecesSpreadFraction`.
fn spawn_pieces(
    out: &mut Vec<Piece>,
    pieces: &XModelPieces,
    (origin, axes): (Vec3, [Vec3; 3]),
    hit: Vec3,
    dir: Vec3,
    seed: &mut u32,
) {
    for piece in pieces.pieces.iter() {
        let Some(model) = &piece.model else { continue };
        let Some(preset) = &model.phys_preset else {
            continue;
        };
        if out.len() >= MAX_PIECES {
            break;
        }
        let o = Vec3::from(piece.offset);
        let at = origin + axes[0] * o.x - axes[1] * o.y + axes[2] * o.z;
        let up = Vec3::Z * preset.pieces_upward_velocity;
        let mut body = Body::new(preset, &Shape::of_model(model), at, axes, up);
        let noise = Vec3::new(random(seed), random(seed), random(seed));
        let force = dir.lerp(noise, preset.pieces_spread_fraction.clamp(0.0, 1.0));
        body.bullet_impact(hit, force, PIECES_FORCE, preset.bullet_force_scale);
        out.push(Piece {
            model: model.clone(),
            body,
            carry: 0.0,
        });
    }
}

impl Props {
    pub fn new(defs: &[Arc<[DynEntityDef]>]) -> Self {
        let mut props = Vec::new();
        for d in defs.iter().flat_map(|l| l.iter()) {
            let Some(model) = d.model.clone() else {
                continue;
            };
            let (mins, maxs) = (Vec3::from(model.mins), Vec3::from(model.maxs));
            if (maxs - mins).min_element() <= 0.0 {
                continue;
            }
            // The map stores the pose as a quaternion; its forward, left and up are these.
            let q = glam::Quat::from_xyzw(
                d.pose.quat[0],
                d.pose.quat[1],
                d.pose.quat[2],
                d.pose.quat[3],
            )
            .normalize();
            let axes = [q * Vec3::X, q * Vec3::NEG_Y, q * Vec3::Z];
            let mass = Mass {
                center: Vec3::from(d.center_of_mass),
                moments: Vec3::from(d.moments_of_inertia),
                products: Vec3::from(d.products_of_inertia),
            };
            let surface = model
                .coll_surfs
                .first()
                .map_or(0, |c| surface_type(c.surface_flags));
            props.push(Prop {
                shape: Shape::of_model(&model).with_mass(mass),
                model,
                preset: d.phys_preset.clone(),
                origin: Vec3::from(d.pose.origin),
                axes,
                body: None,
                carry: 0.0,
                age: 0.0,
                destructible: d.kind == 2,
                health: d.health,
                destroy_fx: d.destroy_fx.clone(),
                destroy_pieces: d.destroy_pieces.clone(),
                gone: false,
                surface,
            });
        }
        Self {
            props,
            woken: 0,
            seed: 0x9E37_79B9,
            pieces: Vec::new(),
            out: Happened::default(),
        }
    }

    pub fn len(&self) -> usize {
        self.props.len()
    }

    /// Reacts to `ev`: blasts and physics events near a prop, and shots that would hit one. `weapon` resolves a weapon
    /// index to its definition (radii, damage, impact type).
    pub fn event(
        &mut self,
        ev: &ClientEvent,
        weapon: &dyn Fn(u16) -> Option<Arc<WeaponDef>>,
        world: &dyn Collide,
    ) {
        let before = self.props.iter().filter(|p| p.body.is_some()).count();
        match ev {
            ClientEvent::Explosion {
                origin, weapon: w, ..
            } => {
                if let Some(d) = weapon(*w) {
                    // A rocket's blast is full strength out to its radius; a grenade's falls off from the centre.
                    let radius = d.explosion_radius as f32;
                    self.blast(
                        Vec3::from(*origin),
                        Blast {
                            inner: if d.impact_type == IMPACT_GRENADE_EXPLODE {
                                0.0
                            } else {
                                radius
                            },
                            outer: radius,
                            damage: (d.explosion_inner_damage, d.explosion_outer_damage),
                            ..Blast::default()
                        },
                    );
                }
            }
            ClientEvent::Physics { origin, what } => self.physics(Vec3::from(*origin), what),
            ClientEvent::WeaponFire {
                eye,
                angles,
                weapon: w,
                shooter,
                ..
            } => {
                if let Some(d) = weapon(*w).filter(|d| {
                    sim::weapon::WeaponType::from_raw(d.weap_type)
                        == sim::weapon::WeaponType::Bullet
                }) {
                    self.shot(Vec3::from(*eye), *angles, (*w, *shooter, d.damage), world);
                }
            }
            _ => {}
        }
        self.woken += (self.props.iter().filter(|p| p.body.is_some()).count() - before) as u64;
    }

    /// What the props asked the client to play since the last call.
    pub fn take(&mut self) -> Happened {
        std::mem::take(&mut self.out)
    }

    fn physics(&mut self, at: Vec3, what: &Physics) {
        match *what {
            Physics::Explosion {
                cylinder,
                outer,
                inner,
                magnitude,
            } => self.blast(
                at,
                Blast {
                    cylinder,
                    inner,
                    outer,
                    scale: magnitude,
                    ..Blast::default()
                },
            ),
            Physics::Jolt {
                outer,
                inner,
                impulse,
            } => self.blast(
                at,
                Blast {
                    cylinder: true,
                    inner,
                    outer,
                    impulse: Vec3::from(impulse),
                    ..Blast::default()
                },
            ),
            Physics::Jitter {
                outer,
                inner,
                min,
                max,
            } => jitter(
                &mut self.props,
                at,
                (outer, inner),
                (min + max) * 0.5,
                &mut self.seed,
            ),
        }
    }

    /// `DynEntCl_ExplosionEvent` and `DynEntCl_GetClosestEntities`: the nearest few props within the outer radius are
    /// pushed and hurt by how far they are.
    fn blast(&mut self, at: Vec3, b: Blast) {
        if b.outer <= 0.0 {
            return;
        }
        let mut near: Vec<(f32, usize)> = self
            .props
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.gone)
            .filter_map(|(i, p)| {
                let (lo, hi) = p.bounds();
                let (lo, hi, at) = if b.cylinder {
                    (lo.with_z(0.0), hi.with_z(0.0), at.with_z(0.0))
                } else {
                    (lo, hi, at)
                };
                let d = at.clamp(lo, hi).distance(at);
                (d < b.outer).then_some((d, i))
            })
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        near.truncate(EXPLODE_MAX_ENTS);
        for (dist, i) in near {
            let scale = b.falloff(dist);
            let p = &mut self.props[i];
            let (lo, hi) = p.bounds();
            let centre = (lo + hi) / 2.0;
            let spin = Vec3::new(
                random(&mut self.seed),
                random(&mut self.seed),
                random(&mut self.seed),
            ) * EXPLODE_SPIN_SCALE;
            let force =
                scale * p.preset.as_ref().map_or(1.0, |q| q.explosive_force_scale) * EXPLODE_FORCE;
            let dir = if b.impulse != Vec3::ZERO {
                b.impulse
            } else if force < EXPLODE_MIN_FORCE {
                continue;
            } else {
                let away = if b.cylinder {
                    (centre - at).with_z(0.0)
                } else {
                    centre - at
                };
                (away.normalize_or(Vec3::Z) + Vec3::Z * EXPLODE_UP_BIAS).normalize()
            };
            let mut point = centre;
            p.strike(|body, _| {
                point = body.center_of_mass() + spin;
                body.impulse(point, dir * force);
            });
            let damage = ((b.damage.0 - b.damage.1) as f32 * scale + b.damage.1 as f32) as i32;
            self.hurt(i, point, dir.normalize_or(Vec3::Z), damage);
        }
    }

    /// `DynEntCl_Damage`: breaks the prop when its health is spent, which plays its destroy effect and throws its pieces.
    fn hurt(&mut self, i: usize, hit: Vec3, dir: Vec3, damage: i32) {
        let p = &mut self.props[i];
        let pose = (p.origin(), p.axes());
        if !p.hurt(damage) {
            return;
        }
        if let Some(def) = p.destroy_fx.clone() {
            let frame = Frame {
                origin: pose.0,
                axis: pose.1,
            };
            self.out.fx.push((def, frame));
        }
        if let Some(pieces) = p.destroy_pieces.clone() {
            spawn_pieces(&mut self.pieces, &pieces, pose, hit, dir, &mut self.seed);
        }
    }

    /// `DynEntCl_DynEntImpactEvent`: a bullet leaving `eye` along `angles` hits the first prop on its way, if the map
    /// does not stop it first. The prop is struck, hurt by the weapon's damage, and the weapon's impact plays on it.
    fn shot(
        &mut self,
        eye: Vec3,
        angles: [f32; 3],
        (weapon, shooter, damage): (u16, u16, i32),
        world: &dyn Collide,
    ) {
        let (f, _, _) = sim::pm::math::angle_vectors(&angles);
        let f = Vec3::from(f);
        let mut best: Option<(f32, usize, Vec3)> = None;
        for (i, p) in self.props.iter().enumerate().filter(|(_, p)| !p.gone) {
            if let Some((t, n)) = ray_prop(p, eye, f)
                && t < SHOT_RANGE
                && best.is_none_or(|(b, _, _)| t < b)
            {
                best = Some((t, i, n));
            }
        }
        let Some((t, i, normal)) = best else { return };
        // The map stops the bullet first if it is nearer.
        let wall = world.trace(
            eye.to_array(),
            (eye + f * (t - 1.0).max(0.0)).to_array(),
            [0.0; 3],
            [0.0; 3],
            sim::cm::ENTITYNUM_NONE,
            sim::contents::SOLID,
        );
        if wall.fraction < 1.0 {
            return;
        }
        let at = eye + f * t;
        self.out.impacts.push(ClientEvent::BulletImpact {
            origin: at.to_array(),
            normal: normal.to_array(),
            surface: self.props[i].surface,
            weapon,
            shooter,
            exit: false,
        });
        self.props[i].strike(|b, preset| {
            b.bullet_impact(at, f, BULLET_FORCE, preset.bullet_force_scale);
        });
        self.hurt(i, at, f, damage);
    }

    /// Advances the falling props and pieces to the end of this frame.
    pub fn update(&mut self, dt: f32, world: &dyn Collide) {
        let tracer = Tracer(world);
        for p in &mut self.props {
            if let Some((prefix, hit)) = p.advance(dt, &tracer) {
                self.out.collisions.push(heard(world, prefix, &hit));
            }
        }
        for p in &mut self.pieces {
            if let Some(hit) = p.advance(dt, &tracer)
                && let Some(prefix) = p.body.sound_prefix()
            {
                self.out.collisions.push(heard(world, prefix.clone(), &hit));
            }
        }
    }

    /// Every prop and piece as a model to draw.
    pub fn instances(&self) -> impl Iterator<Item = ModelInstance> + '_ {
        let pieces = self.pieces.iter().map(|p| {
            let origin = p.body.origin().to_array();
            let mut m = ModelInstance::new(p.model.clone(), ModelKind::World);
            m.origin = origin;
            m.angles = angles_of(p.body.axes());
            m.light_origin = origin;
            m
        });
        self.props
            .iter()
            .filter(|p| !p.gone)
            .map(Prop::instance)
            .chain(pieces)
    }
}

/// `weapImpactType_t` of a grenade's explosion: its blast falls off from the centre; rockets' and custom
/// explosions are full strength out to the radius.
const IMPACT_GRENADE_EXPLODE: i32 = 6;
/// How far a bullet flies.
const SHOT_RANGE: f32 = 4000.0;

/// An explosion's reach and strength.
struct Blast {
    cylinder: bool,
    inner: f32,
    outer: f32,
    /// `inScale`: the strength at the inner radius.
    scale: f32,
    /// The push direction a physics jolt forces; zero for away from the centre.
    impulse: Vec3,
    /// Damage at the inner and at the outer radius.
    damage: (i32, i32),
}

impl Default for Blast {
    fn default() -> Self {
        Blast {
            cylinder: false,
            inner: 0.0,
            outer: 0.0,
            scale: 1.0,
            impulse: Vec3::ZERO,
            damage: (0, 0),
        }
    }
}

impl Blast {
    /// The strength at `dist`: full inside the inner radius, falling linearly to nothing at the outer.
    fn falloff(&self, dist: f32) -> f32 {
        if dist <= self.inner || self.outer <= self.inner {
            self.scale
        } else {
            (self.outer - dist) / (self.outer - self.inner) * self.scale
        }
    }
}

/// The script models the server `physicslaunch`ed. Every client simulates each as a rigid body with its model's
/// PhysPreset, struck at the launch point by the launch force as a bullet would (`CG_CreatePhysicsObject`), and draws
/// it where the body is.
#[derive(Default)]
pub struct Launches(HashMap<u16, Launch>);

/// How long a launched model that is out of view keeps its body.
const KEEP_UNSEEN: f32 = 5.0;
/// How long a launch with nothing to simulate stays drawn where it started (`CG_ExpiredLaunch`).
const ABANDON: f32 = 1.0;

struct Launch {
    /// What the server said: origin, angles, force and point. A different launch makes a new body.
    key: [f32; 12],
    /// `None` when the model has no PhysPreset: the original gives such a launch up.
    body: Option<Body>,
    carry: f32,
    age: f32,
    seen: bool,
    unseen: f32,
}

impl Launch {
    fn new(e: &net::entity::EntityState, model: &XModel, key: [f32; 12]) -> Launch {
        let body = model.phys_preset.as_ref().map(|preset| {
            let (f, r, u) = sim::pm::math::angle_vectors(&e.angles);
            let axes = [Vec3::from(f), -Vec3::from(r), Vec3::from(u)];
            let mut b = Body::new(
                preset,
                &Shape::of_model(model),
                Vec3::from(e.origin),
                axes,
                Vec3::ZERO,
            );
            let force = Vec3::from(e.velocity);
            b.bullet_impact(
                Vec3::from(e.launch_point),
                force.normalize_or_zero(),
                force.length(),
                preset.bullet_force_scale,
            );
            b
        });
        Launch {
            key,
            body,
            carry: 0.0,
            age: 0.0,
            seen: true,
            unseen: 0.0,
        }
    }
}

impl Launches {
    /// Where launched entity `e` (a script model of `model`) is and how it is turned, `dt` seconds on; `None` once
    /// the launch has been given up. The body is made the first time the entity is seen with this launch.
    pub fn pose(
        &mut self,
        e: &net::entity::EntityState,
        model: &XModel,
        dt: f32,
        world: &dyn Collide,
    ) -> Option<([f32; 3], [f32; 3])> {
        let [a, b, c, d] = [e.origin, e.angles, e.velocity, e.launch_point];
        let key: [f32; 12] = std::array::from_fn(|i| [a, b, c, d][i / 3][i % 3]);
        let launch = self
            .0
            .entry(e.number)
            .or_insert_with(|| Launch::new(e, model, key));
        if launch.key != key {
            *launch = Launch::new(e, model, key);
        }
        launch.seen = true;
        launch.age += dt;
        let Some(b) = &mut launch.body else {
            // Nothing to simulate: it stays where it started for a second, then goes.
            return (launch.age < ABANDON).then_some((e.origin, e.angles));
        };
        launch.carry += dt.min(0.1);
        let world = Tracer(world);
        while launch.carry >= STEP {
            launch.carry -= STEP;
            b.step(STEP, &world);
        }
        Some((b.origin().to_array(), angles_of(b.axes())))
    }

    /// Ends a frame of `dt` seconds: a body whose entity was out of view keeps where it was for a while, then goes.
    pub fn finish_frame(&mut self, dt: f32) {
        self.0.retain(|_, l| {
            if std::mem::take(&mut l.seen) {
                l.unseen = 0.0;
            } else {
                l.unseen += dt;
            }
            l.unseen < KEEP_UNSEEN
        });
    }
}

/// A random number in -1..1 from `seed`.
fn random(seed: &mut u32) -> f32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 17;
    *seed ^= *seed << 5;
    *seed as f32 / u32::MAX as f32 * 2.0 - 1.0
}

/// `physicsjitter`: props within the outer radius hop up, the harder the nearer, in proportion to `amount`.
fn jitter(props: &mut [Prop], at: Vec3, (outer, inner): (f32, f32), amount: f32, seed: &mut u32) {
    if outer <= 0.0 {
        return;
    }
    for p in props.iter_mut().filter(|p| !p.gone) {
        let (lo, hi) = p.bounds();
        let dist = at
            .with_z(0.0)
            .clamp(lo.with_z(0.0), hi.with_z(0.0))
            .distance(at.with_z(0.0));
        if dist > outer {
            continue;
        }
        let fall = if dist <= inner || outer <= inner {
            1.0
        } else {
            1.0 - (dist - inner) / (outer - inner)
        };
        let spin = Vec3::new(random(seed), random(seed), random(seed)) * EXPLODE_SPIN_SCALE;
        p.strike(|b, _| {
            b.impulse(
                b.center_of_mass() + spin,
                Vec3::Z * (b.mass() * amount * JITTER_KICK * fall),
            );
        });
    }
}

/// Where a ray from `o` along unit `d` enters the prop's oriented bounds, and the face's normal.
fn ray_prop(p: &Prop, o: Vec3, d: Vec3) -> Option<(f32, Vec3)> {
    // The model's axes are forward, minus left and up.
    let a = p.axes();
    let axes = [a[0], -a[1], a[2]];
    let local = |v: Vec3| Vec3::new(v.dot(axes[0]), v.dot(axes[1]), v.dot(axes[2]));
    let (t, n) = ray_box(
        local(o - p.origin()),
        local(d),
        Vec3::from(p.model.mins),
        Vec3::from(p.model.maxs),
    )?;
    let n = axes[0] * n.x + axes[1] * n.y + axes[2] * n.z;
    Some((t, if n == Vec3::ZERO { -d } else { n }))
}

/// Where a ray from `o` along unit `d` first meets the box, and the normal of the face it enters by (zero when the
/// ray starts inside).
fn ray_box(o: Vec3, d: Vec3, lo: Vec3, hi: Vec3) -> Option<(f32, Vec3)> {
    let (mut near, mut far, mut normal) = (0.0f32, f32::MAX, Vec3::ZERO);
    for k in 0..3 {
        if d[k].abs() < 1e-9 {
            if o[k] < lo[k] || o[k] > hi[k] {
                return None;
            }
            continue;
        }
        let (mut t0, mut t1, mut side) = ((lo[k] - o[k]) / d[k], (hi[k] - o[k]) / d[k], -1.0);
        if t0 > t1 {
            std::mem::swap(&mut t0, &mut t1);
            side = 1.0;
        }
        if t0 > near {
            near = t0;
            normal = Vec3::ZERO;
            normal[k] = side;
        }
        far = far.min(t1);
        if far < near {
            return None;
        }
    }
    Some((near, normal))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Floor;

    impl fx::World for Floor {
        fn trace(&self, a: Vec3, b: Vec3, _: Vec3, _: Vec3) -> Option<(f32, Vec3)> {
            (a.z >= 0.0 && b.z < 0.0).then(|| (a.z / (a.z - b.z), Vec3::Z))
        }
    }

    fn crate_model(preset: Option<PhysPreset>) -> Arc<XModel> {
        use assets::zone::xmodel::LodInfo;
        let lod = || LodInfo {
            dist: 0.0,
            surf_count: 0,
            surf_index: 0,
            part_bits: [0; 4],
            lod: 0,
            smc_index_plus_one: 0,
            smc_alloc_bits: 0,
        };
        Arc::new(XModel {
            name: Some("crate".into()),
            num_bones: 0,
            num_root_bones: 0,
            lod_ramp_type: 0,
            bone_names: Arc::new([]),
            parent_list: Arc::new([]),
            quats: Arc::new([]),
            trans: Arc::new([]),
            part_classification: Arc::new([]),
            base_mat: Arc::new([]),
            surfs: Arc::new([]),
            materials: Arc::new([]),
            lod_info: [lod(), lod(), lod(), lod()],
            coll_surfs: Arc::new([]),
            contents: 0,
            bone_info: Arc::new([]),
            radius: 0.0,
            mins: [-10.0, -10.0, 0.0],
            maxs: [10.0, 10.0, 30.0],
            num_lods: 1,
            coll_lod: 0,
            mem_usage: 0,
            flags: 0,
            bad: false,
            phys_preset: preset.map(Arc::new),
            phys_geoms: None,
        })
    }

    fn prop(preset: Option<PhysPreset>) -> Prop {
        let model = crate_model(None);
        Prop {
            shape: Shape::of_model(&model),
            model,
            preset: preset.map(Arc::new),
            origin: Vec3::new(0.0, 0.0, 0.5),
            axes: [Vec3::X, Vec3::Y, Vec3::Z],
            body: None,
            carry: 0.0,
            age: 0.0,
            destructible: false,
            health: 0,
            destroy_fx: None,
            destroy_pieces: None,
            gone: false,
            surface: 0,
        }
    }

    fn preset() -> PhysPreset {
        PhysPreset {
            name: None,
            kind: 0,
            mass: 20.0,
            bounce: 0.2,
            friction: 0.6,
            bullet_force_scale: 1.0,
            explosive_force_scale: 1.0,
            snd_alias_prefix: None,
            pieces_spread_fraction: 0.0,
            pieces_upward_velocity: 0.0,
            temp_default_to_cylinder: false,
        }
    }

    #[test]
    fn a_struck_prop_flies_settles_and_sleeps_and_one_without_a_preset_stays() {
        let mut p = prop(Some(preset()));
        assert!(p.body.is_none(), "a prop is still until struck");
        assert!(
            p.strike(|b, _| b.impulse(Vec3::new(0.0, 0.0, 15.0), Vec3::new(8000.0, 0.0, 8000.0)))
        );
        for _ in 0..600 {
            p.advance(1.0 / 60.0, &Floor);
        }
        assert!(p.body.as_ref().unwrap().asleep());
        let (lo, _) = p.bounds();
        assert!(lo.z > -0.5 && lo.z < 1.0, "resting on the floor: {lo}");
        assert!(p.origin().x > 30.0, "it was thrown: {}", p.origin());

        let mut fixed = prop(None);
        assert!(!fixed.strike(|b, _| b.impulse(Vec3::ZERO, Vec3::X)));
        assert!(fixed.body.is_none());
    }

    struct Void;

    impl Collide for Void {
        fn trace(
            &self,
            _: [f32; 3],
            _: [f32; 3],
            _: [f32; 3],
            _: [f32; 3],
            _: u16,
            _: i32,
        ) -> sim::cm::Trace {
            sim::cm::Trace::MISS
        }

        fn point_contents(&self, _: [f32; 3], _: u16, _: i32) -> i32 {
            0
        }
    }

    #[test]
    fn a_launch_with_nothing_to_simulate_goes_after_a_second_and_a_body_outlives_a_short_absence() {
        let mut e = net::entity::EntityState::new(30);
        e.origin = [1.0, 2.0, 3.0];
        e.velocity = [0.0, 0.0, 300.0];
        let mut l = Launches::default();
        let still = crate_model(None);
        assert!(l.pose(&e, &still, 0.5, &Void).is_some());
        l.finish_frame(0.5);
        assert!(
            l.pose(&e, &still, 0.4, &Void).is_some(),
            "held for a second"
        );
        l.finish_frame(0.4);
        assert!(l.pose(&e, &still, 0.2, &Void).is_none(), "then given up");

        // A model with a preset falls from where it was launched; out of view for 3 s it keeps its body, for 6 s not.
        let m = crate_model(Some(preset()));
        let mut l = Launches::default();
        for _ in 0..30 {
            l.pose(&e, &m, 1.0 / 60.0, &Void);
            l.finish_frame(1.0 / 60.0);
        }
        let (fell, _) = l.pose(&e, &m, 0.0, &Void).unwrap();
        assert!(fell[2] < 3.0, "{fell:?}");
        for _ in 0..3 {
            l.finish_frame(1.0);
        }
        assert_eq!(l.pose(&e, &m, 0.0, &Void).unwrap().0, fell, "kept");
        let mut moved = e.clone();
        moved.velocity = [0.0, 0.0, 600.0];
        let again = l.pose(&moved, &m, 0.0, &Void).unwrap().0;
        assert_eq!(again, [1.0, 2.0, 3.0], "a new launch starts over");
        for _ in 0..6 {
            l.finish_frame(1.0);
        }
        assert_eq!(l.0.len(), 0);
    }

    fn props_of(ps: Vec<Prop>) -> Props {
        Props {
            props: ps,
            woken: 0,
            seed: 1,
            pieces: Vec::new(),
            out: Happened::default(),
        }
    }

    fn woken(what: Physics, at: Vec3, ps: Vec<Prop>) -> Vec<bool> {
        let mut props = props_of(ps);
        props.physics(at, &what);
        props.props.iter().map(|p| p.body.is_some()).collect()
    }

    fn at_x(x: f32) -> Prop {
        let mut p = prop(Some(preset()));
        p.origin = Vec3::new(x, 0.0, 0.5);
        p
    }

    #[test]
    fn a_physics_event_moves_the_props_inside_its_radius_only() {
        let mk = || vec![at_x(100.0), at_x(900.0)];
        let cyl = Physics::Explosion {
            cylinder: true,
            outer: 300.0,
            inner: 100.0,
            magnitude: 1.0,
        };
        assert_eq!(woken(cyl, Vec3::ZERO, mk()), [true, false]);
        // A cylinder reaches any height; a sphere does not.
        let high = Vec3::new(0.0, 0.0, 2000.0);
        assert_eq!(woken(cyl, high, mk()), [true, false]);
        let sphere = Physics::Explosion {
            cylinder: false,
            outer: 300.0,
            inner: 100.0,
            magnitude: 1.0,
        };
        assert_eq!(woken(sphere, high, mk()), [false, false]);
        let jitter = Physics::Jitter {
            outer: 300.0,
            inner: 100.0,
            min: 0.5,
            max: 1.0,
        };
        assert_eq!(woken(jitter, Vec3::ZERO, mk()), [true, false]);
    }

    #[test]
    fn a_blast_pushes_by_its_radii_and_wakes_only_the_nearest_few() {
        let speed = |x: f32, inner: f32, outer: f32| {
            let mut p = props_of(vec![at_x(x)]);
            p.blast(
                Vec3::ZERO,
                Blast {
                    inner,
                    outer,
                    ..Blast::default()
                },
            );
            p.props[0]
                .body
                .as_ref()
                .map_or(0.0, |b| b.velocity().length())
        };
        let (full, half, out) = (
            speed(30.0, 50.0, 250.0),
            speed(150.0, 50.0, 250.0),
            speed(400.0, 50.0, 250.0),
        );
        assert!(full > 0.0 && out == 0.0);
        assert!(half < full * 0.8 && half > full * 0.2, "{half} vs {full}");
        // The same blast with a larger outer radius reaches the far prop; one with a larger inner radius is stronger.
        assert!(speed(400.0, 50.0, 800.0) > 0.0);
        assert!(speed(150.0, 200.0, 250.0) > half);

        let mut crowd = props_of((0..30).map(|i| at_x(20.0 + i as f32 * 5.0)).collect());
        crowd.blast(
            Vec3::ZERO,
            Blast {
                outer: 500.0,
                ..Blast::default()
            },
        );
        let woken: Vec<_> = crowd.props.iter().map(|p| p.body.is_some()).collect();
        assert_eq!(woken.iter().filter(|w| **w).count(), EXPLODE_MAX_ENTS);
        assert!(
            woken[..EXPLODE_MAX_ENTS].iter().all(|w| *w),
            "the nearest wake"
        );
    }

    fn barrel(health: i32) -> Prop {
        let mut p = prop(Some(preset()));
        p.destructible = true;
        p.health = health;
        p.destroy_fx = Some(Arc::new(FxEffectDef {
            name: None,
            flags: 0,
            total_size: 0,
            msec_looping_life: 0,
            looping_count: 0,
            one_shot_count: 0,
            emission_count: 0,
            elems: Arc::from(Vec::new()),
        }));
        p
    }

    #[test]
    fn a_destructible_prop_breaks_when_its_health_is_spent_and_plays_its_effect_once() {
        let mut props = props_of(vec![barrel(30), prop(Some(preset()))]);
        props.hurt(0, Vec3::ZERO, Vec3::X, 20);
        assert!(
            !props.props[0].gone && props.out.fx.is_empty(),
            "wounded, not broken"
        );
        props.hurt(0, Vec3::ZERO, Vec3::X, 20);
        assert!(props.props[0].gone);
        assert_eq!(props.out.fx.len(), 1);
        props.hurt(0, Vec3::ZERO, Vec3::X, 50);
        assert_eq!(props.out.fx.len(), 1, "a broken prop does not break again");
        assert_eq!(props.instances().count(), 1, "it is no longer drawn");
        props.hurt(1, Vec3::ZERO, Vec3::X, 5000);
        assert!(!props.props[1].gone, "clutter has no health");
    }

    #[test]
    fn a_blast_damages_by_distance_between_its_inner_and_outer_damage() {
        let hurt_at = |x: f32| {
            let mut p = props_of(vec![{
                let mut b = barrel(100);
                b.origin.x = x;
                b
            }]);
            p.blast(
                Vec3::ZERO,
                Blast {
                    inner: 0.0,
                    outer: 200.0,
                    damage: (100, 0),
                    ..Blast::default()
                },
            );
            p.props[0].health
        };
        assert!(hurt_at(20.0) < 40, "near: {}", hurt_at(20.0));
        assert!(hurt_at(150.0) > 60, "far: {}", hurt_at(150.0));
        assert_eq!(hurt_at(500.0), 100);
    }

    #[test]
    fn a_shot_hits_the_nearest_prop_by_its_turned_bounds_and_the_map_covers_what_is_behind() {
        let mut props = props_of(vec![barrel(100), at_x(300.0)]);
        // The first crate sits at the origin: 10 half-width, so a ray along +x from -100 meets it at 90.
        let eye = Vec3::new(-100.0, 0.0, 10.0);
        props.shot(eye, [0.0, 0.0, 0.0], (3, 7, 60), &Void);
        let Some(ClientEvent::BulletImpact {
            origin,
            normal,
            shooter,
            weapon,
            ..
        }) = props.out.impacts.first().cloned()
        else {
            panic!("no impact");
        };
        assert!(
            (origin[0] + 10.0).abs() < 0.01
                && normal == [-1.0, 0.0, 0.0]
                && (shooter, weapon) == (7, 3)
        );
        assert_eq!(props.props[0].health, 40, "hurt by the weapon's damage");
        assert!(
            props.props[0].body.is_some() && props.props[1].body.is_none(),
            "only the first"
        );
        // A shot that misses the box to the side hits nothing.
        let mut miss = props_of(vec![barrel(100)]);
        miss.shot(Vec3::new(-100.0, 50.0, 10.0), [0.0; 3], (3, 7, 60), &Void);
        assert!(miss.out.impacts.is_empty());
    }

    #[test]
    fn a_broken_prop_throws_its_pieces_and_a_hard_landing_is_heard() {
        let mut preset = preset();
        preset.snd_alias_prefix = Some("physics_wood".into());
        preset.pieces_upward_velocity = 100.0;
        let model = crate_model(Some(preset));
        let pieces = XModelPieces {
            name: None,
            pieces: Arc::from(vec![assets::zone::clipmap::XModelPiece {
                model: Some(model),
                offset: [0.0, 0.0, 5.0],
            }]),
        };
        let mut props = props_of(vec![]);
        spawn_pieces(
            &mut props.pieces,
            &pieces,
            (Vec3::new(0.0, 0.0, 100.0), [Vec3::X, Vec3::Y, Vec3::Z]),
            Vec3::new(0.0, 0.0, 100.0),
            Vec3::X,
            &mut 1,
        );
        assert_eq!(props.pieces.len(), 1);
        assert_eq!(props.instances().count(), 1);
        let mut loud = None;
        for _ in 0..300 {
            loud = loud.or(props.pieces[0].advance(1.0 / 60.0, &Floor));
        }
        assert!(loud.is_some(), "it landed hard enough to hear");
        assert_eq!(
            props.pieces[0].body.sound_prefix().map(|p| &**p),
            Some("physics_wood")
        );
    }

    #[test]
    fn angles_round_trip_through_the_engine_basis() {
        for a in [
            [0.0, 0.0, 0.0],
            [0.0, 90.0, 0.0],
            [30.0, 45.0, 0.0],
            [-20.0, 200.0, 35.0],
            [10.0, -60.0, -50.0],
        ] {
            let (f, r, u) = sim::pm::math::angle_vectors(&a);
            let back = angles_of([Vec3::from(f), -Vec3::from(r), Vec3::from(u)]);
            let (f2, r2, u2) = sim::pm::math::angle_vectors(&back);
            for (x, y) in [(f, f2), (r, r2), (u, u2)] {
                assert!(
                    Vec3::from(x).distance(Vec3::from(y)) < 1e-3,
                    "{a:?} -> {back:?}"
                );
            }
        }
    }

    #[test]
    fn a_landing_inside_a_long_frame_is_still_heard() {
        let mut preset = preset();
        preset.snd_alias_prefix = Some("physics_wood".into());
        let mut p = prop(Some(preset));
        p.origin = Vec3::new(0.0, 0.0, 40.0);
        p.strike(|b, _| b.impulse(b.center_of_mass(), Vec3::new(0.0, 0.0, -9000.0)));
        // One 0.1 s frame holds six steps: the fall, the landing and the rest of the settling.
        let heard = p.advance(0.1, &Floor);
        assert!(heard.is_some_and(|(prefix, _)| &*prefix == "physics_wood"));
    }

    #[test]
    fn the_bounds_follow_the_models_y_axis_not_the_left_axis() {
        let mut p = prop(None);
        let model = crate_model(None);
        // A model 0..20 wide in y, turned a quarter so its y runs along world x.
        p.model = Arc::new(XModel {
            mins: [-5.0, 0.0, 0.0],
            maxs: [5.0, 20.0, 10.0],
            ..Arc::try_unwrap(model).ok().expect("one owner")
        });
        p.origin = Vec3::ZERO;
        p.axes = [Vec3::Y, Vec3::NEG_X, Vec3::Z];
        // Model y is -left = +x.
        let (lo, hi) = p.bounds();
        assert!(
            (lo.x + 0.0).abs() < 1e-4 && (hi.x - 20.0).abs() < 1e-4,
            "{lo} {hi}"
        );
        // The ray test agrees: a ray down +x from the left meets the box at x = 0.
        let hit = ray_prop(&p, Vec3::new(-30.0, 0.0, 5.0), Vec3::X).map(|h| h.0);
        assert_eq!(hit, Some(30.0));
    }

    #[test]
    fn a_ray_hits_the_nearer_face_and_misses_to_the_side() {
        let (lo, hi) = (Vec3::new(10.0, -5.0, -5.0), Vec3::new(20.0, 5.0, 5.0));
        assert_eq!(ray_box(Vec3::ZERO, Vec3::X, lo, hi), Some((10.0, -Vec3::X)));
        assert_eq!(ray_box(Vec3::ZERO, Vec3::Y, lo, hi), None);
        assert_eq!(ray_box(Vec3::ZERO, -Vec3::X, lo, hi), None);
        assert_eq!(
            ray_box(Vec3::new(30.0, 0.0, 0.0), -Vec3::X, lo, hi),
            Some((10.0, Vec3::X))
        );
    }
}
