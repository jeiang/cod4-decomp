// SPDX-License-Identifier: GPL-3.0-or-later
//! The static map plus the entities linked into it: entity clipping (`SV_Trace`,
//! `SV_PointContents`, `SV_SightTrace`, `SV_AreaEntities`) over a preallocated sector tree.
//!
//! Entities are identified by their entity number (`u16`); numbers at or above
//! [`cm::ENTITYNUM_WORLD`] never exist and every call taking one tolerates it.

use std::sync::Arc;

use assets::zone::clipmap::Clipmap;

use crate::Vec3;
use crate::cm::{self, ClipModel, Collide, CollisionWorld, ENTITYNUM_NONE, ENTITYNUM_WORLD, Trace};

/// Entity slots (`MAX_GENTITIES`); valid numbers are `0..ENTITYNUM_WORLD`.
pub const MAX_ENTS: usize = 1024;

const SECTORS: usize = 1024;
const HEAD: u16 = 1;
/// Sectors no larger than this (in the wider horizontal axis) are not split further.
const MIN_SECTOR_SIZE: f32 = 512.0;

/// What the collision code needs to know about an entity. The server owns the game entity and
/// copies these fields in whenever they change ([`World::link`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipEnt {
    /// `r.contents`: what the entity is made of. Zero means it blocks nothing.
    pub contents: i32,
    /// `r.currentOrigin`.
    pub origin: Vec3,
    /// `r.currentAngles`; only brush models rotate.
    pub angles: Vec3,
    /// `r.mins`/`r.maxs`: the hull of an entity without a brush model (clipped as a capsule).
    pub mins: Vec3,
    pub maxs: Vec3,
    /// `r.bmodel` with `s.index.brushmodel`: the inline model `*N` this entity is.
    pub brush_model: Option<u16>,
    /// `r.ownerNum`, or [`ENTITYNUM_NONE`].
    pub owner: u16,
}

impl ClipEnt {
    pub const EMPTY: Self = Self {
        contents: 0,
        origin: [0.0; 3],
        angles: [0.0; 3],
        mins: [0.0; 3],
        maxs: [0.0; 3],
        brush_model: None,
        owner: ENTITYNUM_NONE,
    };

    fn model(&self) -> ClipModel {
        match self.brush_model {
            Some(n) => ClipModel::Submodel(n),
            None => ClipModel::Box {
                mins: self.mins,
                maxs: self.maxs,
                contents: self.contents,
            },
        }
    }

    /// Angles the entity's model is rotated by (boxes never rotate).
    fn clip_angles(&self) -> Vec3 {
        if self.brush_model.is_some() {
            self.angles
        } else {
            [0.0; 3]
        }
    }
}

/// An entity as the world last saw it, with the absolute bounds it was linked under.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinkedEnt {
    pub ent: ClipEnt,
    pub abs_min: Vec3,
    pub abs_max: Vec3,
}

#[derive(Clone, Copy)]
struct Slot {
    data: Option<LinkedEnt>,
    /// Model contents the entity was linked with (what a trace mask is matched against before
    /// the entity is looked at).
    link_contents: i32,
    /// Sector holding the entity, or 0.
    sector: u16,
    /// Next entity (number + 1) in the same sector, or 0.
    next: u16,
}

#[derive(Clone, Copy, Default)]
struct Sector {
    /// OR of `contents` of every entity in this sector and below.
    contents: i32,
    link_contents: i32,
    /// First entity (number + 1), or 0.
    first: u16,
    dist: f32,
    axis: u8,
    /// Parent while in use, next free sector while on the free list.
    link: u16,
    child: [u16; 2],
}

pub struct World {
    cm: CollisionWorld,
    ents: Box<[Slot]>,
    sectors: Box<[Sector]>,
    free_head: u16,
    mins: [f32; 2],
    maxs: [f32; 2],
}

/// A trace the entity walk is carrying out.
struct Clip {
    tw: cm::Tw,
    start: Vec3,
    end: Vec3,
    mins: Vec3,
    maxs: Vec3,
    mask: i32,
    pass: u16,
    pass_owner: u16,
    /// Half extent of the hull plus one: how far from a sector plane an entity can still be hit.
    outer: Vec3,
    is_point: bool,
}

impl World {
    pub fn new(clipmap: Arc<Clipmap>) -> Self {
        let bounds = clipmap
            .cmodels
            .first()
            .map_or(([0.0; 3], [0.0; 3]), |m| (m.mins, m.maxs));
        let cm = CollisionWorld::new(clipmap);
        let mut w = Self {
            cm,
            ents: vec![
                Slot {
                    data: None,
                    link_contents: 0,
                    sector: 0,
                    next: 0
                };
                MAX_ENTS
            ]
            .into_boxed_slice(),
            sectors: vec![Sector::default(); SECTORS].into_boxed_slice(),
            free_head: 2,
            mins: [bounds.0[0], bounds.0[1]],
            maxs: [bounds.1[0], bounds.1[1]],
        };
        for i in 2..SECTORS - 1 {
            w.sectors[i].link = (i + 1) as u16;
        }
        let size = [w.maxs[0] - w.mins[0], w.maxs[1] - w.mins[1]];
        let axis = usize::from(size[1] >= size[0]);
        w.sectors[usize::from(HEAD)].axis = axis as u8;
        w.sectors[usize::from(HEAD)].dist = (w.maxs[axis] + w.mins[axis]) * 0.5;
        w
    }

    /// The static map: brush model bounds/contents, leaf and cluster queries.
    pub fn collision(&self) -> &CollisionWorld {
        &self.cm
    }

    /// The entity last passed to [`link`](Self::link) for `num`, linked or not.
    pub fn entity(&self, num: u16) -> Option<&LinkedEnt> {
        self.ents.get(usize::from(num))?.data.as_ref()
    }

    fn slot_ok(num: u16) -> bool {
        num < ENTITYNUM_WORLD
    }

    /// Stores `ent` under `num` and files it into the sector tree (`SV_LinkEntity`). An entity
    /// without contents, or whose brush model is empty, is unlinked instead. Call it after any
    /// change to the entity's origin, angles, bounds, model, contents or owner.
    pub fn link(&mut self, num: u16, ent: &ClipEnt) {
        if !Self::slot_ok(num) {
            return;
        }
        let mut e = *ent;
        for a in &mut e.angles {
            let r = a.round();
            if (r - *a) * (r - *a) < 0.000001 {
                *a = r;
            }
        }
        let (abs_min, abs_max) = abs_bounds(&e);
        self.ents[usize::from(num)].data = Some(LinkedEnt {
            ent: e,
            abs_min,
            abs_max,
        });
        let link_contents = match e.brush_model {
            Some(n) => self.cm.model_contents(n),
            None => -1,
        };
        if e.contents == 0 || link_contents == 0 {
            self.unlink(num);
            return;
        }
        self.link_into_tree(num, e.contents, link_contents, abs_min, abs_max);
    }

    /// Takes the entity out of the sector tree; its data stays readable through
    /// [`entity`](Self::entity).
    pub fn unlink(&mut self, num: u16) {
        if !Self::slot_ok(num) {
            return;
        }
        let mut idx = self.ents[usize::from(num)].sector;
        if idx == 0 {
            return;
        }
        self.ents[usize::from(num)].sector = 0;
        let after = self.ents[usize::from(num)].next;
        // Remove from the sector's list.
        let target = num + 1;
        if self.sectors[usize::from(idx)].first == target {
            self.sectors[usize::from(idx)].first = after;
        } else {
            let mut scan = self.sectors[usize::from(idx)].first;
            while scan != 0 {
                let s = &mut self.ents[usize::from(scan - 1)];
                if s.next == target {
                    s.next = after;
                    break;
                }
                scan = s.next;
            }
        }
        self.ents[usize::from(num)].next = 0;
        // Free sectors that became empty leaves.
        loop {
            let s = self.sectors[usize::from(idx)];
            if s.first != 0 || s.child != [0, 0] {
                break;
            }
            self.sectors[usize::from(idx)].contents = 0;
            self.sectors[usize::from(idx)].link_contents = 0;
            if s.link == 0 {
                break;
            }
            let parent = s.link;
            self.sectors[usize::from(idx)].link = self.free_head;
            self.free_head = idx;
            let p = &mut self.sectors[usize::from(parent)];
            if p.child[0] == idx {
                p.child[0] = 0;
            } else {
                p.child[1] = 0;
            }
            idx = parent;
        }
        // Recompute the contents from here to the root.
        loop {
            let s = self.sectors[usize::from(idx)];
            let mut contents = self.sectors[usize::from(s.child[0])].contents
                | self.sectors[usize::from(s.child[1])].contents;
            let mut link = self.sectors[usize::from(s.child[0])].link_contents
                | self.sectors[usize::from(s.child[1])].link_contents;
            let mut e = s.first;
            while e != 0 {
                let slot = &self.ents[usize::from(e - 1)];
                contents |= slot.data.map_or(0, |d| d.ent.contents);
                link |= slot.link_contents;
                e = slot.next;
            }
            let sec = &mut self.sectors[usize::from(idx)];
            sec.contents = contents;
            sec.link_contents = link;
            if sec.link == 0 {
                break;
            }
            idx = sec.link;
        }
    }

    fn link_into_tree(
        &mut self,
        num: u16,
        contents: i32,
        link_contents: i32,
        abs_min: Vec3,
        abs_max: Vec3,
    ) {
        let (probe, _, _) = self.descend(abs_min, abs_max, 0, 0);
        let slot = self.ents[usize::from(num)];
        let (node, lo, hi);
        if slot.sector == probe && slot.link_contents & !link_contents == 0 {
            // Same sector and no new contents: only refresh the path's contents.
            (node, lo, hi) = self.descend(abs_min, abs_max, contents, link_contents);
            self.ents[usize::from(num)].link_contents = link_contents;
        } else {
            self.unlink(num);
            (node, lo, hi) = self.descend(abs_min, abs_max, contents, link_contents);
            self.add_to_sector(num, node);
            self.ents[usize::from(num)].link_contents = link_contents;
        }
        self.sort_sector(node, lo, hi);
    }

    /// Finds the deepest sector `abs_min..abs_max` fits in, OR-ing the contents into the path
    /// (a zero `contents` leaves the tree untouched). Returns the sector and the bounds of the
    /// region the descent ended in.
    fn descend(
        &mut self,
        abs_min: Vec3,
        abs_max: Vec3,
        contents: i32,
        link: i32,
    ) -> (u16, [f32; 2], [f32; 2]) {
        let mut lo = self.mins;
        let mut hi = self.maxs;
        let mut idx = HEAD;
        loop {
            let s = &mut self.sectors[usize::from(idx)];
            s.contents |= contents;
            s.link_contents |= link;
            let axis = usize::from(s.axis);
            if s.dist >= abs_min[axis] {
                if s.dist <= abs_max[axis] {
                    return (idx, lo, hi);
                }
                hi[axis] = s.dist;
                if s.child[1] == 0 {
                    return (idx, lo, hi);
                }
                idx = s.child[1];
            } else {
                lo[axis] = s.dist;
                if s.child[0] == 0 {
                    return (idx, lo, hi);
                }
                idx = s.child[0];
            }
        }
    }

    /// Inserts into the sector's list keeping entity numbers ascending.
    fn add_to_sector(&mut self, num: u16, sector: u16) {
        self.ents[usize::from(num)].sector = sector;
        let first = self.sectors[usize::from(sector)].first;
        if first == 0 || first > num + 1 {
            self.ents[usize::from(num)].next = first;
            self.sectors[usize::from(sector)].first = num + 1;
            return;
        }
        let mut prev = first - 1;
        loop {
            let next = self.ents[usize::from(prev)].next;
            if next == 0 || next > num + 1 {
                self.ents[usize::from(num)].next = next;
                self.ents[usize::from(prev)].next = num + 1;
                return;
            }
            prev = next - 1;
        }
    }

    /// Moves entities that fit entirely into one half of the sector down into its children,
    /// creating them while the region is still large enough to split.
    fn sort_sector(&mut self, idx: u16, lo: [f32; 2], hi: [f32; 2]) {
        let axis = usize::from(self.sectors[usize::from(idx)].axis);
        let dist = self.sectors[usize::from(idx)].dist;
        let mut prev: Option<u16> = None;
        let mut cur = self.sectors[usize::from(idx)].first;
        while cur != 0 {
            let num = cur - 1;
            let slot = self.ents[usize::from(num)];
            let d = slot.data.expect("a listed entity has data");
            let side = if dist >= d.abs_min[axis] {
                if dist > d.abs_max[axis] {
                    Some(1)
                } else {
                    None
                }
            } else {
                Some(0)
            };
            let mut child = side.map_or(0, |s| self.sectors[usize::from(idx)].child[s]);
            if let Some(s) = side
                && child == 0
            {
                child = self.alloc_sector(lo, hi);
                if child != 0 {
                    self.sectors[usize::from(idx)].child[s] = child;
                    self.sectors[usize::from(child)].link = idx;
                }
            }
            if child == 0 {
                prev = Some(num);
            } else {
                self.add_to_sector(num, child);
                let c = &mut self.sectors[usize::from(child)];
                c.contents |= d.ent.contents;
                c.link_contents |= slot.link_contents;
                match prev {
                    Some(p) => self.ents[usize::from(p)].next = slot.next,
                    None => self.sectors[usize::from(idx)].first = slot.next,
                }
            }
            cur = slot.next;
        }
    }

    fn alloc_sector(&mut self, lo: [f32; 2], hi: [f32; 2]) -> u16 {
        let idx = self.free_head;
        if idx == 0 {
            return 0;
        }
        let size = [hi[0] - lo[0], hi[1] - lo[1]];
        let axis = usize::from(size[1] >= size[0]);
        if size[axis] <= MIN_SECTOR_SIZE {
            return 0;
        }
        self.free_head = self.sectors[usize::from(idx)].link;
        self.sectors[usize::from(idx)] = Sector {
            axis: axis as u8,
            dist: (hi[axis] + lo[axis]) * 0.5,
            ..Sector::default()
        };
        idx
    }

    /// Calls `visit` with every linked entity whose absolute bounds meet `mins..maxs` and whose
    /// contents match `mask` (`CM_AreaEntities`). Stops when `visit` returns `false`.
    pub fn area_entities(
        &self,
        mins: Vec3,
        maxs: Vec3,
        mask: i32,
        mut visit: impl FnMut(u16) -> bool,
    ) {
        self.area_r(HEAD, mins, maxs, mask, &mut visit);
    }

    fn area_r(
        &self,
        mut idx: u16,
        mins: Vec3,
        maxs: Vec3,
        mask: i32,
        visit: &mut impl FnMut(u16) -> bool,
    ) -> bool {
        loop {
            let s = &self.sectors[usize::from(idx)];
            if s.contents & mask == 0 {
                return true;
            }
            let mut e = s.first;
            while e != 0 {
                let slot = &self.ents[usize::from(e - 1)];
                if let Some(d) = &slot.data
                    && d.ent.contents & mask != 0
                    && (0..3).all(|i| maxs[i] >= d.abs_min[i] && mins[i] <= d.abs_max[i])
                    && !visit(e - 1)
                {
                    return false;
                }
                e = slot.next;
            }
            let axis = usize::from(s.axis);
            if s.dist >= maxs[axis] {
                if s.dist <= mins[axis] {
                    return true;
                }
                idx = s.child[1];
            } else if s.dist <= mins[axis] {
                idx = s.child[0];
            } else {
                if !self.area_r(s.child[0], mins, maxs, mask, visit) {
                    return false;
                }
                idx = s.child[1];
            }
        }
    }

    fn pass_owner(&self, pass: u16) -> u16 {
        self.entity(pass).map_or(ENTITYNUM_NONE, |e| e.ent.owner)
    }

    fn clip(&self, start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3, pass: u16, mask: i32) -> Clip {
        let half = [
            (maxs[0] - mins[0]) * 0.5,
            (maxs[1] - mins[1]) * 0.5,
            (maxs[2] - mins[2]) * 0.5,
        ];
        let is_point = maxs[0] - mins[0] + maxs[1] - mins[1] + maxs[2] - mins[2] == 0.0;
        Clip {
            tw: cm::Tw::new(start, end, mins, maxs, mask),
            start,
            end,
            mins,
            maxs,
            mask,
            pass,
            pass_owner: self.pass_owner(pass),
            outer: if is_point {
                [0.0; 3]
            } else {
                [half[0] + 1.0, half[1] + 1.0, half[2] + 1.0]
            },
            is_point,
        }
    }

    /// Whether `touch` (an entity number) is the pass entity, owned by it, or a sibling.
    fn ignored(clip: &Clip, num: u16, owner: u16) -> bool {
        clip.pass != ENTITYNUM_NONE
            && (num == clip.pass
                || (owner != ENTITYNUM_NONE && (owner == clip.pass || owner == clip.pass_owner)))
    }

    fn clip_to_entity(&self, clip: &Clip, num: u16, trace: &mut Trace) {
        let slot = &self.ents[usize::from(num)];
        let Some(d) = &slot.data else { return };
        if d.ent.contents & clip.mask == 0 || Self::ignored(clip, num, d.ent.owner) {
            return;
        }
        let half_lo = [
            -(clip.maxs[0] - clip.mins[0]) * 0.5,
            -(clip.maxs[1] - clip.mins[1]) * 0.5,
            -(clip.maxs[2] - clip.mins[2]) * 0.5,
        ];
        let half_hi = [-half_lo[0], -half_lo[1], -half_lo[2]];
        let lo = [
            d.abs_min[0] + half_lo[0],
            d.abs_min[1] + half_lo[1],
            d.abs_min[2] + half_lo[2],
        ];
        let hi = [
            d.abs_max[0] + half_hi[0],
            d.abs_max[1] + half_hi[1],
            d.abs_max[2] + half_hi[2],
        ];
        if clip.tw.misses_box(lo, hi, trace.fraction) {
            return;
        }
        let old = trace.fraction;
        self.cm.transformed_trace(
            trace,
            clip.start,
            clip.end,
            clip.mins,
            clip.maxs,
            &d.ent.model(),
            clip.mask,
            d.ent.origin,
            d.ent.clip_angles(),
        );
        if old > trace.fraction {
            trace.hit_id = num;
            if clip.is_point {
                trace.contents = d.ent.contents;
                trace.material = u32::MAX;
            }
        }
    }

    fn clip_move_r(
        &self,
        clip: &Clip,
        mut idx: u16,
        mut p: [f32; 4],
        p2: [f32; 4],
        trace: &mut Trace,
    ) {
        loop {
            let s = &self.sectors[usize::from(idx)];
            if clip.mask & s.contents == 0 || clip.mask & s.link_contents == 0 {
                return;
            }
            let mut e = s.first;
            while e != 0 {
                let slot = &self.ents[usize::from(e - 1)];
                if slot.link_contents & clip.mask != 0 {
                    self.clip_move_r_one(clip, e - 1, trace);
                }
                e = slot.next;
            }
            let axis = usize::from(s.axis);
            let t1 = p[axis] - s.dist;
            let t2 = p2[axis] - s.dist;
            let offset = clip.outer[axis];
            let (tmin, tmax) = (t1.min(t2), t1.max(t2));
            if offset <= tmin {
                idx = s.child[0];
            } else if tmax <= -offset {
                idx = s.child[1];
            } else {
                if p[3] >= trace.fraction {
                    return;
                }
                let diff = t2 - t1;
                let (side, near, far) = if diff == 0.0 {
                    (0, 1.0f32, 0.0f32)
                } else {
                    let v = if diff < 0.0 { t1 } else { -t1 };
                    let inv = 1.0 / diff.abs();
                    (
                        usize::from(diff >= 0.0),
                        (v + offset) * inv,
                        (v - offset) * inv,
                    )
                };
                let lerp = |t: f32| {
                    [
                        (p2[0] - p[0]) * t + p[0],
                        (p2[1] - p[1]) * t + p[1],
                        (p2[2] - p[2]) * t + p[2],
                        (p2[3] - p[3]) * t + p[3],
                    ]
                };
                let mid = lerp(near.min(1.0));
                self.clip_move_r(clip, s.child[side], p, mid, trace);
                p = lerp(far.max(0.0));
                idx = s.child[1 - side];
            }
        }
    }

    fn clip_move_r_one(&self, clip: &Clip, num: u16, trace: &mut Trace) {
        self.clip_to_entity(clip, num, trace);
    }

    fn clip_to_entities(&self, clip: &Clip, trace: &mut Trace) {
        let p = [clip.tw.start[0], clip.tw.start[1], clip.tw.start[2], 0.0];
        let q = [
            clip.tw.end[0],
            clip.tw.end[1],
            clip.tw.end[2],
            trace.fraction,
        ];
        self.clip_move_r(clip, HEAD, p, q, trace);
    }

    /// Whether anything stands in the way of a sight line (`SV_TracePassed`): the map first,
    /// then entities, ignoring `pass0` and `pass1` and what they own.
    #[allow(clippy::too_many_arguments)]
    pub fn trace_passed(
        &self,
        start: Vec3,
        end: Vec3,
        mins: Vec3,
        maxs: Vec3,
        pass0: u16,
        pass1: u16,
        mask: i32,
    ) -> bool {
        self.sight_trace(0, start, end, mins, maxs, pass0, pass1, mask) == 0
    }

    /// `SV_SightTrace`: nonzero when the line is blocked. Pass the previous result back as
    /// `old_hit` for the same viewer to try the blocking brush first.
    #[allow(clippy::too_many_arguments)]
    pub fn sight_trace(
        &self,
        old_hit: i32,
        start: Vec3,
        end: Vec3,
        mins: Vec3,
        maxs: Vec3,
        pass0: u16,
        pass1: u16,
        mask: i32,
    ) -> i32 {
        let hit = self
            .cm
            .sight_trace(old_hit, start, end, mins, maxs, &ClipModel::World, mask);
        if hit != 0 {
            return hit;
        }
        let clip = self.clip(start, end, mins, maxs, pass0, mask);
        let mut blocked = 0;
        self.sight_r(&clip, pass1, HEAD, clip.tw.start, clip.tw.end, &mut blocked);
        blocked
    }

    fn sight_r(
        &self,
        clip: &Clip,
        pass1: u16,
        mut idx: u16,
        mut p: Vec3,
        p2: Vec3,
        blocked: &mut i32,
    ) {
        loop {
            if *blocked != 0 {
                return;
            }
            let s = &self.sectors[usize::from(idx)];
            if clip.mask & s.contents == 0 || clip.mask & s.link_contents == 0 {
                return;
            }
            let mut e = s.first;
            while e != 0 {
                let slot = &self.ents[usize::from(e - 1)];
                if let Some(d) = &slot.data
                    && d.ent.contents & clip.mask != 0
                    && !sight_ignored(clip.pass, pass1, e - 1, d.ent.owner)
                    && self.cm.transformed_sight_trace(
                        0,
                        clip.start,
                        clip.end,
                        clip.mins,
                        clip.maxs,
                        &d.ent.model(),
                        clip.mask,
                        d.ent.origin,
                        d.ent.clip_angles(),
                    ) != 0
                {
                    *blocked = -1;
                    return;
                }
                e = slot.next;
            }
            let axis = usize::from(s.axis);
            let t1 = p[axis] - s.dist;
            let t2 = p2[axis] - s.dist;
            let offset = clip.outer[axis];
            let (tmin, tmax) = (t1.min(t2), t1.max(t2));
            if offset <= tmin {
                idx = s.child[0];
            } else if tmax <= -offset {
                idx = s.child[1];
            } else {
                let diff = t2 - t1;
                let (side, near, far) = if diff == 0.0 {
                    (0, 1.0f32, 0.0f32)
                } else {
                    let v = if diff < 0.0 { t1 } else { -t1 };
                    let inv = 1.0 / diff.abs();
                    (
                        usize::from(diff >= 0.0),
                        (v + offset) * inv,
                        (v - offset) * inv,
                    )
                };
                let mid = [
                    (p2[0] - p[0]) * near.min(1.0) + p[0],
                    (p2[1] - p[1]) * near.min(1.0) + p[1],
                    (p2[2] - p[2]) * near.min(1.0) + p[2],
                ];
                self.sight_r(clip, pass1, s.child[side], p, mid, blocked);
                let f = far.max(0.0);
                p = [
                    (p2[0] - p[0]) * f + p[0],
                    (p2[1] - p[1]) * f + p[1],
                    (p2[2] - p[2]) * f + p[2],
                ];
                idx = s.child[1 - side];
            }
        }
    }

    /// `G_TestEntityPosition`: the entity hull at its own origin overlaps something solid.
    /// Returns what it is stuck in (the hit entity number, or [`ENTITYNUM_WORLD`]).
    pub fn test_entity_position(&self, num: u16, mask: i32) -> Option<u16> {
        let e = self.entity(num)?.ent;
        self.box_in_solid(e.origin, e.mins, e.maxs, num, mask)
    }

    /// Whether the hull `mins..maxs` standing at `origin` starts inside something matching
    /// `mask` (other than `pass` and what it owns).
    pub fn box_in_solid(
        &self,
        origin: Vec3,
        mins: Vec3,
        maxs: Vec3,
        pass: u16,
        mask: i32,
    ) -> Option<u16> {
        let t = self.trace(origin, origin, mins, maxs, pass, mask);
        (t.start_solid || t.all_solid).then_some(t.hit_id)
    }
}

impl Collide for World {
    fn trace(
        &self,
        start: Vec3,
        end: Vec3,
        mins: Vec3,
        maxs: Vec3,
        pass_ent: u16,
        mask: i32,
    ) -> Trace {
        let mut trace = self
            .cm
            .box_trace(start, end, mins, maxs, &ClipModel::World, mask);
        trace.hit_id = if trace.fraction == 1.0 {
            ENTITYNUM_NONE
        } else {
            ENTITYNUM_WORLD
        };
        if trace.fraction == 0.0 {
            return trace;
        }
        let clip = self.clip(start, end, mins, maxs, pass_ent, mask);
        self.clip_to_entities(&clip, &mut trace);
        trace
    }

    fn point_contents(&self, p: Vec3, pass_ent: u16, mask: i32) -> i32 {
        let mut contents = self.cm.point_contents(p, &ClipModel::World);
        self.area_entities(p, p, mask, |num| {
            if num != pass_ent
                && let Some(d) = self.entity(num)
            {
                contents |= self.cm.transformed_point_contents(
                    p,
                    &d.ent.model(),
                    d.ent.origin,
                    d.ent.angles,
                );
            }
            true
        });
        contents & mask
    }
}

/// Sight traces ignore two entities, each with what it owns (but not its siblings).
fn sight_ignored(pass0: u16, pass1: u16, num: u16, owner: u16) -> bool {
    [pass0, pass1]
        .into_iter()
        .any(|p| p != ENTITYNUM_NONE && (num == p || (owner != ENTITYNUM_NONE && owner == p)))
}

/// Absolute bounds of an entity as `SV_LinkEntity` computes them, one unit larger on every side.
fn abs_bounds(e: &ClipEnt) -> (Vec3, Vec3) {
    let (mut lo, mut hi);
    if e.brush_model.is_none() || e.angles == [0.0; 3] {
        lo = [
            e.origin[0] + e.mins[0],
            e.origin[1] + e.mins[1],
            e.origin[2] + e.mins[2],
        ];
        hi = [
            e.origin[0] + e.maxs[0],
            e.origin[1] + e.maxs[1],
            e.origin[2] + e.maxs[2],
        ];
    } else if e.angles[0] == 0.0 && e.angles[2] == 0.0 {
        let r = radius(&e.mins, &e.maxs, 2);
        lo = [e.origin[0] - r, e.origin[1] - r, e.origin[2] + e.mins[2]];
        hi = [e.origin[0] + r, e.origin[1] + r, e.origin[2] + e.maxs[2]];
    } else {
        let r = radius(&e.mins, &e.maxs, 3);
        lo = [e.origin[0] - r, e.origin[1] - r, e.origin[2] - r];
        hi = [e.origin[0] + r, e.origin[1] + r, e.origin[2] + r];
    }
    for i in 0..3 {
        lo[i] -= 1.0;
        hi[i] += 1.0;
    }
    (lo, hi)
}

/// Distance from the origin to the farthest corner over the first `axes` axes.
fn radius(mins: &Vec3, maxs: &Vec3, axes: usize) -> f32 {
    (0..axes)
        .map(|i| {
            let m = mins[i].abs().max(maxs[i].abs());
            m * m
        })
        .sum::<f32>()
        .sqrt()
}
