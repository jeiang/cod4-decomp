// SPDX-License-Identifier: GPL-3.0-or-later
//! Boots stock maps with TDM and idles them. Skips without `COD4_PATH`.

use server::server::Server;
use std::path::PathBuf;

fn boot(args: &[&str]) -> Option<Server> {
    let root = PathBuf::from(std::env::var_os("COD4_PATH")?);
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    Some(Server::boot(&root, &args, false).expect("boot"))
}

#[test]
fn tdm_on_crash_idles_without_script_errors() {
    let Some(mut s) = boot(&[
        "+set",
        "net_port",
        "0",
        "+set",
        "g_gametype",
        "war",
        "+map",
        "mp_crash",
    ]) else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    assert_eq!(s.map_name(), Some("mp_crash"));
    s.run_frames(30 * 30);
    assert!(s.script_errors.is_empty(), "{:#?}", s.script_errors);
    assert!(s.thread_count() > 10, "gametype threads should be parked");
    // 3 settle frames of 100 ms, then 900 frames of 33 ms.
    assert_eq!(s.level_time(), 300 + 900 * 33);
}

#[test]
fn config_exec_order_and_map_rotation() {
    let Some(mut s) = boot(&["+set", "net_port", "0"]) else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    // The stock server_map.cfg sets a rotation at boot; a later set replaces it.
    s.exec_line("set sv_mapRotation \"gametype war map mp_crash gametype dm map mp_backlot\"")
        .unwrap();
    s.exec_line("map_rotate").unwrap();
    assert_eq!(s.map_name(), Some("mp_crash"));
    assert_eq!(s.game.cvars.string("g_gametype"), "war");
    s.exec_line("map_rotate").unwrap();
    assert_eq!(s.map_name(), Some("mp_backlot"));
    assert_eq!(s.game.cvars.string("g_gametype"), "dm");
    s.run_frames(60);
    assert!(s.script_errors.is_empty(), "{:#?}", s.script_errors);
}
