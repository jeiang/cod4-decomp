// SPDX-License-Identifier: GPL-3.0-or-later
//! The compass and the full-screen map: the map's frame of reference and the transforms between world and screen.
//!
//! The map script's `setMiniMap` tells the clients which image shows the level and which two world points are the
//! image's upper left and lower right corners; the level's `northyaw` says which way up is. Everything here is plain
//! arithmetic on those, so the HUD pieces that mark things on the compass (players, objectives) share one definition
//! of where a world point lands: [`MapInfo::to_map`] for the full-screen map and [`to_compass`] for the corner
//! minimap, which scrolls with the player and turns with the view.
//!
//! Screen offsets are returned from the centre of the map or compass rect with y growing downward, in the same unit
//! as the rect they are given (pixels for the HUD).

/// The level's map image and its frame, from the `setMiniMap` configstring and `northyaw`.
#[derive(Clone, Debug, PartialEq)]
pub struct MapInfo {
    /// Material name, e.g. `compass_map_mp_crash`.
    pub material: String,
    /// World x, y of the image's upper left corner.
    pub upper_left: [f32; 2],
    /// World extent along the image's x (east) and y (south) axes, in inches.
    pub world_size: [f32; 2],
    /// Direction of north in world yaw degrees.
    pub north_yaw: f32,
}

impl MapInfo {
    /// Reads `"<material>" <ulx> <uly> <lrx> <lry>`. `None` when it is not there yet or the corners are not
    /// south-east of each other along the north yaw (the original refuses such a map).
    pub fn parse(minimap: &str, north_yaw: &str) -> Option<Self> {
        let rest = minimap.trim().strip_prefix('"')?;
        let (material, rest) = rest.split_once('"')?;
        let n: Vec<f32> = rest
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        let [ulx, uly, lrx, lry] = n[..] else {
            return None;
        };
        let north_yaw = north_yaw.trim().parse().unwrap_or(0.0);
        let n = north_vec(north_yaw);
        let (dx, dy) = (lrx - ulx, lry - uly);
        let world_size = [dx * n[1] - dy * n[0], -dx * n[0] - dy * n[1]];
        (world_size[0] > 0.0 && world_size[1] > 0.0 && !material.is_empty()).then(|| Self {
            material: material.to_owned(),
            upper_left: [ulx, uly],
            world_size,
            north_yaw,
        })
    }

    /// Where a world point lands on the full-screen map, from the map rect's centre (`CG_WorldPosToCompass`, full).
    pub fn to_map(&self, rect: (f32, f32), p: [f32; 2]) -> [f32; 2] {
        let n = north_vec(self.north_yaw);
        let (dx, dy) = (p[0] - self.upper_left[0], p[1] - self.upper_left[1]);
        let x = n[1] * dx - n[0] * dy;
        let y = -n[1] * dy - n[0] * dx;
        [
            (x / self.world_size[0] - 0.5) * rect.0,
            (y / self.world_size[1] - 0.5) * rect.1,
        ]
    }

    /// Where the image's corner `(0,0)` lies for a viewer at world point `p`, in world inches east and south of `p`.
    /// The minimap scales these to pixels and turns them with the view.
    pub fn corner_from(&self, p: [f32; 2]) -> [f32; 2] {
        let n = north_vec(self.north_yaw);
        let (dx, dy) = (p[0] - self.upper_left[0], p[1] - self.upper_left[1]);
        // east is (n1, -n0), south is (-n0, -n1).
        [-(n[1] * dx - n[0] * dy), -(-n[0] * dx - n[1] * dy)]
    }
}

/// North as a unit vector in the world plane.
pub fn north_vec(north_yaw: f32) -> [f32; 2] {
    let r = north_yaw.to_radians();
    [r.cos(), r.sin()]
}

/// The direction that points up on the minimap: the view direction when the compass rotates, north otherwise
/// (`CG_CompassUpYawVector`).
pub fn up_vector(rotation: bool, view_yaw: f32, north_yaw: f32) -> [f32; 2] {
    if rotation {
        north_vec(view_yaw)
    } else {
        north_vec(north_yaw)
    }
}

/// Where a world point lands on the corner minimap relative to its centre: `up` is [`up_vector`], `rect_h` the
/// minimap's height in the output unit and `max_range` the world distance from the player to the minimap's top
/// edge (`compassMaxRange`).
pub fn to_compass(
    up: [f32; 2],
    player: [f32; 2],
    p: [f32; 2],
    rect_h: f32,
    max_range: f32,
) -> [f32; 2] {
    let per_inch = rect_h / max_range;
    let (dx, dy) = ((p[0] - player[0]) * per_inch, (p[1] - player[1]) * per_inch);
    [up[1] * dx - up[0] * dy, -up[1] * dy - up[0] * dx]
}

/// Pulls an offset from the centre of a `w` by `h` rect onto its edge along the line to the centre. The flag is
/// true when the point was outside (an icon that is off the minimap sits on its rim).
pub fn clip_to_rect(mut xy: [f32; 2], w: f32, h: f32) -> ([f32; 2], bool) {
    let mut clipped = false;
    for (axis, half) in [(0, w * 0.5), (1, h * 0.5)] {
        if xy[axis].abs() > half {
            let k = half / xy[axis].abs();
            xy = [xy[0] * k, xy[1] * k];
            clipped = true;
        }
    }
    (xy, clipped)
}

/// The largest rect of the map's aspect that fits `rect` (`x, y, w, h`), centred, shrunk by `border` on every side
/// (`CG_CompassCalcDimensions`, full).
pub fn fit_map(world_size: [f32; 2], rect: [f32; 4], border: f32) -> [f32; 4] {
    let [x, y, w, h] = rect;
    let map_aspect = world_size[0] / world_size[1];
    let (mut fx, mut fy, mut fw, mut fh) = if w / h >= map_aspect {
        let fw = map_aspect / (w / h) * w;
        (x + w * 0.5 - fw * 0.5, y, fw, h)
    } else {
        let fh = (w / h) / map_aspect * h;
        (x, y + h * 0.5 - fh * 0.5, w, fh)
    };
    let b = border.min(fw * 0.25).min(fh * 0.25);
    fx += b;
    fy += b;
    fw -= b * 2.0;
    fh -= b * 2.0;
    [fx, fy, fw, fh]
}

/// Normalises an angle to `0..360`.
pub fn norm360(a: f32) -> f32 {
    a.rem_euclid(360.0)
}

/// `AngleDelta(a, b)`: `a - b` folded into `-180..=180`.
pub fn angle_delta(a: f32, b: f32) -> f32 {
    let d = norm360(a - b);
    if d > 180.0 { d - 360.0 } else { d }
}

/// Fraction of the tickertape texture that is visible at a view yaw, as `(left, right)` texture coordinates
/// (`stretch` is `compassTickertapeStretch`).
pub fn tape_span(view_yaw: f32, north_yaw: f32, stretch: f32) -> (f32, f32) {
    let centre = -(view_yaw - north_yaw) / 360.0;
    (centre - stretch * 0.5, centre + stretch * 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 2], b: [f32; 2]) {
        assert!(
            (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3,
            "{a:?} != {b:?}"
        );
    }

    #[test]
    fn the_minimap_string_gives_the_frame_and_rejects_a_map_that_runs_backwards() {
        // North at yaw 0 is +x, so east is -y and south is -x: the lower right corner has lower x and y.
        let m = MapInfo::parse("\"compass_map_mp_x\" 1000 2000 -1000 -2000", "0").unwrap();
        assert_eq!(m.material, "compass_map_mp_x");
        close(m.world_size, [4000.0, 2000.0]);
        // With north at yaw 90 east is +x and south is -y.
        let m = MapInfo::parse("\"a\" 0 0 200 -100", "90").unwrap();
        close(m.world_size, [200.0, 100.0]);
        assert!(MapInfo::parse("\"a\" 0 0 100 100", "0").is_none());
        assert!(MapInfo::parse("", "0").is_none());
        assert!(MapInfo::parse("\"a\" 1 2 3", "0").is_none());
    }

    #[test]
    fn world_points_land_on_the_full_map_where_the_image_has_them() {
        let m = MapInfo::parse("\"a\" 0 0 200 -100", "90").unwrap();
        // The upper left corner is the top left of the image, the lower right the bottom right.
        close(m.to_map((200.0, 100.0), [0.0, 0.0]), [-100.0, -50.0]);
        close(m.to_map((200.0, 100.0), [200.0, -100.0]), [100.0, 50.0]);
        close(m.to_map((200.0, 100.0), [100.0, -50.0]), [0.0, 0.0]);
    }

    #[test]
    fn a_point_ahead_of_the_player_is_up_on_the_minimap_whichever_way_the_player_faces() {
        for yaw in [0.0f32, 37.0, 90.0, 200.0] {
            let r = yaw.to_radians();
            let ahead = [100.0 * r.cos() + 5.0, 100.0 * r.sin() + 7.0];
            let xy = to_compass(up_vector(true, yaw, 0.0), [5.0, 7.0], ahead, 100.0, 1000.0);
            close(xy, [0.0, -10.0]);
            // Right of the player is right on the screen.
            let right = [100.0 * r.sin() + 5.0, -100.0 * r.cos() + 7.0];
            close(
                to_compass(up_vector(true, yaw, 0.0), [5.0, 7.0], right, 100.0, 1000.0),
                [10.0, 0.0],
            );
        }
        // Without rotation north is up.
        close(
            to_compass(
                up_vector(false, 123.0, 90.0),
                [0.0, 0.0],
                [0.0, 500.0],
                100.0,
                1000.0,
            ),
            [0.0, -50.0],
        );
    }

    #[test]
    fn the_map_image_corner_is_west_and_north_of_a_viewer_inside_the_map() {
        let m = MapInfo::parse("\"a\" 1000 2000 -1000 -2000", "0").unwrap();
        close(m.corner_from([1000.0, 2000.0]), [0.0, 0.0]);
        // 40 inches east (-y) and 30 south (-x) of the corner.
        close(m.corner_from([970.0, 1960.0]), [-40.0, -30.0]);
    }

    #[test]
    fn icons_outside_the_minimap_are_pulled_to_its_rim() {
        let (xy, clipped) = clip_to_rect([100.0, 25.0], 40.0, 40.0);
        close(xy, [20.0, 5.0]);
        assert!(clipped);
        let (xy, clipped) = clip_to_rect([3.0, -4.0], 40.0, 40.0);
        close(xy, [3.0, -4.0]);
        assert!(!clipped);
        let (xy, _) = clip_to_rect([10.0, -90.0], 40.0, 20.0);
        close(xy, [10.0 * 10.0 / 90.0, -10.0]);
    }

    #[test]
    fn the_full_map_keeps_its_aspect_inside_the_menu_rect() {
        // A map twice as wide as tall in a square: full width, half height, centred.
        let r = fit_map([200.0, 100.0], [0.0, 0.0, 400.0, 400.0], 0.0);
        assert_eq!(r, [0.0, 100.0, 400.0, 200.0]);
        // A tall map in a square: full height, narrowed.
        let r = fit_map([100.0, 200.0], [0.0, 0.0, 400.0, 400.0], 2.0);
        assert_eq!(r, [102.0, 2.0, 196.0, 396.0]);
        // The border never eats more than a quarter.
        let r = fit_map([100.0, 100.0], [0.0, 0.0, 8.0, 8.0], 50.0);
        assert_eq!(r, [2.0, 2.0, 4.0, 4.0]);
    }

    #[test]
    fn angles_fold_to_the_short_way_round() {
        assert_eq!(angle_delta(10.0, 350.0), 20.0);
        assert_eq!(angle_delta(350.0, 10.0), -20.0);
        assert_eq!(norm360(-90.0), 270.0);
        let (l, r) = tape_span(90.0, 0.0, 0.5);
        assert!((l + 0.5).abs() < 1e-6 && (r - 0.0).abs() < 1e-6);
    }
}
