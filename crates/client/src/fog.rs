// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (GPL-3.0, KisakCOD contributors): `cgame_mp/cg_servercmds_mp.cpp`
// (`CG_ParseFog`), `gfx_d3d/r_fog.cpp` (`R_SetFogFromServer`, `R_SwitchFog`) and `gfx_d3d/r_scene.cpp`
// (`R_UpdateFrameFog`).
//! The fog the server sets with `setExpFog` (its `cs::FOGVARS` configstring) and the blend from the fog before it.

use render::art::Fog;

/// One fog: where it starts, how fast it thickens (the exponent, `ln 2` over the halfway distance) and its colour.
/// A density of 0 is no fog.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Settings {
    start: f32,
    density: f32,
    color: [f32; 3],
}

impl Settings {
    fn lerp(self, to: Settings, t: f32) -> Settings {
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Settings {
            start: mix(self.start, to.start),
            density: mix(self.density, to.density),
            color: [0, 1, 2].map(|i| mix(self.color[i], to.color[i])),
        }
    }
}

/// The fog the server last announced and the blend toward it.
#[derive(Debug, Clone, Default)]
pub struct FogState {
    /// The configstring last read, so the same text is not a new fog.
    text: String,
    from: Settings,
    to: Settings,
    /// The server times the blend starts and ends at (equal: no blend).
    start: i32,
    finish: i32,
}

impl FogState {
    /// Forgets the fog (a new level).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn now(&self, time: i32) -> Settings {
        if time >= self.finish {
            return self.to;
        }
        let span = (self.finish - self.start).max(1) as f32;
        let t = ((time - self.start) as f32 / span).clamp(0.0, 1.0);
        self.from.lerp(self.to, t)
    }

    /// `CG_ParseFog`: reads the configstring `text` (`<start> <density> <r> <g> <b> <transition ms>`, or a lone
    /// number, the milliseconds to fade the fog out over); a changed one starts a blend at `time`.
    pub fn follow(&mut self, text: &str, time: i32) {
        if text == self.text {
            return;
        }
        self.text = text.to_owned();
        let mut it = text.split_whitespace().map(|t| t.parse::<f32>().ok());
        let first = it.next().flatten().unwrap_or(0.0);
        let rest: Vec<f32> = it.map_while(|t| t).collect();
        let (target, ms) = match rest[..] {
            [density, r, g, b, ms, ..] => (
                Settings {
                    start: first,
                    density,
                    color: [r, g, b],
                },
                ms as i32,
            ),
            _ => (Settings::default(), first as i32),
        };
        // From nothing there is nothing to blend: the fog is simply there.
        let from = self.now(time);
        let ms = if from.density == 0.0 { 0 } else { ms.max(0) };
        self.from = if ms == 0 { target } else { from };
        self.to = target;
        (self.start, self.finish) = (time, time + ms);
    }

    /// The fog to draw at server time `time`, if any.
    pub fn at(&self, time: i32) -> Option<Fog> {
        let s = self.now(time);
        (s.density > 0.0).then(|| Fog {
            start: s.start,
            halfway: std::f32::consts::LN_2 / s.density,
            color: s.color,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn halfway(f: Option<Fog>) -> Option<f32> {
        f.map(|f| f.halfway)
    }

    #[test]
    fn a_fog_the_server_sets_is_drawn_and_none_is_none() {
        let mut f = FogState::default();
        assert_eq!(f.at(0), None);
        f.follow("0", 0);
        assert_eq!(f.at(0), None);
        f.follow("100 0.001 0.5 0.25 0 0", 10);
        let fog = f.at(10).unwrap();
        assert_eq!((fog.start, fog.color), (100.0, [0.5, 0.25, 0.0]));
        assert!((fog.halfway - std::f32::consts::LN_2 / 0.001).abs() < 1e-3);
    }

    #[test]
    fn a_new_fog_blends_from_the_old_over_the_transition() {
        let mut f = FogState::default();
        f.follow("100 0.001 1 0 0 0", 0);
        // Over two seconds to a thicker, darker one.
        f.follow("300 0.003 0 0 1 2000", 1000);
        let at = |t| f.at(t).unwrap();
        assert_eq!((at(1000).start, at(1000).color), (100.0, [1.0, 0.0, 0.0]));
        let mid = at(2000);
        assert_eq!((mid.start, mid.color), (200.0, [0.5, 0.0, 0.5]));
        assert!((mid.halfway - std::f32::consts::LN_2 / 0.002).abs() < 1e-2);
        assert_eq!((at(3000).start, at(9000).color), (300.0, [0.0, 0.0, 1.0]));
    }

    #[test]
    fn the_first_fog_and_an_unchanged_one_do_not_blend() {
        let mut f = FogState::default();
        f.follow("100 0.001 1 0 0 5000", 0);
        assert_eq!(f.at(0).unwrap().color, [1.0, 0.0, 0.0]);
        let before = f.at(0);
        // The same text again must not restart anything, even later.
        f.follow("100 0.001 1 0 0 5000", 4000);
        assert_eq!(f.at(4000), before);
    }

    #[test]
    fn fog_fades_out_over_the_time_a_lone_number_gives() {
        let mut f = FogState::default();
        f.follow("0 0.002 0 0 0 0", 0);
        f.follow("1000", 100);
        let thin = halfway(f.at(600)).unwrap();
        assert!(
            thin > std::f32::consts::LN_2 / 0.002,
            "thinner half way: {thin}"
        );
        assert_eq!(f.at(1100), None);
    }
}
