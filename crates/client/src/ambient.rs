// SPDX-License-Identifier: GPL-3.0-only
//! The map's client-side createfx effects. The mapping file gives each effect key its definition;
//! the generated createfx file gives the definition key's placement and start time.
//! `CG_LoadClientEffectMapping`, `CG_ParseClientEffects` and `CG_ParseEffect` in KisakCOD
//! `cgame_mp/cg_client_side_effects_mp.cpp` (CoD4 MP 1.8, Activision/IW).
use fx::{Frame, Fx, Library};
use glam::Vec3;
use server::content::Content;
use std::collections::{BTreeMap, BTreeSet};

/// One effect from a map's generated createfx file.
pub struct AmbientEffect {
    pub name: String,
    frame: Frame,
    start_ms: i32,
    handle: u64,
}

pub struct Ambient {
    defs: Vec<AmbientEffect>,
    pub played: BTreeMap<String, u64>,
    pub quads: BTreeMap<String, u64>,
}

impl Ambient {
    pub fn empty() -> Self {
        Self {
            defs: Vec::new(),
            played: BTreeMap::new(),
            quads: BTreeMap::new(),
        }
    }

    pub fn new(content: &Content, fx: &mut Fx) -> Self {
        let mut defs = content
            .map_name
            .as_deref()
            .map_or_else(Vec::new, |map| parse(content, map, fx.library()));
        let mut played = BTreeMap::new();
        let mut quads = BTreeMap::new();
        for d in &mut defs {
            if let Some(def) = fx.library().get(&d.name).cloned() {
                d.handle = fx.play_at(&def, d.frame, d.start_ms);
                if d.handle != 0 {
                    *played.entry(d.name.clone()).or_default() += 1;
                    quads.entry(d.name.clone()).or_default();
                }
            }
        }
        Self {
            defs,
            played,
            quads,
        }
    }

    pub fn live_effects(&self, fx: &Fx) -> usize {
        self.defs.iter().filter(|d| fx.is_live(d.handle)).count()
    }

    /// Every effect placement, for looking directly at a map's ambient effect.
    pub fn poses(&self) -> impl Iterator<Item = (&str, Vec3, Vec3)> {
        self.defs
            .iter()
            .map(|d| (d.name.as_str(), d.frame.origin, d.frame.axis[0]))
    }

    /// Names of ambient effects that have never drawn anything (sprite quad, cloud, model or trail), counting what
    /// their runner children draw.
    pub fn without_quads(&self) -> BTreeSet<&str> {
        self.defs
            .iter()
            .filter(|d| self.quads.get(&d.name).copied().unwrap_or(0) == 0)
            .map(|d| d.name.as_str())
            .collect()
    }
}

fn parse(content: &Content, map: &str, lib: &Library) -> Vec<AmbientEffect> {
    let Some(map_text) = content.rawfile(&format!("maps/mp/{map}_fx.gsc")) else {
        tracing::warn!(map, "map effect mapping is missing");
        return Vec::new();
    };
    let Ok(map_text) = std::str::from_utf8(map_text) else {
        tracing::warn!(map, "map effect mapping is not UTF-8");
        return Vec::new();
    };
    let names = mappings(map_text, map);
    let path = format!("maps/createfx/{map}_fx.gsc");
    let Some(text) = content.rawfile(&path) else {
        tracing::warn!(map, path, "map createfx file is missing");
        return Vec::new();
    };
    let Ok(text) = std::str::from_utf8(text) else {
        tracing::warn!(map, path, "map createfx file is not UTF-8");
        return Vec::new();
    };
    let marker = "ent = maps\\mp\\_utility::createOneshotEffect";
    let mut rest = text;
    let mut out = Vec::new();
    while let Some(p) = rest.find(marker) {
        rest = &rest[p + marker.len()..];
        let end = rest.find(marker).unwrap_or(rest.len());
        let body = &rest[..end];
        rest = &rest[end..];
        let Some(origin) = vector_after(body, "\"origin\"") else {
            tracing::warn!(map, path, "createfx effect has no origin");
            continue;
        };
        let Some(angles) = vector_after(body, "\"angles\"") else {
            tracing::warn!(map, path, "createfx effect has no angles");
            continue;
        };
        let Some(key) = string_after(body, "\"fxid\"") else {
            tracing::warn!(map, path, "createfx effect has no effect key");
            continue;
        };
        let Some(delay) = number_after(body, "\"delay\"") else {
            tracing::warn!(map, path, "createfx effect has no delay");
            continue;
        };
        let Some(name) = names.get(key) else {
            tracing::warn!(map, path, key, "createfx effect key has no mapping");
            continue;
        };
        if !lib
            .get(name)
            .is_some_and(|d| !d.name.as_deref().is_some_and(|n| n.starts_with(',')))
        {
            tracing::warn!(
                map,
                path,
                effect = name,
                "createfx effect definition is missing"
            );
            continue;
        };
        let (forward, right, up) = sim::pm::math::angle_vectors(&angles);
        out.push(AmbientEffect {
            // Lower case, as the library resolves names: the counters are keyed by it.
            name: name.to_lowercase(),
            frame: Frame {
                origin: Vec3::from(origin),
                axis: [Vec3::from(forward), -Vec3::from(right), Vec3::from(up)],
            },
            start_ms: (delay * 1000.0) as i32,
            handle: 0,
        });
    }
    out
}

/// The effect key to definition mappings a map's effect script assigns, one per line that begins
/// `level._effect[`; script that merely reads `level._effect[...]` is not a mapping.
fn mappings(map_text: &str, map: &str) -> BTreeMap<String, String> {
    let mut names = BTreeMap::new();
    for line in map_text.lines() {
        let Some(rest) = line.trim_start().strip_prefix("level._effect[") else {
            continue;
        };
        let Some(p) = rest.find("loadfx") else {
            continue;
        };
        // A key that is not a literal (`level._effect[ fxName ]`) is a script helper, not a map mapping.
        let Some(key) = quoted(&rest[..p]) else {
            continue;
        };
        let rest = &rest[p + "loadfx".len()..];
        let Some(file) = rest.find('(').and_then(|p| quoted(&rest[p + 1..])) else {
            tracing::warn!(map, "malformed map effect definition");
            continue;
        };
        names.insert(key.to_owned(), file.to_owned());
    }
    names
}

fn quoted(s: &str) -> Option<&str> {
    let start = s.find('"')? + 1;
    let end = s[start..].find('"')?;
    Some(&s[start..start + end])
}

fn string_after<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let p = s.find(key)?;
    quoted(&s[p + key.len()..])
}

fn vector_after(s: &str, key: &str) -> Option<[f32; 3]> {
    let p = s.find(key)?;
    let s = &s[p + key.len()..];
    let open = s.find('(')?;
    let close = s[open + 1..].find(')')?;
    let values: Vec<_> = s[open + 1..open + 1 + close]
        .split(',')
        .map(str::trim)
        .map(str::parse::<f32>)
        .collect::<Result<_, _>>()
        .ok()?;
    values.try_into().ok()
}

fn number_after(s: &str, key: &str) -> Option<f32> {
    let p = s.find(key)?;
    let s = s[p + key.len()..].split_once('=')?.1;
    s.split_once(';')?.0.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_placements_parse_without_content() {
        let s = "ent.v[ \"origin\" ] = ( 1, -2.5, 30 );\nent.v[ \"angles\" ] = ( 270, 0, 0 );";
        assert_eq!(vector_after(s, "\"origin\""), Some([1.0, -2.5, 30.0]));
        assert_eq!(vector_after(s, "\"angles\""), Some([270.0, 0.0, 0.0]));
        assert_eq!(
            string_after("ent.v[ \"fxid\" ] = \"rain\";", "\"fxid\""),
            Some("rain")
        );
        assert_eq!(
            number_after("ent.v[ \"delay\" ] = 0.2;", "\"delay\""),
            Some(0.2)
        );
    }

    #[test]
    fn only_lines_that_assign_a_mapping_are_mappings() {
        let s = "main()\n{\n\tlevel._effect[ \"rain\" ] = loadfx( \"weather/rain_a\" );\n\
                 \tif ( !isdefined( level._effect[ fxName ] ) )\n\t\tlevel._effect[ fxName ] = loadfx( fxFile );\n}\n";
        let names = mappings(s, "test");
        assert_eq!(names.len(), 1);
        assert_eq!(
            names.get("rain").map(String::as_str),
            Some("weather/rain_a")
        );
    }
}
