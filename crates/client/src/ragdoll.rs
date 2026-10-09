// SPDX-License-Identifier: GPL-3.0-only
// Collision sounds follow KisakCOD (GPL-3.0, KisakCOD contributors and Activision) `physics/phys_ode.cpp` (`Phys_PlayCollisionSound`).
//! A dead player's body as a Verlet ragdoll: every bone is a point, every bone-to-parent link a fixed-length
//! constraint, and the points fall and slide on the map's solid geometry. It is the client's own simulation (the
//! server's body stays the death animation's box); the bones it hands back replace the animated pose.
//!
//! A bone's matrix is rebuilt from its points: the rest rotation turned by the shortest arc from where the bone's first
//! child was (in the pose at death) to where it is now. Twist about the bone is not simulated.

use fx::{Impact, MIN_IMPACT_MOMENTUM};
use glam::{Quat, Vec3};
use sim::cm::Collide;
use sim::skel::BoneMat;

const STEP: f32 = 1.0 / 60.0;
const GRAVITY: f32 = 800.0;
/// Velocity kept each step.
const DAMPING: f32 = 0.995;
/// Tangential velocity kept in a step that touched something.
const FRICTION: f32 = 0.7;
const ITERATIONS: usize = 6;
/// The most a push may move a point per step (units per step).
const MAX_PUSH: f32 = 6.0;
/// Seconds a body keeps simulating, at most.
const LIFE: f32 = 8.0;
/// Below this movement per step, in units, the whole body is at rest.
const REST: f32 = 0.02;
/// Links shorter than this are welds: the two bones are one point.
const WELD: f32 = 0.01;

/// The collision sound of a body hitting the map. The original gives a ragdoll bone a preset with no sound prefix,
/// whose class is 0: the first class its stock presets register; this is the stock wood.
pub const SOUND: &str = "physics_wood";
/// What one hit of a body weighs, for `phys_minImpactMomentum` (the sum of the original's bone masses).
const BODY_MASS: f32 = 10.0;
/// Approach speeds under this are sliding and resting, not hits (the rigid bodies' bounce threshold).
const HIT_SPEED: f32 = 30.0;
/// Seconds between two hits a body is heard making.
const HIT_GAP: f32 = 0.25;

struct Link {
    a: usize,
    b: usize,
    len: f32,
    /// How much of the error one pass removes.
    stiffness: f32,
}

pub struct Ragdoll {
    pos: Vec<Vec3>,
    prev: Vec<Vec3>,
    links: Vec<Link>,
    /// The bone a bone copies the matrix of.
    alias: Vec<Option<usize>>,
    /// Each bone's first child, to read its direction from.
    child: Vec<Option<usize>>,
    parent: Vec<Option<usize>>,
    /// Rest rotation in world space and rest point of every bone.
    rest_rot: Vec<Quat>,
    rest_pos: Vec<Vec3>,
    origin: Vec3,
    yaw: Quat,
    age: f32,
    carry: f32,
    asleep: bool,
    /// The hardest unheard hit, and the time before another may be heard.
    impact: Option<Impact>,
    quiet: f32,
}

fn quat_of(b: &BoneMat) -> Quat {
    Quat::from_xyzw(b.quat[0], b.quat[1], b.quat[2], b.quat[3]).normalize()
}

impl Ragdoll {
    /// A body in the pose `bones` (entity space) for an entity at `origin` facing `yaw` degrees, thrown with velocity
    /// `push` (units per second). `parent(i)` and `alias(i)` are the rig's.
    pub fn new(
        bones: &[BoneMat],
        parent: impl Fn(usize) -> Option<usize>,
        alias: impl Fn(usize) -> Option<usize>,
        origin: [f32; 3],
        yaw_degrees: f32,
        push: [f32; 3],
    ) -> Self {
        let origin = Vec3::from(origin);
        let yaw = Quat::from_rotation_z(yaw_degrees.to_radians());
        let n = bones.len();
        let rest_pos: Vec<Vec3> = bones
            .iter()
            .map(|b| origin + yaw * Vec3::from(b.trans))
            .collect();
        let rest_rot: Vec<Quat> = bones.iter().map(|b| yaw * quat_of(b)).collect();
        let parents: Vec<Option<usize>> = (0..n).map(&parent).collect();
        let alias: Vec<Option<usize>> = (0..n).map(&alias).collect();
        let mut child = vec![None; n];
        let mut links = Vec::new();
        for i in 0..n {
            let Some(p) = parents[i].filter(|p| *p < n) else {
                continue;
            };
            let len = rest_pos[i].distance(rest_pos[p]);
            if len > WELD {
                child[p].get_or_insert(i);
            }
            links.push(Link {
                a: p,
                b: i,
                len,
                stiffness: 1.0,
            });
            // A bone may not fold back onto its grandparent: limbs bend, but only so far.
            if let Some(g) = parents[p].filter(|g| *g < n) {
                links.push(Link {
                    a: g,
                    b: i,
                    len: rest_pos[i].distance(rest_pos[g]),
                    stiffness: 0.3,
                });
            }
        }
        let v = Vec3::from(push).clamp_length_max(MAX_PUSH / STEP);
        Self {
            prev: rest_pos.iter().map(|p| *p - v * STEP).collect(),
            pos: rest_pos.clone(),
            links,
            alias,
            child,
            parent: parents,
            rest_rot,
            rest_pos,
            origin,
            yaw,
            age: 0.0,
            carry: 0.0,
            asleep: false,
            impact: None,
            quiet: 0.0,
        }
    }

    /// The hit to make a sound for, once.
    pub fn take_impact(&mut self) -> Option<Impact> {
        self.impact.take()
    }

    #[cfg(test)]
    /// True once the body has stopped (or been simulated for [`LIFE`] seconds).
    pub fn at_rest(&self) -> bool {
        self.asleep
    }

    #[cfg(test)]
    /// The lowest and highest point.
    pub fn bounds(&self) -> (Vec3, Vec3) {
        self.pos.iter().fold(
            (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
            |(lo, hi), p| (lo.min(*p), hi.max(*p)),
        )
    }

    /// Advances by `dt` seconds against `world`.
    pub fn update(&mut self, dt: f32, world: &dyn Collide) {
        if self.asleep {
            return;
        }
        self.carry += dt.min(0.1);
        while self.carry >= STEP {
            self.carry -= STEP;
            self.step(world);
            self.age += STEP;
            self.quiet -= STEP;
        }
    }

    fn step(&mut self, world: &dyn Collide) {
        let mut moved = 0.0f32;
        for i in 0..self.pos.len() {
            let v = (self.pos[i] - self.prev[i]) * DAMPING;
            let mut next = self.pos[i] + v + Vec3::new(0.0, 0.0, -GRAVITY * STEP * STEP);
            if let Some(a) = self.alias[i] {
                next = self.pos[a];
            }
            self.prev[i] = self.pos[i];
            self.pos[i] = next;
        }
        for _ in 0..ITERATIONS {
            for l in &self.links {
                let d = self.pos[l.b] - self.pos[l.a];
                let dist = d.length();
                if dist < 1e-4 {
                    continue;
                }
                let fix = d * ((dist - l.len) / dist * 0.5 * l.stiffness);
                self.pos[l.a] += fix;
                self.pos[l.b] -= fix;
            }
        }
        for i in 0..self.pos.len() {
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
                // What is left of the move slides along the surface.
                let rest = self.pos[i] - hit;
                let slide = rest - n * rest.dot(n);
                let v = self.pos[i] - from;
                let approach = -v.dot(n) / STEP;
                if approach > HIT_SPEED
                    && self.quiet <= 0.0
                    && approach * BODY_MASS >= MIN_IMPACT_MOMENTUM
                    && self
                        .impact
                        .is_none_or(|h| h.momentum < approach * BODY_MASS)
                {
                    self.impact = Some(Impact {
                        at: hit,
                        normal: n,
                        momentum: approach * BODY_MASS,
                    });
                }
                let vt = v - n * v.dot(n);
                self.pos[i] = hit + slide * FRICTION;
                self.prev[i] = self.pos[i] - vt * FRICTION;
            }
            moved = moved.max((self.pos[i] - self.prev[i]).length());
        }
        if self.impact.is_some() {
            self.quiet = HIT_GAP;
        }
        if moved < REST || self.age >= LIFE {
            self.asleep = true;
        }
    }

    /// The bones in entity space, for an entity drawn at the origin and yaw the body was made with.
    pub fn bones(&self) -> Vec<BoneMat> {
        let n = self.pos.len();
        let mut delta = vec![Quat::IDENTITY; n];
        // Parents come before children in a rig, so one pass inherits down the tree.
        for i in 0..n {
            let inherited = self.parent[i]
                .filter(|p| *p < i)
                .map_or(Quat::IDENTITY, |p| delta[p]);
            delta[i] = match self.child[i] {
                Some(c) => {
                    let rest = self.rest_pos[c] - self.rest_pos[i];
                    let now = self.pos[c] - self.pos[i];
                    if rest.length() > WELD && now.length() > WELD {
                        Quat::from_rotation_arc(rest.normalize(), now.normalize())
                    } else {
                        inherited
                    }
                }
                None => inherited,
            };
        }
        let un = self.yaw.inverse();
        (0..n)
            .map(|i| {
                let src = self.alias[i].unwrap_or(i);
                let q = un * delta[src] * self.rest_rot[src];
                let t = un * (self.pos[src] - self.origin);
                BoneMat {
                    quat: q.to_array(),
                    trans: t.to_array(),
                }
            })
            .collect()
    }
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

    fn id(q: Quat) -> [f32; 4] {
        q.to_array()
    }

    /// A pelvis with a spine and a head standing up, and a hip with a foot hanging off the pelvis.
    fn standing() -> Vec<BoneMat> {
        let at = |x: f32, z: f32| BoneMat {
            quat: id(Quat::IDENTITY),
            trans: [x, 0.0, z],
        };
        vec![
            at(0.0, 40.0),
            at(0.0, 55.0),
            at(0.0, 70.0),
            at(5.0, 20.0),
            at(5.0, 2.0),
        ]
    }

    fn chain(i: usize) -> Option<usize> {
        [None, Some(0), Some(1), Some(0), Some(3)]
            .get(i)
            .copied()
            .flatten()
    }

    #[test]
    fn a_body_falls_and_lies_on_the_floor_with_every_limb_the_length_it_was() {
        let mut r = Ragdoll::new(
            &standing(),
            chain,
            |_| None,
            [0.0; 3],
            0.0,
            [100.0, 0.0, 0.0],
        );
        for _ in 0..600 {
            r.update(STEP, &Floor);
        }
        assert!(r.at_rest());
        let (lo, hi) = r.bounds();
        assert!(lo.z > -0.5, "{lo}");
        // Lying down: no taller than a limb, which standing was 70 units.
        assert!(hi.z < 25.0, "{hi}");
        let b = r.bones();
        let d = |a: usize, c: usize| Vec3::from(b[a].trans).distance(Vec3::from(b[c].trans));
        assert!((d(1, 2) - 15.0).abs() < 1.5, "{}", d(1, 2));
        assert!((d(3, 4) - 18.0).abs() < 1.8, "{}", d(3, 4));
        // The push carried the body along +x.
        assert!(b[0].trans[0] > 5.0, "{:?}", b[0].trans);
    }

    #[test]
    fn a_body_in_free_fall_without_a_push_stays_over_where_it_died() {
        let mut r = Ragdoll::new(
            &standing(),
            chain,
            |_| None,
            [10.0, 20.0, 0.0],
            90.0,
            [0.0; 3],
        );
        for _ in 0..600 {
            r.update(STEP, &Floor);
        }
        let b = r.bones();
        // Entity space is turned by the yaw it was made with: the body is within reach of its origin.
        let p = Vec3::from(b[0].trans);
        assert!(p.truncate().length() < 60.0, "{p}");
    }

    #[test]
    fn an_untouched_pose_comes_back_as_it_went_in() {
        let r = Ragdoll::new(
            &standing(),
            chain,
            |_| None,
            [3.0, 4.0, 5.0],
            37.0,
            [0.0; 3],
        );
        for (got, want) in r.bones().iter().zip(standing()) {
            let (g, w) = (Vec3::from(got.trans), Vec3::from(want.trans));
            assert!(g.distance(w) < 1e-3, "{g} {w}");
            assert!(Quat::from_array(got.quat).angle_between(quat_of(&want)) < 1e-3);
        }
    }

    #[test]
    fn a_body_is_heard_landing_but_not_lying_still() {
        let mut r = Ragdoll::new(
            &standing(),
            chain,
            |_| None,
            [0.0, 0.0, 30.0],
            0.0,
            [0.0; 3],
        );
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
