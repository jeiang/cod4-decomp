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
const FRAG: &str = "frag_grenade_mp";
const EXPLOSION: &str = "explosions/grenadeexp_dirt_1";
const VISION: &str = "mp_crash";
/// A stock map whose puddles use the water simulation.
const WATER_MAP: &str = "mp_farm";
/// A stock map with destructible props.
const PROPS_MAP: &str = "mp_bog";
const SHOCK: &str = "concussion_grenade_mp";
/// A stock smoke grenade: sprites and a runner, two of the sprites block sight.
const SMOKE: &str = "smoke/smoke_grenade_11sec_mp";
/// A stock explosion with a particle cloud among its elements.
const CLOUDY: &str = "explosions/artilleryexp_dirt_brown";

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
    let any = any_mark();
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
                let z = hi[2] + (lo[2] - hi[2]) * t.fraction;
                // Some floors refuse marks; shoot at one that takes them.
                let p = glam::Vec3::new(x, y, z);
                let takes = crate::decal::world_triangles(&data.world, p - 2.0, p + 2.0, &any)
                    .next()
                    .is_some();
                if takes {
                    floor = Some(([x, y, z], t.normal));
                    break 'find;
                }
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
                exit: false,
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
            last_shot: false,
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

    // Stock effects of each kind of element draw what they are made of.
    let surfaces: Vec<u8> = decals_by_surface
        .iter()
        .enumerate()
        .filter(|(_, d)| **d > 0)
        .map(|(i, _)| i as u8)
        .collect();
    elements(
        &lib,
        &data,
        world,
        eye,
        &weapon,
        &surfaces,
        &mut report,
        &mut bad,
    );

    // A burst of tracer rounds draws a beam for each while it flies, from the gun to where it struck, and none once
    // they have landed; the beams are drawn with the stock tracer material.
    let mut ui = crate::ui::assets::UiAssets::default();
    let tracer_material = match server::content::Install::open(install)
        .map_err(|e| e.to_string())
        .and_then(|i| ui.load_zone(&i, "localized_common_mp"))
    {
        Ok(()) => ui.material("gfx_tracer").cloned(),
        Err(e) => {
            bad.push(format!("cannot read the tracer material: {e}"));
            None
        }
    };
    if tracer_material.is_none() {
        bad.push("the stock tracer material gfx_tracer is not in the install".into());
    }
    let mut fx = Effects::new(&lib.content, data.world.clone());
    fx.set_tracer_material(tracer_material);
    fx.set_tracer_cvars(crate::tracer::Cvars {
        chance: 1.0,
        own_chance: 1.0,
        ..crate::tracer::Cvars::default()
    });
    // Burst fire of the stock rifle by another player, aimed where there are a thousand units of open air.
    let open = (0..16).map(|i| [-10.0, i as f32 * 22.5, 0.0]).find(|a| {
        let (f, _, _) = sim::pm::math::angle_vectors(a);
        let far = eye + glam::Vec3::from(f) * 1000.0;
        world
            .trace(
                eye.to_array(),
                far.to_array(),
                [0.0; 3],
                [0.0; 3],
                sim::cm::ENTITYNUM_NONE,
                sim::contents::SOLID,
            )
            .fraction
            >= 1.0
    });
    let Some(aim) = open else {
        return Err(vec!["no open air found to fire tracers into".into()]);
    };
    fx.update(1000, world);
    for _ in 0..5 {
        fx.event(
            &ClientEvent::WeaponFire {
                eye: eye.to_array(),
                angles: aim,
                weapon: 0,
                shooter: 7,
                vehicle: false,
                last_shot: false,
            },
            &weapon,
        );
    }
    fx.update(1020, world);
    // Seen from the side: a beam coming straight at the eye is edge on.
    let flying = fx.draw(eye + glam::Vec3::Z * 50.0, 0.0, 0.0, 0.0);
    let beam_meshes = flying
        .meshes
        .iter()
        .filter(|m| m.material.name.as_deref() == Some("gfx_tracer"))
        .count();
    fx.update(1000 + 8192 * 1000 / 7500 + 100, world);
    let landed = fx.draw(eye, 0.0, 0.0, 0.0).tracers;
    report.insert("tracers_in_flight".into(), flying.tracers.into());
    report.insert("tracer_meshes".into(), beam_meshes.into());
    if flying.tracers != 5 || beam_meshes != 1 {
        bad.push(format!(
            "five tracer rounds should draw five beams in one mesh, drew {} in {beam_meshes}",
            flying.tracers
        ));
    }
    if landed != 0 {
        bad.push(format!(
            "{landed} tracers were still flying after they had landed"
        ));
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

    // A frag is a model in flight, and its blast plays the surface's effect (and the weapon's own when it has one).
    match lib.content.weapon(FRAG).cloned() {
        Some(frag) => {
            report.insert(
                "frag_projectile_model".into(),
                frag.projectile_model.is_some().into(),
            );
            if frag.projectile_model.is_none() {
                bad.push(format!("weapon {FRAG} has no projectile model"));
            }
            let fw = |_: u16| Some(frag.clone());
            let mut fx = Effects::new(&lib.content, data.world.clone());
            fx.event(
                &ClientEvent::Explosion {
                    origin: at,
                    normal,
                    surface: 0,
                    weapon: 0,
                    owner: 1023,
                },
                &fw,
            );
            fx.event(
                &ClientEvent::Dud {
                    settled: false,
                    origin: at,
                    normal,
                    surface: 0,
                    weapon: 0,
                    owner: 1023,
                },
                &fw,
            );
            let played = |k: &str| fx.played.get(k).copied().unwrap_or(0);
            report.insert(
                "frag_blast_impact_fx".into(),
                played("explosion_impact").into(),
            );
            report.insert("frag_blast_weapon_fx".into(), played("explosion").into());
            report.insert("frag_dud_fx".into(), played("dud").into());
            if played("explosion_impact") == 0 {
                bad.push("a frag's blast played no effect for the surface".into());
            }
        }
        None => bad.push(format!("weapon {FRAG} is not in the content")),
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
    destructible(install, &mut report, &mut bad);

    if bad.is_empty() {
        Ok(Value::Object(report))
    } else {
        Err(bad)
    }
}

/// A stock map's destructible prop breaks under rifle fire and plays its destroy effect.
fn destructible(
    install: &Path,
    report: &mut serde_json::Map<String, Value>,
    bad: &mut Vec<String>,
) {
    use crate::props::Props;
    use glam::{Quat, Vec3};
    let lib = match Library::load(install, PROPS_MAP) {
        Ok(l) => l,
        Err(e) => return bad.push(format!("{PROPS_MAP}: {e}")),
    };
    let (Some(clipmap), Some(gun)) = (lib.content.clipmap(), lib.content.weapon(GUN).cloned())
    else {
        return bad.push(format!("{PROPS_MAP}: no collision data or {GUN}"));
    };
    let Some(def) = clipmap
        .dyn_entities
        .iter()
        .flat_map(|l| l.iter())
        .find(|d| d.kind == 2 && d.health > 0 && d.destroy_fx.is_some() && d.model.is_some())
    else {
        return bad.push(format!("{PROPS_MAP} has no destructible prop"));
    };
    let model = def.model.as_ref().expect("filtered");
    let q = Quat::from_array(def.pose.quat).normalize();
    let centre = (Vec3::from(model.mins) + Vec3::from(model.maxs)) / 2.0;
    let target = Vec3::from(def.pose.origin) + q * centre;
    let shots = (def.health / gun.damage.max(1) + 1) as usize;
    // Another prop may stand in the way from one side: shoot from each of five sides until one clear one breaks it.
    let mut happened = crate::props::Happened::default();
    for (dir, angles) in [
        (Vec3::X, [0.0, 0.0, 0.0]),
        (Vec3::NEG_X, [0.0, 180.0, 0.0]),
        (Vec3::Y, [0.0, 90.0, 0.0]),
        (Vec3::NEG_Y, [0.0, 270.0, 0.0]),
        (Vec3::Z, [89.0, 0.0, 0.0]),
    ] {
        let mut props = Props::new(&clipmap.dyn_entities);
        for _ in 0..shots {
            props.event(
                &ClientEvent::WeaponFire {
                    eye: (target - dir * 60.0).to_array(),
                    angles,
                    weapon: 1,
                    shooter: 0,
                    vehicle: false,
                    last_shot: false,
                },
                &|_| Some(gun.clone()),
                &Open,
            );
        }
        happened = props.take();
        if !happened.fx.is_empty() {
            break;
        }
    }
    report.insert("props_destroy_fx".into(), json!(happened.fx.len()));
    report.insert("props_impacts".into(), json!(happened.impacts.len()));
    if happened.fx.is_empty() || happened.impacts.is_empty() {
        bad.push(format!(
            "shooting a {PROPS_MAP} destructible ({} health, {} damage) did not break it: {} impacts, {} destroy effects",
            def.health,
            gun.damage,
            happened.impacts.len(),
            happened.fx.len()
        ));
    }
}

/// A world with nothing in it.
struct Open;

impl Collide for Open {
    fn trace(
        &self,
        _: [f32; 3],
        _: [f32; 3],
        _: [f32; 3],
        _: [f32; 3],
        _: u16,
        _: i32,
    ) -> sim::cm::Trace {
        sim::cm::Trace::MISS
    }

    fn point_contents(&self, _: [f32; 3], _: u16, _: i32) -> i32 {
        0
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

/// How many elements of each type `name` is made of, by the names of the types.
fn element_types(lib: &Library, name: &str) -> Option<serde_json::Map<String, Value>> {
    const NAMES: [&str; 11] = [
        "sprite_billboard",
        "sprite_oriented",
        "tail",
        "trail",
        "cloud",
        "model",
        "omni_light",
        "spot_light",
        "sound",
        "decal",
        "runner",
    ];
    let def = lib.content.effects().into_iter().find(|e| {
        e.name
            .as_deref()
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
    })?;
    let mut counts = serde_json::Map::new();
    for d in def.elems.iter() {
        let n = counts
            .entry(NAMES[usize::from(d.elem_type).min(NAMES.len() - 1)])
            .or_insert(0.into());
        *n = (n.as_u64().unwrap_or(0) + 1).into();
    }
    Some(counts)
}

/// A material that takes a mark of any kind.
fn any_mark() -> assets::zone::gfx::Material {
    assets::zone::gfx::Material {
        name: None,
        game_flags: 0,
        sort_key: 0,
        atlas_rows: 1,
        atlas_columns: 1,
        draw_surf: 0,
        surface_type_bits: 0,
        hash_index: 0,
        state_bits_entry: [0; 34],
        state_flags: 0,
        camera_region: 0,
        technique_set: None,
        textures: std::sync::Arc::from(Vec::new()),
        constants: std::sync::Arc::from(Vec::new()),
        state_bits: std::sync::Arc::from(Vec::new()),
    }
}

/// What the effects of each kind of element draw: a stock smoke grenade's sprites and the smoke that blocks the line
/// of sight, a stock explosion's particle clouds, and the bullet marks on a static model that no map surface is near.
#[allow(clippy::too_many_arguments)]
fn elements(
    lib: &Library,
    data: &render::MapData,
    world: &dyn Collide,
    eye: glam::Vec3,
    weapon: &dyn Fn(u16) -> Option<std::sync::Arc<assets::zone::weapon::WeaponDef>>,
    surfaces: &[u8],
    report: &mut serde_json::Map<String, Value>,
    bad: &mut Vec<String>,
) {
    // The smoke grenade: six sprites and the runner, two of the sprites blocking sight.
    let Some(types) = element_types(lib, SMOKE) else {
        return bad.push(format!("effect {SMOKE} is not in the content"));
    };
    report.insert("smoke_elements".into(), Value::Object(types.clone()));
    let count = |k: &str| types.get(k).and_then(Value::as_u64).unwrap_or(0);
    if count("sprite_billboard") < 2 || count("runner") != 1 {
        bad.push(format!("{SMOKE} is not a runner and sprites: {types:?}"));
    }
    let mut fx = Effects::new(&lib.content, data.world.clone());
    fx.demo(SMOKE, eye, 0.0, 0.0);
    let along = glam::Vec3::X * 400.0;
    let (mut quads, mut seen) = (0, 1.0f32);
    for step in 1..=400 {
        fx.update(step * 50, world);
        quads = quads.max(fx.draw(eye, 0.0, 0.0, 0.0).quads);
        seen = seen.min(fx.visibility(eye, eye + along));
    }
    report.insert("smoke_quads_max".into(), quads.into());
    report.insert("smoke_visibility_min".into(), seen.into());
    if quads == 0 {
        bad.push("the smoke grenade drew no sprite".into());
    }
    if seen > 0.5 {
        bad.push(format!(
            "the line of sight through a smoke grenade stayed {seen} clear"
        ));
    }

    // An explosion with particle clouds draws them next to its sprites.
    let cloudy = element_types(lib, CLOUDY).unwrap_or_default();
    report.insert(
        "cloud_effect_elements".into(),
        Value::Object(cloudy.clone()),
    );
    if cloudy.get("cloud").and_then(Value::as_u64).unwrap_or(0) == 0 {
        bad.push(format!("effect {CLOUDY} has no cloud element: {cloudy:?}"));
    }
    let mut fx = Effects::new(&lib.content, data.world.clone());
    fx.demo(CLOUDY, eye, 0.0, 0.0);
    let (mut clouds, mut quads, mut cloud_meshes) = (0, 0, 0);
    for step in 1..=400 {
        fx.update(step * 50, world);
        let d = fx.draw(eye, 0.0, 0.0, 0.0);
        clouds = clouds.max(d.clouds);
        quads = quads.max(d.quads);
        cloud_meshes = cloud_meshes.max(d.meshes.iter().filter(|m| m.cloud.is_some()).count());
    }
    report.insert("cloud_effect_clouds_max".into(), clouds.into());
    report.insert("cloud_effect_quads_max".into(), quads.into());
    if clouds == 0 || clouds != cloud_meshes {
        bad.push(format!(
            "{CLOUDY} drew {clouds} clouds as {cloud_meshes} cloud meshes"
        ));
    }

    // A bullet into the top of a static model that has no map surface beside it leaves a mark on the model.
    let w = &data.world;
    let any = any_mark();
    let (mut candidates, mut marked) = (0, None);
    'models: for (inst, draw) in w
        .dpvs
        .smodel_insts
        .iter()
        .zip(&w.dpvs.smodel_draw_insts)
        .step_by(5)
    {
        if draw.model.is_none() {
            continue;
        }
        let (mins, maxs) = (glam::Vec3::from(inst.mins), glam::Vec3::from(inst.maxs));
        for t in crate::decal::model_triangles(w, mins, maxs, &any) {
            let cross = (t[1] - t[0]).cross(t[2] - t[0]);
            let n = cross.normalize_or_zero();
            if n.z < 0.95 || cross.length() < 40.0 {
                continue;
            }
            let c = (t[0] + t[1] + t[2]) / 3.0;
            let p = crate::decal::Placement {
                origin: c,
                normal: n,
                up: n.any_orthonormal_vector(),
                half_size: [4.0, 4.0],
            };
            let reach = glam::Vec3::splat(24.0);
            if !crate::decal::clip(
                crate::decal::world_triangles(w, c - reach, c + reach, &any),
                &p,
            )
            .is_empty()
            {
                continue;
            }
            candidates += 1;
            for &surface in surfaces {
                let mut fx = Effects::new(&lib.content, data.world.clone());
                fx.event(
                    &ClientEvent::BulletImpact {
                        origin: c.to_array(),
                        normal: n.to_array(),
                        surface,
                        weapon: 0,
                        shooter: 0,
                        exit: false,
                    },
                    weapon,
                );
                let (_, decals, _, _) =
                    run_for(&mut fx, world, 0, 1, c + glam::Vec3::Z * 60.0, -1.2);
                if decals > 0 {
                    marked = Some(surface);
                    break 'models;
                }
            }
            if candidates >= 30 {
                break 'models;
            }
        }
    }
    report.insert("static_model_mark_candidates".into(), candidates.into());
    report.insert(
        "static_model_mark_surface".into(),
        marked.map_or(Value::Null, |s| s.into()),
    );
    if marked.is_none() {
        bad.push(format!(
            "no bullet mark landed on a static model ({candidates} places tried); the map has none to try if 0"
        ));
    }
}
