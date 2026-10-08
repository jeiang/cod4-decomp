// SPDX-License-Identifier: GPL-3.0-only
//! The art settings a map carries: its exponential fog (the `setExpFog` call of its `createart` script) and the glow
//! and film values of its vision file (dvar assignments, one per line).
//!
//! The original engine takes both from script and dvars at run time; until the game scripts drive the client, the
//! renderer reads them straight from the map's raw files.

/// `setExpFog(start, halfway, r, g, b, transition)`: fog that starts at `start` and doubles its coverage at every
/// `halfway` units beyond it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fog {
    pub start: f32,
    pub halfway: f32,
    pub color: [f32; 3],
}

impl Fog {
    /// Exponential density: half of the scene colour survives `halfway` units past the start.
    pub fn density(&self) -> f32 {
        std::f32::consts::LN_2 / self.halfway
    }

    /// The `FOG` code constant: the vertex shaders compute `exp(distance * -density + start * density)`.
    pub fn constant(&self) -> [f32; 4] {
        let d = self.density();
        [0.0, 1.0, -d, self.start * d]
    }

    /// The `FOG` constant when fog is off: the factor stays at one.
    pub const OFF: [f32; 4] = [0.0, 1.0, 0.0, 0.0];
}

/// Bloom and sky-bleed settings of the glow post effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glow {
    pub enabled: bool,
    /// In virtual 640x480 pixels.
    pub radius: f32,
    pub bloom_cutoff: f32,
    pub bloom_desaturation: f32,
    pub bloom_intensity: f32,
    pub sky_bleed_intensity: f32,
}

impl Default for Glow {
    fn default() -> Self {
        Glow {
            enabled: false,
            radius: 0.0,
            bloom_cutoff: 0.5,
            bloom_desaturation: 0.0,
            bloom_intensity: 0.0,
            sky_bleed_intensity: 0.0,
        }
    }
}

/// Colour grading of the film post effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Film {
    pub enabled: bool,
    pub contrast: f32,
    pub brightness: f32,
    pub desaturation: f32,
    pub invert: bool,
    pub tint_light: [f32; 3],
    pub tint_dark: [f32; 3],
}

impl Default for Film {
    fn default() -> Self {
        Film {
            enabled: false,
            contrast: 1.0,
            brightness: 0.0,
            desaturation: 0.0,
            invert: false,
            tint_light: [1.0; 3],
            tint_dark: [1.0; 3],
        }
    }
}

impl Film {
    /// Whether the grade changes the picture at all.
    pub fn active(&self) -> bool {
        self.enabled
            && (self.contrast != 1.0
                || self.brightness != 0.0
                || self.desaturation != 0.0
                || self.invert
                || self.tint_dark != [1.0; 3]
                || self.tint_light != [1.0; 3])
    }

    /// The `COLOR_BIAS`, `COLOR_TINT_BASE` and `COLOR_TINT_DELTA` constants the film shaders read.
    pub fn constants(&self) -> [[f32; 4]; 3] {
        const EPS: f32 = 1.0 / 4096.0;
        if !self.enabled {
            return [
                [0.0, 0.0, 0.0, 4095.0],
                [EPS, EPS, EPS, 0.0],
                [0.0, 0.0, 0.0, 0.0],
            ];
        }
        let d = self.desaturation;
        let keep = if EPS - d < 0.0 { d } else { EPS };
        let desaturation_scale = 1.0 / keep - 1.0;
        let mut scale = self.contrast * keep;
        let mut bias = self.brightness + 0.5 - self.contrast * 0.5;
        if self.invert {
            scale = -scale;
            bias += 1.0;
        }
        let base = self.tint_dark.map(|t| t * scale);
        let delta = [0, 1, 2].map(|i| (self.tint_light[i] - self.tint_dark[i]) * scale);
        [
            [bias, bias, bias, desaturation_scale],
            [base[0], base[1], base[2], 0.0],
            [delta[0], delta[1], delta[2], 0.0],
        ]
    }
}

/// Fog, glow and film of one map.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapArt {
    pub fog: Option<Fog>,
    pub glow: Glow,
    pub film: Film,
}

impl MapArt {
    /// `art` is the text of `maps/createart/<map>_art.gsc`, `vision` the text of the map's vision file.
    pub fn parse(art: Option<&str>, vision: Option<&str>) -> MapArt {
        let mut m = MapArt {
            fog: art.and_then(parse_exp_fog),
            ..Default::default()
        };
        for line in vision.unwrap_or_default().lines() {
            let mut it = line.split_whitespace();
            let (Some(name), Some(first)) = (it.next(), it.next()) else {
                continue;
            };
            let value = line[line.find(first).unwrap_or(0)..]
                .trim()
                .trim_matches('"');
            let floats: Vec<f32> = value
                .split_whitespace()
                .filter_map(|v| v.parse().ok())
                .collect();
            let one = floats.first().copied();
            let vec3 = (floats.len() >= 3).then(|| [floats[0], floats[1], floats[2]]);
            match (name.to_ascii_lowercase().as_str(), one, vec3) {
                ("r_glow", Some(v), _) => m.glow.enabled = v != 0.0,
                ("r_glowradius0", Some(v), _) => m.glow.radius = v,
                ("r_glowbloomcutoff", Some(v), _) => m.glow.bloom_cutoff = v,
                ("r_glowbloomdesaturation", Some(v), _) => m.glow.bloom_desaturation = v,
                ("r_glowbloomintensity0", Some(v), _) => m.glow.bloom_intensity = v,
                ("r_glowskybleedintensity0", Some(v), _) => m.glow.sky_bleed_intensity = v,
                ("r_filmenable", Some(v), _) => m.film.enabled = v != 0.0,
                ("r_filmcontrast", Some(v), _) => m.film.contrast = v,
                ("r_filmbrightness", Some(v), _) => m.film.brightness = v,
                ("r_filmdesaturation", Some(v), _) => m.film.desaturation = v,
                ("r_filminvert", Some(v), _) => m.film.invert = v != 0.0,
                ("r_filmlighttint", _, Some(v)) => m.film.tint_light = v,
                ("r_filmdarktint", _, Some(v)) => m.film.tint_dark = v,
                _ => {}
            }
        }
        m
    }
}

/// The last `setExpFog(...)` call of a script, with literal arguments.
fn parse_exp_fog(script: &str) -> Option<Fog> {
    let lower = script.to_ascii_lowercase();
    let at = lower.rfind("setexpfog")?;
    let rest = &script[at + "setexpfog".len()..];
    let open = rest.find('(')?;
    let close = rest.find(')')?;
    let args: Vec<f32> = rest[open + 1..close]
        .split(',')
        .filter_map(|a| a.trim().parse().ok())
        .collect();
    (args.len() >= 5 && args[1] > 0.0).then(|| Fog {
        start: args[0],
        halfway: args[1],
        color: [args[2], args[3], args[4]],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fog_halves_the_scene_every_halfway_distance_past_the_start() {
        let f = Fog {
            start: 100.0,
            halfway: 800.0,
            color: [0.5; 3],
        };
        let c = f.constant();
        let factor = |dist: f32| (dist * c[2] + c[3]).exp().min(1.0);
        assert_eq!(factor(50.0), 1.0);
        assert!((factor(900.0) - 0.5).abs() < 1e-5);
        assert!((factor(1700.0) - 0.25).abs() < 1e-5);
        assert_eq!(Fog::OFF[2], 0.0);
    }

    #[test]
    fn art_and_vision_text_parse() {
        let art = "main()\n{\n\tsetExpFog( 12, 3500, 0.25, 0.5, 0.75, 0 );\n\tVisionSetNaked( \"x\", 0 );\n}\n";
        let vision = "r_glow\t\t\"1\"\nr_glowRadius0 \"7\"\nr_glowBloomIntensity0 \"0.36\"\n\
                      r_filmEnable \"1\"\nr_filmContrast \"1.4\"\nr_filmLightTint \"1.1 1.0 0.9\"\nunknown \"3\"\n";
        let m = MapArt::parse(Some(art), Some(vision));
        assert_eq!(
            m.fog,
            Some(Fog {
                start: 12.0,
                halfway: 3500.0,
                color: [0.25, 0.5, 0.75]
            })
        );
        assert!(m.glow.enabled && m.glow.radius == 7.0 && m.glow.bloom_intensity == 0.36);
        assert!(m.film.enabled && m.film.contrast == 1.4 && m.film.tint_light == [1.1, 1.0, 0.9]);
        assert!(m.film.active());
    }

    #[test]
    fn a_neutral_film_is_inactive_and_inverting_flips_the_scale() {
        let mut f = Film {
            enabled: true,
            ..Film::default()
        };
        assert!(!f.active());
        let normal = f.constants();
        f.invert = true;
        let inverted = f.constants();
        assert_eq!(inverted[1][0], -normal[1][0]);
        assert_eq!(inverted[0][0], normal[0][0] + 1.0);
    }
}
