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
    assert!(err.contains("script compile errors"), "{err}");

    // Without the mod the same install runs the stock script.
    let mut s = boot(t.path(), &[]).expect("stock boot");
    s.exec_line("map mp_crash").expect("stock map");
}

/// A zip of stored entries, as an IWD.
fn iwd(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let (mut out, mut dir) = (Vec::new(), Vec::new());
    for (name, data) in entries {
        let at = out.len() as u32;
        let crc = {
            let mut c = !0u32;
            for b in *data {
                c ^= u32::from(*b);
                for _ in 0..8 {
                    c = (c >> 1) ^ (0xEDB8_8320 & (c & 1).wrapping_neg());
                }
            }
            !c
        };
        let head = |v: &mut Vec<u8>, central: bool| {
            v.extend(if central { 0x0201_4b50u32 } else { 0x0403_4b50 }.to_le_bytes());
            if central {
                v.extend([20, 0]);
            }
            v.extend([20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            v.extend(crc.to_le_bytes());
            v.extend((data.len() as u32).to_le_bytes().repeat(2));
            v.extend((name.len() as u16).to_le_bytes());
            v.extend([0; 2]);
            if central {
                v.extend([0; 10]);
                v.extend(at.to_le_bytes());
            }
            v.extend(name.as_bytes());
        };
        head(&mut out, false);
        out.extend(*data);
        head(&mut dir, true);
    }
    let at = out.len() as u32;
    out.extend(&dir);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0; 4]);
    out.extend((entries.len() as u16).to_le_bytes().repeat(2));
    out.extend((dir.len() as u32).to_le_bytes());
    out.extend(at.to_le_bytes());
    out.extend([0; 2]);
    out
}

#[test]
fn a_custom_map_in_usermaps_is_hosted_with_its_own_iwd() {
    let Some(root) = install() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let src = root.join("zone/english/mp_crash.ff");
    if !src.exists() {
        eprintln!("no mp_crash.ff; skipping");
        return;
    }
    // A custom map is a map zone of its own name, here the stock crash zone under another one, with an IWD.
    let t = scratch(&root);
    let dir = t.path().join("usermaps/mp_crashcopy");
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(&src, dir.join("mp_crashcopy.ff")).unwrap();
    std::fs::write(
        dir.join("mp_crashcopy.iwd"),
        iwd(&[("usermapcfg.cfg", b"set zz_usermap_loaded 1\n")]),
    )
    .unwrap();

    let mut s = boot(t.path(), &[]).expect("boot");
    assert!(
        s.exec_line("exec usermapcfg.cfg").is_err(),
        "the map's IWD is not mounted before its map runs"
    );
    s.exec_line("map mp_crashcopy")
        .expect("the usermaps map loads");
    assert_eq!(s.map_name(), Some("mp_crashcopy"));
    s.exec_line("exec usermapcfg.cfg")
        .expect("the map's IWD is mounted");
    s.exec_line("map mp_crash").expect("a stock map");
    assert!(
        s.exec_line("exec usermapcfg.cfg").is_err(),
        "and unmounted with the map"
    );
}

#[test]
fn a_mod_that_is_not_installed_fails_the_boot() {
    let Some(root) = install() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let t = scratch(&root);
    for game in ["mods/nonexist", "Mods/x", "mods/../main"] {
        assert!(
            boot(t.path(), &["+set", "fs_game", game]).is_err(),
            "{game}"
        );
    }
}
