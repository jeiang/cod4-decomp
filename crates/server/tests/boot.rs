// SPDX-License-Identifier: GPL-3.0-only
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

#[test]
fn map_restart_reloads_the_level() {
    let Some(mut s) = boot(&["+set", "net_port", "0", "+map", "mp_crash"]) else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    s.run_frames(60);
    let t = s.level_time();
    s.exec_line("map_restart").unwrap();
    assert_eq!(
        s.level_time(),
        300,
        "level time restarts with the settle frames"
    );
    assert!(t > 300);
    s.run_frames(60);
    assert!(s.script_errors.is_empty(), "{:#?}", s.script_errors);
}

#[test]
fn command_line_sets_win_over_the_stock_configs() {
    let Some(mut s) = boot(&[
        "+set",
        "net_port",
        "0",
        "+set",
        "g_gametype",
        "war",
        "+set",
        "scr_war_timelimit",
        "1",
        "+map",
        "mp_crash",
    ]) else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    s.run_frames(30);
    // The stock default_mp_gamesettings.cfg sets 10; the gametype's script copies the dvar into the UI's.
    assert_eq!(s.game.cvars.string("scr_war_timelimit"), "1");
    assert_eq!(s.game.cvars.string("ui_timelimit"), "1");
}

#[test]
fn a_latched_gametype_takes_effect_at_map_restart_but_not_at_fast_restart() {
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
    let loads = s.level_loads;
    s.exec_line("set g_gametype sd").unwrap();
    assert_eq!(
        s.game.cvars.string("g_gametype"),
        "war",
        "latched until a restart"
    );
    // The latched value is published only once it is the running one.
    s.exec_line("map_restart").unwrap();
    assert_eq!(s.game.cvars.string("g_gametype"), "sd");
    assert_eq!(s.level_loads, loads + 1);
    s.run_frames(30);
    assert!(s.script_errors.is_empty(), "{:#?}", s.script_errors);
    let info = s
        .game
        .configstrings
        .get(&u32::from(net::ui::cs::SERVERINFO))
        .cloned();
    assert!(
        info.as_deref()
            .is_some_and(|i| i.contains("\\g_gametype\\sd")),
        "{info:?}"
    );
    // `g_gametype x; fast_restart` has a new gametype waiting: it loads the level afresh too.
    s.exec_line("set g_gametype war").unwrap();
    s.exec_line("fast_restart").unwrap();
    assert_eq!(s.game.cvars.string("g_gametype"), "war");
    // With nothing waiting it restarts in place and keeps the gametype.
    s.exec_line("fast_restart").unwrap();
    assert_eq!(s.game.cvars.string("g_gametype"), "war");
}

#[test]
fn map_rotate_skips_unknown_words_and_wraps_to_the_start() {
    let Some(mut s) = boot(&["+set", "net_port", "0"]) else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    s.exec_line("set sv_mapRotation \"bogus gametype dm map mp_crash map mp_backlot\"")
        .unwrap();
    s.exec_line("set sv_mapRotationCurrent \"\"").unwrap();
    s.exec_line("map_rotate").unwrap();
    assert_eq!(s.map_name(), Some("mp_crash"));
    assert_eq!(s.game.cvars.string("g_gametype"), "dm");
    s.exec_line("map_rotate").unwrap();
    assert_eq!(s.map_name(), Some("mp_backlot"));
    // The rotation is used up: the next call starts over.
    s.exec_line("map_rotate").unwrap();
    assert_eq!(s.map_name(), Some("mp_crash"));
    s.run_frames(30);
    assert!(s.script_errors.is_empty(), "{:#?}", s.script_errors);
}

#[test]
fn the_game_log_records_the_game_start_chat_script_lines_and_the_end() {
    let path = std::env::temp_dir().join(format!("cod4e-boot-games-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let p = path.to_str().unwrap();
    let Some(mut s) = boot(&[
        "+set", "net_port", "0", "+set", "g_log", p, "+map", "mp_crash",
    ]) else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    s.run_frames(10);
    s.game.log_print("J;0;1;Ann\n");
    s.exec_line("map mp_backlot").unwrap();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    assert!(text.contains(" InitGame: \\"), "{text}");
    assert!(text.contains(" J;0;1;Ann\n"), "{text}");
    assert!(text.contains(" ShutdownGame:\n"), "{text}");
    assert_eq!(
        text.matches("InitGame:").count(),
        2,
        "one per level: {text}"
    );
}

#[test]
fn getinfo_and_getstatus_carry_the_server_settings() {
    let Some(s) = boot(&[
        "+set",
        "net_port",
        "0",
        "+set",
        "g_password",
        "secret",
        "+map",
        "mp_crash",
    ]) else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let get =
        |kv: &[(String, String)], k: &str| kv.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let info = s.server_info();
    assert_eq!(get(&info, "mapname").as_deref(), Some("mp_crash"));
    assert_eq!(get(&info, "pswrd").as_deref(), Some("1"));
    for key in [
        "protocol",
        "hostname",
        "gametype",
        "sv_maxclients",
        "hw",
        "mod",
        "voice",
        "pb",
    ] {
        assert!(get(&info, key).is_some(), "getinfo has no {key}: {info:?}");
    }
    assert_eq!(
        get(&info, "clients"),
        None,
        "no clients: the key is left out"
    );
    let (status, players) = s.server_status();
    assert_eq!(get(&status, "mapname").as_deref(), Some("mp_crash"));
    assert_eq!(get(&status, "pswrd").as_deref(), Some("1"));
    assert!(
        get(&status, "g_password").is_none(),
        "the password itself is never sent"
    );
    assert!(players.is_empty());
}
