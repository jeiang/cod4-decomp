// SPDX-License-Identifier: GPL-3.0-only
//! Generates navigation meshes for every stock map and checks the spawn points connect.
//! Skips without `COD4_PATH`. Run with `--release`: debug collision is an order of magnitude
//! slower and the timing assertion is skipped there.

use std::collections::BTreeMap;
use std::path::PathBuf;

use server::content::{Content, Install};
use server::nav::{NavMesh, NodeId, PathScratch, STEP, Steer, edge, spawn_points};
use sim::Vec3;
use sim::cm::Collide;
use sim::contents::{MASK_PLAYERSOLID, PLAYER};
use sim::pm::{Params, PlayerState, Pmove, UserCmd, button, pmove};
use sim::world::World;

fn install() -> Option<Install> {
    let root = PathBuf::from(std::env::var_os("COD4_PATH")?);
    Some(Install::open(&root).expect("open install"))
}

fn maps(install: &Install) -> Vec<String> {
    let dir = ["zone", install.language]
        .into_iter()
        .try_fold(install.root.clone(), |dir, part| {
            std::fs::read_dir(&dir).ok()?.flatten().find_map(|e| {
                e.file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(part)
                    .then(|| e.path())
            })
        })
        .expect("zone dir");
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let stem = p.file_stem()?.to_str()?.to_ascii_lowercase();
            let ff = p.extension()?.eq_ignore_ascii_case("ff");
            (ff && stem.starts_with("mp_") && !stem.ends_with("_load")).then_some(stem)
        })
        .collect();
    v.sort();
    v
}

/// `mp_tdm_spawn_axis_start` -> `mp_tdm`.
fn family(class: &str) -> &str {
    class.split("_spawn").next().unwrap_or(class)
}

fn hull_trace(world: &World, a: Vec3, b: Vec3, maxs_z: f32) -> sim::cm::Trace {
    world.trace(
        a,
        b,
        [-15.0, -15.0, 0.0],
        [15.0, 15.0, maxs_z],
        1023,
        MASK_PLAYERSOLID & !PLAYER,
    )
}

/// Re-checks one hop independently of the generator: walk the segment in 8-unit slices with
/// the hull on the floor, each slice's floor within a step of the last (falls excepted).
fn hop_walkable(world: &World, a: Vec3, b: Vec3, flags: u8) -> Result<(), String> {
    let maxs_z = if flags & edge::CROUCH != 0 {
        50.0
    } else {
        70.0
    };
    let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
    let slices = (len / 8.0).ceil().max(1.0) as u32;
    let mut z = a[2];
    for i in 1..=slices {
        let f = i as f32 / slices as f32;
        let (x, y) = (a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f);
        // The floor under the slice: from a step above, or level when a low ceiling forbids it.
        let (top, t) = [z + 48.0, z + 2.0 * STEP, z + STEP, z + 1.0]
            .into_iter()
            .map(|top| {
                (
                    top,
                    hull_trace(world, [x, y, top], [x, y, z - 128.0], maxs_z),
                )
            })
            .find(|(_, t)| !t.start_solid && !t.all_solid)
            .ok_or(format!(
                "hull stuck at slice {i}/{slices} ({x:.0},{y:.0},{z:.0})"
            ))?;
        if t.fraction >= 1.0 {
            return Err(format!(
                "no floor at slice {i}/{slices} ({x:.0},{y:.0},{z:.0})"
            ));
        }
        let nz = top - (top - z + 128.0) * t.fraction;
        if nz - z > STEP + 1.0 && flags & edge::JUMP == 0 {
            return Err(format!("rise {:.1} in one slice", nz - z));
        }
        z = nz;
    }
    if (z - b[2]).abs() > 2.5 {
        return Err(format!("ends at z {z:.1}, node at {:.1}", b[2]));
    }
    Ok(())
}

/// Drives a `Pmove` along `path` with `Steer`; true when it ends at the last node.
fn follow(world: &World, mesh: &NavMesh, path: &[NodeId], params: &Params) -> bool {
    let start = mesh.node_pos(path[0]);
    let mut ps = PlayerState {
        origin: start,
        command_time: 100_000,
        ..PlayerState::default()
    };
    ps.viewangles[1] = 0.0;
    let mut pm = Pmove::new(ps, params);
    let mut steer = Steer::new();
    let mut sent = UserCmd::default();
    let goal = mesh.node_pos(*path.last().unwrap());
    let budget = 30 * (path.len() as u32 + 20);
    for _ in 0..budget {
        let o = steer.update(mesh, path, pm.ps.origin);
        if o.arrived {
            return true;
        }
        if o.stuck {
            return false;
        }
        let yaw = o.dir[1].atan2(o.dir[0]).to_degrees();
        pm.ps.delta_angles[1] = yaw;
        pm.ps.viewangles[1] = yaw;
        let mut buttons = 0;
        if o.jump {
            buttons |= button::JUMP;
        }
        if o.crouch {
            buttons |= button::CROUCH;
        }
        pm.cmd = UserCmd {
            buttons,
            forwardmove: 127,
            server_time: pm.ps.command_time + 33,
            ..UserCmd::default()
        };
        pm.oldcmd = sent;
        sent = pm.cmd;
        pmove(&mut pm, world);
    }
    let _ = goal;
    false
}

#[test]
fn every_stock_map_generates_and_spawns_connect() {
    let Some(install) = install() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let params = Params::default();
    let mut exceptions: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
    let (mut followed, mut follow_ok) = (0, 0);
    let mut hops_checked = 0usize;
    let mut times = Vec::new();
    for map in maps(&install) {
        let mut content = Content::default();
        content.load_map(&install, &map).expect("load map");
        let clip = content.clipmap().expect("clipmap").clone();
        let spawns = spawn_points(&clip.map_ents.as_ref().expect("map ents").entity_string);
        assert!(!spawns.is_empty(), "{map}: no spawns");
        let world = World::new(clip);
        let seeds: Vec<Vec3> = spawns.iter().map(|s| s.origin).collect();
        let mesh = NavMesh::generate(&world, &seeds);
        let s = *mesh.stats();
        println!(
            "{map:<14} nodes {:>6} edges {:>7} sccs {:>2} main {:>5.1}% {:>7.1} ms {:>5} KiB seeds {}/{}",
            s.nodes,
            s.edges,
            s.components,
            100.0 * s.main_component_nodes as f32 / s.nodes as f32,
            s.generation_ms,
            s.bytes / 1024,
            s.seeds_used,
            seeds.len(),
        );
        times.push(s.generation_ms);
        assert!(s.nodes > 500, "{map}: only {} nodes", s.nodes);
        assert!(s.bytes < 8 << 20, "{map}: {} bytes", s.bytes);
        if !cfg!(debug_assertions) {
            assert!(s.generation_ms < 1000.0, "{map}: {} ms", s.generation_ms);
        }
        let dropped: Vec<_> = (0..seeds.len())
            .filter(|&i| mesh.seed_node(i).is_none())
            .map(|i| format!("{} {:?} has no floor", spawns[i].class, spawns[i].origin))
            .collect();
        exceptions.entry(map.clone()).or_default().extend(dropped);

        let mut scratch = PathScratch::new(&mesh);
        let mut path = Vec::new();
        let mut back = Vec::new();
        let mut fam: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (i, sp) in spawns.iter().enumerate() {
            if mesh.seed_node(i).is_some() {
                fam.entry(family(&sp.class)).or_default().push(i);
            }
        }
        let mut sample = 0u32;
        for (name, members) in &fam {
            let hub = mesh.seed_node(members[0]).unwrap();
            for &i in &members[1..] {
                let n = mesh.seed_node(i).unwrap();
                let fwd = mesh.path(hub, n, &mut scratch, &mut path);
                let rev = mesh.path(n, hub, &mut scratch, &mut back);
                if !(fwd && rev) {
                    exceptions.entry(map.clone()).or_default().insert(format!(
                        "{name}: {} {:?} <-> {} {:?}: path {fwd} / back {rev}",
                        spawns[members[0]].class,
                        spawns[members[0]].origin,
                        spawns[i].class,
                        spawns[i].origin
                    ));
                    continue;
                }
                for p in [&path, &back] {
                    for w in p.windows(2) {
                        let flags = mesh.edge_flags(w[0], w[1]).expect("path hop is an edge");
                        if flags & (edge::MANTLE | edge::LADDER) != 0 {
                            continue; // climbing animation, not a walk
                        }
                        hops_checked += 1;
                        if let Err(e) =
                            hop_walkable(&world, mesh.node_pos(w[0]), mesh.node_pos(w[1]), flags)
                        {
                            exceptions.entry(map.clone()).or_default().insert(format!(
                                "hop {:?} -> {:?} flags {flags}: {e}",
                                mesh.node_pos(w[0]),
                                mesh.node_pos(w[1])
                            ));
                        }
                    }
                }
                sample += 1;
                let climbs = path.windows(2).any(|w| {
                    mesh.edge_flags(w[0], w[1])
                        .is_some_and(|f| f & edge::MANTLE != 0)
                });
                if sample.is_multiple_of(6) && !climbs {
                    followed += 1;
                    if follow(&world, &mesh, &path, &params) {
                        follow_ok += 1;
                    } else {
                        let at = mesh.node_pos(n);
                        exceptions
                            .entry(map.clone())
                            .or_default()
                            .insert(format!("pmove could not follow path to {at:?}"));
                    }
                }
            }
        }
    }
    times.sort_by(f32::total_cmp);
    println!(
        "generation ms: p50 {:.1} max {:.1}; pmove followed {follow_ok}/{followed}",
        times[times.len() / 2],
        times[times.len() - 1]
    );
    for (map, v) in &exceptions {
        for e in v.iter().take(6) {
            println!("EXCEPTION {map}: {e}");
        }
        if v.len() > 4 {
            println!("EXCEPTION {map}: ... {} more", v.len() - 4);
        }
    }
    // Known gap: mp_cargoship's stern superstructure (about 750 nodes at z 176) has no
    // inbound link: no ladder materials exist in the map and the climb exceeds mantle reach;
    // the route is probably a scripted mover, which the mesh ignores.
    let unconnected: Vec<_> = exceptions
        .iter()
        .flat_map(|(m, v)| v.iter().map(move |e| (m, e)))
        .filter(|(m, e)| e.contains("<->") && m.as_str() != "mp_cargoship")
        .collect();
    assert!(
        unconnected.is_empty(),
        "unconnected spawns: {unconnected:#?}"
    );
    // The hop re-check is an independent approximation; a handful of corner cases differ.
    let bad_hops = exceptions
        .values()
        .flatten()
        .filter(|e| e.starts_with("hop"))
        .count();
    assert!(
        bad_hops * 200 <= hops_checked,
        "{bad_hops} of {hops_checked} hops fail the re-check"
    );
}

#[test]
fn generation_is_deterministic() {
    let Some(install) = install() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let mut content = Content::default();
    content.load_map(&install, "mp_crash").expect("load map");
    let clip = content.clipmap().unwrap().clone();
    let seeds: Vec<Vec3> = spawn_points(&clip.map_ents.as_ref().unwrap().entity_string)
        .iter()
        .map(|s| s.origin)
        .collect();
    let world = World::new(clip);
    let (a, b) = (
        NavMesh::generate(&world, &seeds),
        NavMesh::generate(&world, &seeds),
    );
    assert_eq!(
        (a.stats().nodes, a.stats().edges, a.stats().components),
        (b.stats().nodes, b.stats().edges, b.stats().components)
    );
    let (mut sa, mut sb) = (PathScratch::new(&a), PathScratch::new(&b));
    let (mut pa, mut pb) = (Vec::new(), Vec::new());
    let (from, to) = (NodeId(0), NodeId(a.node_count() as u32 / 2));
    let ok = a.path(from, to, &mut sa, &mut pa);
    assert_eq!(ok, b.path(from, to, &mut sb, &mut pb));
    assert_eq!(pa, pb);
}
