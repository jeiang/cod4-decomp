// SPDX-License-Identifier: GPL-3.0-only
//! `fs_game`: a synthetic mod built in a scratch install (symlinks to the real `main` and `zone`) is mounted, its
//! scripts override the stock ones and run. Skips without `COD4_PATH` or off unix.
#![cfg(unix)]

use server::server::Server;
use std::path::{Path, PathBuf};

/// A scratch install: the real content linked in, a mods folder of its own.
fn scratch(root: &Path) -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    for d in ["main", "zone", "main_shared", "localization.txt"] {
        if root.join(d).exists() {
            std::os::unix::fs::symlink(root.join(d), t.path().join(d)).unwrap();
        }
    }
    t
}

fn boot(root: &Path, args: &[&str]) -> Result<Server, String> {
    let args: Vec<String> = ["+set", "net_port", "0"]
        .iter()
        .chain(args)
        .map(|s| s.to_string())
        .collect();
    Server::boot(root, &args, false)
}

fn install() -> Option<PathBuf> {
    let root = PathBuf::from(std::env::var_os("COD4_PATH")?);
    root.join("main").is_dir().then_some(root)
}

#[test]
fn a_mods_scripts_override_and_add_to_the_stock_ones() {
    let Some(root) = install() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let stock = boot(&root, &[]).expect("stock boot");
    let dm = String::from_utf8(
        stock
            .game
            .content
            .rawfile("maps/mp/gametypes/dm.gsc")
            .expect("stock dm.gsc")
            .to_vec(),
    )
    .unwrap();
    drop(stock);

    let t = scratch(&root);
    let scripts = t.path().join("mods/testmod/maps/mp/gametypes");
    std::fs::create_dir_all(&scripts).unwrap();
    // A new gametype that is only in the mod.
    std::fs::write(scripts.join("zzmod.gsc"), &dm).unwrap();
    let mut s = boot(
        t.path(),
        &[
            "+set",
            "fs_game",
            "mods/testmod",
            "+set",
            "g_gametype",
            "zzmod",
            "+map",
            "mp_crash",
        ],
    )
    .expect("modded boot");
    assert_eq!(s.map_name(), Some("mp_crash"));
    s.run_frames(30);
    assert!(s.script_errors.is_empty(), "{:#?}", s.script_errors);

    // A stock script replaced by the mod's, which does not compile: the mod's file is the one read.
    std::fs::write(scripts.join("dm.gsc"), "main( { this is not script").unwrap();
    let mut s = boot(t.path(), &["+set", "fs_game", "mods/testmod"]).expect("boot");
    let err = s
        .exec_line("map mp_crash")
        .expect_err("the broken script is compiled");
    assert!(err.contains("gametypes\\dm"), "{err}");

    // Without the mod the same install runs the stock script.
    let mut s = boot(t.path(), &[]).expect("stock boot");
    s.exec_line("map mp_crash").expect("stock map");
}
