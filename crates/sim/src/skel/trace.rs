// SPDX-License-Identifier: GPL-3.0-or-later
//! Locational hit detection against a posed [`Rig`] (`DObjTraceline`).
//!
//! The original does not intersect player models with triangles (the stock player models have
//! no `coll_surfs`); each bone carries an oriented box in `bone_info` (`bounds`, relative to the
//! bone) and a bounding sphere (`offset`, `radius_squared`), and a zero radius means "not a hit
//! volume". A segment is tested bone by bone against the box, the sphere being an early out, and
//! the bone's `part_classification` is the hit location.
//!
//! Which bone wins when a segment crosses several is decided by a priority map indexed by
//! classification ([`BULLET_PRIORITY`], [`RIFLE_PRIORITY`]): a higher priority beats a nearer
//! lower one, equal priorities go to the nearest entry, priority 0/1 is never a hit. A bone whose
//! classification has priority 1 (the stock "none" bones) takes the classification of its parent
//! (a melded duplicate takes its source bone's).
//!
//! A segment that starts and ends inside a box hits at fraction 0 only while heading toward the
//! entity's vertical axis in the horizontal plane (the original's start-solid rule); a segment
//! that starts inside and leaves is not a hit.

use super::hitloc::{HitLocation, PriorityMap};
use super::quat;
use super::rig::{MAX_BONES, Pose, Rig};
use crate::Vec3;
use crate::pm::math;

/// A segment's entry into a bone's hit volume.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocHit {
    /// Fraction of the segment, in `[0, max_fraction)`.
    pub fraction: f32,
    /// Rig bone index.
    pub bone: usize,
    /// Hit location index into [`HitLocation`] / the weapon's damage multipliers.
    pub hitloc: u8,
    /// Outward normal of the box face hit, in the space of the query.
    pub normal: Vec3,
}

impl LocHit {
    pub fn location(&self) -> HitLocation {
        HitLocation::from_index(self.hitloc).unwrap_or_default()
    }
}

/// Traces `start`..`end` (entity space, the space [`Rig::pose`] fills) against the pose.
/// Only a hit nearer than `max_fraction` counts; pass 1.0 for the whole segment, or the
/// fraction a coarser clip already found.
pub fn locational_trace(
    rig: &Rig,
    pose: &Pose,
    start: &Vec3,
    end: &Vec3,
    priority: &PriorityMap,
    max_fraction: f32,
) -> Option<LocHit> {
    let delta = math::sub(end, start);
    let len2 = math::length_sq(&delta);
    if len2 == 0.0 {
        return None;
    }
    let inv = 1.0 / len2;
    let prio_of = |cls: u8| priority.get(usize::from(cls)).copied().unwrap_or(0);
    let mut lowest = 2u8;
    let mut best = max_fraction;
    let mut hit: Option<(usize, u8, usize, f32)> = None;
    let mut class = [0u8; MAX_BONES];

    for g in 0..rig.len().min(pose.len) {
        let (model, local) = rig.bone_model(g);
        let mut cls = model.part_classification.get(local).copied().unwrap_or(0);
        let mut prio = prio_of(cls);
        if let Some(src) = rig.bone_duplicate_of(g) {
            if prio == 1 {
                cls = class[src];
                prio = prio_of(cls);
            }
        } else if prio == 1 {
            cls = rig.bone_parent(g).map_or(0, |p| class[p]);
            prio = prio_of(cls);
        }
        class[g] = cls;

        let Some(info) = model.bone_info.get(local) else {
            continue;
        };
        if info.radius_squared == 0.0 || lowest > prio {
            continue;
        }
        let m = &pose.bones[g];
        let r = quat::rotate(&m.quat, &info.offset);
        let center = [r[0] + m.trans[0], r[1] + m.trans[1], r[2] + m.trans[2]];
        let to_start = math::sub(start, &center);
        let sphere = -math::dot(&to_start, &delta) * inv;
        let capped = sphere.clamp(0.0, 1.0);
        let off = math::mad(&to_start, capped, &delta);
        let diff2 = info.radius_squared - math::length_sq(&off);
        if diff2 <= 0.0 {
            continue;
        }
        if lowest == prio && sphere - (diff2 * inv).sqrt() >= best {
            continue;
        }

        let ls = quat::rotate_inv(&m.quat, &math::sub(start, &m.trans));
        let le = quat::rotate_inv(&m.quat, &math::sub(end, &m.trans));
        let mut enter = 0.0f32;
        let mut leave = if lowest == prio { best } else { max_fraction };
        let (mut start_solid, mut end_solid) = (true, true);
        let (mut hit_axis, mut hit_sign) = (0usize, 0.0f32);
        let mut miss = false;
        'planes: for (sign, bounds) in [(-1.0f32, &info.bounds[0]), (1.0, &info.bounds[1])] {
            for t in 0..3 {
                let d1 = (ls[t] - bounds[t]) * sign;
                let d2 = (le[t] - bounds[t]) * sign;
                if d1 <= 0.0 {
                    if d2 > 0.0 {
                        end_solid = false;
                        let dist = d1 - d2;
                        if d1 > leave * dist {
                            leave = d1 / dist;
                            if leave <= enter {
                                miss = true;
                                break 'planes;
                            }
                        }
                    }
                } else {
                    if d2 > 0.0 {
                        miss = true;
                        break 'planes;
                    }
                    start_solid = false;
                    let dist = d1 - d2;
                    if d1 > enter * dist {
                        enter = d1 / dist;
                        if leave <= enter {
                            miss = true;
                            break 'planes;
                        }
                        hit_sign = sign;
                        hit_axis = t;
                    }
                }
            }
        }
        if miss {
            continue;
        }
        if start_solid {
            if end_solid && start[0] * delta[0] + start[1] * delta[1] < 0.0 {
                let mut n = [start[0], start[1], 0.0];
                math::normalize2(&mut n);
                return Some(LocHit {
                    fraction: 0.0,
                    bone: g,
                    hitloc: cls,
                    normal: n,
                });
            }
            continue;
        }
        if lowest == prio {
            if best <= enter {
                continue;
            }
        } else {
            lowest = prio;
        }
        best = enter;
        hit = Some((g, cls, hit_axis, hit_sign));
    }

    hit.map(|(g, cls, axis, sign)| {
        let a = quat::axes(&pose.bones[g].quat)[axis];
        LocHit {
            fraction: best,
            bone: g,
            hitloc: cls,
            normal: [a[0] * sign, a[1] * sign, a[2] * sign],
        }
    })
}

/// [`locational_trace`] for a world-space segment against an entity at `origin` with Euler
/// `angles` (the entity's `currentAngles`; players only have yaw): the segment is moved into
/// the entity frame, and the hit normal comes back in world space.
///
/// Use after a coarse clip against the player's box said the entity was hit, passing the
/// clip's fraction as `max_fraction`.
pub fn trace_player(
    rig: &Rig,
    pose: &Pose,
    origin: &Vec3,
    angles: &Vec3,
    start: &Vec3,
    end: &Vec3,
    priority: &PriorityMap,
    max_fraction: f32,
) -> Option<LocHit> {
    let (fwd, right, up) = math::angle_vectors(angles);
    let left = [-right[0], -right[1], -right[2]];
    let to_local = |p: &Vec3| {
        let d = math::sub(p, origin);
        [math::dot(&d, &fwd), math::dot(&d, &left), math::dot(&d, &up)]
    };
    let mut hit = locational_trace(rig, pose, &to_local(start), &to_local(end), priority, max_fraction)?;
    let n = hit.normal;
    hit.normal = [
        n[0] * fwd[0] + n[1] * left[0] + n[2] * up[0],
        n[0] * fwd[1] + n[1] * left[1] + n[2] * up[1],
        n[0] * fwd[2] + n[1] * left[2] + n[2] * up[2],
    ];
    Some(hit)
}
