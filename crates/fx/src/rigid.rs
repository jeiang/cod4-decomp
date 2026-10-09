// SPDX-License-Identifier: GPL-3.0-only
// Parts translated from KisakCOD (physics/phys_ode.cpp: Phys_ObjCreate, Phys_ObjSetCollisionFromXModel, Phys_ObjBulletImpact,
// Phys_TweakBulletImpact, the auto-disable thresholds; GPL-3.0, copyright the KisakCOD contributors and Activision).
//! The PhysPreset rigid body the client simulates for effect models with `USE_MODEL_PHYSICS`, for the map's dynamic
//! entities and for script models the server launched with `physicslaunch`. The original runs ODE; this is a small
//! impulse solver with the same inputs: the preset's mass, bounce and friction, the model's physics geometry (its
//! box, cylinder and brush corners are the collision points) and mass distribution, gravity `phys_gravity`, and the
//! auto-disable thresholds that put a settled body to sleep.

use crate::World;
use assets::zone::phys::PhysPreset;
use assets::zone::xmodel::{PhysGeom, PhysGeomList, XModel};
use glam::{Mat3, Quat, Vec3};
use std::sync::Arc;

/// `phys_gravity`, units per second squared (downwards).
pub const GRAVITY: f32 = 800.0;
/// `phys_autoDisableLinear`: a body slower than this is idle.
const IDLE_LINEAR: f32 = 20.0;
/// `phys_autoDisableAngular`, radians per second.
const IDLE_ANGULAR: f32 = 1.0;
/// `phys_autoDisableTime`: how long a body must be idle to sleep.
const IDLE_TIME: f32 = 0.9;
/// How far from a surface a colliding point is left.
const SKIN: f32 = 0.05;
/// Impact speeds under this do not bounce, so a resting body does not chatter on gravity alone.
const BOUNCE_MIN_SPEED: f32 = 30.0;
const ITERATIONS: usize = 4;
/// The deepest a body is pulled out of solid in one step.
const MAX_ESCAPE: f32 = 64.0;
/// Points on each rim of a cylinder.
const RIM_POINTS: usize = 16;
/// `phys_minImpactMomentum`: a hit with less momentum than this makes no sound.
pub const MIN_IMPACT_MOMENTUM: f32 = 250.0;
const BULLET_MASS: f32 = 0.5;
/// `phys_bulletUpBias` and `phys_bulletSpinScale`.
const BULLET_UP_BIAS: f32 = 0.5;
const BULLET_SPIN_SCALE: f32 = 3.0;
/// Geometry type of a box in [`assets::zone::xmodel::PhysGeom::kind`]; anything else without a brush is a cylinder.
const GEOM_BOX: i32 = 1;

/// Where a model's mass is centred and how it is spread, per unit mass, in the model's frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mass {
    pub center: Vec3,
    pub moments: Vec3,
    pub products: Vec3,
}

/// What collides and how the mass is spread, in the model's frame.
pub struct Shape {
    /// Collision points relative to the centre of mass.
    pts: Arc<[Vec3]>,
    mass: Mass,
}

impl Shape {
    pub fn of_model(model: &XModel) -> Shape {
        Shape::new(
            Vec3::from(model.mins),
            Vec3::from(model.maxs),
            model.phys_geoms.as_deref(),
        )
    }

    /// The shape of a model with these bounds and physics geometry. Without geometry it is a solid box of the
    /// bounds (a 100 unit cube when the bounds are flat, as in the original).
    pub fn new(mins: Vec3, maxs: Vec3, geoms: Option<&PhysGeomList>) -> Shape {
        let (lo, hi) = if mins.x == maxs.x || mins.y == maxs.y || mins.z == maxs.z {
            (Vec3::splat(-50.0), Vec3::splat(50.0))
        } else {
            (mins, maxs)
        };
        let mut pts = Vec::new();
        let mass = match geoms.filter(|g| !g.geoms.is_empty()) {
            Some(list) => {
                for g in list.geoms.iter() {
                    geom_points(g, &mut pts);
                }
                Mass {
                    center: Vec3::from(list.center_of_mass),
                    moments: Vec3::from(list.moments_of_inertia),
                    products: Vec3::from(list.products_of_inertia),
                }
            }
            None => {
                box_corners(lo, hi, &mut pts);
                let d = hi - lo;
                Mass {
                    center: (lo + hi) / 2.0,
                    moments: Vec3::new(
                        d.y * d.y + d.z * d.z,
                        d.x * d.x + d.z * d.z,
                        d.x * d.x + d.y * d.y,
                    ) / 12.0,
                    products: Vec3::ZERO,
                }
            }
        };
        Shape::around(pts, mass)
    }

    fn around(pts: Vec<Vec3>, mass: Mass) -> Shape {
        Shape {
            pts: pts.into_iter().map(|p| p - mass.center).collect(),
            mass,
        }
    }

    /// The same shape with another mass distribution (a map's dynamic entity carries its own).
    pub fn with_mass(&self, mass: Mass) -> Shape {
        let shift = mass.center - self.mass.center;
        Shape {
            pts: self.pts.iter().map(|p| *p - shift).collect(),
            mass,
        }
    }
}

fn box_corners(lo: Vec3, hi: Vec3, out: &mut Vec<Vec3>) {
    for i in 0..8 {
        out.push(Vec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        ));
    }
}

/// The points of one physics geometry that collide, in the model's frame.
fn geom_points(g: &PhysGeom, pts: &mut Vec<Vec3>) {
    let o = g.orientation.map(Vec3::from);
    let at = |v: Vec3| Vec3::from(g.offset) + o[0] * v.x + o[1] * v.y + o[2] * v.z;
    let h = Vec3::from(g.half_lengths);
    if let Some(b) = &g.brush {
        box_corners(Vec3::from(b.mins), Vec3::from(b.maxs), pts);
    } else if g.kind == GEOM_BOX {
        let mut c = Vec::with_capacity(8);
        box_corners(-h, h, &mut c);
        pts.extend(c.into_iter().map(at));
    } else {
        // A cylinder along its x axis: radius `half_lengths[1]`, half height `half_lengths[0]`.
        for end in [-h.x, h.x] {
            for k in 0..RIM_POINTS {
                let a = k as f32 / RIM_POINTS as f32 * std::f32::consts::TAU;
                pts.push(at(Vec3::new(end, a.cos() * h.y, a.sin() * h.y)));
            }
        }
    }
}

/// The first point straight above `p` that is not in solid, searching in growing steps; `p` itself, lifted by the
/// largest step, if there is none.
fn escape_up(world: &dyn World, p: Vec3) -> Vec3 {
    let mut up = 2.0;
    while up <= MAX_ESCAPE {
        let at = p + Vec3::Z * up;
        if world
            .trace(at, at + Vec3::Z * 0.1, Vec3::ZERO, Vec3::ZERO)
            .is_none()
        {
            return at;
        }
        up *= 2.0;
    }
    p + Vec3::Z * MAX_ESCAPE
}

/// A hit hard enough to be heard (`Phys_PlayCollisionSound`): where, off which surface normal, and with what momentum.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Impact {
    pub at: Vec3,
    pub normal: Vec3,
    pub momentum: f32,
}

struct Contact {
    n: Vec3,
    /// From the centre of mass to the point after the push out of the surface.
    r: Vec3,
    /// How fast the point was moving into the surface when it hit.
    approach: f32,
    /// Where it touched the surface.
    hit: Vec3,
    jn: f32,
    jt: f32,
}

fn hit_of(contacts: &[Contact], mass: f32) -> Option<Impact> {
    let loudest = contacts.iter().map(|c| c.approach).fold(0.0, f32::max);
    if loudest <= BOUNCE_MIN_SPEED {
        return None;
    }
    let n = contacts.len() as f32;
    let momentum = contacts.iter().map(|c| c.approach).sum::<f32>() / n * mass;
    (momentum >= MIN_IMPACT_MOMENTUM).then(|| Impact {
        at: contacts.iter().map(|c| c.hit).sum::<Vec3>() / n,
        normal: contacts[0].n,
        momentum,
    })
}

/// One simulated body.
pub struct Body {
    /// Centre of mass in the world, and the rotation from the model's frame.
    pos: Vec3,
    rot: Quat,
    vel: Vec3,
    /// World frame, radians per second.
    ang: Vec3,
    /// The model's origin relative to the centre of mass, in the model's frame.
    origin_local: Vec3,
    /// Collision points relative to the centre of mass, in the model's frame.
    pts: Arc<[Vec3]>,
    mass: f32,
    inertia: Mat3,
    inv_inertia: Mat3,
    bounce: f32,
    friction: f32,
    idle: f32,
    asleep: bool,
    contacts: Vec<Contact>,
    /// The preset's `sndAliasPrefix`, which names the collision sounds.
    sound: Option<Arc<str>>,
    impact: Option<Impact>,
}

impl Body {
    /// A body of `shape` with `preset`'s mass, bounce and friction, with the model's origin at `origin`, its forward,
    /// left and up axes along `axes` and moving at `vel`.
    pub fn new(
        preset: &PhysPreset,
        shape: &Shape,
        origin: Vec3,
        axes: [Vec3; 3],
        vel: Vec3,
    ) -> Body {
        let m = shape.mass;
        let rot = Quat::from_mat3(&Mat3::from_cols(axes[0], axes[1], axes[2])).normalize();
        let total = if preset.mass > 0.0 { preset.mass } else { 1.0 };
        // A non-positive moment is the original's cue for a placeholder of 100.
        let moment = |v: f32| if v > 0.0 { v } else { 100.0 };
        let (p, d) = (m.products, [m.moments.x, m.moments.y, m.moments.z]);
        let inertia = Mat3::from_cols(
            Vec3::new(moment(d[0]), p.x, p.y),
            Vec3::new(p.x, moment(d[1]), p.z),
            Vec3::new(p.y, p.z, moment(d[2])),
        ) * total;
        let inv_inertia = if inertia.determinant().abs() > 1e-12 {
            inertia.inverse()
        } else {
            Mat3::from_diagonal(Vec3::new(
                1.0 / (moment(d[0]) * total),
                1.0 / (moment(d[1]) * total),
                1.0 / (moment(d[2]) * total),
            ))
        };
        Body {
            pos: origin + rot * m.center,
            rot,
            vel,
            ang: Vec3::ZERO,
            origin_local: -m.center,
            pts: shape.pts.clone(),
            mass: total,
            inertia,
            inv_inertia,
            bounce: preset.bounce,
            friction: preset.friction,
            idle: 0.0,
            asleep: false,
            contacts: Vec::new(),
            sound: preset.snd_alias_prefix.clone().filter(|p| !p.is_empty()),
            impact: None,
        }
    }

    /// The preset's collision sound prefix (`physics_wood`), if it has one.
    pub fn sound_prefix(&self) -> Option<&Arc<str>> {
        self.sound.as_ref()
    }

    /// The loud hit of the latest step, once: the average speed into the surfaces over the contacts, times the mass,
    /// at least [`MIN_IMPACT_MOMENTUM`]. A body resting under gravity alone never qualifies (it is not approaching
    /// faster than it bounces).
    pub fn take_impact(&mut self) -> Option<Impact> {
        self.impact.take()
    }

    /// The same body, spinning at `ang` radians per second about the world's axes.
    pub fn spinning(mut self, ang: Vec3) -> Body {
        self.ang = ang;
        self
    }

    /// Puts the body to sleep: it stays where it is until something wakes it.
    pub fn sleep(&mut self) {
        self.asleep = true;
        self.vel = Vec3::ZERO;
        self.ang = Vec3::ZERO;
    }

    pub fn mass(&self) -> f32 {
        self.mass
    }

    pub fn asleep(&self) -> bool {
        self.asleep
    }

    pub fn velocity(&self) -> Vec3 {
        self.vel
    }

    pub fn angular_velocity(&self) -> Vec3 {
        self.ang
    }

    pub fn center_of_mass(&self) -> Vec3 {
        self.pos
    }

    /// Where the model's origin is.
    pub fn origin(&self) -> Vec3 {
        self.pos + self.rot * self.origin_local
    }

    /// The model's forward, left and up axes.
    pub fn axes(&self) -> [Vec3; 3] {
        [self.rot * Vec3::X, self.rot * Vec3::Y, self.rot * Vec3::Z]
    }

    fn world_inv_inertia(&self) -> Mat3 {
        let r = Mat3::from_quat(self.rot);
        r * self.inv_inertia * r.transpose()
    }

    /// Adds momentum `j` at the world point `at` and wakes the body.
    pub fn impulse(&mut self, at: Vec3, j: Vec3) {
        self.vel += j / self.mass;
        self.ang += self.world_inv_inertia() * (at - self.pos).cross(j);
        self.wake();
    }

    /// Wakes the body so it falls and settles again.
    pub fn wake(&mut self) {
        self.asleep = false;
        self.idle = 0.0;
    }

    /// A bullet of `speed` travelling along `dir` hits the world point `at` (`Phys_ObjBulletImpact`); `scale` is the
    /// preset's bullet force scale.
    pub fn bullet_impact(&mut self, at: Vec3, dir: Vec3, speed: f32, scale: f32) {
        let mut dir = dir;
        dir.z += BULLET_UP_BIAS;
        let dir = dir.normalize_or(Vec3::Z);
        let at = at + (at - self.pos) * BULLET_SPIN_SCALE;
        let relative = speed - self.vel.dot(dir);
        let mut axis = (at - self.pos).cross(dir);
        let radius = axis.length();
        if radius > 0.0 {
            axis /= radius;
        }
        let body_axis = self.rot.inverse() * axis;
        let moment = body_axis.dot(self.inertia * body_axis);
        let numerator = (relative - self.ang.dot(axis) * radius) * 2.0 * BULLET_MASS;
        if numerator <= 0.0 {
            return;
        }
        let mut denominator = self.mass + BULLET_MASS;
        if radius != 0.0 && moment > 0.0 {
            denominator += radius * radius * self.mass * BULLET_MASS / moment;
        }
        self.impulse(at, dir * (scale * self.mass * numerator / denominator));
    }

    /// Advances the body `dt` seconds (at most about 17 ms) against `world`.
    pub fn step(&mut self, dt: f32, world: &dyn World) {
        if self.asleep {
            return;
        }
        let (old_pos, old_rot) = (self.pos, self.rot);
        self.vel.z -= GRAVITY * dt;
        self.pos += self.vel * dt;
        self.rot = (Quat::from_scaled_axis(self.ang * dt) * self.rot).normalize();

        // Every point is traced from where it was to where it went; the ones that hit are the contacts.
        let mut contacts = std::mem::take(&mut self.contacts);
        contacts.clear();
        let mut hits = Vec::new();
        for p in self.pts.iter() {
            let (w0, w1) = (old_pos + old_rot * *p, self.pos + self.rot * *p);
            if let Some((f, n)) = world.trace(w0, w1, Vec3::ZERO, Vec3::ZERO) {
                match n.try_normalize() {
                    Some(n) => hits.push((w1, w0.lerp(w1, f), n)),
                    // No surface to push away from: the point is inside the solid. Find the way up out of it.
                    None => hits.push((w1, escape_up(world, w1), Vec3::Z)),
                }
            }
        }
        // Push the body out of the surfaces: each hit moves it only as far as the earlier ones left it short.
        let mut shift = Vec3::ZERO;
        for &(w1, hit, n) in &hits {
            let depth = (w1 + shift - hit).dot(n);
            if depth < SKIN {
                shift += n * (SKIN - depth);
            }
        }
        self.pos += shift;
        for &(w1, hit, n) in &hits {
            let r = w1 + shift - self.pos;
            let approach = -(self.vel + self.ang.cross(r)).dot(n);
            contacts.push(Contact {
                n,
                r,
                approach,
                hit,
                jn: 0.0,
                jt: 0.0,
            });
        }
        self.impact = hit_of(&contacts, self.mass);
        self.solve(&mut contacts);
        self.contacts = contacts;

        if self.vel.length() < IDLE_LINEAR && self.ang.length() < IDLE_ANGULAR {
            self.idle += dt;
            if self.idle >= IDLE_TIME {
                self.sleep();
            }
        } else {
            self.idle = 0.0;
        }
    }

    /// Sequential impulses at the contacts: stop the approach (bouncing off a hard enough hit by the preset's
    /// bounce), then take the sliding off the surface up to the preset's friction times the normal impulse.
    fn solve(&mut self, contacts: &mut [Contact]) {
        let inv_i = self.world_inv_inertia();
        let inv_m = 1.0 / self.mass;
        for _ in 0..ITERATIONS {
            for c in contacts.iter_mut() {
                let vp = self.vel + self.ang.cross(c.r);
                let vn = vp.dot(c.n);
                let target = if c.approach > BOUNCE_MIN_SPEED {
                    self.bounce * c.approach
                } else {
                    0.0
                };
                let k = inv_m + c.n.dot((inv_i * c.r.cross(c.n)).cross(c.r));
                let jn = ((target - vn) / k).max(-c.jn);
                c.jn += jn;
                self.apply(c.r, c.n * jn, inv_i, inv_m);

                let vp = self.vel + self.ang.cross(c.r);
                let vt = vp - c.n * vp.dot(c.n);
                let speed = vt.length();
                if speed > 1e-4 {
                    let t = vt / speed;
                    let kt = inv_m + t.dot((inv_i * c.r.cross(t)).cross(c.r));
                    let jt = (speed / kt).min((self.friction * c.jn - c.jt).max(0.0));
                    c.jt += jt;
                    self.apply(c.r, -t * jt, inv_i, inv_m);
                }
            }
        }
    }

    fn apply(&mut self, r: Vec3, j: Vec3, inv_i: Mat3, inv_m: f32) {
        self.vel += j * inv_m;
        self.ang += inv_i * r.cross(j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Floor;

    impl World for Floor {
        fn trace(&self, a: Vec3, b: Vec3, _: Vec3, _: Vec3) -> Option<(f32, Vec3)> {
            (a.z >= 0.0 && b.z < 0.0).then(|| (a.z / (a.z - b.z), Vec3::Z))
        }
    }

    fn preset(mass: f32, bounce: f32, friction: f32) -> PhysPreset {
        PhysPreset {
            name: None,
            kind: 0,
            mass,
            bounce,
            friction,
            bullet_force_scale: 1.0,
            explosive_force_scale: 1.0,
            snd_alias_prefix: None,
            pieces_spread_fraction: 0.0,
            pieces_upward_velocity: 0.0,
            temp_default_to_cylinder: false,
        }
    }

    fn shape(mins: [f32; 3], maxs: [f32; 3], geoms: Option<Vec<PhysGeom>>) -> Shape {
        let list = geoms.map(|g| PhysGeomList {
            geoms: g.into(),
            center_of_mass: [0.0; 3],
            moments_of_inertia: [0.0; 3],
            products_of_inertia: [0.0; 3],
        });
        Shape::new(Vec3::from(mins), Vec3::from(maxs), list.as_ref())
    }

    const AXES: [Vec3; 3] = [Vec3::X, Vec3::Y, Vec3::Z];

    fn run(b: &mut Body, seconds: f32) {
        for _ in 0..(seconds * 60.0) as usize {
            b.step(1.0 / 60.0, &Floor);
        }
    }

    fn cube(p: &PhysPreset) -> Body {
        let s = shape([-5.0; 3], [5.0; 3], None);
        Body::new(p, &s, Vec3::new(0.0, 0.0, 100.0), AXES, Vec3::ZERO)
    }

    #[test]
    fn a_dropped_body_falls_at_gravity_and_comes_to_rest_on_the_floor() {
        let mut b = cube(&preset(5.0, 0.0, 0.5));
        // Half a second of free fall: z = 100 - g t^2 / 2 to within one step's integration error.
        for _ in 0..30 {
            b.step(1.0 / 60.0, &Floor);
        }
        let fell = 100.0 - b.origin().z;
        assert!((fell - 0.5 * GRAVITY * 0.25).abs() < 10.0, "{fell}");
        run(&mut b, 4.0);
        assert!(b.asleep());
        let z = b.origin().z;
        assert!((4.9..5.5).contains(&z), "{z}");
    }

    /// A floor at z = 0 with solid below it, as a trace sees it: a path that starts inside the solid is stuck, with
    /// no surface normal.
    struct Slab;

    impl World for Slab {
        fn trace(&self, a: Vec3, b: Vec3, _: Vec3, _: Vec3) -> Option<(f32, Vec3)> {
            if a.z < 0.0 {
                Some((0.0, Vec3::ZERO))
            } else {
                Floor.trace(a, b, Vec3::ZERO, Vec3::ZERO)
            }
        }
    }

    #[test]
    fn a_body_inside_solid_is_pulled_out_at_once_and_rests() {
        let s = shape([-5.0; 3], [5.0; 3], None);
        let mut b = Body::new(
            &preset(5.0, 0.0, 0.5),
            &s,
            Vec3::new(0.0, 0.0, 2.0),
            AXES,
            Vec3::ZERO,
        );
        // Its lowest corners are 3 units in the slab; one step puts them back above the surface.
        b.step(1.0 / 60.0, &Slab);
        let low = b.origin().z - 5.0;
        assert!((-0.1..1.0).contains(&low), "{low}");
        for _ in 0..240 {
            b.step(1.0 / 60.0, &Slab);
        }
        assert!(b.asleep());
        assert!((4.9..5.5).contains(&b.origin().z), "{}", b.origin().z);
    }

    #[test]
    fn bounce_decides_how_high_it_comes_back() {
        let apex = |bounce: f32| {
            let mut b = cube(&preset(5.0, bounce, 0.5));
            let (mut hit, mut top) = (false, 0.0f32);
            for _ in 0..240 {
                let before = b.velocity().z;
                b.step(1.0 / 60.0, &Floor);
                hit |= before < -100.0 && b.velocity().z > 0.0;
                if hit {
                    top = top.max(b.origin().z);
                }
            }
            assert!(hit);
            top
        };
        let (dead, lively) = (apex(0.0), apex(0.6));
        assert!(dead < 6.0, "{dead}");
        assert!(lively > 20.0 && lively < 80.0, "{lively}");
    }

    #[test]
    fn friction_stops_a_sliding_body_sooner() {
        let slide = |friction: f32| {
            let s = shape([-5.0; 3], [5.0; 3], None);
            let mut b = Body::new(
                &preset(5.0, 0.0, friction),
                &s,
                Vec3::new(0.0, 0.0, 5.1),
                AXES,
                Vec3::new(200.0, 0.0, 0.0),
            );
            run(&mut b, 12.0);
            assert!(b.asleep());
            b.origin().x
        };
        let (grippy, slick) = (slide(0.9), slide(0.05));
        assert!(grippy < slick - 20.0, "{grippy} {slick}");
    }

    #[test]
    fn a_body_that_stays_slow_for_the_idle_time_sleeps_and_a_hit_wakes_it() {
        let mut b = cube(&preset(5.0, 0.0, 0.5));
        run(&mut b, 1.0);
        assert!(!b.asleep(), "still falling after a second");
        run(&mut b, 3.0);
        assert!(b.asleep());
        let at = b.origin();
        b.bullet_impact(at + Vec3::new(-5.0, 0.0, 2.0), Vec3::X, 1000.0, 1.0);
        assert!(!b.asleep());
        assert!(b.velocity().x > 5.0, "{:?}", b.velocity());
    }

    #[test]
    fn it_rests_on_the_physics_geometry_not_the_bounds() {
        let geom = |kind, half_lengths| PhysGeom {
            brush: None,
            kind,
            orientation: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            offset: [0.0; 3],
            half_lengths,
        };
        // Bounds of a 100 unit cube; the geometry is a 10 unit box or a cylinder of radius 4 lying along x.
        let rest = |g: PhysGeom| {
            let s = shape([-50.0; 3], [50.0; 3], Some(vec![g]));
            let mut b = Body::new(
                &preset(5.0, 0.0, 0.8),
                &s,
                Vec3::new(0.0, 0.0, 60.0),
                AXES,
                Vec3::ZERO,
            );
            run(&mut b, 6.0);
            b.origin().z
        };
        let boxed = rest(geom(1, [5.0, 5.0, 5.0]));
        assert!((4.5..5.5).contains(&boxed), "{boxed}");
        let rolled = rest(geom(4, [12.0, 4.0, 4.0]));
        assert!((3.5..4.5).contains(&rolled), "{rolled}");
    }
    #[test]
    fn only_a_hard_heavy_hit_is_loud_and_resting_is_silent() {
        let loud = |mass: f32, drop: f32| {
            let s = shape([-5.0; 3], [5.0; 3], None);
            let mut b = Body::new(
                &preset(mass, 0.0, 0.5),
                &s,
                Vec3::new(0.0, 0.0, drop),
                AXES,
                Vec3::ZERO,
            );
            let mut heard = Vec::new();
            for _ in 0..180 {
                b.step(1.0 / 60.0, &Floor);
                heard.extend(b.take_impact());
            }
            heard
        };
        let heavy = loud(20.0, 100.0);
        assert_eq!(heavy.len(), 1, "one landing, then rest: {heavy:?}");
        assert!(heavy[0].momentum > MIN_IMPACT_MOMENTUM && heavy[0].normal == Vec3::Z);
        assert!(loud(0.1, 100.0).is_empty(), "too light to hear");
        assert!(
            loud(20.0, 5.6).is_empty(),
            "a body that is barely falling lands silently"
        );
    }
}
