// SPDX-License-Identifier: GPL-3.0-only
//! Bots asked for at the first frame of a map spawn without script errors: the spawn logic scores a spawn point
//! only after its first sweep of the level's points has reached it, and the stock scripts add test clients well
//! after that. Covers KOTH (which places by `getSpawnpoint_NearTeam`) on every stock map, each loaded by itself and
//! all of them one after the other in one server (the bots rejoin each map at its start). Skips without
//! `COD4_PATH`.

use server::server::Server;
use std::path::PathBuf;

const MAPS: &[&str] = &[
    "mp_backlot",
    "mp_bloc",
    "mp_bog",
    "mp_broadcast",
    "mp_carentan",
    "mp_cargoship",
    "mp_citystreets",
    "mp_convoy",
    "mp_countdown",
    "mp_crash",
    "mp_crash_snow",
    "mp_creek",
    "mp_crossfire",
    "mp_farm",
    "mp_killhouse",
    "mp_overgrown",
    "mp_pipeline",
    "mp_shipment",
    "mp_showdown",
    "mp_strike",
    "mp_vacant",
];

const BOTS: u64 = 10;

fn boot() -> Option<Server> {
    let root = PathBuf::from(std::env::var_os("COD4_PATH")?);
    let args: Vec<String> = ["+set", "net_port", "0"].map(String::from).to_vec();
    Some(Server::boot(&root, &args, false).expect("boot"))
}

fn configure(s: &mut Server, gametype: &str) {
    for line in [
        format!("set g_gametype {gametype}"),
        format!("set scr_{gametype}_timelimit 0"),
        format!("set scr_{gametype}_scorelimit 0"),
    ] {
        s.exec_line(&line).expect(&line);
    }
}

/// Runs the level for ten seconds of server time and checks every bot spawned and nothing failed.
fn expect_spawned(s: &mut Server, map: &str, spawned_before: u64) {
    s.run_frames(10 * 30);
    assert!(
        s.all_script_errors.is_empty(),
        "{map}: {:#?}",
        s.all_script_errors
    );
    let spawned = s.game.stats.spawns - spawned_before;
    assert!(spawned >= BOTS, "{map}: {spawned} spawns of {BOTS} bots");
}

#[test]
fn bots_joining_at_the_first_frame_of_koth_spawn_on_every_stock_map() {
    let Some(mut s) = boot() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    configure(&mut s, "koth");
    for map in MAPS {
        let before = s.game.stats.spawns;
        s.exec_line(&format!("map {map}")).expect(map);
        s.exec_line(&format!("bots {BOTS}")).expect("bots");
        expect_spawned(&mut s, map, before);
        s.exec_line("bots 0").expect("bots 0");
    }
}

#[test]
fn maps_run_in_sequence_in_one_server_with_their_bots_raise_no_script_errors() {
    let Some(mut s) = boot() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    for gametype in ["koth", "war"] {
        configure(&mut s, gametype);
        s.exec_line(&format!("set bot_count {BOTS}")).unwrap();
        for map in MAPS {
            let before = s.game.stats.spawns;
            s.exec_line(&format!("map {map}")).expect(map);
            expect_spawned(&mut s, &format!("{gametype} {map}"), before);
        }
    }
}

#[test]
fn a_restart_of_a_koth_level_with_its_bots_raises_no_script_errors() {
    let Some(mut s) = boot() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    configure(&mut s, "koth");
    // The largest spawn set, where the first sweep of the points takes longest.
    s.exec_line("map mp_bloc").unwrap();
    s.exec_line(&format!("bots {BOTS}")).unwrap();
    s.run_frames(10 * 30);
    let before = s.game.stats.spawns;
    s.exec_line("map_restart").unwrap();
    expect_spawned(&mut s, "mp_bloc restarted", before);
}
