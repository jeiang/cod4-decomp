// SPDX-License-Identifier: GPL-3.0-or-later
//! Bot navigation mesh generated from collision geometry.
//!
//! Stock maps ship no waypoints, so the mesh is derived at map load from the clipmap alone:
//! a uniform XY lattice of floor samples, flood-filled from the spawn points, with each
//! directed link validated by the same swept player hull `pmove` uses. Nothing here is a
//! recovered engine structure; the walkability limits are the movement constants
//! ([`STEP`] 18 units, floor normal `z >= 0.7`, the 30x30x70 stand and 30x30x50 crouch
//! hulls) and the only fact source is the collision world.
//!
//! Ceilings of the approximation: doors and movers are ignored (generate on a [`World`]
//! with no entities linked), jumps and mantles are not links, and links are validated
//! hull-to-hull on the lattice, not by simulating `pmove`, so a link that crosses a
//! sub-lattice obstacle shorter than a few units may be optimistic. Falls up to
//! [`MAX_DROP`] are one-way links.
//!
//! Layout is flat: positions, CSR adjacency, a dense per-cell index and one component id per
//! node. Queries allocate nothing once the caller's [`PathScratch`] has seen the mesh.

use std::collections::HashMap;
use std::time::Instant;

use sim::Vec3;
use sim::cm::{Collide, ENTITYNUM_NONE, Trace};
use sim::contents::{self, MASK_PLAYERSOLID, PLAYER};
use sim::pm::{PLAYER_MAXS, PLAYER_MINS};

use crate::game::{SpawnVars, parse_spawn_vars};

/// Highest step a player walks onto without jumping.
pub const STEP: f32 = 18.0;
/// Longest fall a link may cross (below the engine's fall-damage threshold).
pub const MAX_DROP: f32 = 120.0;
/// Lattice spacing. 32 keeps at least one sample inside any passage the 30-wide hull fits.
pub const CELL: f32 = 32.0;
/// Tallest ledge a jump reaches (`jump_height` 39 less a margin for the approach).
pub const JUMP_UP: f32 = 34.0;
/// Hull top while crouched.
const CROUCH_MAXS_Z: f32 = 50.0;
/// Walkable floors have a normal at least this vertical.
const MIN_FLOOR_NORMAL: f32 = 0.7;
/// How far under the hull's floor the centre may find ground: slopes lift the hull's
/// contact point above the centre's by up to half a cell.
const CENTER_DEPTH: f32 = 18.0;
/// Heights above a floor the hull travels at, to avoid grazing it.
const LIFT: f32 = 1.0;
/// Samples this close vertically in one column are the same floor.
const SAME_FLOOR: f32 = 16.0;
/// How far below itself a seed may find its floor: spawn entities float above the ground
/// and the engine settles them at spawn time.
const SEED_DROP: f32 = 512.0;
/// How far a seed that grazes a wall steps toward its target before sweeping.
const SEED_NUDGE: f32 = 2.0;
/// Mantle surface flags and the probe's reach (`mantle_check_range` 20 + the hull's 15).
const SURF_LADDER: i32 = 0x8;
const SURF_MANTLEON: i32 = 0x0200_0000;
const SURF_MANTLEOVER: i32 = 0x0400_0000;
const MANTLE_REACH: f32 = 40.0;
/// Safety valve for maps with huge outdoor floors.
const MAX_NODES: usize = 400_000;

/// Edge flags.
pub mod edge {
    /// Only the crouched hull fits.
    pub const CROUCH: u8 = 1;
    /// A fall steeper than any walkable slope: one-way in practice.
    pub const DROP: u8 = 2;
    /// A ledge above a step but within jump height: the bot must jump near the target.
    pub const JUMP: u8 = 4;
    /// A mantle onto a ledge the map marks climbable; the bot faces the ledge and jumps.
    pub const MANTLE: u8 = 8;
    /// A ladder up (never down: bots drop instead). The bot walks into the ladder and
    /// holds forward while looking up.
    pub const LADDER: u8 = 16;
}

/// A node in the mesh.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

impl NodeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Generation figures.
#[derive(Clone, Copy, Debug, Default)]
pub struct NavStats {
    pub nodes: u32,
    pub edges: u32,
    /// Strongly connected components (mutually reachable groups).
    pub components: u32,
    /// Node count of the largest component.
    pub main_component_nodes: u32,
    pub seeds_used: u32,
    pub seeds_dropped: u32,
    /// Collision traces spent generating.
    pub traces: u32,
    pub generation_ms: f32,
    /// Heap bytes held by the finished mesh.
    pub bytes: u32,
}

/// A spawn-like entity found in the map's entity string.
#[derive(Clone, Debug, PartialEq)]
pub struct SpawnPoint {
    pub class: String,
    pub origin: Vec3,
}

/// Every `mp_*spawn*` and `mp_global_intermission` entity, in entity-string order.
pub fn spawn_points(entity_string: &[u8]) -> Vec<SpawnPoint> {
    let Ok(ents) = parse_spawn_vars(entity_string) else {
        return Vec::new();
    };
    ents.iter().filter_map(spawn_point).collect()
}

fn spawn_point(vars: &SpawnVars) -> Option<SpawnPoint> {
    let get = |k: &str| {
        vars.iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.as_str())
    };
    let class = get("classname")?.to_ascii_lowercase();
    if !class.starts_with("mp_") || !(class.contains("spawn") || class == "mp_global_intermission")
    {
        return None;
    }
    let mut it = get("origin")?.split_whitespace().map(str::parse::<f32>);
    let origin = [it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?];
    Some(SpawnPoint { class, origin })
}

/// Flat navigation graph: nodes are standing-hull origins on floors.
#[derive(Debug)]
pub struct NavMesh {
    pos: Vec<Vec3>,
    edge_start: Vec<u32>,
    edge_to: Vec<u32>,
    edge_flags: Vec<u8>,
    /// Strongly connected component of each node.
    comp: Vec<u32>,
    /// Nodes grouped by component: `comp_start[c]..comp_start[c + 1]` into `comp_nodes`.
    comp_start: Vec<u32>,
    comp_nodes: Vec<u32>,
    main_comp: u32,
    /// Node of each input seed, `u32::MAX` for dropped ones.
    seed_nodes: Vec<u32>,
    grid_min: [i32; 2],
    grid_dim: [u32; 2],
    grid_start: Vec<u32>,
    grid_nodes: Vec<u32>,
    stats: NavStats,
}

/// Reusable A* working memory. Sized to a mesh on first use; after that [`NavMesh::path`]
/// does not allocate.
#[derive(Debug)]
pub struct PathScratch {
    g: Vec<f32>,
    f: Vec<f32>,
    parent: Vec<u32>,
    stamp: Vec<u32>,
    /// Heap slot of each open node, [`CLOSED`] once expanded.
    slot: Vec<u32>,
    heap: Vec<u32>,
    cur: u32,
    /// Paths costing more than this are not searched (default unbounded).
    pub max_cost: f32,
}

const CLOSED: u32 = u32::MAX;

impl PathScratch {
    pub fn new(mesh: &NavMesh) -> Self {
        let n = mesh.node_count();
        Self {
            g: vec![0.0; n],
            f: vec![0.0; n],
            parent: vec![0; n],
            stamp: vec![0; n],
            slot: vec![0; n],
            heap: Vec::with_capacity(n),
            cur: 0,
            max_cost: f32::INFINITY,
        }
    }

    fn begin(&mut self, n: usize) {
        if self.stamp.len() < n {
            self.g.resize(n, 0.0);
            self.f.resize(n, 0.0);
            self.parent.resize(n, 0);
            self.stamp.resize(n, 0);
            self.slot.resize(n, 0);
        }
        self.heap.clear();
        self.cur = self.cur.wrapping_add(1);
        if self.cur == 0 {
            self.stamp.fill(0);
            self.cur = 1;
        }
    }

    /// Lower f first, then the farther-along node (smaller heuristic), then the lower id.
    fn less(&self, a: u32, b: u32) -> bool {
        let (a, b) = (a as usize, b as usize);
        match self.f[a].total_cmp(&self.f[b]) {
            std::cmp::Ordering::Equal => match self.g[b].total_cmp(&self.g[a]) {
                std::cmp::Ordering::Equal => a < b,
                o => o.is_lt(),
            },
            o => o.is_lt(),
        }
    }

    fn sift_up(&mut self, mut i: usize) {
        let n = self.heap[i];
        while i > 0 {
            let p = (i - 1) / 2;
            if !self.less(n, self.heap[p]) {
                break;
            }
            self.heap[i] = self.heap[p];
            self.slot[self.heap[i] as usize] = i as u32;
            i = p;
        }
        self.heap[i] = n;
        self.slot[n as usize] = i as u32;
    }

    fn sift_down(&mut self, mut i: usize) {
        let n = self.heap[i];
        let len = self.heap.len();
        loop {
            let mut c = 2 * i + 1;
            if c >= len {
                break;
            }
            if c + 1 < len && self.less(self.heap[c + 1], self.heap[c]) {
                c += 1;
            }
            if !self.less(self.heap[c], n) {
                break;
            }
            self.heap[i] = self.heap[c];
            self.slot[self.heap[i] as usize] = i as u32;
            i = c;
        }
        self.heap[i] = n;
        self.slot[n as usize] = i as u32;
    }

    fn pop(&mut self) -> Option<u32> {
        let top = *self.heap.first()?;
        let last = self.heap.pop()?;
        self.slot[top as usize] = CLOSED;
        if !self.heap.is_empty() {
            self.heap[0] = last;
            self.sift_down(0);
        }
        Some(top)
    }
}

fn dist3(a: Vec3, b: Vec3) -> f32 {
    let (x, y, z) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    (x * x + y * y + z * z).sqrt()
}

fn dist_xy(a: Vec3, b: Vec3) -> f32 {
    let (x, y) = (a[0] - b[0], a[1] - b[1]);
    (x * x + y * y).sqrt()
}

/// Cost of walking `a -> b`: distance, dearer crouched, and a fixed penalty for falls.
fn edge_cost(a: Vec3, b: Vec3, flags: u8) -> f32 {
    let mut c = dist3(a, b);
    if flags & edge::CROUCH != 0 {
        c *= 1.5;
    }
    if flags & edge::DROP != 0 {
        c += 32.0;
    }
    if flags & edge::JUMP != 0 {
        c += 40.0;
    }
    if flags & edge::MANTLE != 0 {
        c += 60.0;
    }
    if flags & edge::LADDER != 0 {
        c += 40.0;
    }
    c
}

impl NavMesh {
    /// Builds the mesh for `world`, flood-filling from `seeds`. Seeds with no floor within
    /// 512 units below them are dropped (see [`NavStats::seeds_dropped`]). The world
    /// should have no entities linked: movers would otherwise be walls.
    pub fn generate(world: &impl Collide, seeds: &[Vec3]) -> NavMesh {
        let t0 = Instant::now();
        let mut g = Gen::new(world);
        let nseeds = g.place_seeds(seeds);
        g.link_seeds(nseeds);
        g.flood(nseeds);
        let flooded = g.pos.len();
        g.rescue(nseeds);
        g.flood(flooded as u32);
        let seed_nodes = std::mem::take(&mut g.seed_nodes);
        let mut mesh = g.finish();
        mesh.stats.bytes += seed_nodes.len() as u32 * 4;
        mesh.seed_nodes = seed_nodes;
        mesh.stats.generation_ms = t0.elapsed().as_secs_f32() * 1000.0;
        mesh
    }

    /// Mesh from an explicit graph, for tests and tools. Edges are `(from, to, flags)`.
    pub fn from_graph(pos: Vec<Vec3>, edges: &[(u32, u32, u8)]) -> NavMesh {
        build(pos, edges, 0, 0, 0)
    }

    /// The node seed `i` of [`generate`](Self::generate) settled onto, if it found a floor.
    pub fn seed_node(&self, i: usize) -> Option<NodeId> {
        self.seed_nodes
            .get(i)
            .filter(|&&n| n != u32::MAX)
            .map(|&n| NodeId(n))
    }

    pub fn stats(&self) -> &NavStats {
        &self.stats
    }

    pub fn node_count(&self) -> usize {
        self.pos.len()
    }

    pub fn node_pos(&self, id: NodeId) -> Vec3 {
        self.pos[id.index()]
    }

    /// Outgoing links of `id` as `(target, flags)`.
    pub fn neighbors(&self, id: NodeId) -> impl Iterator<Item = (NodeId, u8)> + '_ {
        let r = self.edge_start[id.index()] as usize..self.edge_start[id.index() + 1] as usize;
        self.edge_to[r.clone()]
            .iter()
            .zip(&self.edge_flags[r])
            .map(|(&t, &f)| (NodeId(t), f))
    }

    /// Flags of the link `a -> b`, if there is one.
    pub fn edge_flags(&self, a: NodeId, b: NodeId) -> Option<u8> {
        self.neighbors(a).find(|&(t, _)| t == b).map(|(_, f)| f)
    }

    /// Component (mutually reachable group) of `id`.
    pub fn component(&self, id: NodeId) -> u32 {
        self.comp[id.index()]
    }

    /// The largest component; spawn points normally all land in it.
    pub fn main_component(&self) -> u32 {
        self.main_comp
    }

    /// True when `a` and `b` can reach each other (same strongly connected component).
    /// `false` does not rule out a one-way route `a -> b`; ask [`NavMesh::path`].
    pub fn reachable(&self, a: NodeId, b: NodeId) -> bool {
        self.comp[a.index()] == self.comp[b.index()]
    }

    fn cell_of(p: Vec3) -> [i32; 2] {
        [(p[0] / CELL).floor() as i32, (p[1] / CELL).floor() as i32]
    }

    fn grid_cell(&self, cx: i32, cy: i32) -> &[u32] {
        let (x, y) = (cx - self.grid_min[0], cy - self.grid_min[1]);
        if x < 0 || y < 0 || x as u32 >= self.grid_dim[0] || y as u32 >= self.grid_dim[1] {
            return &[];
        }
        let i = y as usize * self.grid_dim[0] as usize + x as usize;
        &self.grid_nodes[self.grid_start[i] as usize..self.grid_start[i + 1] as usize]
    }

    /// The node nearest `p` in 3D, favouring the matching floor (vertical distance counts
    /// double), within about three lattice cells and 96 units of height.
    pub fn nearest_node(&self, p: Vec3) -> Option<NodeId> {
        const RINGS: i32 = 3;
        let [cx, cy] = Self::cell_of(p);
        let mut best: Option<(f32, u32)> = None;
        let mut found_at = None;
        for r in 0..=RINGS {
            if found_at.is_some_and(|f| r > f + 1) {
                break;
            }
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx.abs().max(dy.abs()) != r {
                        continue;
                    }
                    for &n in self.grid_cell(cx + dx, cy + dy) {
                        let q = self.pos[n as usize];
                        let dz = (q[2] - p[2]).abs();
                        if dz > 96.0 {
                            continue;
                        }
                        let (x, y) = (q[0] - p[0], q[1] - p[1]);
                        let score = x * x + y * y + 4.0 * dz * dz;
                        if best.is_none_or(|(s, id)| score < s || (score == s && n < id)) {
                            best = Some((score, n));
                            found_at.get_or_insert(r);
                        }
                    }
                }
            }
        }
        best.map(|(_, n)| NodeId(n))
    }

    /// A uniformly random node of the main component.
    pub fn random_walkable(&self, rng: &mut impl FnMut() -> u32) -> NodeId {
        self.random_in_component(self.main_comp, rng)
    }

    /// A uniformly random node of component `comp`.
    pub fn random_in_component(&self, comp: u32, rng: &mut impl FnMut() -> u32) -> NodeId {
        let (a, b) = (
            self.comp_start[comp as usize] as usize,
            self.comp_start[comp as usize + 1] as usize,
        );
        NodeId(self.comp_nodes[a + rng() as usize % (b - a)])
    }

    /// A* from `from` to `to` (inclusive both ends) into `out`. Returns false, leaving `out`
    /// empty, when there is no route or it costs more than `scratch.max_cost`. Ties break
    /// on the farther-along node, then the lower id, so results are deterministic.
    pub fn path(
        &self,
        from: NodeId,
        to: NodeId,
        scratch: &mut PathScratch,
        out: &mut Vec<NodeId>,
    ) -> bool {
        out.clear();
        if from == to {
            out.push(from);
            return true;
        }
        scratch.begin(self.node_count());
        let goal = self.pos[to.index()];
        let h = |n: u32| dist3(self.pos[n as usize], goal);
        let cur = scratch.cur;
        let s = from.0 as usize;
        scratch.stamp[s] = cur;
        scratch.g[s] = 0.0;
        scratch.f[s] = h(from.0);
        scratch.parent[s] = from.0;
        scratch.heap.push(from.0);
        scratch.slot[s] = 0;
        while let Some(n) = scratch.pop() {
            if n == to.0 {
                let mut at = n;
                loop {
                    out.push(NodeId(at));
                    if at == from.0 {
                        break;
                    }
                    at = scratch.parent[at as usize];
                }
                out.reverse();
                return true;
            }
            let gn = scratch.g[n as usize];
            let a = self.pos[n as usize];
            let r = self.edge_start[n as usize] as usize..self.edge_start[n as usize + 1] as usize;
            for e in r {
                let m = self.edge_to[e];
                let mi = m as usize;
                if scratch.stamp[mi] == cur && scratch.slot[mi] == CLOSED {
                    continue;
                }
                let ng = gn + edge_cost(a, self.pos[mi], self.edge_flags[e]);
                let nf = ng + h(m);
                if nf > scratch.max_cost {
                    continue;
                }
                if scratch.stamp[mi] != cur {
                    scratch.stamp[mi] = cur;
                    scratch.g[mi] = ng;
                    scratch.f[mi] = nf;
                    scratch.parent[mi] = n;
                    scratch.heap.push(m);
                    let i = scratch.heap.len() - 1;
                    scratch.sift_up(i);
                } else if ng < scratch.g[mi] {
                    scratch.g[mi] = ng;
                    scratch.f[mi] = nf;
                    scratch.parent[mi] = n;
                    let i = scratch.slot[mi] as usize;
                    scratch.sift_up(i);
                }
            }
        }
        false
    }

    /// True when a standing player can walk the straight line `a -> b`: the hull sweeps
    /// clear at floor level and a floor lies under every lattice step of the line within
    /// [`STEP`] of the interpolated height. Allocation-free.
    pub fn line_walkable(&self, world: &impl Collide, a: NodeId, b: NodeId) -> bool {
        let (pa, pb) = (self.pos[a.index()], self.pos[b.index()]);
        let dz = pb[2] - pa[2];
        if dz.abs() > STEP {
            return false;
        }
        let h = pa[2].max(pb[2]) + LIFT;
        let t = trace(world, [pa[0], pa[1], h], [pb[0], pb[1], h], STAND_MAXS);
        if !clear(&t) {
            return false;
        }
        let len = dist_xy(pa, pb);
        let steps = (len / (CELL * 0.5)) as u32;
        for i in 1..steps {
            let f = i as f32 / steps as f32;
            let p = [
                pa[0] + (pb[0] - pa[0]) * f,
                pa[1] + (pb[1] - pa[1]) * f,
                pa[2] + dz * f,
            ];
            let t = trace(
                world,
                [p[0], p[1], p[2] + STEP],
                [p[0], p[1], p[2] - STEP],
                STAND_MAXS,
            );
            if !clear(&t) || t.normal[2] < MIN_FLOOR_NORMAL {
                return false;
            }
        }
        true
    }

    /// Drops nodes a straight walk skips: a node is removed when its neighbours on either
    /// side are joined by a [`line_walkable`](Self::line_walkable) line, looking ahead at
    /// most 8 nodes. Crouch and drop links are never skipped.
    pub fn smooth(&self, world: &impl Collide, path: &mut Vec<NodeId>) {
        const LOOKAHEAD: usize = 8;
        if path.len() < 3 {
            return;
        }
        let mut w = 1;
        let mut i = 0;
        while i + 1 < path.len() {
            let mut best = i + 1;
            let mut k = i + 2;
            while k < path.len() && k <= i + LOOKAHEAD {
                if self.edge_flags(path[k - 1], path[k]) != Some(0)
                    || self.edge_flags(path[i], path[i + 1]) != Some(0)
                    || !self.line_walkable(world, path[i], path[k])
                {
                    break;
                }
                best = k;
                k += 1;
            }
            path[w] = path[best];
            w += 1;
            i = best;
        }
        path.truncate(w);
    }
}

const STAND_MAXS: f32 = PLAYER_MAXS[2];

fn hull(maxs_z: f32) -> (Vec3, Vec3) {
    (PLAYER_MINS, [PLAYER_MAXS[0], PLAYER_MAXS[1], maxs_z])
}

fn trace(world: &impl Collide, a: Vec3, b: Vec3, maxs_z: f32) -> Trace {
    let (mins, maxs) = hull(maxs_z);
    world.trace(a, b, mins, maxs, ENTITYNUM_NONE, MASK_PLAYERSOLID & !PLAYER)
}

fn clear(t: &Trace) -> bool {
    !t.start_solid && !t.all_solid && t.fraction >= 1.0
}

/// Which start heights `land` tries.
#[derive(Clone, Copy, PartialEq)]
enum Reach {
    /// Walking: a step up, level, then a ramp's rise.
    Walk,
    /// Jumping: from above the highest ledge a jump clears.
    Ledge,
}

#[derive(Clone, Copy)]
enum Land {
    Floor(f32),
    /// Every start was inside geometry.
    Solid,
    /// A pit deeper than [`MAX_DROP`] or a floor too steep to stand on.
    None,
}

/// FNV-style hasher for the generation caches: keys are small integers, never adversarial.
#[derive(Default, Clone, Copy)]
struct FastHash(u64);

impl std::hash::Hasher for FastHash {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
        }
    }

    fn write_u32(&mut self, v: u32) {
        self.0 = (self.0.rotate_left(5) ^ u64::from(v)).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl std::hash::BuildHasher for FastHash {
    type Hasher = FastHash;

    fn build_hasher(&self) -> FastHash {
        FastHash(0xcbf2_9ce4_8422_2325)
    }
}

struct Gen<'a, W: Collide> {
    world: &'a W,
    pos: Vec<Vec3>,
    /// First node of each lattice column, chained through `next`.
    cols: HashMap<(i32, i32), u32>,
    next: Vec<u32>,
    lands: HashMap<[u32; 4], Land, FastHash>,
    /// Mantle landing nodes by lattice column (they sit where the ledge is, off-centre).
    mantle_nodes: HashMap<(i32, i32), u32>,
    edges: Vec<(u32, u32, u8)>,
    traces: u32,
    seeds_used: u32,
    seeds_dropped: u32,
    seed_nodes: Vec<u32>,
}

impl<'a, W: Collide> Gen<'a, W> {
    fn new(world: &'a W) -> Self {
        Self {
            world,
            pos: Vec::new(),
            cols: HashMap::new(),
            next: Vec::new(),
            lands: HashMap::default(),
            mantle_nodes: HashMap::new(),
            edges: Vec::new(),
            traces: 0,
            seeds_used: 0,
            seeds_dropped: 0,
            seed_nodes: Vec::new(),
        }
    }

    fn tr(&mut self, a: Vec3, b: Vec3, maxs_z: f32) -> Trace {
        self.traces += 1;
        trace(self.world, a, b, maxs_z)
    }

    /// The floor under `(x, y)` for a hull starting around `from_z`. Every lattice cell is
    /// asked by up to eight neighbours at the same height on flat ground, so answers are kept.
    fn land(
        &mut self,
        from_z: f32,
        x: f32,
        y: f32,
        maxs_z: f32,
        centre: bool,
        reach: Reach,
    ) -> Land {
        let key = [
            x.to_bits(),
            y.to_bits(),
            from_z.to_bits(),
            maxs_z.to_bits() ^ u32::from(centre) ^ (u32::from(reach == Reach::Ledge) << 1),
        ];
        if let Some(&l) = self.lands.get(&key) {
            return l;
        }
        let l = self.land_trace(from_z, x, y, maxs_z, centre, reach);
        self.lands.insert(key, l);
        l
    }

    fn land_trace(
        &mut self,
        from_z: f32,
        x: f32,
        y: f32,
        maxs_z: f32,
        centre: bool,
        reach: Reach,
    ) -> Land {
        let end_z = from_z - MAX_DROP;
        // Starts: a step up, level (under a low ceiling), then high enough to clear a ramp that
        // rises more than a step per cell.
        let walk = [from_z + STEP, from_z + LIFT, from_z + CELL * 1.5];
        // Triangle-soup collision (models, terrain) is hollow: a hull starting below a ledge
        // falls through it, so ledges need a start above their tops.
        let ledge = [from_z + JUMP_UP + LIFT];
        let tops: &[f32] = if reach == Reach::Walk { &walk } else { &ledge };
        for &top in tops {
            let t = self.tr([x, y, top], [x, y, end_z], maxs_z);
            if t.start_solid || t.all_solid {
                continue;
            }
            if t.fraction >= 1.0 || t.normal[2] < MIN_FLOOR_NORMAL {
                return Land::None;
            }
            let z = top + (end_z - top) * t.fraction;
            if !centre {
                return Land::Floor(z);
            }
            // The hull can touch a ledge with a sliver of its footprint; only stand where
            // the centre has ground, within a few units of the hull's floor.
            self.traces += 1;
            let c = self.world.trace(
                [x, y, z + 2.0],
                [x, y, z - CENTER_DEPTH],
                [0.0; 3],
                [0.0; 3],
                ENTITYNUM_NONE,
                MASK_PLAYERSOLID & !PLAYER,
            );
            if c.start_solid || c.all_solid || c.fraction >= 1.0 {
                return Land::None;
            }
            return Land::Floor(z);
        }
        Land::Solid
    }

    /// Whether the hull walks from `from` to the floor point `to` (already known to be a
    /// floor): flat and stepped ground at one height, ramps along the slope, stairs split
    /// into halves that each rise at most a step.
    fn walks(&mut self, from: Vec3, to: Vec3, maxs_z: f32, depth: u32) -> bool {
        let dz = to[2] - from[2];
        if dz <= STEP {
            let h = from[2].max(to[2]) + LIFT;
            let t = self.tr([from[0], from[1], h], [to[0], to[1], h], maxs_z);
            return clear(&t);
        }
        let t = self.tr(
            [from[0], from[1], from[2] + LIFT],
            [to[0], to[1], to[2] + LIFT],
            maxs_z,
        );
        if clear(&t) && self.hugs_floor(from, to, maxs_z) {
            return true;
        }
        if depth == 0 {
            return false;
        }
        let (mx, my) = ((from[0] + to[0]) * 0.5, (from[1] + to[1]) * 0.5);
        let Land::Floor(mz) = self.land(from[2], mx, my, maxs_z, true, Reach::Walk) else {
            return false;
        };
        let m = [mx, my, mz];
        self.walks(from, m, maxs_z, depth - 1) && self.walks(m, to, maxs_z, depth - 1)
    }

    /// A ramp sweep flies the hull above the ground, so a ledge taller than a step would
    /// pass unseen: demand the floor within a few units of the line every 8 units. Hull
    /// floors on one plane are linear, so a true ramp stays inside the tolerance; stairs
    /// do not and take the split path in [`walks`](Self::walks).
    fn hugs_floor(&mut self, from: Vec3, to: Vec3, maxs_z: f32) -> bool {
        const TOL: f32 = 6.0;
        let slices = (dist_xy(from, to) / 8.0).ceil().max(1.0) as u32;
        for i in 1..slices {
            let f = i as f32 / slices as f32;
            let p = [
                from[0] + (to[0] - from[0]) * f,
                from[1] + (to[1] - from[1]) * f,
                from[2] + (to[2] - from[2]) * f,
            ];
            let t = self.tr([p[0], p[1], p[2] + TOL], [p[0], p[1], p[2] - TOL], maxs_z);
            if t.start_solid || t.all_solid || t.fraction >= 1.0 {
                return false;
            }
        }
        true
    }

    /// A standing jump onto a ledge: straight up beside it, then across at ledge height.
    fn jumps(&mut self, from: Vec3, to: Vec3, maxs_z: f32) -> bool {
        let h = to[2] + LIFT;
        let up = self.tr(
            [from[0], from[1], from[2] + LIFT],
            [from[0], from[1], h],
            maxs_z,
        );
        if !clear(&up) {
            return false;
        }
        let across = self.tr([from[0], from[1], h], [to[0], to[1], h], maxs_z);
        clear(&across)
    }

    /// Walks from `from` toward lattice/seed point `(x, y)`: the landing height and flags.
    /// `centre` demands ground under the landing centre, not just under the hull (seeds,
    /// which sit where the map put them, are exempt).
    fn walk(&mut self, from: Vec3, x: f32, y: f32, centre: bool) -> Option<(f32, u8)> {
        let mut floor = None;
        for maxs_z in [STAND_MAXS, CROUCH_MAXS_Z] {
            let crouch = maxs_z != STAND_MAXS;
            let z = match floor {
                Some(z) => z,
                None => match self.land(from[2], x, y, maxs_z, centre, Reach::Walk) {
                    Land::Floor(z) => z,
                    Land::Solid => continue,
                    Land::None => return None,
                },
            };
            floor = Some(z);
            let to = [x, y, z];
            let rise = z - from[2];
            let (z, rise, jump) = if self.walks(from, to, maxs_z, 2) {
                (z, rise, false)
            } else if rise > STEP && rise <= JUMP_UP && self.jumps(from, to, maxs_z) {
                (z, rise, true)
            } else if maxs_z == STAND_MAXS
                && rise <= STEP
                && let Land::Floor(z2) = self.land(from[2], x, y, maxs_z, centre, Reach::Ledge)
                && z2 - from[2] > STEP
                && z2 - from[2] <= JUMP_UP
                && self.jumps(from, [x, y, z2], maxs_z)
            {
                (z2, z2 - from[2], true)
            } else {
                continue;
            };
            let drop = -rise;
            let mut flags = if crouch { edge::CROUCH } else { 0 };
            if drop > STEP && drop > dist_xy(from, [x, y, z]) {
                flags |= edge::DROP;
            }
            if jump {
                flags |= edge::JUMP;
            }
            return Some((z, flags));
        }
        None
    }

    fn find(&self, c: (i32, i32), z: f32) -> Option<u32> {
        let mut n = *self.cols.get(&c)?;
        while n != u32::MAX {
            if (self.pos[n as usize][2] - z).abs() < SAME_FLOOR {
                return Some(n);
            }
            n = self.next[n as usize];
        }
        None
    }

    /// The lattice node of column `c` on the floor at `z`, creating it if new.
    fn node(&mut self, c: (i32, i32), z: f32) -> Option<u32> {
        if let Some(n) = self.find(c, z) {
            return Some(n);
        }
        if self.pos.len() >= MAX_NODES {
            return None;
        }
        let id = self.pos.len() as u32;
        self.pos
            .push([(c.0 as f32 + 0.5) * CELL, (c.1 as f32 + 0.5) * CELL, z]);
        let head = self.cols.insert(c, id).unwrap_or(u32::MAX);
        self.next.push(head);
        Some(id)
    }

    /// Creates one node per seed that rests on a floor; returns how many.
    fn place_seeds(&mut self, seeds: &[Vec3]) -> u32 {
        // Spawn entities float, or sit a little inside the floor or under a low ceiling, so
        // try several start heights for the drop to the ground.
        const STARTS: [f32; 5] = [8.0, 32.0, 64.0, 0.0, -12.0];
        'seeds: for s in seeds {
            for maxs_z in [STAND_MAXS, CROUCH_MAXS_Z] {
                for start in STARTS {
                    let t = self.tr(
                        [s[0], s[1], s[2] + start],
                        [s[0], s[1], s[2] - SEED_DROP],
                        maxs_z,
                    );
                    if t.start_solid || t.all_solid {
                        continue;
                    }
                    if t.fraction >= 1.0 || t.normal[2] < MIN_FLOOR_NORMAL {
                        break;
                    }
                    let z = s[2] + start - (start + SEED_DROP) * t.fraction;
                    self.pos.push([s[0], s[1], z]);
                    self.next.push(u32::MAX);
                    self.seed_nodes.push(self.seeds_used);
                    self.seeds_used += 1;
                    continue 'seeds;
                }
            }
            self.seed_nodes.push(u32::MAX);
            self.seeds_dropped += 1;
        }
        self.seeds_used
    }

    /// Joins each seed with the lattice samples around it, both ways.
    fn link_seeds(&mut self, nseeds: u32) {
        for s in 0..nseeds {
            let sp = self.pos[s as usize];
            let [cx, cy] = NavMesh::cell_of(sp);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let c = (cx + dx, cy + dy);
                    let (x, y) = ((c.0 as f32 + 0.5) * CELL, (c.1 as f32 + 0.5) * CELL);
                    // Map spawns can graze a wall by a fraction of a unit; step off it before
                    // giving up on the link.
                    let len = dist_xy(sp, [x, y, 0.0]).max(1e-3);
                    let off = [
                        sp[0] + (x - sp[0]) / len * SEED_NUDGE,
                        sp[1] + (y - sp[1]) / len * SEED_NUDGE,
                        sp[2],
                    ];
                    let Some((z, flags)) = self
                        .walk(sp, x, y, true)
                        .or_else(|| self.walk(off, x, y, true))
                    else {
                        continue;
                    };
                    let Some(n) = self.node(c, z) else {
                        continue;
                    };
                    self.edges.push((s, n, flags));
                    let np = self.pos[n as usize];
                    if flags == 0 && (sp[2] - np[2]).abs() <= STEP {
                        // The sweep is the same both ways, and the seed is where the map
                        // itself put a player (it may graze a wall the lattice sweep sees).
                        self.edges.push((n, s, 0));
                    } else if let Some((rz, rf)) = self.walk(np, sp[0], sp[1], false)
                        && (rz - sp[2]).abs() < SAME_FLOOR
                    {
                        self.edges.push((n, s, rf));
                    }
                }
            }
        }
    }

    fn flood(&mut self, nseeds: u32) {
        const DIRS: [(i32, i32); 8] = [
            (1, 0),
            (-1, 0),
            (0, 1),
            (0, -1),
            (1, 1),
            (1, -1),
            (-1, 1),
            (-1, -1),
        ];
        // Direction index of the way back for each of `DIRS`.
        const BACK: [usize; 8] = [1, 0, 3, 2, 7, 6, 5, 4];
        let mut done: Vec<u8> = Vec::new();
        let mut i = nseeds as usize;
        while i < self.pos.len() {
            let p = self.pos[i];
            let [cx, cy] = NavMesh::cell_of(p);
            // A mantle landing sits where the ledge is, not on a lattice sample, and may
            // perch on its lip: it leads on, but nothing is assumed to lead back to it.
            let perch = self.mantle_nodes.get(&(cx, cy)) == Some(&(i as u32));
            done.resize(self.pos.len(), 0);
            for (k, (dx, dy)) in DIRS.into_iter().enumerate() {
                if done[i] & (1 << k) != 0 {
                    continue;
                }
                let c = (cx + dx, cy + dy);
                let (x, y) = ((c.0 as f32 + 0.5) * CELL, (c.1 as f32 + 0.5) * CELL);
                let Some((z, flags)) = self.walk(p, x, y, true) else {
                    if k < 4 {
                        self.ladder(i as u32, p, [dx as f32, dy as f32]);
                    }
                    continue;
                };
                if let Some(n) = self.node(c, z) {
                    self.edges.push((i as u32, n, flags));
                    // Up to a step the sweep height is the higher floor either way: same volume.
                    if !perch && flags == 0 && (z - p[2]).abs() <= STEP {
                        self.edges.push((n, i as u32, 0));
                        done.resize(self.pos.len(), 0);
                        done[n as usize] |= 1 << BACK[k];
                    }
                }
            }
            self.mantles(i as u32, p);
            i += 1;
        }
    }

    /// A ladder up from `p` in direction `d` (blocked by a wall): found by the same reduced
    /// hull probe `pmove` grabs a ladder with, climbed in 16-unit steps while the face holds,
    /// then off the top onto the floor beyond.
    fn ladder(&mut self, i: u32, p: Vec3, d: [f32; 2]) {
        const REACH: f32 = 40.0;
        const MINS: Vec3 = [-9.0, -9.0, 8.0];
        const MAXS: Vec3 = [9.0, 9.0, 70.0];
        const MIN_CLIMB: f32 = 40.0;
        let mask = MASK_PLAYERSOLID & !PLAYER;
        let probe = |g: &mut Self, a: Vec3, b: Vec3| {
            g.traces += 1;
            g.world.trace(a, b, MINS, MAXS, ENTITYNUM_NONE, mask)
        };
        let t = probe(self, p, [p[0] + d[0] * REACH, p[1] + d[1] * REACH, p[2]]);
        if t.start_solid || t.fraction >= 1.0 || t.surface_flags & SURF_LADDER == 0 {
            return;
        }
        let touch = [
            p[0] + d[0] * REACH * t.fraction,
            p[1] + d[1] * REACH * t.fraction,
        ];
        let mut top = p[2];
        for k in 1..=40 {
            let z = p[2] + 16.0 * k as f32;
            let t = probe(
                self,
                [touch[0], touch[1], z],
                [touch[0] + d[0] * 8.0, touch[1] + d[1] * 8.0, z],
            );
            if t.start_solid || t.fraction >= 1.0 || t.surface_flags & SURF_LADDER == 0 {
                break;
            }
            top = z;
        }
        if top - p[2] < MIN_CLIMB {
            return;
        }
        let (x, y) = (touch[0] + d[0] * 24.0, touch[1] + d[1] * 24.0);
        let Land::Floor(z) = self.land(top, x, y, STAND_MAXS, true, Reach::Walk) else {
            return;
        };
        if (z - top).abs() > 48.0 {
            return;
        }
        let c = NavMesh::cell_of([x, y, z]);
        let id = match self
            .mantle_nodes
            .get(&(c[0], c[1]))
            .copied()
            .filter(|&m| (self.pos[m as usize][2] - z).abs() < SAME_FLOOR)
        {
            Some(id) => id,
            None if self.pos.len() < MAX_NODES => {
                let id = self.pos.len() as u32;
                self.pos.push([x, y, z]);
                self.next.push(u32::MAX);
                self.mantle_nodes.insert((c[0], c[1]), id);
                id
            }
            None => return,
        };
        self.edges.push((i, id, edge::LADDER));
    }

    /// Mantle links from `p`: where the player facing a map-marked ledge and pressing jump
    /// ends up, found with the same probes `pmove` uses (default `mantle_check_*`).
    fn mantles(&mut self, i: u32, p: Vec3) {
        const DIRS: [[f32; 2]; 8] = [
            [1.0, 0.0],
            [0.0, 1.0],
            [-1.0, 0.0],
            [0.0, -1.0],
            [0.707_106_77, 0.707_106_77],
            [-0.707_106_77, 0.707_106_77],
            [-0.707_106_77, -0.707_106_77],
            [0.707_106_77, -0.707_106_77],
        ];
        // Cheap reject: is any mantle brush within the probe's reach of this node?
        self.traces += 1;
        let near = self.world.trace(
            p,
            p,
            [-MANTLE_REACH, -MANTLE_REACH, 0.0],
            [MANTLE_REACH, MANTLE_REACH, 100.0],
            ENTITYNUM_NONE,
            contents::MANTLE,
        );
        if !(near.start_solid || near.all_solid) {
            return;
        }
        for d in DIRS {
            let (start, end) = (
                [p[0] - 14.9 * d[0], p[1] - 14.9 * d[1], p[2]],
                [p[0] + 34.9 * d[0], p[1] + 34.9 * d[1], p[2]],
            );
            self.traces += 1;
            let t = self.world.trace(
                start,
                end,
                [-0.1, -0.1, 0.0],
                [0.1, 0.1, 70.0],
                ENTITYNUM_NONE,
                contents::MANTLE,
            );
            if t.start_solid
                || t.all_solid
                || t.fraction >= 1.0
                || t.surface_flags & (SURF_MANTLEON | SURF_MANTLEOVER) == 0
            {
                continue;
            }
            let n = (t.normal[0] * t.normal[0] + t.normal[1] * t.normal[1]).sqrt();
            if n < 1e-4 {
                continue;
            }
            let face = [-t.normal[0] / n, -t.normal[1] / n];
            if (d[0] * face[0] + d[1] * face[1])
                .clamp(-1.0, 1.0)
                .acos()
                .to_degrees()
                > 60.0
            {
                continue;
            }
            let over = t.surface_flags & SURF_MANTLEOVER != 0;
            let Some(top) = [60.0, 40.0, 20.0]
                .into_iter()
                .find_map(|h| self.ledge(p, face, h, over))
            else {
                continue;
            };
            let c = NavMesh::cell_of(top);
            let existing = self
                .mantle_nodes
                .get(&(c[0], c[1]))
                .copied()
                .filter(|&m| (self.pos[m as usize][2] - top[2]).abs() < SAME_FLOOR);
            let id = match existing {
                Some(id) => id,
                None if self.pos.len() < MAX_NODES => {
                    let id = self.pos.len() as u32;
                    self.pos.push(top);
                    self.next.push(u32::MAX);
                    self.mantle_nodes.insert((c[0], c[1]), id);
                    id
                }
                None => continue,
            };
            self.edges.push((i, id, edge::MANTLE));
        }
    }

    /// One `Mantle_CheckLedge` probe `height` up: the hull origin on the ledge, or past it
    /// for an over-mantle.
    fn ledge(&mut self, p: Vec3, dir: [f32; 2], height: f32, over: bool) -> Option<Vec3> {
        let (mins, maxs) = ([-15.0, -15.0, 0.0], [15.0, 15.0, 30.0]);
        let mask = MASK_PLAYERSOLID & !PLAYER;
        let mut tr = |a: Vec3, b: Vec3, mx: Vec3| {
            self.traces += 1;
            self.world.trace(a, b, mins, mx, ENTITYNUM_NONE, mask)
        };
        let start = [p[0], p[1], p[2] + height];
        let end = [start[0] + 16.0 * dir[0], start[1] + 16.0 * dir[1], start[2]];
        let t = tr(start, end, maxs);
        if t.start_solid || t.fraction < 1.0 {
            return None;
        }
        let down = [end[0], end[1], p[2] + STEP];
        let t = tr(end, down, maxs);
        if t.start_solid || t.fraction >= 1.0 || !t.walkable {
            return None;
        }
        let ledge = [end[0], end[1], end[2] + (down[2] - end[2]) * t.fraction];
        let stand = [15.0, 15.0, 50.0];
        if tr(ledge, ledge, stand).start_solid {
            return None;
        }
        if !over {
            return Some(ledge);
        }
        let ahead = [ledge[0] + 31.0 * dir[0], ledge[1] + 31.0 * dir[1], ledge[2]];
        let t = tr(ledge, ahead, stand);
        if t.start_solid || t.fraction < 1.0 {
            return Some(ledge);
        }
        let low = [ahead[0], ahead[1], ahead[2] - STEP];
        let t = tr(ahead, low, stand);
        if t.start_solid || t.fraction < 1.0 {
            return Some(ledge);
        }
        Some(low)
    }

    /// Seeds in a pocket the lattice cannot enter (a room only just wider than the hull, where
    /// no 32-unit sample fits) get a local flood on a finer, seed-aligned grid until it meets
    /// the main component.
    fn rescue(&mut self, nseeds: u32) {
        const POCKET: u32 = 64;
        const BUDGET: usize = 160;
        let n = self.pos.len();
        let (start, to, _) = csr(n, &self.edges);
        let (comp, ncomp) = scc(n, &start, &to);
        let mut size = vec![0u32; ncomp];
        for &c in &comp {
            size[c as usize] += 1;
        }
        let main = (0..ncomp)
            .max_by_key(|&c| (size[c], std::cmp::Reverse(c)))
            .unwrap_or(0);
        for s in 0..nseeds as usize {
            let c = comp[s] as usize;
            if c == main || size[c] >= POCKET {
                continue;
            }
            self.free_flood(s as u32, &comp, main as u32, BUDGET);
        }
    }

    fn free_flood(&mut self, seed: u32, comp: &[u32], main: u32, budget: usize) {
        const DIRS: [[f32; 2]; 8] = [
            [1.0, 0.0],
            [-1.0, 0.0],
            [0.0, 1.0],
            [0.0, -1.0],
            [1.0, 1.0],
            [1.0, -1.0],
            [-1.0, 1.0],
            [-1.0, -1.0],
        ];
        const FINE: f32 = CELL * 0.5;
        let key = |p: Vec3| {
            (
                (p[0] / FINE).floor() as i32,
                (p[1] / FINE).floor() as i32,
                (p[2] / SAME_FLOOR).round() as i32,
            )
        };
        let mut fine: HashMap<(i32, i32, i32), u32, FastHash> = HashMap::default();
        fine.insert(key(self.pos[seed as usize]), seed);
        let mut queue = vec![seed];
        let mut head = 0;
        let mut made = 0;
        while head < queue.len() && made < budget {
            let cur = queue[head];
            head += 1;
            let p = self.pos[cur as usize];
            for d in DIRS {
                let (x, y) = (p[0] + d[0] * FINE, p[1] + d[1] * FINE);
                let Some((z, flags)) = self.walk(p, x, y, true) else {
                    continue;
                };
                let k = key([x, y, z]);
                let id = match fine.get(&k) {
                    Some(&id) => id,
                    None => {
                        let id = self.pos.len() as u32;
                        self.pos.push([x, y, z]);
                        self.next.push(u32::MAX);
                        fine.insert(k, id);
                        queue.push(id);
                        made += 1;
                        id
                    }
                };
                self.edges.push((cur, id, flags));
                if flags == 0 && (z - p[2]).abs() <= STEP {
                    self.edges.push((id, cur, 0));
                }
            }
            // Try to join the lattice around this node.
            let [cx, cy] = NavMesh::cell_of(p);
            let mut joined = false;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let c = (cx + dx, cy + dy);
                    let (x, y) = ((c.0 as f32 + 0.5) * CELL, (c.1 as f32 + 0.5) * CELL);
                    let Some((z, flags)) = self.walk(p, x, y, true) else {
                        continue;
                    };
                    let Some(n) = self.node(c, z) else { continue };
                    self.edges.push((cur, n, flags));
                    if flags == 0 && (z - p[2]).abs() <= STEP {
                        self.edges.push((n, cur, 0));
                        joined |= comp.get(n as usize) == Some(&main);
                    }
                }
            }
            if joined {
                return;
            }
        }
    }

    fn finish(self) -> NavMesh {
        build(
            self.pos,
            &self.edges,
            self.traces,
            self.seeds_used,
            self.seeds_dropped,
        )
    }
}

/// Edges grouped by source node: `(start, to, flags)` in CSR form.
fn csr(n: usize, edges: &[(u32, u32, u8)]) -> (Vec<u32>, Vec<u32>, Vec<u8>) {
    let mut start = vec![0u32; n + 1];
    for &(a, _, _) in edges {
        start[a as usize + 1] += 1;
    }
    for i in 0..n {
        start[i + 1] += start[i];
    }
    let mut fill = start.clone();
    let mut to = vec![0u32; edges.len()];
    let mut flags = vec![0u8; edges.len()];
    for &(a, b, f) in edges {
        let k = fill[a as usize] as usize;
        to[k] = b;
        flags[k] = f;
        fill[a as usize] += 1;
    }
    (start, to, flags)
}

/// CSR adjacency, component ids, cell index and stats from raw positions and edges.
fn build(
    pos: Vec<Vec3>,
    edges: &[(u32, u32, u8)],
    traces: u32,
    used: u32,
    dropped: u32,
) -> NavMesh {
    let n = pos.len();
    let (edge_start, edge_to, edge_flags) = csr(n, edges);
    let (comp, ncomp) = scc(n, &edge_start, &edge_to);
    let mut comp_start = vec![0u32; ncomp + 1];
    for &c in &comp {
        comp_start[c as usize + 1] += 1;
    }
    let mut main_comp = 0;
    for c in 0..ncomp {
        if comp_start[c + 1] > comp_start[main_comp + 1] {
            main_comp = c;
        }
    }
    let main_nodes = comp_start.get(main_comp + 1).copied().unwrap_or(0);
    for c in 0..ncomp {
        comp_start[c + 1] += comp_start[c];
    }
    let mut fill = comp_start.clone();
    let mut comp_nodes = vec![0u32; n];
    for (i, &c) in comp.iter().enumerate() {
        comp_nodes[fill[c as usize] as usize] = i as u32;
        fill[c as usize] += 1;
    }

    let (mut lo, mut hi) = ([i32::MAX; 2], [i32::MIN; 2]);
    for &p in &pos {
        let c = NavMesh::cell_of(p);
        for k in 0..2 {
            lo[k] = lo[k].min(c[k]);
            hi[k] = hi[k].max(c[k]);
        }
    }
    let (grid_min, grid_dim) = if n == 0 {
        ([0; 2], [0; 2])
    } else {
        (lo, [(hi[0] - lo[0] + 1) as u32, (hi[1] - lo[1] + 1) as u32])
    };
    let cells = grid_dim[0] as usize * grid_dim[1] as usize;
    let cell_index = |p: Vec3| {
        let c = NavMesh::cell_of(p);
        (c[1] - grid_min[1]) as usize * grid_dim[0] as usize + (c[0] - grid_min[0]) as usize
    };
    let mut grid_start = vec![0u32; cells + 1];
    for &p in &pos {
        grid_start[cell_index(p) + 1] += 1;
    }
    for i in 0..cells {
        grid_start[i + 1] += grid_start[i];
    }
    let mut fill = grid_start.clone();
    let mut grid_nodes = vec![0u32; n];
    for (i, &p) in pos.iter().enumerate() {
        let c = cell_index(p);
        grid_nodes[fill[c] as usize] = i as u32;
        fill[c] += 1;
    }

    let mut mesh = NavMesh {
        pos,
        edge_start,
        edge_to,
        edge_flags,
        comp,
        comp_start,
        comp_nodes,
        main_comp: main_comp as u32,
        seed_nodes: Vec::new(),
        grid_min,
        grid_dim,
        grid_start,
        grid_nodes,
        stats: NavStats::default(),
    };
    let bytes = mesh.pos.len() * 12
        + (mesh.edge_start.len()
            + mesh.edge_to.len()
            + mesh.comp.len()
            + mesh.comp_start.len()
            + mesh.comp_nodes.len()
            + mesh.grid_start.len()
            + mesh.grid_nodes.len())
            * 4
        + mesh.edge_flags.len();
    mesh.stats = NavStats {
        nodes: n as u32,
        edges: edges.len() as u32,
        components: ncomp as u32,
        main_component_nodes: main_nodes,
        seeds_used: used,
        seeds_dropped: dropped,
        traces,
        generation_ms: 0.0,
        bytes: bytes as u32,
    };
    mesh
}

/// Iterative Tarjan: component id per node and the component count.
fn scc(n: usize, start: &[u32], to: &[u32]) -> (Vec<u32>, usize) {
    const UNSEEN: u32 = u32::MAX;
    let mut index = vec![UNSEEN; n];
    let mut low = vec![0u32; n];
    let mut comp = vec![UNSEEN; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<u32> = Vec::new();
    let mut call: Vec<(u32, u32)> = Vec::new();
    let (mut counter, mut ncomp) = (0u32, 0u32);
    for root in 0..n as u32 {
        if index[root as usize] != UNSEEN {
            continue;
        }
        call.push((root, start[root as usize]));
        index[root as usize] = counter;
        low[root as usize] = counter;
        counter += 1;
        stack.push(root);
        on_stack[root as usize] = true;
        while let Some(&mut (v, ref mut e)) = call.last_mut() {
            let vi = v as usize;
            if *e < start[vi + 1] {
                let w = to[*e as usize];
                *e += 1;
                let wi = w as usize;
                if index[wi] == UNSEEN {
                    index[wi] = counter;
                    low[wi] = counter;
                    counter += 1;
                    stack.push(w);
                    on_stack[wi] = true;
                    call.push((w, start[wi]));
                } else if on_stack[wi] {
                    low[vi] = low[vi].min(index[wi]);
                }
            } else {
                call.pop();
                if low[vi] == index[vi] {
                    while let Some(w) = stack.pop() {
                        on_stack[w as usize] = false;
                        comp[w as usize] = ncomp;
                        if w == v {
                            break;
                        }
                    }
                    ncomp += 1;
                }
                if let Some(&(p, _)) = call.last() {
                    low[p as usize] = low[p as usize].min(low[vi]);
                }
            }
        }
    }
    (comp, ncomp as usize)
}

/// Walks a bot along a node path: where to steer, when a waypoint is reached, and whether
/// the bot has stopped making progress.
#[derive(Clone, Debug)]
pub struct Steer {
    /// Horizontal distance at which a waypoint counts as reached.
    pub reach_radius: f32,
    /// Ticks without closing on the waypoint before [`SteerOut::stuck`] is raised.
    pub stuck_ticks: u32,
    next: usize,
    best: f32,
    stalled: u32,
}

/// What the bot should do this tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SteerOut {
    /// Horizontal unit vector toward the waypoint; zero when arrived.
    pub dir: Vec3,
    pub waypoint: Vec3,
    /// Index of the waypoint in the path.
    pub index: usize,
    /// The final node is reached (or the path is empty).
    pub arrived: bool,
    /// No progress for [`Steer::stuck_ticks`]: repath, jump or give up, then [`Steer::reset`].
    pub stuck: bool,
    /// The waypoint is a ledge above the bot (a [`edge::JUMP`] link) or the bot is stuck.
    pub jump: bool,
    /// The link into the waypoint only admits the crouched hull.
    pub crouch: bool,
    /// The link into the waypoint is a ladder: walk into it, hold forward, look up.
    pub ladder: bool,
}

impl Default for Steer {
    fn default() -> Self {
        Self::new()
    }
}

impl Steer {
    pub fn new() -> Self {
        Self {
            reach_radius: 20.0,
            stuck_ticks: 45,
            next: 0,
            best: f32::INFINITY,
            stalled: 0,
        }
    }

    /// Starts over on a fresh path.
    pub fn reset(&mut self) {
        self.next = 0;
        self.best = f32::INFINITY;
        self.stalled = 0;
    }

    /// One tick: advance past reached waypoints and steer at the next.
    pub fn update(&mut self, mesh: &NavMesh, path: &[NodeId], pos: Vec3) -> SteerOut {
        let Some(last) = path.len().checked_sub(1) else {
            return SteerOut {
                dir: [0.0; 3],
                waypoint: pos,
                index: 0,
                arrived: true,
                stuck: false,
                jump: false,
                crouch: false,
                ladder: false,
            };
        };
        self.next = self.next.min(last);
        let mut arrived = false;
        let (wp, d) = loop {
            let wp = mesh.node_pos(path[self.next]);
            let d = dist_xy(pos, wp);
            if d < self.reach_radius && (wp[2] - pos[2]).abs() < STEP * 2.0 {
                if self.next == last {
                    arrived = true;
                    break (wp, d);
                }
                self.next += 1;
                self.best = f32::INFINITY;
                self.stalled = 0;
                continue;
            }
            break (wp, d);
        };
        if d < self.best - 1.0 {
            self.best = d;
            self.stalled = 0;
        } else {
            self.stalled += 1;
        }
        let stuck = !arrived && self.stalled >= self.stuck_ticks;
        let dir = if arrived || d < 1e-3 {
            [0.0; 3]
        } else {
            [(wp[0] - pos[0]) / d, (wp[1] - pos[1]) / d, 0.0]
        };
        let into = (self.next > 0)
            .then(|| mesh.edge_flags(path[self.next - 1], path[self.next]))
            .flatten()
            .unwrap_or(0);
        let crouch = into & edge::CROUCH != 0;
        SteerOut {
            dir,
            waypoint: wp,
            index: self.next,
            arrived,
            stuck,
            jump: !arrived
                && (stuck
                    || (wp[2] - pos[2] > 10.0 && d < CELL * 1.5)
                    || (into & (edge::JUMP | edge::MANTLE) != 0 && d < CELL * 1.5)),
            crouch,
            ladder: into & edge::LADDER != 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim::cm::Trace;

    /// Axis-aligned boxes swept by an axis-aligned hull; the floor is a box too.
    struct Boxes(Vec<(Vec3, Vec3)>);

    impl Collide for Boxes {
        fn trace(&self, s: Vec3, e: Vec3, mins: Vec3, maxs: Vec3, _: u16, _: i32) -> Trace {
            let mut out = Trace::MISS;
            let d = [e[0] - s[0], e[1] - s[1], e[2] - s[2]];
            for (lo, hi) in &self.0 {
                let lo = [lo[0] - maxs[0], lo[1] - maxs[1], lo[2] - maxs[2]];
                let hi = [hi[0] - mins[0], hi[1] - mins[1], hi[2] - mins[2]];
                if (0..3).all(|a| s[a] > lo[a] && s[a] < hi[a]) {
                    out.start_solid = true;
                    out.all_solid = true;
                    out.fraction = 0.0;
                    return out;
                }
                let (mut t0, mut t1, mut axis, mut sign) = (0.0f32, 1.0f32, 3, 0.0);
                let mut hit = true;
                for a in 0..3 {
                    if d[a] == 0.0 {
                        if s[a] <= lo[a] || s[a] >= hi[a] {
                            hit = false;
                        }
                        continue;
                    }
                    let (mut ta, mut tb) = ((lo[a] - s[a]) / d[a], (hi[a] - s[a]) / d[a]);
                    let sg = if d[a] > 0.0 { -1.0 } else { 1.0 };
                    if ta > tb {
                        std::mem::swap(&mut ta, &mut tb);
                    }
                    if ta > t0 {
                        t0 = ta;
                        axis = a;
                        sign = sg;
                    }
                    t1 = t1.min(tb);
                }
                if hit && t0 < t1 && axis < 3 && t0 < out.fraction {
                    out.fraction = t0;
                    out.normal = [0.0; 3];
                    out.normal[axis] = sign;
                }
            }
            out
        }

        fn point_contents(&self, _: Vec3, _: u16, _: i32) -> i32 {
            0
        }
    }

    const FLOOR: (Vec3, Vec3) = ([-1024.0, -1024.0, -64.0], [1024.0, 1024.0, 0.0]);

    fn walled(mut extra: Vec<(Vec3, Vec3)>) -> Boxes {
        // Room boundary so the flood fill terminates.
        for (lo, hi) in [
            ([-1024.0, -1024.0, 0.0], [-256.0, 1024.0, 400.0]),
            ([256.0, -1024.0, 0.0], [1024.0, 1024.0, 400.0]),
            ([-256.0, -1024.0, 0.0], [256.0, -256.0, 400.0]),
            ([-256.0, 256.0, 0.0], [256.0, 1024.0, 400.0]),
        ] {
            extra.push((lo, hi));
        }
        extra.push(FLOOR);
        Boxes(extra)
    }

    fn mesh(w: &Boxes, seeds: &[Vec3]) -> NavMesh {
        NavMesh::generate(w, seeds)
    }

    fn route(m: &NavMesh, a: Vec3, b: Vec3) -> Option<Vec<NodeId>> {
        let mut sc = PathScratch::new(m);
        let mut out = Vec::new();
        let (a, b) = (m.nearest_node(a)?, m.nearest_node(b)?);
        m.path(a, b, &mut sc, &mut out).then_some(out)
    }

    #[test]
    fn flat_room_is_one_component_covering_the_floor() {
        let m = mesh(&walled(vec![]), &[[0.0, 0.0, 0.0]]);
        let s = m.stats();
        assert_eq!(s.seeds_used, 1);
        assert_eq!(s.components, 1);
        assert!(s.nodes > 200, "{s:?}");
        let p = route(&m, [-200.0, -200.0, 0.0], [200.0, 200.0, 0.0]).unwrap();
        assert!(p.len() >= 12);
        for n in &p {
            assert_eq!(m.node_pos(*n)[2], 0.0);
        }
    }

    #[test]
    fn wall_forces_detour_and_a_gap_is_found() {
        // A wall across x=0 with a doorway at y in 96..160.
        let w = walled(vec![
            ([-8.0, -256.0, 0.0], [8.0, 96.0, 200.0]),
            ([-8.0, 160.0, 0.0], [8.0, 256.0, 200.0]),
        ]);
        let m = mesh(&w, &[[-100.0, 0.0, 0.0]]);
        let p = route(&m, [-100.0, 0.0, 0.0], [100.0, 0.0, 0.0]).unwrap();
        let through = p.iter().any(|n| {
            let q = m.node_pos(*n);
            q[0].abs() < 20.0 && q[1] > 96.0 && q[1] < 160.0
        });
        assert!(through, "path must use the doorway: {p:?}");
        // A straight line across the wall is not walkable.
        let a = m.nearest_node([-100.0, 0.0, 0.0]).unwrap();
        let b = m.nearest_node([100.0, 0.0, 0.0]).unwrap();
        assert!(!m.line_walkable(&w, a, b));
    }

    #[test]
    fn stairs_are_walked_up_and_down() {
        // Eight 12-unit risers, 32 deep each, over x 0..256.
        let steps = (0..8).map(|i| {
            let x = i as f32 * 32.0;
            ([x, -64.0, 0.0], [x + 32.0, 64.0, 12.0 * (i + 1) as f32])
        });
        let w = walled(steps.collect());
        let m = mesh(&w, &[[-100.0, 0.0, 0.0]]);
        let p = route(&m, [-100.0, 0.0, 0.0], [240.0, 0.0, 96.0]).expect("up the stairs");
        let top = m.node_pos(*p.last().unwrap());
        assert!((top[2] - 96.0).abs() < 1.0, "{top:?}");
        assert!(route(&m, [240.0, 0.0, 96.0], [-100.0, 0.0, 0.0]).is_some());
    }

    #[test]
    fn pit_is_avoided_and_ledge_drop_is_one_way() {
        // A 300-deep pit in the middle row; the south row keeps a path around it.
        let mut floor = vec![
            ([-1024.0, -1024.0, -400.0], [1024.0, 1024.0, -300.0]),
            ([-1024.0, -1024.0, -64.0], [-64.0, 1024.0, 0.0]),
            ([64.0, -1024.0, -64.0], [1024.0, 1024.0, 0.0]),
            ([-64.0, -1024.0, -64.0], [64.0, -128.0, 0.0]),
            ([-64.0, 128.0, -64.0], [64.0, 1024.0, 0.0]),
        ];
        let mut w = walled(vec![]);
        w.0.retain(|b| *b != FLOOR);
        w.0.append(&mut floor);
        let m = mesh(&w, &[[-100.0, 0.0, 0.0]]);
        let p = route(&m, [-100.0, 0.0, 0.0], [100.0, 0.0, 0.0]).unwrap();
        assert!(p.iter().all(|n| m.node_pos(*n)[2] == 0.0));
        assert!(p.iter().any(|n| m.node_pos(*n)[1].abs() > 128.0));
        // The pit floor is below MAX_DROP: never sampled.
        assert!((0..m.node_count()).all(|i| m.node_pos(NodeId(i as u32))[2] > -1.0));

        // A 100-unit ledge: one-way down.
        let w = walled(vec![([-256.0, -256.0, 0.0], [0.0, 256.0, 100.0])]);
        let m = mesh(&w, &[[100.0, 0.0, 0.0], [-100.0, 0.0, 100.0]]);
        let (up, down) = (
            m.nearest_node([-100.0, 0.0, 100.0]).unwrap(),
            m.nearest_node([100.0, 0.0, 0.0]).unwrap(),
        );
        let mut sc = PathScratch::new(&m);
        let mut out = Vec::new();
        assert!(m.path(up, down, &mut sc, &mut out));
        assert!(!m.path(down, up, &mut sc, &mut out) && out.is_empty());
        assert!(!m.reachable(up, down));
        assert!(
            (0..m.node_count())
                .flat_map(|i| m.neighbors(NodeId(i as u32)))
                .any(|(_, f)| f & edge::DROP != 0)
        );
    }

    #[test]
    fn low_ceiling_is_a_crouch_link() {
        // A 56-high ceiling over x > 0 only: crouch fits, standing does not.
        let w = walled(vec![([0.0, -256.0, 56.0], [256.0, 256.0, 100.0])]);
        let m = mesh(&w, &[[-100.0, 0.0, 0.0]]);
        let p = route(&m, [-100.0, 0.0, 0.0], [150.0, 0.0, 0.0]).expect("crouch under the slab");
        let crouched: Vec<_> = p
            .windows(2)
            .filter(|w| m.edge_flags(w[0], w[1]).unwrap() & edge::CROUCH != 0)
            .collect();
        assert!(!crouched.is_empty());
        assert!(
            crouched.iter().all(|w| m.node_pos(w[1])[0] > -16.0),
            "only links into the low area crouch"
        );
        // No smoothing across crouch links.
        let mut q = p.clone();
        m.smooth(&w, &mut q);
        assert!(q.windows(2).any(|w| {
            m.edge_flags(w[0], w[1])
                .is_some_and(|f| f & edge::CROUCH != 0)
        }));
    }

    #[test]
    fn nearest_prefers_the_matching_floor() {
        // Two storeys 120 apart.
        let w = walled(vec![([-256.0, -256.0, 120.0], [256.0, 256.0, 130.0])]);
        let m = mesh(&w, &[[0.0, 0.0, 0.0], [0.0, 0.0, 130.0]]);
        assert!(m.stats().components >= 2);
        let lo = m.nearest_node([0.0, 0.0, 2.0]).unwrap();
        let hi = m.nearest_node([0.0, 0.0, 128.0]).unwrap();
        assert!(m.node_pos(lo)[2] < 1.0 && m.node_pos(hi)[2] > 129.0);
        assert!(!m.reachable(lo, hi));
    }

    /// 0 -> 1 -> 2 -> 3 long way round, 0 -> 3 shortcut costing more than walking only
    /// when the cap bites; ties between two equal detours resolve to the lower id.
    fn diamond() -> NavMesh {
        let pos = vec![
            [0.0, 0.0, 0.0],
            [100.0, 100.0, 0.0],
            [100.0, -100.0, 0.0],
            [200.0, 0.0, 0.0],
            [500.0, 500.0, 0.0],
        ];
        let e = |a, b| [(a, b, 0), (b, a, 0)];
        let edges: Vec<_> = [e(0, 1), e(0, 2), e(1, 3), e(2, 3)].concat();
        NavMesh::from_graph(pos, &edges)
    }

    #[test]
    fn astar_is_deterministic_capped_and_reusable() {
        let m = diamond();
        let mut sc = PathScratch::new(&m);
        let mut out = Vec::new();
        for _ in 0..3 {
            assert!(m.path(NodeId(0), NodeId(3), &mut sc, &mut out));
            assert_eq!(out, [NodeId(0), NodeId(1), NodeId(3)]);
        }
        assert!(!m.path(NodeId(0), NodeId(4), &mut sc, &mut out));
        sc.max_cost = 250.0;
        assert!(!m.path(NodeId(0), NodeId(3), &mut sc, &mut out));
        sc.max_cost = 300.0;
        assert!(m.path(NodeId(0), NodeId(3), &mut sc, &mut out));
        assert!(m.path(NodeId(3), NodeId(3), &mut sc, &mut out) && out == [NodeId(3)]);
        assert_eq!(m.stats().components, 2);
        assert!(m.reachable(NodeId(0), NodeId(3)) && !m.reachable(NodeId(0), NodeId(4)));
    }

    #[test]
    fn astar_takes_cheaper_detour_over_costly_crouch() {
        // Direct 0->1 is crouch (x1.5 = 150); the 0->2->1 detour is 2 * 72 = 144.
        let pos = vec![[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [50.0, 52.0, 0.0]];
        let m = NavMesh::from_graph(pos, &[(0, 1, edge::CROUCH), (0, 2, 0), (2, 1, 0)]);
        let (mut sc, mut out) = (PathScratch::new(&m), Vec::new());
        assert!(m.path(NodeId(0), NodeId(1), &mut sc, &mut out));
        assert_eq!(out, [NodeId(0), NodeId(2), NodeId(1)]);
    }

    #[test]
    fn steer_advances_flags_crouch_and_detects_stuck() {
        let pos = vec![[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [200.0, 0.0, 0.0]];
        let m = NavMesh::from_graph(pos, &[(0, 1, 0), (1, 2, edge::CROUCH)]);
        let path = [NodeId(0), NodeId(1), NodeId(2)];
        let mut s = Steer::new();
        let o = s.update(&m, &path, [0.0, 0.0, 0.0]);
        assert_eq!((o.index, o.dir), (1, [1.0, 0.0, 0.0]));
        assert!(!o.crouch);
        let o = s.update(&m, &path, [90.0, 0.0, 0.0]);
        assert_eq!(o.index, 2);
        assert!(o.crouch && !o.arrived);
        let o = s.update(&m, &path, [195.0, 0.0, 0.0]);
        assert!(o.arrived && o.dir == [0.0; 3]);

        let mut s = Steer::new();
        let mut o = s.update(&m, &path, [50.0, 0.0, 0.0]);
        for _ in 0..s.stuck_ticks {
            assert!(!o.stuck);
            o = s.update(&m, &path, [50.0, 0.0, 0.0]);
        }
        assert!(o.stuck && o.jump);
        s.reset();
        assert!(!s.update(&m, &path, [50.0, 0.0, 0.0]).stuck);
    }

    #[test]
    fn spawn_entities_are_picked_by_class() {
        let ents = br#"
{ "classname" "worldspawn" }
{ "classname" "mp_tdm_spawn" "origin" "1 2 3" }
{ "classname" "mp_global_intermission" "origin" "4 5 6" }
{ "classname" "mp_tdm_spawn_axis_start" "origin" "7 8 9" }
{ "classname" "trigger_multiple" "origin" "0 0 0" }
{ "classname" "mp_tdm_spawn" }
"#;
        let p = spawn_points(ents);
        let classes: Vec<_> = p.iter().map(|s| s.class.as_str()).collect();
        assert_eq!(
            classes,
            [
                "mp_tdm_spawn",
                "mp_global_intermission",
                "mp_tdm_spawn_axis_start"
            ]
        );
        assert_eq!(p[2].origin, [7.0, 8.0, 9.0]);
    }
}
