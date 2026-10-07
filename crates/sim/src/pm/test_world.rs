// SPDX-License-Identifier: GPL-3.0-or-later
//! A tiny axis-aligned-box world for movement tests: boxes with contents and surface flags,
//! swept by the Minkowski expansion of the mover's bounds.

use crate::Vec3;
use crate::cm::{Collide, ENTITYNUM_NONE, ENTITYNUM_WORLD, Trace};
use crate::contents;

pub const SURF_LADDER: i32 = 0x8;
pub const SURF_MANTLEON: i32 = 0x0200_0000;
/// Surface type 5 (a footstep material) so landing events are not silent.
pub const SURF_CONCRETE: i32 = 5 << 20;

#[derive(Clone, Copy)]
pub struct Block {
    pub mins: Vec3,
    pub maxs: Vec3,
    pub contents: i32,
    pub surface_flags: i32,
    pub entity: u16,
}

pub struct TestWorld {
    pub blocks: Vec<Block>,
}

impl TestWorld {
    /// A large floor whose top is at z = 0.
    pub fn floor() -> Self {
        let mut w = Self { blocks: Vec::new() };
        w.add(
            [-4000.0, -4000.0, -100.0],
            [4000.0, 4000.0, 0.0],
            contents::SOLID,
            SURF_CONCRETE,
        );
        w
    }

    pub fn add(&mut self, mins: Vec3, maxs: Vec3, contents: i32, surface_flags: i32) -> &mut Self {
        self.blocks.push(Block {
            mins,
            maxs,
            contents,
            surface_flags,
            entity: ENTITYNUM_WORLD,
        });
        self
    }
}

impl Collide for TestWorld {
    fn trace(
        &self,
        start: Vec3,
        end: Vec3,
        mins: Vec3,
        maxs: Vec3,
        _pass: u16,
        mask: i32,
    ) -> Trace {
        let mut best = Trace::MISS;
        let delta = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];
        for b in &self.blocks {
            if b.contents & mask == 0 {
                continue;
            }
            let lo: Vec3 = std::array::from_fn(|i| b.mins[i] - maxs[i]);
            let hi: Vec3 = std::array::from_fn(|i| b.maxs[i] - mins[i]);
            let inside = |p: &Vec3| (0..3).all(|i| p[i] > lo[i] && p[i] < hi[i]);
            if inside(&start) {
                best.start_solid = true;
                best.contents |= b.contents;
                best.hit_id = b.entity;
                if inside(&end) {
                    best.all_solid = true;
                    best.fraction = 0.0;
                }
                continue;
            }
            let (mut enter, mut leave) = (f32::NEG_INFINITY, f32::INFINITY);
            let mut normal = [0.0; 3];
            let mut hit = true;
            for i in 0..3 {
                if delta[i] == 0.0 {
                    if start[i] <= lo[i] || start[i] >= hi[i] {
                        hit = false;
                        break;
                    }
                    continue;
                }
                let (t0, t1, n) = if delta[i] > 0.0 {
                    (
                        (lo[i] - start[i]) / delta[i],
                        (hi[i] - start[i]) / delta[i],
                        -1.0,
                    )
                } else {
                    (
                        (hi[i] - start[i]) / delta[i],
                        (lo[i] - start[i]) / delta[i],
                        1.0,
                    )
                };
                if t0 > enter {
                    enter = t0;
                    normal = [0.0; 3];
                    normal[i] = n;
                }
                leave = leave.min(t1);
            }
            if !hit || enter >= leave || !(0.0..1.0).contains(&enter) {
                continue;
            }
            let len = (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt();
            let fraction = (enter - 0.125 / len).max(0.0);
            if fraction < best.fraction {
                best.fraction = fraction;
                best.normal = normal;
                best.surface_flags = b.surface_flags;
                best.contents = b.contents;
                best.hit_id = b.entity;
                best.walkable = normal[2] >= 0.7;
            }
        }
        if best.fraction == 1.0 && !best.start_solid {
            best.hit_id = ENTITYNUM_NONE;
        }
        best
    }

    fn point_contents(&self, p: Vec3, _pass: u16, mask: i32) -> i32 {
        self.blocks
            .iter()
            .filter(|b| (0..3).all(|i| p[i] >= b.mins[i] && p[i] <= b.maxs[i]))
            .fold(0, |acc, b| acc | (b.contents & mask))
    }
}
