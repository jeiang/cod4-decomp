// SPDX-License-Identifier: GPL-3.0-only
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
/// A stock explosion whose chunks are physics models.
const DEBRIS: &str = "explosions/grenadeexp_wood";
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
        let d = effects.draw(eye, 0.0, pitch, 0.0);
        quads = quads.max(d.quads);
        decals = decals.max(d.decals);
        last = (d.quads, effects.live_elems());
    }
    (quads, decals, last.0, last.1)
}

/// A point 60 units above a flat floor that has more floor, within 20 units of its height, 200 units around it in
/// every direction: somewhere a shell that is ejected and rolls a little does not fall off an edge.
fn wide_floor(world: &dyn Collide, lo: [f32; 3], hi: [f32; 3]) -> Option<glam::Vec3> {
    // The first upward-facing surface below `from`, skipping ceilings and walls on the way down.
    let down = |x: f32, y: f32, from: f32| {
        let mut top = from;
        for _ in 0..8 {
            let t = world.trace(
                [x, y, top],
                [x, y, lo[2]],
                [0.0; 3],
                [0.0; 3],
                sim::cm::ENTITYNUM_NONE,
                sim::contents::SOLID,
            );
            if t.fraction >= 1.0 {
                return None;
            }
            let z = top + (lo[2] - top) * t.fraction;
            if !t.start_solid && t.normal[2] > 0.9 {
                return Some(z);
            }
            top = z - 1.0;
        }
        None
    };
    for i in 0..60 {
        for j in 0..60 {
            let x = lo[0] + (hi[0] - lo[0]) * (i as f32 + 0.5) / 60.0;
            let y = lo[1] + (hi[1] - lo[1]) * (j as f32 + 0.5) / 60.0;
            let Some(z) = down(x, y, hi[2]) else {
                continue;
            };
            let flat = (0..8).all(|k| {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                down(x + a.cos() * 200.0, y + a.sin() * 200.0, z + 40.0)
                    .is_some_and(|z2| (z2 - z).abs() < 20.0)
            });
            if flat {
                return Some(glam::Vec3::new(x, y, z + 60.0));
            }
        }
    }
    None
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

    // The debris of an explosion are PhysPreset rigid bodies: chunks come down from where they were thrown, stay
    // above the floor they land on and come to rest; they do not hang in the air or fall through the map.
    let Some(spot) = wide_floor(world, lo, hi) else {
        return Err(vec!["no wide floor found to throw debris on".into()]);
    };
    let mut fx = Effects::new(&lib.content, data.world.clone());
    fx.demo(DEBRIS, spot, 0.0, std::f32::consts::FRAC_PI_2);
    if !fx.missing.is_empty() {
        bad.push(format!("effect {DEBRIS} is missing: {:?}", fx.missing));
    }
    let mut track: Vec<Vec<glam::Vec3>> = Vec::new();
    for step in 1..=160 {
        fx.update(step * 50, world);
        let d = fx.draw(spot, 0.0, 0.0, 0.0);
        track.push(
            d.models
                .iter()
                .map(|m| glam::Vec3::from(m.origin))
                .collect(),
        );
    }
    let floor_z = spot.z - 60.0;
    let highest = track.iter().flatten().map(|p| p.z).fold(f32::MIN, f32::max);
    let alive = track.iter().rposition(|f| !f.is_empty());
    match alive.filter(|&i| i >= 10) {
        None => bad.push(format!("{DEBRIS} threw no debris models")),
        Some(i) => {
            let (now, before) = (&track[i], &track[i - 10]);
            let moving = now
                .iter()
                .map(|p| {
                    before
                        .iter()
                        .map(|q| q.distance(*p))
                        .fold(f32::MAX, f32::min)
                })
                .fold(0.0f32, f32::max);
            let lowest = now.iter().map(|p| p.z).fold(f32::MAX, f32::min);
            report.insert("debris_drop".into(), (highest - lowest).into());
            report.insert("debris_movement_last_half_second".into(), moving.into());
            if highest - lowest < 10.0 {
                bad.push(format!(
                    "the debris hung in the air: z {highest} down to {lowest}"
                ));
            }
            if lowest < floor_z - 20.0 {
                bad.push(format!(
                    "the debris fell through the floor at z {floor_z}: {lowest}"
                ));
            }
            if moving > 0.5 {
                bad.push(format!(
                    "the debris had not come to rest (moved {moving} in its last half second)"
                ));
            }
        }
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
                let d = fx.draw(eye, 0.0, 0.0, 0.0);
                strips = strips.max(d.trails);
                tris = tris.max(d.meshes.iter().map(|m| m.verts.len() / 3).sum::<usize>());
            }
            fx.missiles(&[], &rw);
            let end = (2..=240)
                .map(|s| {
                    fx.update(60 * 33 + s * 100, world);
                    fx.draw(eye, 0.0, 0.0, 0.0).trails
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

    // A looped script effect (`playloopedfx`) keeps spawning for as long as its entity is in the snapshot, and stops once
    // the entity is gone; a camera shake from an earthquake sways the near view and not the far one.
    let effects = lib.content.effects();
    let looped = effects
        .iter()
        .filter(|d| d.looping_count > 0)
        .filter_map(|d| d.name.as_deref())
        .filter(|n| n.contains("fire/") || n.contains("smoke"))
        .min()
        .map(str::to_owned);
    match looped {
        Some(name) => {
            let mut fx = Effects::new(&lib.content, data.world.clone());
            let mut e = net::entity::EntityState::new(100);
            e.etype = net::entity::etype::LOOP_FX;
            e.origin = [eye.x, eye.y, eye.z + 200.0];
            e.angles = [270.0, 0.0, 0.0];
            e.model = 1;
            e.pm_flags = 1000;
            let mut names = crate::events::Events::default();
            names.take_commands(&mut vec![format!("fx 1 {name}")]);
            let names = &names;
            let (mut live_max, mut played) = (0, 0);
            for step in 1..=60 {
                fx.world_fx(std::slice::from_ref(&e), names, eye, step * 50);
                fx.update(step * 50, world);
                live_max = live_max.max(fx.live_elems());
                played = played.max(fx.looped_fx());
            }
            // Out of range of its cull distance the effect is stopped; it starts again when back in range.
            e.velocity[0] = 100.0;
            fx.world_fx(std::slice::from_ref(&e), names, eye, 3000);
            let culled = fx.looped_fx();
            e.velocity[0] = 1000.0;
            fx.world_fx(std::slice::from_ref(&e), names, eye, 3050);
            let back = fx.looped_fx();
            if culled != 0 || back != 1 {
                bad.push(format!(
                    "looped effect cull: {culled} playing out of range, {back} back in range"
                ));
            }
            // A client joining long after a trigger still plays the effect, from the trigger's time.
            let mut once = net::entity::EntityState::new(101);
            once.etype = net::entity::etype::FX;
            once.origin = e.origin;
            once.model = 1;
            once.event_seq = 1;
            once.eflags = 1000;
            fx.world_fx(&[once], names, eye, 60_000);
            if fx.played.get("triggered_fx") != Some(&1) {
                bad.push("a triggered effect seen long after its trigger was not played".into());
            }
            fx.world_fx(&[], names, eye, 3050);
            report.insert("looped_fx_active_max".into(), played.into());
            report.insert("looped_fx_live_elems_max".into(), live_max.into());
            if played == 0 || live_max == 0 {
                bad.push(format!("looped effect {name} never spawned anything"));
            }
            if fx.looped_fx() != 0 {
                bad.push("a looped effect kept playing after its entity was gone".into());
            }
        }
        None => bad.push("the content has no looping fire or smoke effect".into()),
    }
    let mut shakes = sim::shake::CameraShakes::default();
    let src = [eye.x, eye.y, eye.z];
    shakes.start(0, src, 0.5, 2000, src, 1000.0);
    let near = shakes.sway(100, src).iter().any(|a| a.abs() > 0.1);
    let far = shakes.sway(100, [src[0] + 5000.0, src[1], src[2]]);
    report.insert("camera_shake_near".into(), u8::from(near).into());
    if !near || far != [0.0; 3] {
        bad.push(format!("camera shake: near {near}, far {far:?}"));
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
