// SPDX-License-Identifier: GPL-3.0-or-later
//! Generates a navigation mesh for every stock map and prints size and time.
//! Usage: `COD4_PATH=<install> cargo run --release -p server --example nav-all-maps [map...]`

use std::path::PathBuf;

use server::content::{Content, Install};
use server::nav::{NavMesh, spawn_points};
use sim::world::World;

fn maps(install: &Install) -> Vec<String> {
    let mut v: Vec<String> = ["zone"]
        .into_iter()
        .chain([install.language])
        .try_fold(install.root.clone(), |dir, part| {
            std::fs::read_dir(&dir).ok()?.flatten().find_map(|e| {
                e.file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(part)
                    .then(|| e.path())
            })
        })
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .into_iter()
        .flatten()
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

fn main() {
    let Some(root) = std::env::var_os("COD4_PATH").map(PathBuf::from) else {
        eprintln!("COD4_PATH not set");
        std::process::exit(1);
    };
    let install = Install::open(&root).expect("open install");
    let wanted: Vec<String> = std::env::args().skip(1).collect();
    println!(
        "{:<18} {:>7} {:>8} {:>5} {:>8} {:>9} {:>9} {:>6} {:>6}",
        "map", "nodes", "edges", "sccs", "ms", "traces", "bytes", "seeds", "main%"
    );
    for map in maps(&install) {
        if !wanted.is_empty() && !wanted.contains(&map) {
            continue;
        }
        let mut content = Content::default();
        content.load_map(&install, &map).expect("load map");
        let clip = content.clipmap().expect("clipmap").clone();
        let seeds: Vec<_> = clip
            .map_ents
            .as_ref()
            .map(|m| spawn_points(&m.entity_string))
            .unwrap_or_default()
            .iter()
            .map(|s| s.origin)
            .collect();
        let world = World::new(clip);
        let mesh = NavMesh::generate(&world, &seeds);
        let s = mesh.stats();
        println!(
            "{map:<18} {:>7} {:>8} {:>5} {:>8.1} {:>9} {:>9} {:>3}/{:<3} {:>5.1}",
            s.nodes,
            s.edges,
            s.components,
            s.generation_ms,
            s.traces,
            s.bytes,
            s.seeds_used,
            seeds.len(),
            100.0 * s.main_component_nodes as f32 / s.nodes.max(1) as f32,
        );
    }
}
