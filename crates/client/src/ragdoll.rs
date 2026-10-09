// SPDX-License-Identifier: GPL-3.0-only
// The ragdoll definition (`ragdoll.cfg`), bodies, joints, limits and explosion force follow KisakCOD (GPL-3.0, KisakCOD contributors and Activision) `ragdoll/ragdoll.cpp` and `ragdoll/ragdoll_update.cpp`; the collision sounds follow `physics/phys_ode.cpp` (`Phys_PlayCollisionSound`).
//! A dead player's body as a joint-limited rigid-body ragdoll. The install's `ragdoll.cfg` names the bones that get a
//! rigid body (a capsule from one skeleton joint to the next, with a radius and a mass), the joints between them (a
//! ball-and-socket with per-axis angle limits, or a hinge) and the pairs of limbs that must not pass through each
//! other. Every skeleton bone follows the body it hangs from, so the drawn pose is the death pose with the bodies' own
//! motion on top.
//!
//! The original runs ODE; this is a position-based solver of the same bodies: each body has a position, an orientation
//! and velocities, a joint pulls the child's anchor back onto the parent's and turns the child back inside its angle
//! limits (measured from the bind pose, as the original's joint zero is), and joint friction holds the limbs stiff at
//! first and lets go over [`JOINT_LERP`] seconds. Capsules collide with the map by box sweeps of their radius.
//! Bones' twist is part of the body orientation, so it is kept.

use fx::{Impact, MIN_IMPACT_MOMENTUM, Rng};
use glam::{Mat3, Quat, Vec3};
use sim::cm::Collide;
use sim::skel::BoneMat;

use crate::props::Blast;

const STEP: f32 = 1.0 / 60.0;
const SUBSTEPS: usize = 3;
const GRAVITY: f32 = 800.0;
/// Constraint passes per substep.
const ITERATIONS: usize = 4;
/// `ragdoll_max_life`: seconds a body simulates, at most, before it is frozen where it lies.
const MAX_LIFE: f32 = 4.5;
/// `ragdoll_max_simulating`: bodies simulating at once, across the client.
pub const MAX_SIMULATING: usize = 16;
/// `ragdoll_explode_force` and `ragdoll_explode_upbias`.
const EXPLODE_FORCE: f32 = 18000.0;
const EXPLODE_UPBIAS: f32 = 0.8;
/// `ragdoll_self_collision_scale`: how much larger than the bone the capsules that keep limbs apart are.
const SELF_SCALE: f32 = 1.2;
/// `ragdoll_jointlerp_time` (seconds): joint friction falls from its full value to a tenth over this time.
const JOINT_LERP: f32 = 3.0;
/// The torque a unit of the file's joint friction holds (`Ragdoll_CreatePhysJoint` scales it by 15).
const FRICTION_TORQUE: f32 = 15.0;
/// What the ground and the other limbs grip with.
const CONTACT_FRICTION: f32 = 0.8;
/// Distance a body keeps from a surface.
const SKIN: f32 = 0.05;
/// A body asleep when nothing moves faster than this (units per second, radians per second) for [`IDLE_TIME`].
const IDLE_LINEAR: f32 = 20.0;
const IDLE_ANGULAR: f32 = 1.0;
const IDLE_TIME: f32 = 0.9;
/// The fastest a body may move, and turn, which keeps a blast from throwing the solver out of its stable range.
const MAX_SPEED: f32 = 1500.0;
const MAX_SPIN: f32 = 40.0;
/// Segments shorter than this are points.
const EPS: f32 = 1e-4;

/// The collision sound of a body hitting the map. The original gives a ragdoll bone a preset with no sound prefix,
/// whose class is 0: the first class its stock presets register; this is the stock wood.
pub const SOUND: &str = "physics_wood";
/// Approach speeds under this are sliding and resting, not hits (the rigid bodies' bounce threshold).
const HIT_SPEED: f32 = 30.0;
/// Seconds between two hits a body is heard making.
const HIT_GAP: f32 = 0.25;

/// A ragdoll definition: `ragdoll.cfg`'s first (the one the game's bodies use).
#[derive(Debug, Default, Clone)]
pub struct Def {
    bones: Vec<BoneDef>,
    joints: Vec<JointDef>,
    pairs: Vec<[usize; 2]>,
}

#[derive(Debug, Clone)]
struct BoneDef {
    /// The skeleton bones the capsule runs between.
    from: String,
    to: String,
    radius: f32,
    /// Where the centre of mass sits between the two, 0 to 1.
    cog: f32,
    mass: f32,
    parent: Option<usize>,
    /// The skeleton's right-side bones point away from their child.
    mirror: bool,
}

#[derive(Debug, Clone)]
struct JointDef {
    bone: usize,
    hinge: bool,
    limits: Vec<LimitDef>,
}

#[derive(Debug, Clone, Copy)]
struct LimitDef {
    /// Unit axis in the child's frame.
    axis: Vec3,
    friction: f32,
    min: f32,
    max: f32,
}

impl Def {
    /// Reads the `ragdoll_bone`, `ragdoll_joint`, `ragdoll_limit` and `ragdoll_selfpair` lines about definition 0
    /// (`ragdoll_clear 0` empties it). A line that does not parse, or refers to what is not there yet, is skipped as
    /// the original's console command would.
    pub fn parse(text: &str) -> Def {
        let mut d = Def::default();
        for line in text.lines() {
            let line = line.split("//").next().unwrap_or("");
            let a: Vec<&str> = line.split_whitespace().collect();
            match a.as_slice() {
                ["ragdoll_clear", "0"] => d = Def::default(),
                ["ragdoll_bone", "0", cols @ ..] => {
                    if let Some(b) = d.bone(cols) {
                        d.bones.push(b);
                    }
                }
                ["ragdoll_joint", "0", bone, kind, ..] => {
                    let hinge = match *kind {
                        "hinge" => true,
                        "swivel" | "ball" => false,
                        _ => continue,
                    };
                    if let Ok(bone) = bone.parse::<usize>()
                        && bone > 0
                        && bone < d.bones.len()
                    {
                        d.joints.push(JointDef {
                            bone,
                            hinge,
                            limits: Vec::new(),
                        });
                    }
                }
                ["ragdoll_limit", "0", joint, axis, friction, min, max, ..] => {
                    let (Ok(joint), Some(axis)) = (joint.parse::<usize>(), axis_of(axis)) else {
                        continue;
                    };
                    let (Ok(friction), Ok(min), Ok(max)) = (
                        friction.parse::<f32>(),
                        min.parse::<f32>(),
                        max.parse::<f32>(),
                    ) else {
                        continue;
                    };
                    if let Some(j) = d.joints.get_mut(joint)
                        && j.limits.len() < 3
                    {
                        let rad = |v: f32| {
                            v.to_radians()
                                .clamp(-std::f32::consts::PI, std::f32::consts::PI)
                        };
                        j.limits.push(LimitDef {
                            axis,
                            friction,
                            min: rad(min),
                            max: rad(max),
                        });
                    }
                }
                ["ragdoll_selfpair", "0", x, y, ..] => {
                    if let (Ok(x), Ok(y)) = (x.parse::<usize>(), y.parse::<usize>())
                        && x < d.bones.len()
                        && y < d.bones.len()
                    {
                        d.pairs.push([x, y]);
                    }
                }
                _ => {}
            }
        }
        d
    }

    /// One `ragdoll_bone` line's columns after the definition: the two joints, radius, centre of mass, mass, friction
    /// (unused: the grip is [`CONTACT_FRICTION`]), parent bone and mirror flag.
    fn bone(&self, cols: &[&str]) -> Option<BoneDef> {
        let [from, to, radius, cog, mass, _friction, parent, mirror, ..] = cols else {
            return None;
        };
        let parent = match parent.parse::<i32>().ok()? {
            -1 => None,
            p => Some(usize::try_from(p).ok().filter(|p| *p < self.bones.len())?),
        };
        Some(BoneDef {
            from: (*from).to_owned(),
            to: (*to).to_owned(),
            radius: radius.parse().ok()?,
            cog: cog.parse().ok()?,
            mass: mass.parse::<f32>().ok().filter(|m| *m > 0.0)?,
            parent,
            mirror: mirror.parse::<i32>().ok()? != 0,
        })
    }

    /// True when the definition has nothing to simulate.
    pub fn is_empty(&self) -> bool {
        self.bones.is_empty()
    }
}

/// `x`, `y`, `z` or one of them negated, as a unit vector.
fn axis_of(s: &str) -> Option<Vec3> {
    let (sign, name) = match s.strip_prefix('-') {
        Some(n) => (-1.0, n),
        None => (1.0, s),
    };
    let v = match name {
        "x" => Vec3::X,
        "y" => Vec3::Y,
        "z" => Vec3::Z,
        _ => return None,
    };
    Some(v * sign)
}

/// What the ragdoll needs of the skeleton it is made for: bone names, parents, and the bones that copy another's
/// matrix.
pub struct Skel {
    pub names: Vec<String>,
    pub parent: Vec<Option<usize>>,
    pub alias: Vec<Option<usize>>,
}

/// One rigid body: a capsule along its local x axis.
struct Body {
    /// Centre of mass and orientation, and their values at the start of the substep.
    x: Vec3,
    q: Quat,
    x0: Vec3,
    q0: Quat,
    v: Vec3,
    w: Vec3,
    inv_m: f32,
    /// Inverse moments of inertia about the body's axes.
    inv_i: Vec3,
    radius: f32,
    /// The capsule's end points relative to the centre of mass, in the body frame.
    ends: [Vec3; 2],
    /// Where the map is probed: both ends and the middle.
    probes: [Vec3; 3],
}

impl Body {
    fn at(&self, p: Vec3) -> Vec3 {
        self.x + self.q * p
    }

    fn at0(&self, p: Vec3) -> Vec3 {
        self.x0 + self.q0 * p
    }

    fn inv_inertia(&self, v: Vec3) -> Vec3 {
        self.q * (self.inv_i * (self.q.inverse() * v))
    }

    /// How little the point `r` from the centre of mass resists a push along `n`.
    fn effective_inv_mass(&self, r: Vec3, n: Vec3) -> f32 {
        let rn = r.cross(n);
        self.inv_m + rn.dot(self.inv_inertia(rn))
    }

    /// Moves the point `r` from the centre of mass by pushing it with the displacement-impulse `p`.
    fn push(&mut self, r: Vec3, p: Vec3) {
        self.x += p * self.inv_m;
        self.turn(self.inv_inertia(r.cross(p)));
    }

    /// Turns the body by the rotation vector `v` (radians, world axes).
    fn turn(&mut self, v: Vec3) {
        self.q = (Quat::from_scaled_axis(v) * self.q).normalize();
    }

    fn inertia_scalar(&self) -> f32 {
        1.0 / self.inv_i.min_element().max(1e-6)
    }
}

struct Joint {
    parent: usize,
    child: usize,
    /// The joint's anchor in each body's frame (relative to its centre of mass).
    ap: Vec3,
    ac: Vec3,
    /// The child's rotation in the parent's frame in the bind pose.
    rest: Quat,
    hinge: bool,
    /// Per axis of the child's frame: the limits and whether there are any. A hinge keeps `axis`, locking the rest.
    lo: [f32; 3],
    hi: [f32; 3],
    limited: [bool; 3],
    axis: Vec3,
    friction: f32,
}

/// A skeleton bone's place on the body it follows.
#[derive(Clone, Copy)]
struct Follow {
    body: usize,
    q: Quat,
    p: Vec3,
}

pub struct Ragdoll {
    bodies: Vec<Body>,
    joints: Vec<Joint>,
    pairs: Vec<[usize; 2]>,
    follow: Vec<Follow>,
    alias: Vec<Option<usize>>,
    yaw: Quat,
    age: f32,
    carry: f32,
    idle: f32,
    asleep: bool,
    rng: Rng,
    /// The hardest unheard hit, and the time before another may be heard.
    impact: Option<Impact>,
    quiet: f32,
}

fn quat_of(b: &BoneMat) -> Quat {
    Quat::from_xyzw(b.quat[0], b.quat[1], b.quat[2], b.quat[3]).normalize()
}

impl Ragdoll {
    /// A body for an entity at `origin` facing `yaw_degrees`, in the pose `pose` (entity space) thrown with velocity
    /// `push` (units per second). `base` is the skeleton's bind pose, the zero of the joints' angles. `None` when
    /// the skeleton lacks a bone the definition names.
    pub fn new(
        def: &Def,
        skel: &Skel,
        pose: &[BoneMat],
        base: &[BoneMat],
        origin: [f32; 3],
        yaw_degrees: f32,
        push: [f32; 3],
    ) -> Option<Self> {
        let n = skel.names.len();
        if def.is_empty() || pose.len() < n || base.len() < n {
            return None;
        }
        let find = |name: &str| skel.names.iter().position(|s| s.eq_ignore_ascii_case(name));
        let origin = Vec3::from(origin);
        let yaw = Quat::from_rotation_z(yaw_degrees.to_radians());
        let mirror = |b: &BoneDef| {
            if b.mirror {
                Quat::from_rotation_z(std::f32::consts::PI)
            } else {
                Quat::IDENTITY
            }
        };
        let world: Vec<(Vec3, Quat)> = pose[..n]
            .iter()
            .map(|b| (origin + yaw * Vec3::from(b.trans), yaw * quat_of(b)))
            .collect();
        let mut ends = Vec::new();
        for b in &def.bones {
            ends.push((find(&b.from)?, find(&b.to)?));
        }
        let mut bodies = Vec::with_capacity(def.bones.len());
        let mut frames = Vec::with_capacity(def.bones.len());
        let mut bind = Vec::with_capacity(def.bones.len());
        for (b, &(from, to)) in def.bones.iter().zip(&ends) {
            let (p0, p1) = (world[from].0, world[to].0);
            let q = (world[from].1 * mirror(b)).normalize();
            let len = p0.distance(p1);
            let cog = b.cog.clamp(0.0, 1.0) * len;
            let caps = [Vec3::new(-cog, 0.0, 0.0), Vec3::new(len - cog, 0.0, 0.0)];
            // A solid cylinder: twist about the axis, swing about the other two.
            let (r, m) = (b.radius, b.mass);
            let swing = m * (3.0 * r * r + len * len) / 12.0;
            let twist = 0.5 * m * r * r;
            bodies.push(Body {
                x: p0 + q * Vec3::new(cog, 0.0, 0.0),
                q,
                x0: Vec3::ZERO,
                q0: Quat::IDENTITY,
                v: Vec3::ZERO,
                w: Vec3::ZERO,
                inv_m: 1.0 / m,
                inv_i: Vec3::new(
                    1.0 / twist.max(1e-3),
                    1.0 / swing.max(1e-3),
                    1.0 / swing.max(1e-3),
                ),
                radius: r,
                ends: caps,
                probes: [caps[0], (caps[0] + caps[1]) * 0.5, caps[1]],
            });
            frames.push(p0);
            bind.push((quat_of(&base[from]) * mirror(b)).normalize());
        }
        // The body may start with a limb's capsule in the floor under the entity: lift it clear.
        let lift = bodies
            .iter()
            .flat_map(|b| {
                b.probes
                    .iter()
                    .map(|p| b.radius + SKIN - (b.at(*p).z - origin.z))
            })
            .fold(0.0, f32::max);
        for b in &mut bodies {
            b.x.z += lift;
        }
        let joints = def
            .joints
            .iter()
            .filter_map(|j| {
                let parent = def.bones[j.bone].parent?;
                let anchor = frames[j.bone] + Vec3::Z * lift;
                let (c, p) = (&bodies[j.bone], &bodies[parent]);
                let mut lo = [-std::f32::consts::PI; 3];
                let mut hi = [std::f32::consts::PI; 3];
                let mut limited = [false; 3];
                let mut axis = Vec3::Z;
                let mut friction = 0.0f32;
                for (k, l) in j.limits.iter().enumerate() {
                    friction = friction.max(l.friction);
                    if j.hinge {
                        if k == 0 {
                            axis = l.axis;
                            lo[2] = l.min;
                            hi[2] = l.max;
                        }
                        continue;
                    }
                    let i = (0..3).find(|i| l.axis[*i] != 0.0)?;
                    let flip = l.axis[i] < 0.0;
                    limited[i] = true;
                    (lo[i], hi[i]) = if flip {
                        (-l.max, -l.min)
                    } else {
                        (l.min, l.max)
                    };
                }
                if j.limits.is_empty() {
                    // The original's default: a hinge-like swing about z.
                    limited[2] = true;
                    (lo[2], hi[2]) = (-std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2);
                    friction = 0.0;
                }
                Some(Joint {
                    parent,
                    child: j.bone,
                    ap: p.q.inverse() * (anchor - p.x),
                    ac: c.q.inverse() * (anchor - c.x),
                    rest: (bind[parent].inverse() * bind[j.bone]).normalize(),
                    hinge: j.hinge,
                    lo,
                    hi,
                    limited,
                    axis,
                    friction: friction * FRICTION_TORQUE,
                })
            })
            .collect();
        // Every bone follows the nearest body at or above it.
        let owner: Vec<Option<usize>> =
            (0..n).map(|i| ends.iter().position(|e| e.0 == i)).collect();
        let source = |i: usize| skel.alias[i].filter(|a| *a < n).unwrap_or(i);
        let follow = (0..n)
            .map(|i| {
                let mut cur = source(i);
                let mut hops = 0;
                let body = loop {
                    if let Some(k) = owner[cur] {
                        break k;
                    }
                    match skel.parent[cur].filter(|p| *p < n) {
                        Some(p) if hops < n => {
                            cur = source(p);
                            hops += 1;
                        }
                        _ => break 0,
                    }
                };
                let b = &bodies[body];
                let (p, q) = world[source(i)];
                Follow {
                    body,
                    q: b.q.inverse() * q,
                    p: b.q.inverse() * (p + Vec3::Z * lift - b.x),
                }
            })
            .collect();
        let v = Vec3::from(push).clamp_length_max(MAX_SPEED);
        for b in &mut bodies {
            b.v = v;
        }
        let seed = (origin.x.to_bits() ^ origin.y.to_bits().rotate_left(11)) as u64;
        Some(Self {
            bodies,
            joints,
            pairs: def.pairs.clone(),
            follow,
            alias: skel.alias.clone(),
            yaw,
            age: 0.0,
            carry: 0.0,
            idle: 0.0,
            asleep: false,
            rng: Rng::new(seed),
            impact: None,
            quiet: 0.0,
        })
    }

    /// The hit to make a sound for, once.
    pub fn take_impact(&mut self) -> Option<Impact> {
        self.impact.take()
    }

    /// True once the body has stopped (or been simulated for [`MAX_LIFE`] seconds).
    pub fn at_rest(&self) -> bool {
        self.asleep
    }

    /// The yaw (degrees) of the entity the bones are relative to.
    pub fn yaw(&self) -> f32 {
        self.yaw.to_euler(glam::EulerRot::ZYX).0.to_degrees()
    }

    /// Stops the body where it is, as when too many bodies simulate.
    pub fn freeze(&mut self) {
        self.asleep = true;
    }

    /// `Ragdoll_ExplosionEvent`: a blast at `at` wakes a body whose torso it reaches, and every bone within the outer
    /// radius gets its share of the force (away from the centre, tilted up), with a unit of jitter in where it lands.
    /// Returns whether the body was reached.
    pub fn explode(&mut self, at: Vec3, b: &Blast) -> bool {
        let flat = |d: Vec3| if b.cylinder { d.with_z(0.0) } else { d };
        let outer2 = b.outer * b.outer;
        if b.outer <= 0.0 || flat(self.bodies[0].x - at).length_squared() >= outer2 {
            return false;
        }
        let inner2 = b.inner * b.inner;
        let inv_range = if inner2 < outer2 {
            1.0 / (outer2 - inner2)
        } else {
            0.0
        };
        self.asleep = false;
        self.age = 0.0;
        self.idle = 0.0;
        let share = 1.0 / self.bodies.len() as f32;
        for body in &mut self.bodies {
            let rng = &mut self.rng;
            let mut jitter = || rng.f() * 2.0 - 1.0;
            let centre = body.x + Vec3::new(jitter(), jitter(), jitter());
            let d = centre - at;
            let dist2 = d.length_squared();
            if dist2 >= outer2 {
                continue;
            }
            let mut scale = b.scale;
            if inner2 < dist2 {
                scale *= (outer2 - dist2) * inv_range;
            }
            let dir = if b.impulse != Vec3::ZERO {
                b.impulse
            } else {
                flat(d).normalize_or_zero()
            };
            let dir = (dir + Vec3::Z * EXPLODE_UPBIAS).normalize_or(Vec3::Z);
            let j = dir * (scale * EXPLODE_FORCE * share);
            body.v = (body.v + j * body.inv_m).clamp_length_max(MAX_SPEED);
            body.w =
                (body.w + body.inv_inertia((centre - body.x).cross(j))).clamp_length_max(MAX_SPIN);
        }
        true
    }

    /// The lowest and highest point of the capsules' axes.
    #[cfg(test)]
    pub fn bounds(&self) -> (Vec3, Vec3) {
        self.bodies
            .iter()
            .flat_map(|b| b.ends.map(|e| b.at(e)))
            .fold(
                (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
                |(lo, hi), p| (lo.min(p), hi.max(p)),
            )
    }

    /// Advances by `dt` seconds against `world`.
    pub fn update(&mut self, dt: f32, world: &dyn Collide) {
        if self.asleep {
            return;
        }
        self.carry += dt.min(0.1);
        while self.carry >= STEP && !self.asleep {
            self.carry -= STEP;
            self.step(world);
            self.age += STEP;
            self.quiet -= STEP;
        }
    }

    fn step(&mut self, world: &dyn Collide) {
        let dt = STEP / SUBSTEPS as f32;
        let t = (1.0 - self.age / JOINT_LERP).clamp(0.0, 1.0);
        let friction_scale = t * t * 0.9 + 0.1;
        for _ in 0..SUBSTEPS {
            for b in &mut self.bodies {
                b.x0 = b.x;
                b.q0 = b.q;
                b.v.z -= GRAVITY * dt;
                b.v = b.v.clamp_length_max(MAX_SPEED);
                b.w = b.w.clamp_length_max(MAX_SPIN);
                b.x += b.v * dt;
                b.q = (Quat::from_scaled_axis(b.w * dt) * b.q).normalize();
            }
            for _ in 0..ITERATIONS {
                for j in 0..self.joints.len() {
                    self.pull_anchor(j);
                    self.keep_in_limits(j);
                }
                for p in 0..self.pairs.len() {
                    self.keep_apart(self.pairs[p]);
                }
            }
            self.touch_map(world, dt);
            let mut quiet = true;
            for b in &mut self.bodies {
                b.v = (b.x - b.x0) / dt;
                let d = b.q * b.q0.inverse();
                let s = if d.w < 0.0 { -2.0 } else { 2.0 };
                b.w = Vec3::new(d.x, d.y, d.z) * (s / dt);
                quiet &= b.v.length() < IDLE_LINEAR && b.w.length() < IDLE_ANGULAR;
            }
            for j in 0..self.joints.len() {
                self.hold_joint(j, friction_scale, dt);
            }
            self.idle = if quiet { self.idle + dt } else { 0.0 };
        }
        if self.impact.is_some() {
            self.quiet = HIT_GAP;
        }
        if self.idle >= IDLE_TIME || self.age >= MAX_LIFE {
            self.asleep = true;
        }
    }

    /// Two bodies' points, turned and moved toward each other along `n` by `depth` in proportion to how little each
    /// resists.
    fn separate(&mut self, a: (usize, Vec3), b: (usize, Vec3), n: Vec3, depth: f32) {
        let (ra, rb) = (a.1 - self.bodies[a.0].x, b.1 - self.bodies[b.0].x);
        let (wa, wb) = (
            self.bodies[a.0].effective_inv_mass(ra, n),
            self.bodies[b.0].effective_inv_mass(rb, n),
        );
        let lambda = depth / (wa + wb);
        self.bodies[a.0].push(ra, n * lambda);
        self.bodies[b.0].push(rb, -n * lambda);
    }

    /// The child's anchor goes back onto the parent's.
    fn pull_anchor(&mut self, j: usize) {
        let Joint {
            parent,
            child,
            ap,
            ac,
            ..
        } = self.joints[j];
        let pa = self.bodies[parent].at(ap);
        let ca = self.bodies[child].at(ac);
        let d = ca - pa;
        let len = d.length();
        if len > EPS {
            // `separate` moves `a` along +n and `b` along -n: the child toward the parent.
            self.separate((parent, pa), (child, ca), d / len, len);
        }
    }

    /// The child's rotation from the bind pose is clamped to the joint's limits: each axis for a ball-and-socket, the
    /// hinge's own axis alone for a hinge.
    fn keep_in_limits(&mut self, j: usize) {
        let jt = &self.joints[j];
        let (p, c) = (&self.bodies[jt.parent], &self.bodies[jt.child]);
        let e = (jt.rest.inverse() * (p.q.inverse() * c.q)).normalize();
        let clamped = if jt.hinge {
            let a = Vec3::new(e.x, e.y, e.z).dot(jt.axis);
            let angle = wrap(2.0 * a.atan2(e.w));
            Quat::from_axis_angle(jt.axis, angle.clamp(jt.lo[2], jt.hi[2]))
        } else {
            let mut a = euler(e);
            for (k, v) in a.iter_mut().enumerate() {
                if jt.limited[k] {
                    *v = v.clamp(jt.lo[k], jt.hi[k]);
                }
            }
            Quat::from_rotation_z(a[2]) * Quat::from_rotation_y(a[1]) * Quat::from_rotation_x(a[0])
        };
        // The world rotation that takes the child from where it is to where the limits put it.
        let target = p.q * jt.rest * clamped;
        let mut d = target * c.q.inverse();
        if d.w < 0.0 {
            d = -d;
        }
        let rv = Vec3::new(d.x, d.y, d.z) * 2.0;
        if rv.length_squared() < 1e-10 {
            return;
        }
        let (wc, wp) = (c.inv_m, p.inv_m);
        let (fc, fp) = (wc / (wc + wp), wp / (wc + wp));
        let (parent, child) = (jt.parent, jt.child);
        self.bodies[child].turn(rv * fc);
        self.bodies[parent].turn(-rv * fp);
    }

    /// Limbs that must not pass through each other are held a scaled capsule apart.
    fn keep_apart(&mut self, [a, b]: [usize; 2]) {
        let (ba, bb) = (&self.bodies[a], &self.bodies[b]);
        let (pa, pb) = closest_on_segments(
            ba.at(ba.ends[0]),
            ba.at(ba.ends[1]),
            bb.at(bb.ends[0]),
            bb.at(bb.ends[1]),
        );
        let reach = (ba.radius + bb.radius) * SELF_SCALE;
        let d = pa - pb;
        let dist = d.length();
        if dist < reach && dist > EPS {
            self.separate((a, pa), (b, pb), d / dist, reach - dist);
        }
    }

    /// The capsules against the map: each probe sweeps a box of the radius from where it was at the substep's start to
    /// where it is, and is pushed out of what it hit; the grip on the surface takes back the sliding it did.
    fn touch_map(&mut self, world: &dyn Collide, dt: f32) {
        // What one hit of the whole body weighs: the sum of its bones' masses.
        let mass: f32 = self.bodies.iter().map(|b| 1.0 / b.inv_m).sum();
        for i in 0..self.bodies.len() {
            for k in 0..3 {
                let b = &self.bodies[i];
                let (from, to, r) = (b.at0(b.probes[k]), b.at(b.probes[k]), b.radius);
                let t = world.trace(
                    from.to_array(),
                    to.to_array(),
                    [-r; 3],
                    [r; 3],
                    sim::cm::ENTITYNUM_NONE,
                    sim::contents::SOLID,
                );
                if t.fraction >= 1.0 || t.start_solid || t.all_solid {
                    continue;
                }
                let n = Vec3::from(t.normal);
                let hit = from + (to - from) * t.fraction + n * SKIN;
                let depth = (hit - to).dot(n);
                let arm = to - b.x;
                let approach = -(to - from).dot(n) / dt;
                let body = &mut self.bodies[i];
                body.push(arm, n * (depth / body.effective_inv_mass(arm, n)));
                // Coulomb friction: the sliding is undone, up to what the push-out supports.
                let moved = body.at(body.probes[k]) - from;
                let slide = moved - n * moved.dot(n);
                let len = slide.length();
                if len > EPS {
                    let arm = body.at(body.probes[k]) - body.x;
                    let t = slide / len;
                    let grip = len.min(CONTACT_FRICTION * depth.max(0.0));
                    body.push(arm, -t * (grip / body.effective_inv_mass(arm, t)));
                }
                if approach > HIT_SPEED
                    && self.quiet <= 0.0
                    && approach * mass >= MIN_IMPACT_MOMENTUM
                    && self.impact.is_none_or(|h| h.momentum < approach * mass)
                {
                    self.impact = Some(Impact {
                        at: hit,
                        normal: n,
                        momentum: approach * mass,
                    });
                }
            }
        }
    }

    /// Joint friction: the torque the file gives the joint, scaled down over [`JOINT_LERP`], takes the relative spin
    /// of the two bodies away until it is spent.
    fn hold_joint(&mut self, j: usize, scale: f32, dt: f32) {
        let jt = &self.joints[j];
        let (p, c) = (jt.parent, jt.child);
        let rel = self.bodies[c].w - self.bodies[p].w;
        let mag = rel.length();
        if mag < EPS {
            return;
        }
        let (ip, ic) = (
            self.bodies[p].inertia_scalar(),
            self.bodies[c].inertia_scalar(),
        );
        let reduced = 1.0 / (1.0 / ip + 1.0 / ic);
        let spent = (jt.friction * scale * dt / reduced).min(mag);
        let dir = rel / mag;
        self.bodies[c].w -= dir * spent * (reduced / ic);
        self.bodies[p].w += dir * spent * (reduced / ip);
    }

    /// The corpse's origin: the torso's centre (`Ragdoll_GetRootOrigin`). A body thrown by a blast ends up far from
    /// where it died, so the model is drawn, culled and lit from where the body is now, with its bones relative to
    /// that point.
    pub fn root(&self) -> [f32; 3] {
        self.bodies[0].x.to_array()
    }

    /// The bones for a model drawn at [`Ragdoll::root`] and the yaw the body was made with.
    pub fn bones(&self) -> Vec<BoneMat> {
        let un = self.yaw.inverse();
        let root = self.bodies[0].x;
        (0..self.follow.len())
            .map(|i| {
                let f = self.follow[self.alias[i]
                    .filter(|a| *a < self.follow.len())
                    .unwrap_or(i)];
                let b = &self.bodies[f.body];
                let q = un * (b.q * f.q);
                let t = un * (b.x + b.q * f.p - root);
                BoneMat {
                    quat: q.to_array(),
                    trans: t.to_array(),
                }
            })
            .collect()
    }
}

/// `E = Rz(c) * Ry(b) * Rx(a)` as `[a, b, c]`: twist about the bone's own axis innermost.
fn euler(e: Quat) -> [f32; 3] {
    let m = Mat3::from_quat(e);
    [
        m.y_axis.z.atan2(m.z_axis.z),
        (-m.x_axis.z).clamp(-1.0, 1.0).asin(),
        m.x_axis.y.atan2(m.x_axis.x),
    ]
}

/// An angle in `[-pi, pi]`.
fn wrap(a: f32) -> f32 {
    let tau = std::f32::consts::TAU;
    a - tau * (a / tau).round()
}

/// The closest points of the segments `p1..q1` and `p2..q2`.
fn closest_on_segments(p1: Vec3, q1: Vec3, p2: Vec3, q2: Vec3) -> (Vec3, Vec3) {
    let (d1, d2, r) = (q1 - p1, q2 - p2, p1 - p2);
    let (a, e, f) = (d1.length_squared(), d2.length_squared(), d2.dot(r));
    let (s, t);
    if a <= EPS && e <= EPS {
        (s, t) = (0.0, 0.0);
    } else if a <= EPS {
        s = 0.0;
        t = (f / e).clamp(0.0, 1.0);
    } else {
        let c = d1.dot(r);
        if e <= EPS {
            t = 0.0;
            s = (-c / a).clamp(0.0, 1.0);
        } else {
            let b = d1.dot(d2);
            let denom = a * e - b * b;
            let mut s0 = if denom > EPS {
                ((b * f - c * e) / denom).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let mut t0 = (b * s0 + f) / e;
            if t0 < 0.0 {
                t0 = 0.0;
                s0 = (-c / a).clamp(0.0, 1.0);
            } else if t0 > 1.0 {
                t0 = 1.0;
                s0 = ((b - c) / a).clamp(0.0, 1.0);
            }
            (s, t) = (s0, t0);
        }
    }
    (p1 + d1 * s, p2 + d2 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim::cm::Trace;

    /// A flat floor at z = 0.
    struct Floor;

    impl Collide for Floor {
        fn trace(
            &self,
            start: [f32; 3],
            end: [f32; 3],
            mins: [f32; 3],
            _maxs: [f32; 3],
            _pass: u16,
            _mask: i32,
        ) -> Trace {
            let (a, b) = (start[2] + mins[2], end[2] + mins[2]);
            let mut t = Trace::MISS;
            if b < 0.0 && a >= 0.0 {
                t.fraction = a / (a - b);
                t.normal = [0.0, 0.0, 1.0];
            }
            t
        }

        fn point_contents(&self, _p: [f32; 3], _pass: u16, _mask: i32) -> i32 {
            0
        }
    }

    /// Nothing at all.
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
        ) -> Trace {
            Trace::MISS
        }

        fn point_contents(&self, _p: [f32; 3], _pass: u16, _mask: i32) -> i32 {
            0
        }
    }

    /// A small definition of the same shape as the stock one: a torso, a head, two arms and two legs, with swivel
    /// shoulders and hips and hinge elbows and knees, in this file's own numbers.
    const DEF: &str = "
        ragdoll_clear 0
        ragdoll_bone 0 j_mainroot j_neck 5.0 0.5 3.0 0.3 -1 0 capsule
        ragdoll_bone 0 j_neck j_head 3.5 0.5 0.3 0.3 0 0 capsule
        ragdoll_bone 0 j_shoulder_le j_elbow_le 2.5 0.5 0.6 0.3 0 0 capsule
        ragdoll_bone 0 j_elbow_le j_wrist_le 2.5 0.5 0.45 0.3 2 0 capsule
        ragdoll_bone 0 j_shoulder_ri j_elbow_ri 2.5 0.5 0.6 0.3 0 1 capsule
        ragdoll_bone 0 j_elbow_ri j_wrist_ri 2.5 0.5 0.45 0.3 4 1 capsule
        ragdoll_bone 0 j_hip_le j_knee_le 3.0 0.5 0.8 0.3 0 0 capsule
        ragdoll_bone 0 j_knee_le j_ankle_le 2.8 0.5 0.6 0.3 6 0 capsule
        ragdoll_bone 0 j_hip_ri j_knee_ri 3.0 0.5 0.8 0.3 0 1 capsule
        ragdoll_bone 0 j_knee_ri j_ankle_ri 2.8 0.5 0.6 0.3 8 1 capsule
        ragdoll_selfpair 0 0 3
        ragdoll_selfpair 0 0 5
        ragdoll_selfpair 0 3 7
        ragdoll_selfpair 0 7 9
        ragdoll_joint 0 1 swivel // neck
        ragdoll_joint 0 2 swivel // shoulder
        ragdoll_joint 0 3 hinge // elbow
        ragdoll_joint 0 4 swivel
        ragdoll_joint 0 5 hinge
        ragdoll_joint 0 6 swivel // hip
        ragdoll_joint 0 7 hinge // knee
        ragdoll_joint 0 8 swivel
        ragdoll_joint 0 9 hinge
        ragdoll_limit 0 0 x 60 -50 50
        ragdoll_limit 0 0 y 60 -45 45
        ragdoll_limit 0 0 z 60 -20 30
        ragdoll_limit 0 1 x 80 -90 90
        ragdoll_limit 0 1 y 100 -45 20
        ragdoll_limit 0 1 z 100 0 95
        ragdoll_limit 0 2 z 60 -120 -5
        ragdoll_limit 0 3 x 80 -90 90
        ragdoll_limit 0 3 y 100 -45 20
        ragdoll_limit 0 3 z 100 0 95
        ragdoll_limit 0 4 z 60 -120 -5
        ragdoll_limit 0 5 x 80 -30 30
        ragdoll_limit 0 5 y 100 -25 25
        ragdoll_limit 0 5 z 200 0 50
        ragdoll_limit 0 6 z 100 -140 2
        ragdoll_limit 0 7 x 80 -30 30
        ragdoll_limit 0 7 y 100 -25 25
        ragdoll_limit 0 7 z 200 0 50
        ragdoll_limit 0 8 z 100 -140 2
    ";

    /// Joint positions of a standing body, by bone: name, parent and place.
    const BONES: [(&str, Option<usize>, [f32; 3]); 15] = [
        ("j_mainroot", None, [0.0, 0.0, 40.0]),
        ("j_neck", Some(0), [0.0, 0.0, 58.0]),
        ("j_head", Some(1), [0.0, 0.0, 66.0]),
        ("j_shoulder_le", Some(0), [0.0, 8.0, 56.0]),
        ("j_elbow_le", Some(3), [0.0, 8.0, 42.0]),
        ("j_wrist_le", Some(4), [0.0, 8.0, 30.0]),
        ("j_shoulder_ri", Some(0), [0.0, -8.0, 56.0]),
        ("j_elbow_ri", Some(6), [0.0, -8.0, 42.0]),
        ("j_wrist_ri", Some(7), [0.0, -8.0, 30.0]),
        ("j_hip_le", Some(0), [0.0, 4.0, 38.0]),
        ("j_knee_le", Some(9), [0.0, 4.0, 20.0]),
        ("j_ankle_le", Some(10), [0.0, 4.0, 3.0]),
        ("j_hip_ri", Some(0), [0.0, -4.0, 38.0]),
        ("j_knee_ri", Some(12), [0.0, -4.0, 20.0]),
        ("j_ankle_ri", Some(13), [0.0, -4.0, 3.0]),
    ];

    fn skel() -> Skel {
        Skel {
            names: BONES.iter().map(|b| b.0.to_owned()).collect(),
            parent: BONES.iter().map(|b| b.1).collect(),
            alias: vec![None; BONES.len()],
        }
    }

    /// The bones of a standing body with `moved` joints replaced; each bone's x axis runs toward its child (away from
    /// it for the mirrored right side), as the skeleton's do.
    fn standing(moved: &[(usize, [f32; 3])]) -> Vec<BoneMat> {
        let mut at: Vec<Vec3> = BONES.iter().map(|b| Vec3::from(b.2)).collect();
        for (i, p) in moved {
            at[*i] = Vec3::from(*p);
        }
        let child = |i: usize| BONES.iter().position(|b| b.1 == Some(i) && at[i] != at[0]);
        (0..BONES.len())
            .map(|i| {
                // The child that continues the limb: the elbow of a shoulder, the neck of the root.
                let c = match i {
                    0 => Some(1),
                    1 => Some(2),
                    _ => child(i),
                };
                let right = [6, 7, 8, 12, 13, 14].contains(&i);
                let q = c.map_or(Quat::IDENTITY, |c| {
                    let d = (at[c] - at[i]).normalize();
                    Quat::from_rotation_arc(Vec3::X, if right { -d } else { d })
                });
                BoneMat {
                    quat: q.to_array(),
                    trans: at[i].to_array(),
                }
            })
            .collect()
    }

    fn make(pose: &[BoneMat], origin: [f32; 3], push: [f32; 3]) -> Ragdoll {
        let def = Def::parse(DEF);
        Ragdoll::new(&def, &skel(), pose, &standing(&[]), origin, 0.0, push).expect("a ragdoll")
    }

    impl Ragdoll {
        /// The most any joint is turned past its limits, in radians.
        fn violation(&self) -> f32 {
            self.joints
                .iter()
                .map(|j| {
                    let (p, c) = (&self.bodies[j.parent], &self.bodies[j.child]);
                    let e = (j.rest.inverse() * (p.q.inverse() * c.q)).normalize();
                    let fixed = if j.hinge {
                        let a = Vec3::new(e.x, e.y, e.z).dot(j.axis);
                        let angle = wrap(2.0 * a.atan2(e.w));
                        Quat::from_axis_angle(j.axis, angle.clamp(j.lo[2], j.hi[2]))
                    } else {
                        let mut a = euler(e);
                        for (k, v) in a.iter_mut().enumerate() {
                            if j.limited[k] {
                                *v = v.clamp(j.lo[k], j.hi[k]);
                            }
                        }
                        Quat::from_rotation_z(a[2])
                            * Quat::from_rotation_y(a[1])
                            * Quat::from_rotation_x(a[0])
                    };
                    e.angle_between(fixed)
                })
                .fold(0.0, f32::max)
        }

        /// How far apart the capsule axes of bodies `a` and `b` are.
        fn gap(&self, a: usize, b: usize) -> f32 {
            let (ba, bb) = (&self.bodies[a], &self.bodies[b]);
            let (pa, pb) = closest_on_segments(
                ba.at(ba.ends[0]),
                ba.at(ba.ends[1]),
                bb.at(bb.ends[0]),
                bb.at(bb.ends[1]),
            );
            pa.distance(pb)
        }

        fn torso(&self) -> Vec3 {
            self.bodies[0].x
        }
    }

    fn settle(r: &mut Ragdoll, world: &dyn Collide, frames: usize) {
        for _ in 0..frames {
            r.update(STEP, world);
        }
    }

    #[test]
    fn the_definition_reads_bones_joints_limits_and_pairs() {
        let d = Def::parse(DEF);
        assert_eq!((d.bones.len(), d.joints.len(), d.pairs.len()), (10, 9, 4));
        assert!(d.joints[2].hinge && !d.joints[1].hinge);
        // Three axes on a shoulder, one on an elbow; a negated axis flips its limits.
        assert_eq!((d.joints[1].limits.len(), d.joints[2].limits.len()), (3, 1));
        assert!((d.joints[2].limits[0].max + 5f32.to_radians()).abs() < 1e-5);
        let flipped = Def::parse(
            "ragdoll_bone 0 a b 1 0.5 1 0.3 -1 0 capsule
             ragdoll_bone 0 b c 1 0.5 1 0.3 0 0 capsule
             ragdoll_joint 0 1 swivel
             ragdoll_limit 0 0 -y 1 -10 40",
        );
        assert_eq!(flipped.joints[0].limits[0].axis, -Vec3::Y);
        // A bone naming a parent that does not exist yet, and a joint on the root, are left out.
        let bad = Def::parse(
            "ragdoll_bone 0 a b 1 0.5 1 0.3 3 0 capsule
             ragdoll_bone 0 a b 1 0.5 1 0.3 -1 0 capsule
             ragdoll_joint 0 0 hinge",
        );
        assert_eq!((bad.bones.len(), bad.joints.len()), (1, 0));
    }

    #[test]
    fn a_body_falls_and_comes_to_rest_on_the_thickness_of_its_bones() {
        let mut r = make(&standing(&[]), [0.0; 3], [100.0, 0.0, 0.0]);
        settle(&mut r, &Floor, 600);
        assert!(r.at_rest());
        let (lo, hi) = r.bounds();
        // A capsule's axis cannot get nearer the floor than its radius (the thinnest bone is 2.5).
        assert!(lo.z > 2.2, "{lo}");
        assert!(hi.z < 30.0, "lying, not standing: {hi}");
        assert!(
            r.torso().x > 5.0,
            "the push carried it along +x: {}",
            r.torso()
        );
        assert!(r.bodies.iter().all(|b| b.x.is_finite()));
    }

    #[test]
    fn joints_keep_to_their_limits_through_a_fall() {
        let mut r = make(&standing(&[]), [0.0, 0.0, 30.0], [300.0, 0.0, 0.0]);
        let mut worst = 0.0f32;
        for _ in 0..360 {
            r.update(STEP, &Floor);
            worst = worst.max(r.violation());
        }
        // The solver may be a few degrees out in the middle of a hard landing, never far.
        assert!(worst < 20f32.to_radians(), "{}°", worst.to_degrees());
        assert!(
            r.violation() < 4f32.to_radians(),
            "{}°",
            r.violation().to_degrees()
        );
    }

    #[test]
    fn a_pose_outside_the_limits_is_pulled_back_into_them() {
        // The left forearm folded forward at the elbow, the wrong way for the hinge.
        let pose = standing(&[(5, [14.0, 8.0, 44.0])]);
        let mut r = make(&pose, [0.0, 0.0, 100.0], [0.0; 3]);
        let before = r.violation();
        assert!(before > 30f32.to_radians(), "{}°", before.to_degrees());
        settle(&mut r, &Void, 40);
        assert!(
            r.violation() < 5f32.to_radians(),
            "{}°",
            r.violation().to_degrees()
        );
    }

    #[test]
    fn limbs_that_would_pass_through_the_torso_are_held_a_capsule_apart() {
        // The left forearm laid across the torso's axis.
        let pose = standing(&[(4, [0.0, 3.0, 50.0]), (5, [0.0, -4.0, 44.0])]);
        let mut r = make(&pose, [0.0, 0.0, 100.0], [0.0; 3]);
        let before = r.gap(0, 3);
        assert!(before < 1.0, "{before}");
        settle(&mut r, &Void, 90);
        assert!(r.gap(0, 3) > 5.0, "{}", r.gap(0, 3));
    }

    #[test]
    fn a_blast_wakes_a_resting_body_and_throws_it_away_from_the_centre() {
        let blast = |outer| Blast {
            outer,
            ..Blast::default()
        };
        let mut r = make(&standing(&[]), [0.0; 3], [0.0; 3]);
        settle(&mut r, &Floor, 600);
        assert!(r.at_rest());
        let rest = r.torso();
        // Out of reach: nothing happens.
        assert!(!r.explode(rest + Vec3::new(-400.0, 0.0, 0.0), &blast(256.0)));
        assert!(r.at_rest());
        // Within reach, behind it: it wakes and goes the other way and up.
        assert!(r.explode(rest + Vec3::new(-60.0, 0.0, 0.0), &blast(256.0)));
        assert!(!r.at_rest());
        settle(&mut r, &Floor, 30);
        let moved = r.torso() - rest;
        assert!(moved.x > 20.0 && moved.z > 5.0, "{moved}");
        settle(&mut r, &Floor, 600);
        assert!(r.at_rest(), "it settles again");
        assert!(
            r.violation() < 5f32.to_radians(),
            "{}°",
            r.violation().to_degrees()
        );
    }

    #[test]
    fn a_blast_throws_a_near_body_harder_than_a_far_one() {
        let throw = |at: f32| {
            let mut r = make(&standing(&[]), [0.0, 0.0, 0.0], [0.0; 3]);
            settle(&mut r, &Floor, 600);
            let rest = r.torso();
            r.explode(
                rest + Vec3::new(-at, 0.0, 0.0),
                &Blast {
                    outer: 256.0,
                    ..Blast::default()
                },
            );
            settle(&mut r, &Floor, 20);
            (r.torso() - rest).length()
        };
        assert!(throw(40.0) > throw(245.0) * 2.0);
    }

    #[test]
    fn a_body_that_is_not_touched_comes_back_in_the_pose_it_went_in() {
        let pose = standing(&[]);
        let r = make(&pose, [3.0, 4.0, 5.0], [0.0; 3]);
        let got = r.bones();
        // Only a lift clear of the floor separates it from the pose: the same offset everywhere.
        let root = Vec3::from(r.root());
        let at = |i: usize| root + Vec3::from(got[i].trans);
        let lift = at(0) - (Vec3::new(3.0, 4.0, 5.0) + Vec3::from(pose[0].trans));
        assert!(
            lift.x.abs() < 1e-3 && lift.y.abs() < 1e-3 && lift.z >= 0.0,
            "{lift}"
        );
        for (i, w) in pose.iter().enumerate() {
            let want = Vec3::new(3.0, 4.0, 5.0) + Vec3::from(w.trans) + lift;
            assert!(at(i).distance(want) < 1e-2);
            assert!(Quat::from_array(got[i].quat).angle_between(quat_of(w)) < 1e-3);
        }
    }

    #[test]
    fn a_body_thrown_far_keeps_its_bones_near_its_model_origin() {
        let mut r = make(&standing(&[]), [0.0; 3], [0.0; 3]);
        settle(&mut r, &Floor, 600);
        r.explode(
            r.torso() + Vec3::new(-40.0, 0.0, 0.0),
            &Blast {
                outer: 256.0,
                ..Blast::default()
            },
        );
        // Over the floor with nothing to stop it: it flies a long way.
        for _ in 0..90 {
            r.update(STEP, &Void);
        }
        assert!(r.torso().length() > 300.0, "thrown {}", r.torso());
        for b in r.bones() {
            assert!(Vec3::from(b.trans).length() < 100.0, "{:?}", b.trans);
        }
    }

    #[test]
    fn a_body_is_heard_landing_but_not_lying_still() {
        let mut r = make(&standing(&[]), [0.0, 0.0, 30.0], [0.0; 3]);
        let (mut early, mut late) = (Vec::new(), Vec::new());
        for k in 0..600 {
            r.update(STEP, &Floor);
            let hit = r.take_impact();
            if k < 120 {
                early.extend(hit)
            } else {
                late.extend(hit)
            }
        }
        assert!(!early.is_empty(), "it fell from 30 units onto the floor");
        assert!(
            early
                .iter()
                .all(|h| h.normal == Vec3::Z && h.momentum >= MIN_IMPACT_MOMENTUM)
        );
        assert!(late.is_empty(), "lying still makes no sound: {late:?}");
    }
}
