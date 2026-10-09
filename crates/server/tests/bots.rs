// SPDX-License-Identifier: GPL-3.0-only
//! How many bots a server plays with: none unless asked, `bot_count` at every map start, `bots N` at once.
//! Skips without `COD4_PATH`.

use server::server::Server;
use std::path::PathBuf;

fn bots(s: &Server) -> usize {
    s.game.connected_clients().filter(|(_, c)| c.bot).count()
}

#[test]
fn bots_follow_the_cvar_at_map_start_and_the_command_at_once() {
    let Some(root) = std::env::var_os("COD4_PATH") else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let args: Vec<String> = ["+set", "net_port", "0"].map(String::from).to_vec();
    let mut s = Server::boot(&PathBuf::from(root), &args, false).expect("boot");
    for line in [
        "set g_gametype war",
        "set scr_war_timelimit 0",
        "set scr_war_scorelimit 0",
        "map mp_crash",
    ] {
        s.exec_line(line).expect(line);
    }
    s.run_frames(60);
    assert_eq!(bots(&s), 0, "a server asked for no bots has some");

    s.exec_line("set bot_count 3").unwrap();
    s.exec_line("map mp_crash").unwrap();
    s.run_frames(60);
    assert_eq!(bots(&s), 3, "the map start did not read bot_count");

    s.exec_line("bots 1").unwrap();
    s.run_frames(60);
    assert_eq!(bots(&s), 1, "`bots 1` did not kick down to one");
    s.exec_line("bots 0").unwrap();
    s.run_frames(60);
    assert_eq!(bots(&s), 0, "`bots 0` left bots");

    // The command outranks the cvar on later maps.
    s.exec_line("map mp_crash").unwrap();
    s.run_frames(60);
    assert_eq!(bots(&s), 0, "a later map brought the bots back");
}
