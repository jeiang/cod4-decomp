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

/// `clientSideEffects` (`CG_RegisterDvars` registers it as a cheat bool, default on): whether a level starts its
/// `_fx.gsc` effects. `CG_StartClientSideEffects` reads it when the level loads. A cheat dvar: a write is refused
/// while `sv_cheats` is off and cheats going off put it back to 1, so the store's value is what acts.
pub fn enabled_by(cvars: &crate::input::Cvars) -> bool {
    cvars
        .get("clientSideEffects")
        .and_then(|v| v.trim().parse::<f32>().ok())
        .is_none_or(|v| v != 0.0)
}

/// `MAX_CLIENT_ENT_SOUNDS`: the most createfx sounds a level keeps.
pub const MAX_SOUNDS: usize = 128;

pub struct Ambient {
    defs: Vec<AmbientEffect>,
    /// The createfx sounds (`CG_AddClientEntSound`): each alias plays at its origin.
    pub sounds: Vec<([f32; 3], String)>,
    pub played: BTreeMap<String, u64>,
    pub quads: BTreeMap<String, u64>,
}

impl Ambient {
    pub fn empty() -> Self {
        Self {
            defs: Vec::new(),
            sounds: Vec::new(),
            played: BTreeMap::new(),
            quads: BTreeMap::new(),
        }
    }

    pub fn new(content: &Content, fx: &mut Fx) -> Self {
        let (mut defs, sounds) = content
            .map_name
            .as_deref()
            .map_or_else(Default::default, |map| parse(content, map, fx.library()));
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
            sounds,
            played,
            quads,
        }
    }

    pub fn live_effects(&self, fx: &Fx) -> usize {
        self.defs.iter().filter(|d| fx.is_live(d.handle)).count()
    }

    /// Every effect placement, for looking directly at a map's ambient effect.
    #[cfg(not(target_arch = "wasm32"))]
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

const EFFECT: &str = "ent = maps\\mp\\_utility::createOneshotEffect";
const LOOP_SOUND: &str = "ent = maps\\mp\\_createfx::createLoopSound();";

/// The createfx file's entity blocks in file order: whether each is a loop sound, and its body.
fn blocks(text: &str) -> Vec<(bool, &str)> {
    let next = |s: &str| {
        [(false, s.find(EFFECT)), (true, s.find(LOOP_SOUND))]
            .into_iter()
            .filter_map(|(sound, p)| Some((p?, sound)))
            .min()
    };
    let mut out = Vec::new();
    let mut rest = text;
    while let Some((p, sound)) = next(rest) {
        rest = &rest[p + if sound {
            LOOP_SOUND.len()
        } else {
            EFFECT.len()
        }..];
        let end = next(rest).map_or(rest.len(), |(e, _)| e);
        out.push((sound, &rest[..end]));
        rest = &rest[end..];
    }
    out
}

/// The createfx sounds in file order (`CG_ParseSound`, and a oneshot effect's optional `soundalias`, which
/// `CG_ParseEffect` adds whether or not the effect itself resolves), at most [`MAX_SOUNDS`].
fn sounds(text: &str, map: &str) -> Vec<([f32; 3], String)> {
    let mut out = Vec::new();
    for (loop_sound, body) in blocks(text) {
        let alias = string_after(body, "\"soundalias\"");
        let origin = vector_after(body, "\"origin\"");
        let (Some(alias), Some(origin)) = (alias, origin) else {
            if loop_sound {
                tracing::warn!(map, "createfx loop sound has no origin or alias");
            }
            continue;
        };
        if out.len() == MAX_SOUNDS {
            tracing::warn!(
                map,
                "Too many client ent sounds.  Increase MAX_CLIENT_ENT_SOUNDS."
            );
            continue;
        }
        out.push((origin, alias.to_owned()));
    }
    out
}

fn parse(
    content: &Content,
    map: &str,
    lib: &Library,
) -> (Vec<AmbientEffect>, Vec<([f32; 3], String)>) {
    let Some(map_text) = content.rawfile(&format!("maps/mp/{map}_fx.gsc")) else {
        tracing::warn!(map, "map effect mapping is missing");
        return Default::default();
    };
    let Ok(map_text) = std::str::from_utf8(map_text) else {
        tracing::warn!(map, "map effect mapping is not UTF-8");
        return Default::default();
    };
    let names = mappings(map_text, map);
    let path = format!("maps/createfx/{map}_fx.gsc");
    let Some(text) = content.rawfile(&path) else {
        tracing::warn!(map, path, "map createfx file is missing");
        return Default::default();
    };
    let Ok(text) = std::str::from_utf8(text) else {
        tracing::warn!(map, path, "map createfx file is not UTF-8");
        return Default::default();
    };
    let mut out = Vec::new();
    for (_, body) in blocks(text).into_iter().filter(|(sound, _)| !sound) {
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
    (out, sounds(text, map))
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
    fn client_side_effects_turn_off_only_with_cheats() {
        let mut c = crate::input::Cvars::default();
        assert!(enabled_by(&c));
        c.set("clientSideEffects", "0", false);
        assert!(!enabled_by(&c));
        c.set_cheat_state(false);
        assert!(enabled_by(&c));
        c.set_cheat_state(true);
        c.set("clientSideEffects", "1", false);
        assert!(enabled_by(&c));
    }
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

    #[test]
    fn loop_sounds_and_effect_sounds_are_kept_in_file_order() {
        let effect = |o: i32, alias: Option<&str>| {
            format!(
                "\tent = maps\\mp\\_utility::createOneshotEffect( \"a\" );\n\tent.v[ \"origin\" ] = ( {o}, 0, 0 );\n\
                 \tent.v[ \"angles\" ] = ( 0, 0, 0 );\n\tent.v[ \"fxid\" ] = \"a\";\n\tent.v[ \"delay\" ] = -15;\n{}",
                alias.map_or(String::new(), |a| format!(
                    "\tent.v[ \"soundalias\" ] = \"{a}\";\n"
                ))
            )
        };
        let sound = |o: i32, alias: &str| {
            format!(
                "\tent = maps\\mp\\_createfx::createLoopSound();\n\tent.v[ \"origin\" ] = ( {o}, 0, 0 );\n\
                 \tent.v[ \"angles\" ] = ( 0, 0, 0 );\n\tent.v[ \"soundalias\" ] = \"{alias}\";\n"
            )
        };
        let text = [
            effect(1, None),
            sound(2, "hum"),
            effect(3, Some("crackle")),
            sound(4, "wind"),
        ]
        .concat();
        // The loop sound after an effect is not that effect's sound.
        assert_eq!(blocks(&text).iter().filter(|(s, _)| !s).count(), 2);
        assert_eq!(
            sounds(&text, "test"),
            vec![
                ([2.0, 0.0, 0.0], "hum".to_owned()),
                ([3.0, 0.0, 0.0], "crackle".to_owned()),
                ([4.0, 0.0, 0.0], "wind".to_owned()),
            ]
        );
        let many: String = (0..MAX_SOUNDS as i32 + 3)
            .map(|i| sound(i, "hum"))
            .collect();
        assert_eq!(sounds(&many, "test").len(), MAX_SOUNDS);
    }
}
