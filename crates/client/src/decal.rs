// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (gfx_d3d/r_marks.cpp: R_Mark_MaterialAllowsMarks, R_MarkFragment_IsTriangleRejected; GPL-3.0, copyright the KisakCOD contributors and Activision).
//! Decals: an effect's decal element is a box on a surface; the picture is the part of the triangles of the world's
//! surfaces and static models inside it, clipped to the box and textured by projecting along the surface normal (the
//! original's `FX_GenerateMark`). Only surfaces whose material takes marks of the decal's kind receive it.

use assets::zone::gfx::Material;
use assets::zone::gfxworld::GfxWorld;
use assets::zone::xmodel::Surface;
use glam::{Mat4, Vec3};
use render::DynVertex;

/// A box placed on a surface.
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub origin: Vec3,
    /// Out of the surface.
    pub normal: Vec3,
    /// In the surface, the picture's up.
    pub up: Vec3,
    /// Half the picture's width and height.
    pub half_size: [f32; 2],
}

/// Steepest the surface may be to the decal's normal, as a cosine, before the triangle is skipped
/// (`R_MarkFragment_IsTriangleRejected`: more than 60 degrees off).
const MIN_FACING: f32 = 0.5;
/// How far in front of the surface the decal is drawn, against depth-buffer fighting.
const LIFT: f32 = 0.1;

fn clip_plane(poly: &[Vec3], n: Vec3, d: f32) -> Vec<Vec3> {
    // Keeps the part with dot(p, n) <= d.
    let mut out = Vec::with_capacity(poly.len() + 1);
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        let (da, db) = (a.dot(n) - d, b.dot(n) - d);
        if da <= 0.0 {
            out.push(a);
        }
        if (da < 0.0 && db > 0.0) || (da > 0.0 && db < 0.0) {
            out.push(a.lerp(b, da / (da - db)));
        }
    }
    out
}

/// The vertices (three per triangle) of `tris` clipped to the box of `p`, with their texture coordinates.
pub fn clip(tris: impl IntoIterator<Item = [Vec3; 3]>, p: &Placement) -> Vec<DynVertex> {
    let n = p.normal.normalize_or(Vec3::Z);
    let up = (p.up - n * p.up.dot(n)).normalize_or(Vec3::X.cross(n).normalize_or(Vec3::Y));
    let right = up.cross(n);
    let depth = (p.half_size[0].max(p.half_size[1]) * 0.5).clamp(4.0, 16.0);
    let [hw, hh] = p.half_size;
    let planes = [
        (right, p.origin.dot(right) + hw),
        (-right, -p.origin.dot(right) + hw),
        (up, p.origin.dot(up) + hh),
        (-up, -p.origin.dot(up) + hh),
        (n, p.origin.dot(n) + depth),
        (-n, -p.origin.dot(n) + depth),
    ];
    let tangent = right.to_array();
    let mut out = Vec::new();
    for t in tris {
        let tn = (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero();
        if tn.dot(n) < MIN_FACING {
            continue;
        }
        let mut poly = t.to_vec();
        for (pn, d) in planes {
            poly = clip_plane(&poly, pn, d);
            if poly.len() < 3 {
                break;
            }
        }
        if poly.len() < 3 {
            continue;
        }
        let vert = |v: Vec3| {
            let rel = v - p.origin;
            DynVertex {
                pos: (v + n * LIFT).to_array(),
                color: [255; 4],
                uv: [
                    0.5 + rel.dot(right) / (2.0 * hw),
                    0.5 - rel.dot(up) / (2.0 * hh),
                ],
                normal: tn.to_array(),
                tangent,
            }
        };
        for i in 1..poly.len() - 1 {
            // Back to the map's own order, clockwise seen from outside, the way its surfaces are drawn.
            out.extend([vert(poly[0]), vert(poly[i + 1]), vert(poly[i])]);
        }
    }
    out
}

/// Whether a surface of material `receiver` takes a mark of material `mark` (`R_Mark_MaterialAllowsMarks`): not when it
/// refuses marks, and only when it is made of every kind of surface the mark is for.
pub fn receives(receiver: &Material, mark: &Material) -> bool {
    receiver.state_flags & 4 == 0
        && receiver.game_flags & 4 == 0
        && receiver.surface_type_bits & mark.surface_type_bits == mark.surface_type_bits
}

/// The triangles that can take a mark in `[mins, maxs]`: the map's surfaces for `world_mark`'s kind of surface and
/// its static models for `model_mark`'s, counter-clockwise seen from outside.
pub fn receivers<'a>(
    w: &'a GfxWorld,
    mins: Vec3,
    maxs: Vec3,
    world_mark: &'a Material,
    model_mark: &'a Material,
) -> impl Iterator<Item = [Vec3; 3]> + 'a {
    world_triangles(w, mins, maxs, world_mark).chain(model_triangles(w, mins, maxs, model_mark))
}

/// The world's triangles whose surface bounds overlap `[mins, maxs]` and whose material takes `mark`, counter-clockwise
/// seen from outside.
pub fn world_triangles<'a>(
    w: &'a GfxWorld,
    mins: Vec3,
    maxs: Vec3,
    mark: &'a Material,
) -> impl Iterator<Item = [Vec3; 3]> + 'a {
    w.dpvs
        .surfaces
        .iter()
        .filter(move |s| {
            Vec3::from(s.bounds[0]).cmple(maxs).all()
                && Vec3::from(s.bounds[1]).cmpge(mins).all()
                && s.material.as_ref().is_some_and(|m| receives(m, mark))
        })
        .flat_map(move |s| {
            let vert = move |i: u16| -> Vec3 {
                let at = (s.first_vertex as usize + usize::from(i)) * 44;
                let f = |k: usize| {
                    f32::from_le_bytes(w.vertices[at + k * 4..at + k * 4 + 4].try_into().unwrap())
                };
                Vec3::new(f(0), f(1), f(2))
            };
            let first = s.base_index as usize;
            (0..usize::from(s.tri_count)).filter_map(move |t| {
                let ix = w.indices.get(first + t * 3..first + t * 3 + 3)?;
                let at = |i: u16| {
                    (s.first_vertex as usize + usize::from(i) + 1) * 44 <= w.vertices.len()
                };
                (at(ix[0]) && at(ix[1]) && at(ix[2]))
                    // The map's triangles are clockwise seen from outside (Direct3D's front face); the clipper wants
                    // counter-clockwise.
                    .then(|| [vert(ix[0]), vert(ix[2]), vert(ix[1])])
            })
        })
}

/// The triangles of the static models whose bounds overlap `[mins, maxs]`, in their most detailed level, for the
/// surfaces whose material takes `mark`; counter-clockwise seen from outside.
pub fn model_triangles(w: &GfxWorld, mins: Vec3, maxs: Vec3, mark: &Material) -> Vec<[Vec3; 3]> {
    let mut out = Vec::new();
    for (inst, draw) in w.dpvs.smodel_insts.iter().zip(&w.dpvs.smodel_draw_insts) {
        let (Some(model), true) = (
            draw.model.as_ref(),
            Vec3::from(inst.mins).cmple(maxs).all() && Vec3::from(inst.maxs).cmpge(mins).all(),
        ) else {
            continue;
        };
        let place = render::scene::model_matrix(draw);
        let lod = &model.lod_info[0];
        for idx in
            usize::from(lod.surf_index)..usize::from(lod.surf_index) + usize::from(lod.surf_count)
        {
            let (Some(surf), Some(Some(mat))) = (model.surfs.get(idx), model.materials.get(idx))
            else {
                continue;
            };
            if receives(mat, mark) {
                out.extend(surface_triangles(surf, &place));
            }
        }
    }
    out
}

/// The triangles of a model surface placed by `place`, counter-clockwise seen from outside.
fn surface_triangles<'a>(s: &'a Surface, place: &'a Mat4) -> impl Iterator<Item = [Vec3; 3]> + 'a {
    let vert = move |i: u16| -> Option<Vec3> {
        let b = s.verts.get(usize::from(i) * 32..usize::from(i) * 32 + 12)?;
        let f = |k: usize| f32::from_le_bytes(b[k * 4..k * 4 + 4].try_into().unwrap());
        Some(place.transform_point3(Vec3::new(f(0), f(1), f(2))))
    };
    s.tri_indices
        .as_chunks::<3>()
        .0
        .iter()
        // Clockwise seen from outside, like the map's.
        .filter_map(move |t| Some([vert(t[0])?, vert(t[2])?, vert(t[1])?]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn area(v: &[DynVertex]) -> f32 {
        v.chunks(3)
            .map(|t| {
                let [a, b, c] = [0, 1, 2].map(|i| Vec3::from(t[i].pos));
                (b - a).cross(c - a).length() * 0.5
            })
            .sum()
    }

    fn floor() -> Vec<[Vec3; 3]> {
        // Two triangles of a 100 x 100 square, counter-clockwise seen from above.
        let (a, b, c, d) = (
            Vec3::new(-50.0, -50.0, 0.0),
            Vec3::new(50.0, -50.0, 0.0),
            Vec3::new(50.0, 50.0, 0.0),
            Vec3::new(-50.0, 50.0, 0.0),
        );
        vec![[a, b, c], [a, c, d]]
    }

    fn on_floor(half: [f32; 2], at: Vec3) -> Placement {
        Placement {
            origin: at,
            normal: Vec3::Z,
            up: Vec3::Y,
            half_size: half,
        }
    }

    #[test]
    fn a_decal_inside_a_surface_covers_exactly_its_box() {
        let v = clip(floor(), &on_floor([5.0, 3.0], Vec3::new(10.0, 0.0, 0.0)));
        assert!((area(&v) - 10.0 * 6.0).abs() < 1e-3, "{}", area(&v));
        assert!(
            v.iter()
                .all(|x| (-1e-4..=1.0001).contains(&x.uv[0]) && (-1e-4..=1.0001).contains(&x.uv[1]))
        );
        // The texture's top is the decal's up (+y): v is smallest at the largest y.
        let top = v.iter().fold(None::<&DynVertex>, |m, x| match m {
            Some(m) if m.pos[1] >= x.pos[1] => Some(m),
            _ => Some(x),
        });
        assert!(top.unwrap().uv[1].abs() < 1e-4);
    }

    #[test]
    fn a_decal_over_an_edge_is_cut_at_the_edge() {
        // Box x in [45, 55], y in [-5, 5]: half of it is off the floor.
        let v = clip(floor(), &on_floor([5.0, 5.0], Vec3::new(50.0, 0.0, 0.0)));
        assert!((area(&v) - 50.0).abs() < 1e-3, "{}", area(&v));
        assert!(v.iter().all(|x| x.pos[0] <= 50.0 + 1e-4));
    }

    #[test]
    fn surfaces_facing_away_or_out_of_reach_get_no_decal() {
        let flipped: Vec<_> = floor().into_iter().map(|[a, b, c]| [a, c, b]).collect();
        assert!(clip(flipped, &on_floor([5.0, 5.0], Vec3::ZERO)).is_empty());
        // A floor two box-depths below.
        assert!(clip(floor(), &on_floor([5.0, 5.0], Vec3::new(0.0, 0.0, 30.0))).is_empty());
        // A wall perpendicular to the decal.
        let wall = vec![[
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.0, 10.0, 0.0),
            Vec3::new(0.0, 0.0, 10.0),
        ]];
        assert!(clip(wall, &on_floor([5.0, 5.0], Vec3::ZERO)).is_empty());
    }

    #[test]
    fn a_slope_takes_the_decal_up_to_sixty_degrees_off_and_no_further() {
        let tilted = |degrees: f32| -> Vec<[Vec3; 3]> {
            let q = glam::Quat::from_rotation_y(degrees.to_radians());
            floor()
                .into_iter()
                .map(|t| t.map(|v| q * v))
                .collect::<Vec<_>>()
        };
        let at = on_floor([5.0, 5.0], Vec3::ZERO);
        assert!(!clip(tilted(50.0), &at).is_empty());
        assert!(clip(tilted(65.0), &at).is_empty());
    }

    fn material(surface_type_bits: u32, state_flags: u8, game_flags: u8) -> Material {
        Material {
            name: None,
            game_flags,
            sort_key: 0,
            atlas_rows: 1,
            atlas_columns: 1,
            draw_surf: 0,
            surface_type_bits,
            hash_index: 0,
            state_bits_entry: [0; 34],
            state_flags,
            camera_region: 0,
            technique_set: None,
            textures: Arc::from(Vec::new()),
            constants: Arc::from(Vec::new()),
            state_bits: Arc::from(Vec::new()),
        }
    }

    #[test]
    fn a_surface_takes_a_mark_only_if_it_allows_marks_and_is_every_kind_the_mark_is_for() {
        let mark = material(0b0110, 0, 0);
        assert!(receives(&material(0b0111, 0, 0), &mark));
        assert!(receives(&material(0b0110, 0, 0), &mark));
        assert!(
            !receives(&material(0b0100, 0, 0), &mark),
            "only one of the two kinds"
        );
        assert!(
            !receives(&material(0b0111, 4, 0), &mark),
            "state flag 4 refuses marks"
        );
        assert!(
            !receives(&material(0b0111, 0, 4), &mark),
            "game flag 4 refuses marks"
        );
        assert!(
            receives(&material(0, 0, 0), &material(0, 0, 0)),
            "a mark for no kind goes anywhere"
        );
    }

    #[test]
    fn a_model_surface_is_placed_and_wound_like_the_maps() {
        let verts: Vec<u8> = [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]
            .iter()
            .flat_map(|p| {
                let mut v = [0u8; 32];
                for (i, c) in p.iter().enumerate() {
                    v[i * 4..i * 4 + 4].copy_from_slice(&c.to_le_bytes());
                }
                v
            })
            .collect();
        let surf = Surface {
            tile_mode: 0,
            deformed: false,
            vert_count: 3,
            tri_count: 1,
            zone_handle: 0,
            base_tri_index: 0,
            base_vert_index: 0,
            blend_counts: [0; 4],
            blends: Arc::from(Vec::new()),
            vert_list: Arc::from(Vec::new()),
            part_bits: [0; 4],
            verts: Arc::from(verts),
            tri_indices: Arc::from(vec![0u16, 1, 2]),
        };
        // Ten times the size, 100 units up.
        let place = Mat4::from_translation(Vec3::Z * 100.0) * Mat4::from_scale(Vec3::splat(10.0));
        let tris: Vec<_> = surface_triangles(&surf, &place).collect();
        assert_eq!(tris.len(), 1);
        let [a, b, c] = tris[0];
        assert_eq!(
            (a, b, c),
            (
                Vec3::Z * 100.0,
                Vec3::new(0.0, 10.0, 100.0),
                Vec3::new(10.0, 0.0, 100.0)
            )
        );
        // Indices 0, 1, 2 run counter-clockwise seen from above, so in the map's convention (clockwise seen from
        // outside) the outside is below, and the triangle comes out counter-clockwise seen from there: normal down.
        assert!((b - a).cross(c - a).z < 0.0);
    }
}
