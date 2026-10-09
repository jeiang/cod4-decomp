// SPDX-License-Identifier: GPL-3.0-only
//! The helicopter hardpoint end to end on the stock scripts: a bot calls it in, it flies the map's path, fires
//! at players and is shot down by bots. Skips without `COD4_PATH`.

use server::server::Server;
use std::path::PathBuf;

#[test]
fn a_called_in_helicopter_flies_fires_and_is_shot_down() {
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
        // Frail enough for a few bots to bring down.
        "set scr_heli_maxhealth 400",
        "set scr_heli_armor 20",
        "map mp_crash",
        "bots 8",
        "until spawns 8 60",
        "devhardpoint 0 helicopter_mp",
        "until helicopters 1 200",
    ] {
        s.exec_line(line).expect(line);
    }
    let mut start = None;
    let mut flown = 0.0f32;
    let mut last: Option<[f32; 3]> = None;
    for _ in 0..1200 {
        s.run_frames(20);
        match s.game.vehicles().next() {
            Some((_, e, _)) => {
                let o = e.origin;
                start.get_or_insert(o);
                if let Some(l) = last {
                    flown += (0..3)
                        .map(|i| (o[i] - l[i]) * (o[i] - l[i]))
                        .sum::<f32>()
                        .sqrt();
                }
                last = Some(o);
            }
            None if start.is_some() => break,
            None => {}
        }
    }
    let st = s.game.stats;
    assert!(start.is_some(), "no helicopter was spawned");
    assert!(flown > 1500.0, "flew only {flown} units");
    assert!(st.heli_shots > 0, "the helicopter never fired");
    assert!(st.heli_crashes >= 1, "the helicopter was never shot down");
    assert!(
        s.game.vehicles().next().is_none(),
        "the wreck was not removed"
    );
}
