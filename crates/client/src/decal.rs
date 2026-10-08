// SPDX-License-Identifier: GPL-3.0-or-later
//! Decals: an effect's decal element is a box on a surface; the picture is the part of the world's triangles inside
//! it, clipped to the box and textured by projecting along the surface normal (the original's `FX_GenerateMark`).

use assets::zone::gfxworld::GfxWorld;
use glam::Vec3;
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

/// Steepest the surface may be to the decal's normal, as a cosine, before the triangle is skipped.
const MIN_FACING: f32 = 0.3;
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
            out.extend([vert(poly[0]), vert(poly[i]), vert(poly[i + 1])]);
        }
    }
    out
}

/// The world's triangles whose surface bounds overlap `[mins, maxs]`.
pub fn world_triangles<'a>(
    w: &'a GfxWorld,
    mins: Vec3,
    maxs: Vec3,
) -> impl Iterator<Item = [Vec3; 3]> + 'a {
    w.dpvs
        .surfaces
        .iter()
        .filter(move |s| {
            Vec3::from(s.bounds[0]).cmple(maxs).all() && Vec3::from(s.bounds[1]).cmpge(mins).all()
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
                    .then(|| [vert(ix[0]), vert(ix[1]), vert(ix[2])])
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
