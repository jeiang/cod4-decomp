// SPDX-License-Identifier: GPL-3.0-only
//! The map's loose props (the clipmap's clutter and destructible dynamic entities: crates, barrels, bottles). The
//! server does not simulate them, so each client draws them where the map puts them and, when a shot, a blast or a
//! physics explosion reaches one, lets it fall as a Verlet body: the corners of its bounds are points, every pair of
//! corners a fixed-length constraint, and the points collide with the map. What a client does to a prop is not
//! shared; it does not hurt anyone.

use crate::events::ClientEvent;
use assets::zone::clipmap::DynEntityDef;
use assets::zone::xmodel::XModel;
use glam::Vec3;
use render::{ModelInstance, ModelKind};
use server::tempev::Physics;
use sim::cm::Collide;
use std::sync::Arc;

const STEP: f32 = 1.0 / 60.0;
const GRAVITY: f32 = 800.0;
const DAMPING: f32 = 0.995;
const FRICTION: f32 = 0.6;
const ITERATIONS: usize = 8;
const REST: f32 = 0.03;
const LIFE: f32 = 12.0;
/// How far from a prop's bounds an impact still strikes it.
const IMPACT_REACH: f32 = 10.0;
/// Speed a bullet gives a prop, units per second.
const BULLET_KICK: f32 = 90.0;
/// Speed a blast gives a prop at its centre, units per second.
const BLAST_KICK: f32 = 700.0;
const BLAST_REACH: f32 = 220.0;
/// Upward speed a jitter of one unit gives a prop.
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

struct Body {
    pos: [Vec3; 8],
    prev: [Vec3; 8],
    /// Corner positions relative to the model's origin, at rest.
    local: [Vec3; 8],
    pairs: Vec<(usize, usize, f32)>,
    age: f32,
    carry: f32,
    asleep: bool,
}

struct Prop {
    model: Arc<XModel>,
    shape: Shape,
}

/// The geometry and the motion of a prop, without what it looks like.
struct Shape {
    origin: Vec3,
    angles: [f32; 3],
    mins: Vec3,
    maxs: Vec3,
    /// Kept in the map's bounds while at rest, for hit tests.
    body: Option<Body>,
}

impl Shape {
    fn volume(&self) -> f32 {
        let d = self.maxs - self.mins;
        d.x * d.y * d.z
    }

    /// World-space bounds, from the corners that are where they are.
    fn bounds(&self) -> (Vec3, Vec3) {
        match &self.body {
            Some(b) => b.pos.iter().fold(
                (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
                |(lo, hi), p| (lo.min(*p), hi.max(*p)),
            ),
            None => {
                let q = sim::skel::quat::from_angles(&self.angles);
                let (lo, hi) = (0..8).map(|i| corner(i, self.mins, self.maxs)).fold(
                    (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
                    |(lo, hi), c| {
                        let w =
                            self.origin + Vec3::from(sim::skel::quat::rotate(&q, &c.to_array()));
                        (lo.min(w), hi.max(w))
                    },
                );
                (lo, hi)
            }
        }
    }

    /// Wakes the prop as a body if it is not one yet.
    fn body(&mut self) -> &mut Body {
        self.body.get_or_insert_with(|| {
            let q = sim::skel::quat::from_angles(&self.angles);
            let local: [Vec3; 8] = std::array::from_fn(|i| corner(i, self.mins, self.maxs));
            let pos: [Vec3; 8] = std::array::from_fn(|i| {
                self.origin + Vec3::from(sim::skel::quat::rotate(&q, &local[i].to_array()))
            });
            let mut pairs = Vec::with_capacity(28);
            for a in 0..8 {
                for b in a + 1..8 {
                    pairs.push((a, b, pos[a].distance(pos[b])));
                }
            }
            Body {
                prev: pos,
                pos,
                local,
                pairs,
                age: 0.0,
                carry: 0.0,
                asleep: true,
            }
        })
    }

    /// Gives every corner within `reach` of `at` the velocity `v` scaled by how close it is.
    fn kick(&mut self, at: Vec3, reach: f32, v: impl Fn(Vec3, f32) -> Vec3) {
        let b = self.body();
        for i in 0..8 {
            let d = b.pos[i].distance(at);
            if d < reach {
                let dv = v(b.pos[i], 1.0 - d / reach);
                b.prev[i] -= dv * STEP;
            }
        }
        b.asleep = false;
        b.age = 0.0;
    }

    /// Where the model's origin is and how it is turned.
    fn pose(&self) -> ([f32; 3], [f32; 3]) {
        match &self.body {
            None => (self.origin.to_array(), self.angles),
            Some(b) => {
                // The rotation that carries the rest corners onto the simulated ones: read the three box axes off
                // the corner differences.
                let avg = |bit: usize| {
                    (0..8)
                        .filter(|i| i & bit == 0)
                        .map(|i| b.pos[i | bit] - b.pos[i])
                        .sum::<Vec3>()
                        / 4.0
                };
                let x = avg(1).normalize_or(Vec3::X);
                let y = (avg(2) - x * avg(2).dot(x)).normalize_or(Vec3::Y);
                let z = x.cross(y);
                let centre = b.pos.iter().sum::<Vec3>() / 8.0;
                let local_centre = b.local.iter().sum::<Vec3>() / 8.0;
                let origin =
                    centre - (x * local_centre.x + y * local_centre.y + z * local_centre.z);
                (origin.to_array(), angles_of([x, -y, z]))
            }
        }
    }
}

impl Prop {
    fn instance(&self) -> ModelInstance {
        let (origin, angles) = self.shape.pose();
        let mut m = ModelInstance::new(self.model.clone(), ModelKind::World);
        m.origin = origin;
        m.angles = angles;
        m.light_origin = origin;
        m
    }
}

/// Every prop of the map.
pub struct Props {
    props: Vec<Prop>,
    /// Props that started to fall, for the report.
    pub woken: u64,
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
            // The map stores the pose as a quaternion; the model instance wants angles.
            let q = glam::Quat::from_xyzw(
                d.pose.quat[0],
                d.pose.quat[1],
                d.pose.quat[2],
                d.pose.quat[3],
            )
            .normalize();
            let angles = angles_of([q * Vec3::X, q * Vec3::NEG_Y, q * Vec3::Z]);
            props.push(Prop {
                model,
                shape: Shape {
                    origin: Vec3::from(d.pose.origin),
                    angles,
                    mins,
                    maxs,
                    body: None,
                },
            });
        }
        Self { props, woken: 0 }
    }

    pub fn len(&self) -> usize {
        self.props.len()
    }

    /// Reacts to `ev`: impacts near a prop, blasts, and shots that would hit one.
    pub fn event(&mut self, ev: &ClientEvent, hitscan: &dyn Fn(u16) -> bool, world: &dyn Collide) {
        let before = self.props.iter().filter(|p| p.shape.body.is_some()).count();
        match ev {
            ClientEvent::Explosion { origin, .. } => {
                self.blast(Vec3::from(*origin), BLAST_REACH, 1.0)
            }
            ClientEvent::Physics { origin, what } => apply_physics(
                self.props.iter_mut().map(|p| &mut p.shape),
                Vec3::from(*origin),
                what,
            ),
            ClientEvent::WeaponFire {
                eye,
                angles,
                weapon,
                ..
            } if hitscan(*weapon) => self.shot(Vec3::from(*eye), *angles, world),
            _ => {}
        }
        self.woken +=
            (self.props.iter().filter(|p| p.shape.body.is_some()).count() - before) as u64;
    }

    fn blast(&mut self, at: Vec3, reach: f32, strength: f32) {
        for p in self.props.iter_mut().map(|p| &mut p.shape) {
            let (lo, hi) = p.bounds();
            let near = at.clamp(lo, hi);
            if near.distance(at) > reach {
                continue;
            }
            let centre = (lo + hi) / 2.0;
            p.kick(centre, f32::MAX, |c, _| {
                let away = (c - at + Vec3::Z * 20.0).normalize_or(Vec3::Z);
                away * BLAST_KICK * strength * (1.0 - near.distance(at) / reach)
            });
        }
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
        for (i, p) in self.props.iter().map(|p| &p.shape).enumerate() {
            if p.volume() > BULLET_MAX_VOLUME {
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
            self.props[i]
                .shape
                .kick(at, 60.0, |_, w| f * BULLET_KICK * w.max(0.3));
        }
    }

    /// Advances the falling props to the end of this frame.
    pub fn update(&mut self, dt: f32, world: &dyn Collide) {
        for p in &mut self.props {
            if let Some(b) = &mut p.shape.body {
                b.update(dt, world);
            }
        }
    }

    /// Every prop as a model to draw.
    pub fn instances(&self) -> impl Iterator<Item = ModelInstance> + '_ {
        self.props.iter().map(Prop::instance)
    }
}

/// A physics world event: bodies within the outer radius are pushed, fully inside the inner radius and less out to the
/// outer. Explosions throw them away from `at` (a cylinder ignores height), jolts along their impulse, jitters hop
/// them.
fn apply_physics<'a>(shapes: impl Iterator<Item = &'a mut Shape>, at: Vec3, what: &Physics) {
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
    for p in shapes {
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
        let push = match *what {
            Physics::Explosion { magnitude, .. } => {
                let away = (centre - at).with_z(if flat { 0.0 } else { centre.z - at.z });
                (away.normalize_or(Vec3::Z) + Vec3::Z * 0.8).normalize()
                    * BLAST_KICK
                    * magnitude.max(0.1)
            }
            Physics::Jolt { impulse, .. } => Vec3::from(impulse).normalize_or_zero() * BLAST_KICK,
            Physics::Jitter { min, max, .. } => Vec3::Z * (min + max) * 0.5 * JITTER_KICK,
        } * fall;
        p.kick(centre, f32::MAX, |_, _| push);
    }
}

/// Distance along a ray from `o` along unit `d` to the box, if it hits.
fn ray_box(o: Vec3, d: Vec3, lo: Vec3, hi: Vec3) -> Option<f32> {
    let inv = Vec3::ONE / d;
    let (a, b) = ((lo - o) * inv, (hi - o) * inv);
    let (near, far) = (a.min(b).max_element(), a.max(b).min_element());
    (far >= near.max(0.0)).then_some(near.max(0.0))
}

impl Body {
    fn update(&mut self, dt: f32, world: &dyn Collide) {
        if self.asleep {
            return;
        }
        self.carry += dt.min(0.1);
        while self.carry >= STEP {
            self.carry -= STEP;
            self.step(world);
            self.age += STEP;
        }
    }

    fn step(&mut self, world: &dyn Collide) {
        for i in 0..8 {
            let v = (self.pos[i] - self.prev[i]) * DAMPING;
            self.prev[i] = self.pos[i];
            self.pos[i] += v + Vec3::new(0.0, 0.0, -GRAVITY * STEP * STEP);
        }
        for _ in 0..ITERATIONS {
            for &(a, b, len) in &self.pairs {
                let d = self.pos[b] - self.pos[a];
                let dist = d.length();
                if dist < 1e-4 {
                    continue;
                }
                let fix = d * ((dist - len) / dist * 0.5);
                self.pos[a] += fix;
                self.pos[b] -= fix;
            }
        }
        let mut moved = 0.0f32;
        for i in 0..8 {
            let from = self.prev[i];
            let t = world.trace(
                from.to_array(),
                self.pos[i].to_array(),
                [0.0; 3],
                [0.0; 3],
                sim::cm::ENTITYNUM_NONE,
                sim::contents::SOLID,
            );
            if t.fraction < 1.0 {
                let n = Vec3::from(t.normal);
                let hit = from + (self.pos[i] - from) * t.fraction + n * 0.05;
                let rest = self.pos[i] - hit;
                let v = self.pos[i] - from;
                let vt = v - n * v.dot(n);
                self.pos[i] = hit + (rest - n * rest.dot(n)) * FRICTION;
                self.prev[i] = self.pos[i] - vt * FRICTION;
            }
            moved = moved.max((self.pos[i] - self.prev[i]).length());
        }
        if moved < REST || self.age >= LIFE {
            self.asleep = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim::cm::Trace;

    struct Floor;

    impl Collide for Floor {
        fn trace(
            &self,
            start: [f32; 3],
            end: [f32; 3],
            _mins: [f32; 3],
            _maxs: [f32; 3],
            _pass: u16,
            _mask: i32,
        ) -> Trace {
            let mut t = Trace::MISS;
            if end[2] < 0.0 && start[2] >= 0.0 {
                t.fraction = start[2] / (start[2] - end[2]);
                t.normal = [0.0, 0.0, 1.0];
            }
            t
        }

        fn point_contents(&self, _p: [f32; 3], _pass: u16, _mask: i32) -> i32 {
            0
        }
    }

    fn prop(angles: [f32; 3], origin: Vec3) -> Shape {
        Shape {
            origin,
            angles,
            mins: Vec3::new(-10.0, -10.0, 0.0),
            maxs: Vec3::new(10.0, 10.0, 30.0),
            body: None,
        }
    }

    fn kicked(what: Physics, at: Vec3, props: &mut [Shape]) -> Vec<bool> {
        apply_physics(props.iter_mut(), at, &what);
        props.iter().map(|p| p.body.is_some()).collect()
    }

    #[test]
    fn a_physics_event_moves_the_props_inside_its_radius_only() {
        let mk = || {
            vec![
                prop([0.0; 3], Vec3::new(100.0, 0.0, 0.0)),
                prop([0.0; 3], Vec3::new(900.0, 0.0, 0.0)),
            ]
        };
        let cyl = Physics::Explosion {
            cylinder: true,
            outer: 300.0,
            inner: 100.0,
            magnitude: 1.0,
        };
        assert_eq!(kicked(cyl, Vec3::ZERO, &mut mk()), [true, false]);
        // A cylinder reaches any height; a sphere does not.
        let high = Vec3::new(0.0, 0.0, 2000.0);
        assert_eq!(kicked(cyl, high, &mut mk()), [true, false]);
        let sphere = Physics::Explosion {
            cylinder: false,
            outer: 300.0,
            inner: 100.0,
            magnitude: 1.0,
        };
        assert_eq!(kicked(sphere, high, &mut mk()), [false, false]);
        let jitter = Physics::Jitter {
            outer: 300.0,
            inner: 100.0,
            min: 0.5,
            max: 1.0,
        };
        assert_eq!(kicked(jitter, Vec3::ZERO, &mut mk()), [true, false]);
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
    fn a_kicked_box_falls_to_the_floor_and_stops_keeping_its_shape() {
        let mut p = prop([0.0; 3], Vec3::new(0.0, 0.0, 5.0));
        p.kick(Vec3::new(0.0, 0.0, 20.0), 100.0, |_, _| {
            Vec3::new(150.0, 0.0, 100.0)
        });
        for _ in 0..900 {
            p.body.as_mut().unwrap().update(STEP, &Floor);
        }
        let b = p.body.as_ref().unwrap();
        assert!(b.asleep);
        let (lo, _) = p.bounds();
        assert!(lo.z > -0.5, "{lo}");
        assert!(
            b.pairs
                .iter()
                .all(|&(x, y, l)| (b.pos[x].distance(b.pos[y]) - l).abs() < 2.0)
        );
        assert!(p.pose().0[0] > 10.0, "{:?}", p.pose().0);
    }

    #[test]
    fn a_ray_hits_the_nearer_face_and_misses_to_the_side() {
        let (lo, hi) = (Vec3::new(10.0, -5.0, -5.0), Vec3::new(20.0, 5.0, 5.0));
        assert_eq!(ray_box(Vec3::ZERO, Vec3::X, lo, hi), Some(10.0));
        assert_eq!(ray_box(Vec3::ZERO, Vec3::Y, lo, hi), None);
        assert_eq!(ray_box(Vec3::ZERO, -Vec3::X, lo, hi), None);
    }
}
