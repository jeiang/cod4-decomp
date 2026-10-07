// SPDX-License-Identifier: GPL-3.0-or-later
//! The `modelLightingSampler` volume: a CPU-built 3D texture holding one 4x4x4
//! block of light-grid colours per lit model.
//!
//! Layout (all facts from the original engine's observable behaviour):
//! * The volume is `256 x H x 4` texels, `H = total_entries / 16`.
//! * Lighting entry `e` (handle `e + 1`) owns the block at
//!   `x = 4 * (e & 63)`, `y = 4 * (e >> 6)`, `z = 0..4`.
//! * Texel format is `D3DFMT_A8R8G8B8`: bytes in memory are `[b, g, r, a]`.
//!   RGB come from the light-grid colour set, alpha is the primary-light
//!   (sun) visibility weight.
//! * A light-grid colour set is 56 RGB triples (168 bytes); the 64 texels of a
//!   block are filled from it through [`TEXEL_COLOR`] (8 of the 64 texels
//!   repeat a colour so the 4x4x4 trilinear footprint stays continuous).
//! * A static model with non-zero `ground_lighting` fills all 64 texels with
//!   that packed BGRA value instead.
//!
//! Two collision-dependent rules of the original apply when the caller supplies
//! the data: a grid corner flagged `needs_trace` only counts when a sight line from
//! the sample to it is clear (`R_IsValidLightGridSample`, through [`SightTrace`]),
//! and a corner that does not carry the primary light only reduces the light's
//! visible weight when the light could reach it
//! (`R_CanLightInfluenceLightGridCorner`, through the map's primary lights).
//! [`LightingEnv::none`] skips both.

use assets::zone::gfxworld::{GfxWorld, LightGrid};
use assets::zone::world::ComPrimaryLight;

/// `CM_BoxSightTrace`: is anything solid between two points.
pub trait SightTrace {
    fn blocked(&self, from: [f32; 3], to: [f32; 3]) -> bool;
}

impl SightTrace for sim::cm::CollisionWorld {
    /// `SOLID | CLIPSHOT`, the mask the original's grid-sample test passes.
    fn blocked(&self, from: [f32; 3], to: [f32; 3]) -> bool {
        self.sight_trace(0, from, to, [0.0; 3], [0.0; 3], &sim::cm::ClipModel::World, 0x2001) != 0
    }
}

/// What lighting a point may consult besides the grid itself.
#[derive(Clone, Copy)]
pub struct LightingEnv<'a> {
    pub sight: Option<&'a dyn SightTrace>,
    /// The map's primary lights, indexed like the grid's `primary_light_index`.
    pub lights: &'a [ComPrimaryLight],
}

impl LightingEnv<'_> {
    /// No collision and no lights: every corner sample is visible and every light influences every corner.
    pub const fn none() -> LightingEnv<'static> {
        LightingEnv {
            sight: None,
            lights: &[],
        }
    }
}

/// Volume width in texels (fixed by the shader constants).
pub const WIDTH: u32 = 256;
/// Volume depth in texels.
pub const DEPTH: u32 = 4;

/// Position of the grid origin: world coordinate `-131072` on every axis.
const GRID_ORIGIN: f32 = -131072.0;
/// Corner weights below this are dropped (`EQUAL_EPSILON`).
const WEIGHT_EPSILON: f32 = 0.001;

/// Which light-grid colour fills texel `z * 16 + y * 4 + x` of a block.
const TEXEL_COLOR: [u8; 64] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, //
    16, 17, 18, 19, 20, 0, 3, 21, 22, 12, 15, 23, 24, 25, 26, 27, //
    28, 29, 30, 31, 32, 40, 43, 33, 34, 52, 55, 35, 36, 37, 38, 39, //
    40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55,
];

/// Bytes of one light-grid colour set (56 RGB triples).
const COLORS_SIZE: usize = 168;

/// Texel format of [`ModelLighting::texels`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TexelFormat {
    /// Bytes `[b, g, r, a]` per texel: `wgpu::TextureFormat::Bgra8Unorm`
    /// (the original `D3DFMT_A8R8G8B8`), sampled as non-sRGB.
    Bgra8,
}

/// Result of [`light_grid_lookup`].
#[derive(Clone, Debug, PartialEq)]
pub struct GridLookup {
    /// Trilinear weight per corner. Corner index = `row * 4 + col * 2 + z`,
    /// where `row`/`col` follow the grid's row/column axes.
    pub weights: [f32; 8],
    /// Index into [`LightGrid::entries`] per corner; `None` for corners
    /// outside the grid, in empty cells, or with negligible weight.
    pub entries: [Option<u32>; 8],
    /// Primary light chosen from the surviving corners (255 = none/sun).
    pub primary_light: u8,
    /// Colour index to extrapolate with when no corner survives (0 or 1).
    pub default_entry: u32,
}

/// One decoded `GfxLightGridEntry`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridEntry {
    pub colors_index: u16,
    pub primary_light_index: u8,
    pub needs_trace: u8,
}

/// Decode entry `index` of the grid.
pub fn grid_entry(grid: &LightGrid, index: u32) -> Option<GridEntry> {
    let b = grid
        .entries
        .get(index as usize * 4..index as usize * 4 + 4)?;
    Some(GridEntry {
        colors_index: u16::from_le_bytes([b[0], b[1]]),
        primary_light_index: b[2],
        needs_trace: b[3],
    })
}

/// Cell coordinates of the grid corner at or below `pos` (32 x 32 x 64 cells).
fn cell_of(pos: [f32; 3]) -> [u32; 3] {
    let c = |v: f32, shift: u32| ((v.floor() as i32).wrapping_add(0x20000) >> shift) as u32;
    [c(pos[0], 5), c(pos[1], 5), c(pos[2], 6)]
}

/// The four entries of one `(row, col, z..z+1)` quad: `[col z, col z+1,
/// col+1 z, col+1 z+1]`. Clears `default` when the point lies below the
/// populated z range (the original's "missing grid" distinction).
fn entry_quad(grid: &LightGrid, pos: [u32; 3], default: &mut u32) -> [Option<u32>; 4] {
    const NONE: [Option<u32>; 4] = [None; 4];
    let (ra, ca) = (grid.row_axis as usize, grid.col_axis as usize);
    if ra > 2 || ca > 2 {
        return NONE;
    }
    let row_index = pos[ra].wrapping_sub(u32::from(grid.mins[ra]));
    if row_index >= u32::from(grid.maxs[ra]) + 1 - u32::from(grid.mins[ra]) {
        return NONE;
    }
    let Some(&start) = grid.row_data_start.get(row_index as usize) else {
        return NONE;
    };
    if start == 0xFFFF {
        return NONE;
    }
    let base = usize::from(start) * 4;
    let Some(head) = grid.raw_row_data.get(base..base + 12) else {
        return NONE;
    };
    let u16_at = |i: usize| u32::from(u16::from_le_bytes([head[i], head[i + 1]]));
    let (col_start, col_count, z_start, z_count) = (u16_at(0), u16_at(2), u16_at(4), u16_at(6));
    let first_entry = u32::from_le_bytes([head[8], head[9], head[10], head[11]]);

    let mut col_index = pos[ca].wrapping_sub(col_start);
    let z = pos[2].wrapping_sub(z_start);
    if col_index.wrapping_add(1) > col_count {
        return NONE;
    }
    if z.wrapping_add(1) > z_count {
        if pos[2] < z_start {
            *default = 0;
        }
        return NONE;
    }

    // Run-length data after the header: per column run
    // `[columns, zCount, zBase(, zBase hi if the row is wider than 255)]`;
    // a run with zCount 0 is just `[columns, 0]`.
    let rle = &grid.raw_row_data[base + 12..];
    let byte = |i: usize| u32::from(rle.get(i).copied().unwrap_or(0));
    let wide = z_count > 255;
    let full = 3 + usize::from(wide);
    let z_base = |at: usize| byte(at + 2) + if wide { byte(at + 3) << 8 } else { 0 };
    let entry = |i: u32| (i < grid.entry_count).then_some(i);
    // Entry `rel` (and `rel + 1`) of a run's column, if inside its z range.
    let pair = |first: u32, count: u32, rel: u32| {
        [
            (rel < count)
                .then(|| entry(first.wrapping_add(rel)))
                .flatten(),
            (rel.wrapping_add(1) < count)
                .then(|| entry(first.wrapping_add(rel).wrapping_add(1)))
                .flatten(),
        ]
    };
    let mut first = first_entry;
    let mut out = NONE;

    if col_index == u32::MAX {
        // One column left of the first run: only the right-hand pair exists.
        let (count, base_z) = (byte(1), z_base(0));
        if z < base_z {
            *default = 0;
        }
        let [a, b] = pair(first, count, z.wrapping_sub(base_z));
        out[2] = a;
        out[3] = b;
        return out;
    }

    let mut at = 0usize;
    while col_index >= byte(at) {
        if at >= rle.len() {
            return NONE;
        }
        col_index -= byte(at);
        first = first.wrapping_add(byte(at + 1) * byte(at));
        at += if byte(at + 1) != 0 { full } else { 2 };
    }
    let (cols, zc) = (byte(at), byte(at + 1));
    if zc != 0 {
        let zb = z_base(at);
        if z < zb {
            *default = 0;
        }
        let rel = z.wrapping_sub(zb);
        let col_first = first.wrapping_add(col_index * zc);
        let [a, b] = pair(col_first, zc, rel);
        out[0] = a;
        out[1] = b;
        if col_index + 1 < cols {
            let [a, b] = pair(col_first.wrapping_add(zc), zc, rel);
            out[2] = a;
            out[3] = b;
            return out;
        }
    } else {
        if byte(at + 3) != 0 && z < z_base(at + 2) + byte(at + 3) {
            *default = 0;
        }
        if col_index + 1 < cols {
            return out;
        }
    }
    // The next column belongs to the following run.
    if pos[ca].wrapping_add(1) == col_count.wrapping_add(col_start) {
        return out;
    }
    let first2 = first.wrapping_add(zc * cols);
    let next = at + if zc != 0 { full } else { 2 };
    let rel = z.wrapping_sub(z_base(next));
    let [a, b] = pair(first2, byte(next + 1), rel);
    out[2] = a;
    out[3] = b;
    out
}

/// Trilinear light-grid lookup at a world position (`R_LightGridLookup`).
pub fn light_grid_lookup(
    grid: &LightGrid,
    pos: [f32; 3],
    sight: Option<&dyn SightTrace>,
) -> GridLookup {
    let mut cell = cell_of(pos);
    let base_cell = cell;
    let (ra, ca) = (grid.row_axis.min(2) as usize, grid.col_axis.min(2) as usize);
    let lerp = |v: f32, scale: f32, c: u32| (v - GRID_ORIGIN) * scale - c as i32 as f32;
    let (lx, ly, lz) = (
        lerp(pos[ra], 0.03125, cell[ra]),
        lerp(pos[ca], 0.03125, cell[ca]),
        lerp(pos[2], 0.015625, cell[2]),
    );

    let mut weights = [0.0f32; 8];
    for (i, w) in weights.iter_mut().enumerate() {
        let side = |bit: usize, t: f32| if i & bit != 0 { t } else { 1.0 - t };
        *w = side(4, lx) * side(2, ly) * side(1, lz);
    }

    let mut default_entry = 1;
    let mut entries = [None; 8];
    entries[..4].copy_from_slice(&entry_quad(grid, cell, &mut default_entry));
    cell[ra] = cell[ra].wrapping_add(1);
    entries[4..].copy_from_slice(&entry_quad(grid, cell, &mut default_entry));

    // Choose the primary light: the first live corner that passes its sight test seeds it (earlier corners that
    // failed are dropped); later corners that fail are dropped, the others replace it by weight / "has a real
    // light" precedence. Until one corner passes, failed corners still take part.
    let mut primary = 0u8;
    let mut best = 0.0f32;
    let mut seeded = false;
    for i in 0..8 {
        let Some(idx) = entries[i] else { continue };
        if weights[i] < WEIGHT_EPSILON {
            entries[i] = None;
            continue;
        }
        let Some(e) = grid_entry(grid, idx) else {
            entries[i] = None;
            continue;
        };
        let suppressed = sight.is_some_and(|s| {
            trace_corners(grid, e.needs_trace) & (1 << i) != 0
                && !valid_sample(grid, base_cell, i, pos, s)
        });
        if suppressed {
            if seeded {
                entries[i] = None;
                continue;
            }
        } else if !seeded {
            seeded = true;
            best = weights[i];
            primary = e.primary_light_index;
            entries[..i].fill(None);
            continue;
        }
        let p = e.primary_light_index;
        let replace = if primary == 0 {
            true
        } else if p == 0 {
            false
        } else {
            primary == 255 || (p != 255 && f64::from(weights[i]) > f64::from(best))
        };
        if replace {
            best = weights[i];
            primary = p;
        }
    }
    GridLookup {
        weights,
        entries,
        primary_light: primary,
        default_entry,
    }
}

/// The corners (bit `row * 4 + col * 2 + z`) whose samples need a sight test: the entry stores them in
/// world-axis order (x, y, z), the lookup indexes them by row axis.
fn trace_corners(grid: &LightGrid, raw: u8) -> u8 {
    const BY_ROW_X: [u8; 8] = [0, 4, 2, 6, 1, 5, 3, 7];
    const BY_ROW_Y: [u8; 8] = [0, 2, 4, 6, 1, 3, 5, 7];
    let map = if grid.row_axis == 0 { BY_ROW_X } else { BY_ROW_Y };
    (0..8).fold(0u8, |m, c| {
        if raw & (1 << c) != 0 {
            m | 1 << map[c]
        } else {
            m
        }
    })
}

/// World position of corner `corner` of the cell `cell`.
fn corner_position(grid: &LightGrid, cell: [u32; 3], corner: usize) -> [f32; 3] {
    let mut p = [
        cell[0] as f32 * 32.0 + GRID_ORIGIN,
        cell[1] as f32 * 32.0 + GRID_ORIGIN,
        cell[2] as f32 * 64.0 + GRID_ORIGIN,
    ];
    let (ra, ca) = (grid.row_axis.min(2) as usize, grid.col_axis.min(2) as usize);
    p[ra] += if corner & 4 != 0 { 32.0 } else { 0.0 };
    p[ca] += if corner & 2 != 0 { 32.0 } else { 0.0 };
    p[2] += if corner & 1 != 0 { 64.0 } else { 0.0 };
    p
}

/// `R_IsValidLightGridSample`: can the sample at `pos` see the grid corner, from just in front of the corner.
fn valid_sample(
    grid: &LightGrid,
    cell: [u32; 3],
    corner: usize,
    pos: [f32; 3],
    sight: &dyn SightTrace,
) -> bool {
    let g = corner_position(grid, cell, corner);
    let d = [pos[0] - g[0], pos[1] - g[1], pos[2] - g[2]];
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let k = if len > 0.0 { 0.01 / len } else { 0.0 };
    let nudged = [g[0] + d[0] * k, g[1] + d[1] * k, g[2] + d[2] * k];
    !sight.blocked(pos, nudged)
}

/// `Com_CanPrimaryLightAffectPoint`: whether a spot or omni light reaches `p`.
fn light_reaches(l: &ComPrimaryLight, p: [f32; 3]) -> bool {
    let d = [l.origin[0] - p[0], l.origin[1] - p[1], l.origin[2] - p[2]];
    let dist_sq = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
    if dist_sq >= l.radius * l.radius {
        return false;
    }
    // Omni lights, and spots that may turn far enough to face anywhere, reach everything in range.
    if l.kind != 3 || l.rotation_limit <= -l.cos_half_fov_outer {
        return true;
    }
    let along = d[0] * l.dir[0] + d[1] * l.dir[1] + d[2] * l.dir[2];
    let cos_half = if l.rotation_limit == 1.0 {
        l.cos_half_fov_outer
    } else {
        // Cosine of the sum of the two angles.
        let (c0, c1) = (l.cos_half_fov_outer, l.rotation_limit);
        let c = c0 * c1 - ((1.0 - c0 * c0) * (1.0 - c1 * c1)).sqrt();
        if c <= 0.0 {
            return along <= c * l.radius;
        }
        c
    };
    along > 0.0 && cos_half * cos_half * dist_sq <= along * along
}

/// `R_CanLightInfluenceLightGridCorner`: whether `light` (a primary light of the map) can reach corner `corner` of
/// the cell holding `pos`. Directional lights reach everywhere.
fn can_influence(grid: &LightGrid, l: &ComPrimaryLight, pos: [f32; 3], corner: usize) -> bool {
    if l.kind == 1 {
        return true;
    }
    let cell = [
        (pos[0] * 0.03125).floor() * 32.0,
        (pos[1] * 0.03125).floor() * 32.0,
        (pos[2] * 0.015625).floor() * 64.0,
    ];
    let (ra, ca) = (grid.row_axis.min(2) as usize, grid.col_axis.min(2) as usize);
    let mut p = cell;
    p[ra] += if corner & 4 != 0 { 32.0 } else { 0.0 };
    p[ca] += if corner & 2 != 0 { 32.0 } else { 0.0 };
    p[2] += if corner & 1 != 0 { 64.0 } else { 0.0 };
    light_reaches(l, p)
}

/// What to do when a point lies outside the populated grid.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Extrapolate {
    /// Static models: use the grid's default entry.
    Default,
    /// Dynamic models: points *below* the grid get the last ("missing")
    /// colour set so they stand out (`r_showMissingLightGrid` defaults on).
    ShowMissing,
}

/// A resolved block fill (`GfxModelLightingPatch`).
struct Patch {
    colors: [u16; 8],
    /// Fixed-point blend weights summing to 256.
    weights: [u16; 8],
    count: usize,
    /// Alpha channel: primary-light visibility, 0..=255.
    primary_weight: u8,
}

impl Patch {
    fn single(color: u32, primary_weight: f32) -> Patch {
        let mut colors = [0; 8];
        colors[0] = color as u16;
        let mut weights = [0; 8];
        weights[0] = 256;
        Patch {
            colors,
            weights,
            count: 1,
            primary_weight: (primary_weight * 255.0 + 0.5) as u8,
        }
    }
}

/// `R_GetLightingAtPoint`: returns the patch and the effective primary light.
fn lighting_at_point(
    grid: &LightGrid,
    pos: [f32; 3],
    non_sun_primary: u32,
    mode: Extrapolate,
    env: &LightingEnv,
) -> (Patch, u8) {
    let lookup = light_grid_lookup(grid, pos, env.sight);
    let sun = grid.sun_primary_light_index;
    let mut primary = u32::from(lookup.primary_light);
    if primary == 255 {
        primary = sun & 0xFF;
    } else if grid.has_light_regions && primary != sun {
        primary = non_sun_primary;
    }

    let mut colors = [0u16; 8];
    let mut cweights = [0.0f32; 8];
    let mut count = 0usize;
    let (mut max_w, mut visible, mut occluded) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..8 {
        let Some(e) = lookup.entries[i].and_then(|idx| grid_entry(grid, idx)) else {
            continue;
        };
        let w = lookup.weights[i];
        let p = u32::from(e.primary_light_index);
        if p == primary {
            visible += w;
        } else if (p == 0 || (p == 255 && primary != 0))
            && env
                .lights
                .get(primary as usize)
                .is_none_or(|l| can_influence(grid, l, pos, i))
        {
            occluded += w;
        }
        max_w += w;
        match colors[..count].iter().position(|&c| c == e.colors_index) {
            Some(s) => cweights[s] += w,
            None => {
                colors[count] = e.colors_index;
                cweights[count] = w;
                count += 1;
            }
        }
    }

    if count == 0 {
        let missing = mode == Extrapolate::ShowMissing && lookup.default_entry == 0;
        if missing || grid.color_count <= lookup.default_entry {
            let last = grid.color_count.saturating_sub(1);
            return (Patch::single(last, 1.0), 0);
        }
        return (Patch::single(lookup.default_entry, 1.0), (sun & 0xFF) as u8);
    }

    let primary_weight = if primary == 0 {
        0.0
    } else if occluded == 0.0 {
        if visible != 0.0 { 1.0 } else { visible }
    } else {
        visible / (visible + occluded)
    };
    if count == 1 {
        return (
            Patch::single(u32::from(colors[0]), primary_weight),
            primary as u8,
        );
    }

    // Fixed-point weights summing to exactly 256; the largest takes the slack.
    let scale = 1.0 / max_w;
    let mut weights = [0u16; 8];
    let (mut sum, mut max_i) = (0u16, 0usize);
    for i in 0..count {
        weights[i] = (scale * 256.0 * cweights[i] + 0.5) as i32 as u16;
        sum = sum.wrapping_add(weights[i]);
        if weights[max_i] < weights[i] {
            max_i = i;
        }
    }
    weights[max_i] = weights[max_i].wrapping_add(256u16.wrapping_sub(sum));
    let patch = Patch {
        colors,
        weights,
        count,
        primary_weight: (primary_weight * 255.0 + 0.5) as u8,
    };
    (patch, primary as u8)
}

/// The model-lighting volume and its entry bookkeeping.
pub struct ModelLighting {
    height: u32,
    texels: Vec<u8>,
    static_count: u32,
    total_entries: u32,
    next_dynamic: u32,
    /// Effective primary light per entry.
    primary: Vec<u8>,
}

impl ModelLighting {
    /// Build the volume for `world`: every static model is lit once (handle
    /// `smodel index + 1`); `entry_capacity` more entries are reserved for
    /// [`alloc_point`](Self::alloc_point).
    pub fn new(world: &GfxWorld, entry_capacity: u32, env: &LightingEnv) -> ModelLighting {
        let static_count = world.dpvs.smodel_draw_insts.len() as u32;
        let total = (static_count + entry_capacity).next_power_of_two().max(64);
        assert!(total <= u32::from(u16::MAX), "too many lighting entries");
        let height = total / 16;
        let mut me = ModelLighting {
            height,
            texels: vec![0; (WIDTH * height * DEPTH * 4) as usize],
            static_count,
            total_entries: total,
            next_dynamic: static_count,
            primary: vec![0; total as usize],
        };
        let grid = &world.light_grid;
        for (i, (inst, draw)) in world
            .dpvs
            .smodel_insts
            .iter()
            .zip(&world.dpvs.smodel_draw_insts)
            .enumerate()
        {
            let entry = i as u32;
            if inst.ground_lighting != 0 {
                me.fill_ground(entry, inst.ground_lighting.to_le_bytes());
                me.primary[i] = draw.primary_light_index;
            } else {
                let centre = [0, 1, 2].map(|a| (inst.mins[a] + inst.maxs[a]) * 0.5);
                let (patch, light) = lighting_at_point(
                    grid,
                    centre,
                    u32::from(draw.primary_light_index),
                    Extrapolate::Default,
                    env,
                );
                me.apply(grid, entry, &patch);
                me.primary[i] = light;
            }
        }
        me
    }

    /// `(256, H, 4)`.
    pub fn size(&self) -> (u32, u32, u32) {
        (WIDTH, self.height, DEPTH)
    }

    /// Tightly packed texels (rows, then slices), in [`format`](Self::format).
    pub fn texels(&self) -> &[u8] {
        &self.texels
    }

    /// Texel format of [`texels`](Self::texels).
    pub fn format(&self) -> TexelFormat {
        TexelFormat::Bgra8
    }

    /// `BASE_LIGHTING_COORDS` for a handle: the centre of its block.
    pub fn base_coords(&self, handle: u16) -> [f32; 4] {
        let e = self.entry_of(handle);
        [
            (4 * (e & 63) + 2) as f32 / WIDTH as f32,
            (4 * (e >> 6) + 2) as f32 / self.height as f32,
            0.5,
            1.0,
        ]
    }

    /// `LIGHTING_LOOKUP_SCALE`.
    pub fn lookup_scale(&self) -> [f32; 4] {
        [0.005859375, 1.5 / self.height as f32, 0.375, 0.0]
    }

    /// Handle of static model `smodel_index`.
    pub fn static_model_handle(&self, smodel_index: usize) -> u16 {
        assert!((smodel_index as u32) < self.static_count);
        smodel_index as u16 + 1
    }

    /// Effective primary light of a handle's entry.
    pub fn primary_light(&self, handle: u16) -> u8 {
        self.primary[self.entry_of(handle) as usize]
    }

    /// Light a dynamic model at a world point; `None` when the reserved
    /// dynamic entries are used up. `non_sun_primary_light` is the model's
    /// non-sun primary light index (used when the grid has light regions).
    pub fn alloc_point(
        &mut self,
        grid: &LightGrid,
        origin: [f32; 3],
        non_sun_primary_light: u32,
        env: &LightingEnv,
    ) -> Option<u16> {
        if self.next_dynamic >= self.total_entries {
            return None;
        }
        let entry = self.next_dynamic;
        self.next_dynamic += 1;
        let (patch, light) = lighting_at_point(
            grid,
            origin,
            non_sun_primary_light,
            Extrapolate::ShowMissing,
            env,
        );
        self.apply(grid, entry, &patch);
        self.primary[entry as usize] = light;
        Some(entry as u16 + 1)
    }

    /// Release every dynamic entry (call once per frame).
    pub fn reset_dynamic(&mut self) {
        self.next_dynamic = self.static_count;
    }

    fn entry_of(&self, handle: u16) -> u32 {
        assert!(handle != 0 && u32::from(handle) <= self.total_entries);
        u32::from(handle) - 1
    }

    /// Byte offset of sample `s` (0..64) of entry `e`'s block.
    fn texel_offset(&self, e: u32, s: usize) -> usize {
        let x = 4 * (e & 63) + (s as u32 & 3);
        let y = 4 * (e >> 6) + ((s as u32 >> 2) & 3);
        let z = s as u32 >> 4;
        (((z * self.height + y) * WIDTH + x) * 4) as usize
    }

    fn fill_ground(&mut self, e: u32, bgra: [u8; 4]) {
        for s in 0..64 {
            let o = self.texel_offset(e, s);
            self.texels[o..o + 4].copy_from_slice(&bgra);
        }
    }

    fn apply(&mut self, grid: &LightGrid, e: u32, patch: &Patch) {
        let set = |i: u16| {
            let at = usize::from(i) * COLORS_SIZE;
            grid.colors
                .get(at..at + COLORS_SIZE)
                .map_or([0u8; COLORS_SIZE], |c| c.try_into().unwrap())
        };
        let colors = if patch.count == 1 {
            set(patch.colors[0])
        } else {
            let mut acc = [0u16; COLORS_SIZE];
            for k in 0..patch.count {
                let c = set(patch.colors[k]);
                for (a, &v) in acc.iter_mut().zip(&c) {
                    *a = a.wrapping_add(patch.weights[k].wrapping_mul(u16::from(v)));
                }
            }
            acc.map(|a| (a.wrapping_add(127) >> 8) as u8)
        };
        for (s, &ci) in TEXEL_COLOR.iter().enumerate() {
            let c = &colors[usize::from(ci) * 3..usize::from(ci) * 3 + 3];
            let o = self.texel_offset(e, s);
            self.texels[o..o + 4].copy_from_slice(&[c[2], c[1], c[0], patch.primary_weight]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2x2x2 grid at cell (4096, 4096, 2048) = world origin, row axis x.
    /// Entry `i` (corner order row, col, z) uses colour `i`, all bytes `10 * i`.
    fn grid_2x2x2() -> LightGrid {
        let mut raw = Vec::new();
        for first in [0u32, 4] {
            for v in [4096u16, 2, 2048, 2] {
                // colStart, colCount, zStart, zCount
                raw.extend_from_slice(&v.to_le_bytes());
            }
            raw.extend_from_slice(&first.to_le_bytes());
            raw.extend_from_slice(&[2, 2, 0, 0]); // one run: 2 cols, 2 z, base 0 (+pad)
        }
        let mut entries = Vec::new();
        for i in 0..8u16 {
            entries.extend_from_slice(&i.to_le_bytes());
            entries.extend_from_slice(&[255, 0]);
        }
        let colors = (0..8u8).flat_map(|k| [10 * k; COLORS_SIZE]).collect();
        LightGrid {
            has_light_regions: false,
            sun_primary_light_index: 0,
            mins: [4096, 4096, 0],
            maxs: [4097, 4097, 0],
            row_axis: 0,
            col_axis: 1,
            row_data_start: vec![0, 4],
            raw_row_data: raw,
            entry_count: 8,
            entries,
            color_count: 8,
            colors,
        }
    }

    #[test]
    fn synthetic_corners_and_centre() {
        let g = grid_2x2x2();
        let l = light_grid_lookup(&g, [0.0, 0.0, 0.0], None);
        assert_eq!(l.entries[0], Some(0));
        assert!(l.entries[1..].iter().all(Option::is_none));
        // Far corner (row+1, col+1, z+1) is corner 7 / entry 7.
        let l = light_grid_lookup(&g, [31.9999, 31.9999, 63.9999], None);
        assert_eq!(l.entries[7], Some(7));
        assert_eq!(l.entries.iter().flatten().count(), 1);

        let l = light_grid_lookup(&g, [16.0, 16.0, 32.0], None);
        assert_eq!(l.entries, [0, 1, 2, 3, 4, 5, 6, 7].map(Some));
        assert!(l.weights.iter().all(|&w| (w - 0.125).abs() < 1e-6));

        let (patch, _) = lighting_at_point(&g, [16.0, 16.0, 32.0], 0, Extrapolate::Default, &LightingEnv::none());
        assert_eq!(patch.count, 8);
        assert_eq!(
            patch.weights.iter().map(|&w| u32::from(w)).sum::<u32>(),
            256
        );
        let (patch, _) = lighting_at_point(&g, [0.0, 0.0, 0.0], 0, Extrapolate::Default, &LightingEnv::none());
        assert_eq!((patch.count, patch.colors[0]), (1, 0));
    }

    /// `grid_2x2x2` with every corner flagged for a sight test and the given primary lights per corner.
    fn traced_grid(lights: [u8; 8]) -> LightGrid {
        let mut g = grid_2x2x2();
        for (i, l) in lights.into_iter().enumerate() {
            g.entries[i * 4 + 2] = l;
            g.entries[i * 4 + 3] = 0xFF;
        }
        g
    }

    /// Blocks the sight lines that end next to one grid corner.
    struct BlockCorner([f32; 3]);

    impl SightTrace for BlockCorner {
        fn blocked(&self, _: [f32; 3], to: [f32; 3]) -> bool {
            (0..3).all(|a| (to[a] - self.0[a]).abs() < 0.1)
        }
    }

    #[test]
    fn corners_that_fail_their_sight_test_are_dropped_once_another_passes() {
        let g = traced_grid([255; 8]);
        let at = [16.0, 16.0, 32.0];
        let all = light_grid_lookup(&g, at, None);
        assert_eq!(all.entries.iter().flatten().count(), 8);
        // The cell's corner 0 sits at the grid origin: it cannot be seen, the other seven can.
        let lookup = light_grid_lookup(&g, at, Some(&BlockCorner([0.0, 0.0, 0.0])));
        assert_eq!(lookup.entries[0], None);
        assert_eq!(lookup.entries.iter().flatten().count(), 7);
        // Corner 7 is the far corner: (32, 32, 64).
        let lookup = light_grid_lookup(&g, at, Some(&BlockCorner([32.0, 32.0, 64.0])));
        assert_eq!(lookup.entries[7], None);
        assert_eq!(lookup.entries.iter().flatten().count(), 7);
        // Corners that need no test are never traced.
        let plain = grid_2x2x2();
        let lookup = light_grid_lookup(&plain, at, Some(&BlockCorner([0.0, 0.0, 0.0])));
        assert_eq!(lookup.entries.iter().flatten().count(), 8);
    }

    #[test]
    fn when_every_corner_fails_they_all_still_count() {
        struct Blind;
        impl SightTrace for Blind {
            fn blocked(&self, _: [f32; 3], _: [f32; 3]) -> bool {
                true
            }
        }
        let g = traced_grid([255; 8]);
        let lookup = light_grid_lookup(&g, [16.0, 16.0, 32.0], Some(&Blind));
        assert_eq!(lookup.entries.iter().flatten().count(), 8);
    }

    fn omni(origin: [f32; 3], radius: f32) -> ComPrimaryLight {
        ComPrimaryLight {
            kind: 2,
            can_use_shadow_map: false,
            exponent: 0,
            color: [1.0; 3],
            dir: [0.0, 0.0, 1.0],
            origin,
            radius,
            cos_half_fov_outer: 0.0,
            cos_half_fov_inner: 0.0,
            cos_half_fov_expanded: 0.0,
            rotation_limit: 0.0,
            translation_limit: 0.0,
            def_name: None,
        }
    }

    #[test]
    fn a_light_that_cannot_reach_a_corner_does_not_darken_the_blend() {
        // Half the corners carry light 1, the rest none.
        let g = traced_grid([1, 1, 1, 1, 0, 0, 0, 0]);
        let mut g = g;
        for i in 0..8 {
            g.entries[i * 4 + 3] = 0;
        }
        let at = [16.0, 16.0, 32.0];
        let weight = |lights: &[ComPrimaryLight]| {
            let env = LightingEnv { sight: None, lights };
            lighting_at_point(&g, at, 0, Extrapolate::Default, &env).0.primary_weight
        };
        // No light data: the unlit corners count against the light.
        assert_eq!(weight(&[]), 128);
        // A light far away reaches none of them: nothing counts against it.
        let far = [omni([0.0; 3], 1.0), omni([5000.0, 0.0, 0.0], 100.0)];
        assert_eq!(weight(&far), 255);
        // A light that covers the whole cell darkens as without data.
        let near = [omni([0.0; 3], 1.0), omni([16.0, 16.0, 32.0], 500.0)];
        assert_eq!(weight(&near), 128);
    }

    #[test]
    fn spot_lights_reach_only_inside_their_cone() {
        let mut l = omni([0.0; 3], 100.0);
        l.kind = 3;
        // Like the sun's, a light's direction points from the lit surface back toward the light.
        l.dir = [-1.0, 0.0, 0.0];
        l.cos_half_fov_outer = 0.9;
        l.rotation_limit = 1.0;
        assert!(light_reaches(&l, [50.0, 0.0, 0.0]));
        assert!(!light_reaches(&l, [0.0, 50.0, 0.0]));
        assert!(!light_reaches(&l, [-50.0, 0.0, 0.0]));
        assert!(!light_reaches(&l, [150.0, 0.0, 0.0]));
    }

    #[test]
    fn synthetic_blend_and_outside() {
        let g = grid_2x2x2();
        let mut ml = ModelLighting {
            height: 4,
            texels: vec![0; (WIDTH * 4 * DEPTH * 4) as usize],
            static_count: 0,
            total_entries: 64,
            next_dynamic: 0,
            primary: vec![0; 64],
        };
        let h = ml.alloc_point(&g, [16.0, 16.0, 32.0], 0, &LightingEnv::none()).unwrap();
        let o = ml.texel_offset(u32::from(h) - 1, 5);
        // mean of 0,10,..,70 = 35 in every channel; alpha 0 (no primary light).
        assert_eq!(&ml.texels[o..o + 4], &[35, 35, 35, 0]);
        // Entry 1 lands at x=4..8, centre coords use the block centre.
        let h2 = ml.alloc_point(&g, [0.0, 0.0, 0.0], 0, &LightingEnv::none()).unwrap();
        assert_eq!(h2, 2);
        assert_eq!(ml.base_coords(h2), [6.0 / 256.0, 2.0 / 4.0, 0.5, 1.0]);

        // Far outside: no corners, default entry 1 (colour 1 = 10s), no panic.
        let l = light_grid_lookup(&g, [1.0e9, -1.0e9, f32::MAX], None);
        assert!(l.entries.iter().all(Option::is_none));
        let h3 = ml.alloc_point(&g, [1.0e9, -1.0e9, f32::NAN], 0, &LightingEnv::none()).unwrap();
        let o = ml.texel_offset(u32::from(h3) - 1, 0);
        assert_eq!(&ml.texels[o..o + 3], &[10, 10, 10]);
    }

    #[test]
    fn dynamic_entries_run_out_and_reset() {
        let g = grid_2x2x2();
        let mut ml = ModelLighting {
            height: 4,
            texels: vec![0; (WIDTH * 4 * DEPTH * 4) as usize],
            static_count: 62,
            total_entries: 64,
            next_dynamic: 62,
            primary: vec![0; 64],
        };
        assert_eq!(ml.alloc_point(&g, [0.0; 3], 0, &LightingEnv::none()), Some(63));
        assert_eq!(ml.alloc_point(&g, [0.0; 3], 0, &LightingEnv::none()), Some(64));
        assert_eq!(ml.alloc_point(&g, [0.0; 3], 0, &LightingEnv::none()), None);
        ml.reset_dynamic();
        assert_eq!(ml.alloc_point(&g, [0.0; 3], 0, &LightingEnv::none()), Some(63));
    }

    mod mp_crash {
        use super::*;
        use assets::zone::{Asset, Consumer, Zone};
        use std::fs::File;
        use std::io::BufReader;
        use std::sync::{Arc, OnceLock};

        fn world() -> Option<&'static Arc<GfxWorld>> {
            static W: OnceLock<Option<Arc<GfxWorld>>> = OnceLock::new();
            W.get_or_init(|| {
                let root = std::env::var_os("COD4_PATH")?;
                let path = std::path::Path::new(&root).join("zone/english/mp_crash.ff");
                if !path.is_file() {
                    eprintln!("skipping: no original install at {}", path.display());
                    return None;
                }
                let zone = Zone::open(BufReader::new(File::open(path).unwrap())).unwrap();
                let mut world = None;
                zone.decode(&Consumer::Client, |a| {
                    if let Asset::GfxWorld(w) = a {
                        world = Some(w);
                    }
                })
                .unwrap();
                world
            })
            .as_ref()
        }

        #[test]
        fn static_models_get_valid_plausible_blocks() {
            let Some(w) = world() else { return };
            let t = std::time::Instant::now();
            let ml = ModelLighting::new(w, 1024, &LightingEnv::none());
            eprintln!(
                "built {} static entries in {:?}, volume {:?}",
                w.dpvs.smodel_draw_insts.len(),
                t.elapsed(),
                ml.size()
            );
            let n = w.dpvs.smodel_draw_insts.len();
            assert!(n > 4000);
            let (wd, h, d) = ml.size();
            assert_eq!(ml.texels().len(), (wd * h * d * 4) as usize);
            let mut lit = 0;
            for i in 0..n {
                let handle = ml.static_model_handle(i);
                assert_eq!(usize::from(handle), i + 1);
                let c = ml.base_coords(handle);
                assert!(
                    c[0] > 0.0 && c[0] < 1.0 && c[1] > 0.0 && c[1] < 1.0,
                    "{c:?}"
                );
                let e = i as u32;
                let any = (0..64).any(|s| {
                    let o = ml.texel_offset(e, s);
                    ml.texels[o..o + 3].iter().any(|&b| b != 0)
                });
                lit += usize::from(any);
            }
            eprintln!("{lit}/{n} static blocks have non-black colour");
            assert!(lit * 10 > n * 9, "{lit}/{n}");
            // Grid has 12,406 colour sets of 168 bytes and valid indices.
            let g = &w.light_grid;
            assert_eq!(g.colors.len(), g.color_count as usize * COLORS_SIZE);
            for k in 0..g.entry_count {
                assert!(u32::from(grid_entry(g, k).unwrap().colors_index) < g.color_count);
            }
        }

        #[test]
        fn centre_lighting_matches_grid_lookup_and_is_deterministic() {
            let Some(w) = world() else { return };
            let a = ModelLighting::new(w, 64, &LightingEnv::none());
            let b = ModelLighting::new(w, 64, &LightingEnv::none());
            assert!(a.texels() == b.texels(), "build is deterministic");
            let g = &w.light_grid;
            let (mut grounded, mut grid_hit, mut extrapolated) = (0, 0, 0);
            for (i, inst) in w.dpvs.smodel_insts.iter().enumerate() {
                let o0 = a.texel_offset(i as u32, 0);
                if inst.ground_lighting != 0 {
                    grounded += 1;
                    for s in 0..64 {
                        let o = a.texel_offset(i as u32, s);
                        assert_eq!(a.texels[o..o + 4], inst.ground_lighting.to_le_bytes());
                    }
                    continue;
                }
                let centre = [0, 1, 2].map(|k| (inst.mins[k] + inst.maxs[k]) * 0.5);
                let l = light_grid_lookup(g, centre, None);
                let texel = &a.texels[o0..o0 + 3]; // b, g, r of colour 0
                let live: Vec<_> = l.entries.iter().flatten().collect();
                if live.is_empty() {
                    extrapolated += 1;
                    let ci = if g.color_count <= l.default_entry {
                        g.color_count - 1
                    } else {
                        l.default_entry
                    };
                    let c = &g.colors[ci as usize * COLORS_SIZE..][..3];
                    assert_eq!(texel, [c[2], c[1], c[0]]);
                } else {
                    grid_hit += 1;
                    // Blend is a convex combination of the live colour sets (+-1 rounding).
                    for ch in 0..3 {
                        let vals: Vec<u8> = live
                            .iter()
                            .map(|&&e| {
                                let ci = usize::from(grid_entry(g, e).unwrap().colors_index);
                                g.colors[ci * COLORS_SIZE + ch]
                            })
                            .collect();
                        let (lo, hi) = (*vals.iter().min().unwrap(), *vals.iter().max().unwrap());
                        let v = texel[2 - ch];
                        assert!(
                            v + 1 >= lo && v <= hi.saturating_add(1),
                            "model {i} ch {ch}: {v} not in {lo}..={hi}"
                        );
                    }
                }
            }
            eprintln!("grounded {grounded}, grid {grid_hit}, extrapolated {extrapolated}");
            assert!(grid_hit > 0);
        }

        #[test]
        fn far_outside_points_extrapolate() {
            let Some(w) = world() else { return };
            let mut ml = ModelLighting::new(w, 8, &LightingEnv::none());
            for p in [
                [1.0e7, 0.0, 0.0],
                [0.0, -1.0e7, 0.0],
                [0.0, 0.0, 1.0e7],
                [-130000.0, 130000.0, -130000.0],
                [f32::INFINITY, f32::NAN, f32::NEG_INFINITY],
            ] {
                let l = light_grid_lookup(&w.light_grid, p, None);
                assert!(l.entries.iter().all(Option::is_none), "{p:?}");
                assert!(ml.alloc_point(&w.light_grid, p, 0, &LightingEnv::none()).is_some());
            }
        }

        #[test]
        fn dynamic_point_matches_static_block_inside_grid() {
            let Some(w) = world() else { return };
            let mut ml = ModelLighting::new(w, 8, &LightingEnv::none());
            let (i, inst) = w
                .dpvs
                .smodel_insts
                .iter()
                .enumerate()
                .find(|(_, m)| {
                    let c = [0, 1, 2].map(|k| (m.mins[k] + m.maxs[k]) * 0.5);
                    m.ground_lighting == 0
                        && light_grid_lookup(&w.light_grid, c, None)
                            .entries
                            .iter()
                            .any(Option::is_some)
                })
                .unwrap();
            let c = [0, 1, 2].map(|k| (inst.mins[k] + inst.maxs[k]) * 0.5);
            let sun = u32::from(w.dpvs.smodel_draw_insts[i].primary_light_index);
            let h = ml.alloc_point(&w.light_grid, c, sun, &LightingEnv::none()).unwrap();
            assert!(usize::from(h) > w.dpvs.smodel_draw_insts.len());
            for s in 0..64 {
                let (o1, o2) = (
                    ml.texel_offset(i as u32, s),
                    ml.texel_offset(u32::from(h) - 1, s),
                );
                assert_eq!(ml.texels[o1..o1 + 4], ml.texels[o2..o2 + 4]);
            }
        }
    }
}
