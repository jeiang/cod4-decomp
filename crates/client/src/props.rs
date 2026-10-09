// SPDX-License-Identifier: GPL-3.0-only
//! The map's loose props (the clipmap's clutter and destructible dynamic entities: crates, barrels, bottles). The
//! server does not simulate them, so each client draws them where the map puts them and, when a shot, a blast or a
//! physics explosion reaches one, lets it fall as a rigid body with its PhysPreset (mass, bounce, friction, and the
//! model's physics geometry and mass distribution; see [`fx::Body`]) that collides with the map. What a client does to
//! a prop is not shared; it does not hurt anyone.

use crate::effects::Tracer;
use crate::events::ClientEvent;
use assets::zone::clipmap::DynEntityDef;
use assets::zone::phys::PhysPreset;
use assets::zone::xmodel::XModel;
use fx::{Body, Mass, Shape};
use glam::Vec3;
use render::{ModelInstance, ModelKind};
use server::tempev::Physics;
use sim::cm::Collide;
use std::collections::HashMap;
use std::sync::Arc;

const STEP: f32 = 1.0 / 60.0;
/// A prop that has not settled this long is put to sleep where it is.
const LIFE: f32 = 12.0;
/// How far from a prop's bounds an impact still strikes it.
const IMPACT_REACH: f32 = 10.0;
/// `dynEnt_bulletForce`: the speed of the bullet that strikes a prop.
const BULLET_FORCE: f32 = 1000.0;
/// `dynEnt_explodeForce`: the momentum a blast gives a prop at its centre, times the preset's explosive force scale.
const EXPLODE_FORCE: f32 = 12500.0;
/// `dynEnt_explodeUpbias` and `dynEnt_explodeSpinScale` (the random offset of the push from the centre of mass).
const EXPLODE_UP_BIAS: f32 = 0.5;
const EXPLODE_SPIN_SCALE: f32 = 3.0;
const BLAST_REACH: f32 = 220.0;
/// Speed a jolt gives a prop at its centre, and the upward speed a jitter of one unit gives, units per second.
const JOLT_KICK: f32 = 700.0;
const JITTER_KICK: f32 = 120.0;
/// Props with a side shorter than this are too small to bother (they are decoration).
const MIN_SIZE: f32 = 2.0;
/// Props this heavy in bounds-volume terms (cubic units) shrug off a bullet.
const BULLET_MAX_VOLUME: f32 = 60.0 * 60.0 * 60.0;

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
}

impl Prop {
    fn volume(&self) -> f32 {
        let d = Vec3::from(self.model.maxs) - Vec3::from(self.model.mins);
        d.x * d.y * d.z
    }

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
                let w = o + a[0] * c.x + a[1] * c.y + a[2] * c.z;
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

    fn advance(&mut self, dt: f32, world: &dyn fx::World) {
        let Some(b) = &mut self.body else { return };
        if b.asleep() {
            return;
        }
        self.carry += dt.min(0.1);
        while self.carry >= STEP {
            self.carry -= STEP;
            b.step(STEP, world);
            self.age += STEP;
            if self.age >= LIFE {
                b.sleep();
            }
        }
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
}

impl Props {
    pub fn new(defs: &[Arc<[DynEntityDef]>]) -> Self {
        let mut props = Vec::new();
        for d in defs.iter().flat_map(|l| l.iter()) {
            let Some(model) = d.model.clone() else {
                continue;
            };
            let (mins, maxs) = (Vec3::from(model.mins), Vec3::from(model.maxs));
            if (maxs - mins).min_element() < MIN_SIZE {
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
            props.push(Prop {
                shape: Shape::of_model(&model).with_mass(mass),
                model,
                preset: d.phys_preset.clone(),
                origin: Vec3::from(d.pose.origin),
                axes,
                body: None,
                carry: 0.0,
                age: 0.0,
            });
        }
        Self {
            props,
            woken: 0,
            seed: 0x9E37_79B9,
        }
    }

    pub fn len(&self) -> usize {
        self.props.len()
    }

    /// Reacts to `ev`: impacts near a prop, blasts, and shots that would hit one.
    pub fn event(&mut self, ev: &ClientEvent, hitscan: &dyn Fn(u16) -> bool, world: &dyn Collide) {
        let before = self.props.iter().filter(|p| p.body.is_some()).count();
        match ev {
            ClientEvent::Explosion { origin, .. } => {
                let blast = Physics::Explosion {
                    cylinder: false,
                    outer: BLAST_REACH,
                    inner: 0.0,
                    magnitude: 1.0,
                };
                apply_physics(&mut self.props, Vec3::from(*origin), &blast, &mut self.seed);
            }
            ClientEvent::Physics { origin, what } => {
                apply_physics(&mut self.props, Vec3::from(*origin), what, &mut self.seed)
            }
            ClientEvent::WeaponFire {
                eye,
                angles,
                weapon,
                ..
            } if hitscan(*weapon) => self.shot(Vec3::from(*eye), *angles, world),
            _ => {}
        }
        self.woken += (self.props.iter().filter(|p| p.body.is_some()).count() - before) as u64;
    }

    /// A bullet leaving `eye` along `angles` hits the first prop on its way, if the map does not stop it first.
    fn shot(&mut self, eye: Vec3, angles: [f32; 3], world: &dyn Collide) {
        let (f, _, _) = sim::pm::math::angle_vectors(&angles);
        let f = Vec3::from(f);
        let end = eye + f * 4000.0;
        let wall = world
            .trace(
                eye.to_array(),
                end.to_array(),
                [0.0; 3],
                [0.0; 3],
                sim::cm::ENTITYNUM_NONE,
                sim::contents::SOLID,
            )
            .fraction
            * 4000.0;
        let mut best: Option<(f32, usize)> = None;
        for (i, p) in self.props.iter().enumerate() {
            if p.preset.is_none() || p.volume() > BULLET_MAX_VOLUME {
                continue;
            }
            let (lo, hi) = p.bounds();
            if let Some(t) = ray_box(
                eye,
                f,
                lo - Vec3::splat(IMPACT_REACH / 5.0),
                hi + Vec3::splat(IMPACT_REACH / 5.0),
            ) && t < wall
                && best.is_none_or(|(b, _)| t < b)
            {
                best = Some((t, i));
            }
        }
        if let Some((t, i)) = best {
            let at = eye + f * t;
            self.props[i].strike(|b, preset| {
                b.bullet_impact(at, f, BULLET_FORCE, preset.bullet_force_scale);
            });
        }
    }

    /// Advances the falling props to the end of this frame.
    pub fn update(&mut self, dt: f32, world: &dyn Collide) {
        let world = Tracer(world);
        for p in &mut self.props {
            p.advance(dt, &world);
        }
    }

    /// Every prop as a model to draw.
    pub fn instances(&self) -> impl Iterator<Item = ModelInstance> + '_ {
        self.props.iter().map(Prop::instance)
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

/// A physics world event (`DynEntCl_ExplosionEvent`): props within the outer radius are pushed, fully inside the inner
/// radius and less out to the outer. Explosions throw them away from `at` and a little up (a cylinder ignores height)
/// with `dynEnt_explodeForce` from a point a little off their centre of mass, jolts push along their impulse, jitters
/// hop them.
fn apply_physics(props: &mut [Prop], at: Vec3, what: &Physics, seed: &mut u32) {
    let (outer, inner) = match *what {
        Physics::Explosion { outer, inner, .. }
        | Physics::Jolt { outer, inner, .. }
        | Physics::Jitter { outer, inner, .. } => (outer, inner),
    };
    if outer <= 0.0 {
        return;
    }
    let flat = matches!(what, Physics::Explosion { cylinder: true, .. })
        || matches!(what, Physics::Jitter { .. });
    for p in props {
        let (lo, hi) = p.bounds();
        let (lo, hi, at) = if flat {
            (lo.with_z(0.0), hi.with_z(0.0), at.with_z(0.0))
        } else {
            (lo, hi, at)
        };
        let dist = at.clamp(lo, hi).distance(at);
        if dist > outer {
            continue;
        }
        let fall = if dist <= inner || outer <= inner {
            1.0
        } else {
            1.0 - (dist - inner) / (outer - inner)
        };
        let centre = (lo + hi) / 2.0;
        let spin = Vec3::new(random(seed), random(seed), random(seed)) * EXPLODE_SPIN_SCALE;
        p.strike(|b, preset| {
            let scale = if preset.explosive_force_scale > 0.0 {
                preset.explosive_force_scale
            } else {
                1.0
            };
            let push = match *what {
                Physics::Explosion { magnitude, .. } => {
                    let away = (centre - at).with_z(if flat { 0.0 } else { centre.z - at.z });
                    (away.normalize_or(Vec3::Z) + Vec3::Z * EXPLODE_UP_BIAS).normalize()
                        * (EXPLODE_FORCE * magnitude.max(0.1) * scale)
                }
                Physics::Jolt { impulse, .. } => {
                    Vec3::from(impulse).normalize_or_zero() * (b.mass() * JOLT_KICK)
                }
                Physics::Jitter { min, max, .. } => {
                    Vec3::Z * (b.mass() * (min + max) * 0.5 * JITTER_KICK)
                }
            };
            b.impulse(b.center_of_mass() + spin, push * fall);
        });
    }
}

/// Distance along a ray from `o` along unit `d` to the box, if it hits.
fn ray_box(o: Vec3, d: Vec3, lo: Vec3, hi: Vec3) -> Option<f32> {
    let inv = Vec3::ONE / d;
    let (a, b) = ((lo - o) * inv, (hi - o) * inv);
    let (near, far) = (a.min(b).max_element(), a.max(b).min_element());
    (far >= near.max(0.0)).then_some(near.max(0.0))
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

    fn woken(what: Physics, at: Vec3, props: &mut [Prop]) -> Vec<bool> {
        apply_physics(props, at, &what, &mut 1);
        props.iter().map(|p| p.body.is_some()).collect()
    }

    #[test]
    fn a_physics_event_moves_the_props_inside_its_radius_only() {
        let mk = || {
            let mut a = prop(Some(preset()));
            a.origin = Vec3::new(100.0, 0.0, 0.5);
            let mut b = prop(Some(preset()));
            b.origin = Vec3::new(900.0, 0.0, 0.5);
            vec![a, b]
        };
        let cyl = Physics::Explosion {
            cylinder: true,
            outer: 300.0,
            inner: 100.0,
            magnitude: 1.0,
        };
        assert_eq!(woken(cyl, Vec3::ZERO, &mut mk()), [true, false]);
        // A cylinder reaches any height; a sphere does not.
        let high = Vec3::new(0.0, 0.0, 2000.0);
        assert_eq!(woken(cyl, high, &mut mk()), [true, false]);
        let sphere = Physics::Explosion {
            cylinder: false,
            outer: 300.0,
            inner: 100.0,
            magnitude: 1.0,
        };
        assert_eq!(woken(sphere, high, &mut mk()), [false, false]);
        let jitter = Physics::Jitter {
            outer: 300.0,
            inner: 100.0,
            min: 0.5,
            max: 1.0,
        };
        assert_eq!(woken(jitter, Vec3::ZERO, &mut mk()), [true, false]);
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
    fn a_ray_hits_the_nearer_face_and_misses_to_the_side() {
        let (lo, hi) = (Vec3::new(10.0, -5.0, -5.0), Vec3::new(20.0, 5.0, 5.0));
        assert_eq!(ray_box(Vec3::ZERO, Vec3::X, lo, hi), Some(10.0));
        assert_eq!(ray_box(Vec3::ZERO, Vec3::Y, lo, hi), None);
        assert_eq!(ray_box(Vec3::ZERO, -Vec3::X, lo, hi), None);
    }
}
