// SPDX-License-Identifier: GPL-3.0-only
//! `fs_game`: a synthetic mod built in a scratch install (symlinks to the real `main` and `zone`) is mounted, its
//! scripts override the stock ones and run. Skips without `COD4_PATH` or off unix.
#![cfg(unix)]

use net::connect::{ConnectState, Connector};
use net::{Oob, Transport, UdpTransport};
use server::server::Server;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

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

/// The IWD of the synthetic mod of [`a_pure_server_checks_the_iwds_and_gives_its_mod_files`].
fn mod_iwd() -> Vec<u8> {
    iwd(&[("zz_mod.cfg", b"set zz_mod_loaded 1\n")])
}

/// Connects a client that claims the IWD digest `pure` to the server at `addr`, running `s` meanwhile.
fn try_join(s: &mut Server, addr: std::net::SocketAddr, pure: u32, qport: u16) -> ConnectState {
    let lo = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
    let mut t = UdpTransport::bind(lo).unwrap();
    let mut c = Connector::new(addr, qport, "tester", "");
    c.set_pure(pure);
    let mut buf = [0u8; 2048];
    for round in 0..200u64 {
        c.poll(round * 50, &mut t);
        s.run_for(Duration::from_millis(20));
        while let Ok(Some((n, from))) = t.recv_from(&mut buf, None) {
            if let Some(o) = Oob::parse(&buf[..n]) {
                c.handle(from, o);
            }
        }
        if !matches!(
            c.state(),
            ConnectState::Challenging | ConnectState::Connecting(_)
        ) {
            break;
        }
    }
    c.state().clone()
}

/// Runs `client` on its own thread while `s` serves it (the server is not `Send`).
fn serve_while<T: Send + 'static>(
    s: &mut Server,
    client: impl FnOnce() -> T + Send + 'static,
) -> T {
    let h = std::thread::spawn(client);
    while !h.is_finished() {
        s.run_for(Duration::from_millis(10));
    }
    h.join().unwrap()
}

#[test]
fn a_pure_server_checks_the_iwds_and_gives_its_mod_files() {
    let Some(root) = install() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let t = scratch(&root);
    let mod_dir = t.path().join("mods/testmod");
    std::fs::create_dir_all(&mod_dir).unwrap();
    std::fs::write(mod_dir.join("zz_mod.iwd"), mod_iwd()).unwrap();
    std::fs::write(mod_dir.join("notes.txt"), b"not offered").unwrap();
    let mut s = boot(
        t.path(),
        &[
            "+set",
            "fs_game",
            "mods/testmod",
            "+set",
            "sv_pure",
            "1",
            "+set",
            "sv_allowDownload",
            "1",
            "+map",
            "mp_crash",
        ],
    )
    .expect("boot");
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], s.net_addr().unwrap().port()));

    // The client's own digest of the same search path is the server's.
    let vfs = assets::vfs::Vfs::open(t.path(), 0, "mods/testmod", None).unwrap();
    let digest = assets::vfs::pure_digest(&vfs.pure_iwds(t.path()));
    assert!(matches!(
        try_join(&mut s, addr, digest, 1),
        ConnectState::Connected
    ));
    for (pure, qport) in [(0, 2), (digest ^ 1, 3)] {
        match try_join(&mut s, addr, pure, qport) {
            ConnectState::Refused(why) => assert_eq!(why, server::server::IMPURE),
            other => panic!("an impure client got {other:?}"),
        }
    }
    // Without the mod's IWD (a client on the stock search path) the digest is another one.
    let stock = assets::vfs::Vfs::open(t.path(), 0, "", None).unwrap();
    let stock = assets::vfs::pure_digest(&stock.pure_iwds(t.path()));
    assert!(matches!(
        try_join(&mut s, addr, stock, 4),
        ConnectState::Refused(_)
    ));
    // A changed file inside the mod's IWD, same name and same size, is another digest.
    let same_size = iwd(&[("zz_mod.cfg", b"set zz_mod_loaded 2\n")]);
    assert_eq!(same_size.len(), mod_iwd().len());
    std::fs::write(mod_dir.join("zz_mod.iwd"), same_size).unwrap();
    let edited = assets::vfs::Vfs::open(t.path(), 0, "mods/testmod", None).unwrap();
    let edited = assets::vfs::pure_digest(&edited.pure_iwds(t.path()));
    std::fs::write(mod_dir.join("zz_mod.iwd"), mod_iwd()).unwrap();
    assert_ne!(edited, digest);
    assert!(matches!(
        try_join(&mut s, addr, edited, 6),
        ConnectState::Refused(_)
    ));
    // With `sv_pure 0` any digest passes.
    s.exec_line("set sv_pure 0").unwrap();
    assert!(matches!(
        try_join(&mut s, addr, 0, 5),
        ConnectState::Connected
    ));
    s.exec_line("set sv_pure 1").unwrap();

    // The manifest lists the mod's IWD with its size and CRC, and the file comes whole by that name only.
    let (list, www) = serve_while(&mut s, move || {
        let mut c = UdpTransport::bind(std::net::SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        net::manifest::fetch_list(&mut c, addr, &AtomicBool::new(false)).unwrap()
    });
    assert_eq!(www, "");
    let names: Vec<&str> = list.iter().map(|f| f.name.as_str()).collect();
    // The running map's zones are given as a custom map's, wherever the server has them.
    assert_eq!(
        names,
        [
            "usermaps/mp_crash/mp_crash.ff",
            "usermaps/mp_crash/mp_crash_load.ff",
            "mods/testmod/zz_mod.iwd",
        ]
    );
    let bytes = mod_iwd();
    assert_eq!(
        (list[2].size, list[2].crc),
        (bytes.len() as u64, assets::crc::crc32(&bytes))
    );
    let dest = t.path().join("got.iwd");
    let to = dest.clone();
    serve_while(&mut s, move || {
        let mut c = UdpTransport::bind(std::net::SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        net::download::fetch(
            &mut c,
            addr,
            "mods/testmod/zz_mod.iwd",
            &to,
            &net::download::Progress::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    });
    assert_eq!(std::fs::read(&dest).unwrap(), bytes);
    for name in [
        "main/iw_00.iwd",
        "mods/testmod/notes.txt",
        "../mods/testmod/zz_mod.iwd",
        "mp_crash",
    ] {
        let err = serve_while(&mut s, move || {
            let mut c =
                UdpTransport::bind(std::net::SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
            net::download::fetch(
                &mut c,
                addr,
                name,
                &std::env::temp_dir().join("cod4e-never-written"),
                &net::download::Progress::default(),
                &AtomicBool::new(false),
            )
            .unwrap_err()
        });
        assert_eq!(err, net::download::NOT_AVAILABLE, "{name}");
    }

    // The fast-download base is published once the server turns it on, and only when it is a plain http(s) URL.
    s.exec_line("set sv_wwwDownload 1").unwrap();
    s.exec_line(r#"set sv_wwwBaseURL "http://dl.example/fast""#)
        .unwrap();
    let fetch_www = |s: &mut Server| {
        serve_while(s, move || {
            let mut c =
                UdpTransport::bind(std::net::SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
            net::manifest::fetch_list(&mut c, addr, &AtomicBool::new(false))
                .unwrap()
                .1
        })
    };
    assert_eq!(fetch_www(&mut s), "http://dl.example/fast");
    s.exec_line(r#"set sv_wwwBaseURL "file:///etc""#).unwrap();
    assert_eq!(fetch_www(&mut s), "");

    // With downloads off the manifest and the files are refused.
    s.exec_line("set sv_allowDownload 0").unwrap();
    let err = serve_while(&mut s, move || {
        let mut c = UdpTransport::bind(std::net::SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        net::manifest::fetch_list(&mut c, addr, &AtomicBool::new(false)).unwrap_err()
    });
    assert_eq!(err, net::manifest::DOWNLOADS_OFF);
}

#[test]
fn a_pure_server_running_a_custom_map_counts_the_maps_iwd_as_a_client_loads_it() {
    let Some(root) = install() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let src = root.join("zone/english/mp_crash.ff");
    if !src.exists() {
        eprintln!("no mp_crash.ff; skipping");
        return;
    }
    let t = scratch(&root);
    let dir = t.path().join("usermaps/mp_crashcopy");
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(&src, dir.join("mp_crashcopy.ff")).unwrap();
    std::fs::write(
        dir.join("mp_crashcopy.iwd"),
        iwd(&[("usermapcfg.cfg", b"set zz_usermap_loaded 1\n")]),
    )
    .unwrap();
    let mut s = boot(t.path(), &["+set", "sv_pure", "1", "+map", "mp_crashcopy"]).expect("boot");
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], s.net_addr().unwrap().port()));
    // What a client's library works out: the install with the map's package mounted.
    let digest = |map: Option<&str>| {
        let mut install = server::content::Install::open_game(t.path(), "").unwrap();
        if let Some(m) = map {
            install.mount_usermap(m).unwrap();
        }
        assets::vfs::pure_digest(&install.vfs.pure_iwds(&install.root))
    };
    assert!(matches!(
        try_join(&mut s, addr, digest(Some("mp_crashcopy")), 1),
        ConnectState::Connected
    ));
    assert!(
        matches!(
            try_join(&mut s, addr, digest(None), 2),
            ConnectState::Refused(_)
        ),
        "a client that lacks the map's IWD is not pure"
    );
}
