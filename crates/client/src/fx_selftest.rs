// SPDX-License-Identifier: GPL-3.0-or-later
//! `--fx-selftest`: the effects on the real content, without a window or a GPU. An explosion draws sprites and then
//! ends, a bullet that hits the map leaves a decal clipped to the map's surface, a shot plays its muzzle flash and
//! ejects a shell, and the vision and shock files the stock scripts name are in the install and take effect. Reports
//! the numbers as JSON.

use crate::effects::Effects;
use crate::events::ClientEvent;
use crate::look::Look;
use crate::models::Library;
use net::predict::PlayerBoxes;
use net::ui::ServerCmd;
use serde_json::{Value, json};
use sim::cm::Collide;
use std::path::Path;

const GUN: &str = "ak47_mp";
const ROCKET: &str = "rpg_mp";
const EXPLOSION: &str = "explosions/grenadeexp_dirt_1";
const VISION: &str = "mp_crash";
/// A stock map whose puddles use the water simulation.
const WATER_MAP: &str = "mp_farm";
const SHOCK: &str = "concussion_grenade_mp";

/// Plays `effects` from `from_ms` for `secs` seconds in 50 ms steps and returns (most sprites in one frame, most
/// decals, sprites at the end, live elements at the end).
fn run_for(
    effects: &mut Effects,
    world: &dyn Collide,
    from_ms: i32,
    secs: i32,
    eye: glam::Vec3,
    pitch: f32,
) -> (usize, usize, usize, usize) {
    let (mut quads, mut decals) = (0, 0);
    let mut last = (0, 0);
    for step in 1..=secs * 20 {
        effects.update(from_ms + step * 50, world);
        let d = effects.draw(eye, 0.0, pitch);
        quads = quads.max(d.quads);
        decals = decals.max(d.decals);
        last = (d.quads, effects.live_elems());
    }
    (quads, decals, last.0, last.1)
}

pub fn run(install: &Path, map: &str) -> Result<Value, Vec<String>> {
    let lib = Library::load(install, map).map_err(|e| vec![e])?;
    let data = render::MapData::load(install, map).map_err(|e| vec![format!("{e:?}")])?;
    let clipmap = data
        .clipmap
        .clone()
        .ok_or_else(|| vec!["the map has no collision data".to_owned()])?;
    let boxes = PlayerBoxes::new(clipmap);
    let world = boxes.world();
    let mut bad = Vec::new();
    let mut report = serde_json::Map::new();

    // A spot on the floor of the map to shoot at, found by dropping a line from above on a grid.
    let (lo, hi) = (data.world.mins, data.world.maxs);
    let mut floor = None;
    'find: for i in 0..40 {
        for j in 0..40 {
            let x = lo[0] + (hi[0] - lo[0]) * (i as f32 + 0.5) / 40.0;
            let y = lo[1] + (hi[1] - lo[1]) * (j as f32 + 0.5) / 40.0;
            let t = world.trace(
                [x, y, hi[2]],
                [x, y, lo[2]],
                [0.0; 3],
                [0.0; 3],
                sim::cm::ENTITYNUM_NONE,
                sim::contents::SOLID,
            );
            if t.fraction < 1.0 && !t.start_solid && t.normal[2] > 0.9 {
                floor = Some(([x, y, hi[2] + (lo[2] - hi[2]) * t.fraction], t.normal));
                break 'find;
            }
        }
    }
    let Some((at, normal)) = floor else {
        return Err(vec!["no floor found to shoot at".into()]);
    };
    let eye = glam::Vec3::new(at[0], at[1], at[2] + 60.0);

    let Some(gun) = lib.content.weapon(GUN).cloned() else {
        return Err(vec![format!("weapon {GUN} is not in the content")]);
    };
    let weapon = |_: u16| Some(gun.clone());

    // An explosion draws, and ends.
    let mut fx = Effects::new(&lib.content, data.world.clone());
    fx.demo(EXPLOSION, eye, 0.0, 0.0);
    if !fx.missing.is_empty() {
        bad.push(format!("effect {EXPLOSION} is missing: {:?}", fx.missing));
    }
    let (quads, _, end_quads, end_live) = run_for(&mut fx, world, 0, 90, eye, 0.0);
    report.insert("explosion_quads_max".into(), quads.into());
    if quads == 0 {
        bad.push("the explosion never drew a sprite".into());
    }
    if end_quads != 0 || end_live != 0 {
        bad.push(format!(
            "the explosion had not ended after 90 s: {end_quads} sprites, {end_live} elements"
        ));
    }

    // A bullet into the floor leaves a decal on it, for whichever surface type has one.
    let mut decals_by_surface = Vec::new();
    for surface in 0..24u8 {
        let mut fx = Effects::new(&lib.content, data.world.clone());
        fx.event(
            &ClientEvent::BulletImpact {
                origin: at,
                normal,
                surface,
                weapon: 0,
                shooter: 0,
            },
            &weapon,
        );
        let (_, decals, _, _) = run_for(&mut fx, world, 0, 1, eye, -1.2);
        decals_by_surface.push(decals);
    }
    let with_decal = decals_by_surface.iter().filter(|d| **d > 0).count();
    report.insert("surfaces_with_a_decal".into(), with_decal.into());
    if with_decal == 0 {
        bad.push(format!(
            "no surface type's impact left a decal on the floor: {decals_by_surface:?}"
        ));
    }

    // A shot flashes and ejects a shell.
    let mut fx = Effects::new(&lib.content, data.world.clone());
    fx.event(
        &ClientEvent::WeaponFire {
            eye: [eye.x, eye.y, eye.z],
            angles: [0.0, 0.0, 0.0],
            weapon: 0,
            shooter: 7,
            vehicle: false,
        },
        &weapon,
    );
    let flashed = fx.played.get("muzzle_flash").copied().unwrap_or(0);
    let ejected = fx.played.get("shell_eject").copied().unwrap_or(0);
    let (flash_quads, _, _, _) = run_for(&mut fx, world, 0, 1, eye, 0.0);
    report.insert("flash_quads_max".into(), flash_quads.into());
    if flashed != 1 || ejected != 1 {
        bad.push(format!(
            "a shot should play one flash and one shell, played {flashed} and {ejected}"
        ));
    }
    if flash_quads == 0 {
        bad.push("the muzzle flash drew no sprite".into());
    }

    // A rocket in flight leaves a smoke trail behind it: a strip through the points it passed, which ends once the
    // rocket is gone and the smoke has faded.
    match lib.content.weapon(ROCKET).cloned() {
        Some(rocket) if rocket.proj_trail_effect.is_some() => {
            let rw = |_: u16| Some(rocket.clone());
            let mut fx = Effects::new(&lib.content, data.world.clone());
            let (mut strips, mut tris) = (0, 0);
            for step in 1..=60 {
                let at = eye + glam::Vec3::X * (step as f32 * 40.0) + glam::Vec3::Z * 200.0;
                fx.missiles(&[(9, at, glam::Vec3::X * 1200.0, 0)], &rw);
                fx.update(step * 33, world);
                let d = fx.draw(eye, 0.0, 0.0);
                strips = strips.max(d.trails);
                tris = tris.max(d.meshes.iter().map(|m| m.verts.len() / 3).sum::<usize>());
            }
            fx.missiles(&[], &rw);
            let end = (2..=240)
                .map(|s| {
                    fx.update(60 * 33 + s * 100, world);
                    fx.draw(eye, 0.0, 0.0).trails
                })
                .last()
                .unwrap_or(0);
            report.insert("rocket_trail_strips_max".into(), strips.into());
            report.insert("rocket_trail_tris_max".into(), tris.into());
            if strips == 0 || tris == 0 {
                bad.push("the rocket's smoke trail drew no strip".into());
            }
            if end != 0 || fx.live_elems() != 0 {
                bad.push(format!(
                    "the rocket's trail had not ended: {end} strips, {} elements",
                    fx.live_elems()
                ));
            }
        }
        _ => bad.push(format!("weapon {ROCKET} has no projectile trail effect")),
    }

    // The stock scripts' vision and shock files are there and do something.
    let file = |n: &str| {
        lib.content.rawfile(n).map(|b| {
            let end = b.iter().position(|&x| x == 0).unwrap_or(b.len());
            String::from_utf8_lossy(&b[..end]).into_owned()
        })
    };
    let mut look = Look::new((data.art.glow, data.art.film));
    look.command(
        &ServerCmd::Vision {
            night: false,
            name: VISION.into(),
            ms: 0,
        },
        0,
        &file,
    );
    look.command(
        &ServerCmd::ShellShock {
            name: SHOCK.into(),
            ms: 4000,
        },
        0,
        &file,
    );
    look.frame(16);
    let shocked = look.frame(32);
    report.insert("look_missing".into(), json!(look.missing));
    if !look.missing.is_empty() {
        bad.push(format!("files missing: {:?}", look.missing));
    }
    if shocked.shell_shock.is_none() && !shocked.save_screen {
        bad.push(format!("shell shock {SHOCK} did nothing to the picture"));
    }

    water(install, &mut report, &mut bad);

    if bad.is_empty() {
        Ok(Value::Object(report))
    } else {
        Err(bad)
    }
}

/// The ocean simulation of a stock map's water: a height image with real relief that moves and stays in range.
fn water(install: &Path, report: &mut serde_json::Map<String, Value>, bad: &mut Vec<String>) {
    use assets::zone::gfx::TextureSource;
    let data = match render::MapData::load(install, WATER_MAP) {
        Ok(d) => d,
        Err(e) => return bad.push(format!("{WATER_MAP}: {e:?}")),
    };
    let water = data
        .world
        .dpvs
        .surfaces
        .iter()
        .filter_map(|s| s.material.as_ref())
        .flat_map(|m| m.textures.iter())
        .find_map(|t| match &t.source {
            TextureSource::Water(Some(w)) => Some(w.clone()),
            _ => None,
        });
    let Some(mut field) = water.and_then(render::water::WaterField::new) else {
        return bad.push(format!("{WATER_MAP} has no usable water"));
    };
    let before = field.pixels().to_vec();
    let n = before.len() as f32;
    let mean = before.iter().map(|&p| f32::from(p)).sum::<f32>() / n;
    let spread = (before
        .iter()
        .map(|&p| (f32::from(p) - mean).powi(2))
        .sum::<f32>()
        / n)
        .sqrt();
    let clipped = before.iter().filter(|&&p| p == 0 || p == 255).count() as f32 / n;
    field.update(2.0);
    let moved = before
        .iter()
        .zip(field.pixels())
        .map(|(&a, &b)| f32::from(a.abs_diff(b)))
        .sum::<f32>()
        / n;
    report.insert("water_spread".into(), spread.into());
    report.insert("water_clipped".into(), clipped.into());
    report.insert("water_moved".into(), moved.into());
    if !(20.0..=60.0).contains(&spread) {
        bad.push(format!(
            "the water height image has a spread of {spread}, not about 42"
        ));
    }
    if clipped > 0.02 {
        bad.push(format!(
            "{:.1}% of the water image is clipped",
            clipped * 100.0
        ));
    }
    if moved < 3.0 {
        bad.push(format!(
            "the water barely moved in two seconds ({moved} per texel)"
        ));
    }
}
