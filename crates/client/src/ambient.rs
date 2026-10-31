// SPDX-License-Identifier: GPL-3.0-only
//! The map's client-side createfx effects. The mapping file gives each effect key its definition;
//! the generated createfx file gives the definition key's placement and repeat interval.
//! `CG_LoadClientEffectMapping`, `CG_ParseClientEffects` and `CG_ParseEffect` in KisakCOD
//! `cgame_mp/cg_client_side_effects_mp.cpp` (CoD4 MP 1.8, Activision/IW).
use fx::{Camera, Draws, Frame, Fx, Library};
use glam::Vec3;
use server::content::Content;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// One effect from a map's generated createfx file.
pub struct AmbientEffect {
    pub name: String,
    frame: Frame,
}

pub struct Ambient {
    fx: Fx,
    defs: Vec<AmbientEffect>,
    pub quads: BTreeMap<String, u64>,
}

impl Ambient {
    pub fn empty() -> Self {
        Self {
            fx: Fx::new(Arc::new(Library::default())),
            defs: Vec::new(),
            quads: BTreeMap::new(),
        }
    }

    pub fn new(content: &Content) -> Self {
        let mut lib = Library::default();
        for e in content.effects() {
            lib.add(e);
        }
        let defs = content
            .map_name
            .as_deref()
            .map_or_else(Vec::new, |map| parse(content, map, &lib));
        let mut fx = Fx::new(Arc::new(lib));
        for d in &defs {
            fx.play_named(&d.name, d.frame);
        }
        Self {
            fx,
            defs,
            quads: BTreeMap::new(),
        }
    }

    pub fn live_elems(&self) -> usize {
        self.fx.live_elems()
    }

    pub fn update(&mut self, now: i32, world: &dyn fx::World) {
        self.fx.update(now, world);
    }

    /// `FX_SpawnOrientedEffect`: every map effect starts when the level starts; its delay is its
    /// repeat period, including the first play.
    pub fn draw(&mut self, cam: &Camera, out: &mut Draws) {
        let mut frame = BTreeMap::new();
        self.fx.draw_counted(cam, out, &mut |name, n| {
            *frame.entry(name.to_owned()).or_insert(0) += n;
        });
        for (name, n) in frame {
            *self.quads.entry(name).or_default() += n as u64;
        }
    }

    /// Every effect placement, for looking directly at a map's ambient effect.
    pub fn poses(&self) -> impl Iterator<Item = (&str, Vec3, Vec3)> {
        self.defs
            .iter()
            .map(|d| (d.name.as_str(), d.frame.origin, d.frame.axis[0]))
    }

    /// Names of ambient effects that have never contributed a sprite quad.
    pub fn without_quads(&self) -> BTreeSet<&str> {
        self.defs
            .iter()
            .filter(|d| !self.quads.contains_key(&d.name))
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
    let mut names = BTreeMap::new();
    let mut rest = map_text;
    while let Some(p) = rest.find("level._effect[") {
        rest = &rest[p + "level._effect[".len()..];
        let Some(p) = rest.find("loadfx") else { break };
        let line = &rest[..p];
        let Some(key) = quoted(line) else {
            tracing::warn!(map, line, "malformed map effect mapping key");
            rest = &rest[p..];
            continue;
        };
        rest = &rest[p + "loadfx".len()..];
        let Some(p) = rest.find('(') else { break };
        let Some(file) = quoted(&rest[p + 1..]) else {
            tracing::warn!(map, "malformed map effect definition");
            break;
        };
        names.insert(key.to_owned(), file.to_owned());
        rest = &rest[p + 1..];
    }
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
            name: name.clone(),
            frame: Frame {
                origin: Vec3::from(origin),
                axis: [Vec3::from(forward), -Vec3::from(right), Vec3::from(up)],
            },
        });
    }
    out
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
}
