// SPDX-License-Identifier: GPL-3.0-only
//! The `devbotfire` test hook the `client-hud` compass check drives: a standing bot is given a weapon, raises it and
//! fires it through its own usercmds, and only an unsilenced shot marks it for enemy compasses (`last_fire_time`,
//! KisakCOD MP 1.8 `cgame/cg_weapons.cpp:2552-2559`). Skips without `COD4_PATH`.

use server::server::Server;
use std::path::PathBuf;

/// Frames in `secs` seconds at the server's 30 Hz.
fn frames(secs: u32) -> u32 {
    secs * 30
}

/// The first live bot on a team, once the bots have picked their teams and spawned.
fn live_bot(s: &mut Server) -> u16 {
    for _ in 0..60 {
        let found = s.game.connected_clients().find_map(|(n, c)| {
            (c.bot
                && matches!(
                    c.team,
                    server::client::Team::Axis | server::client::Team::Allies
                )
                && c.ps.pm_type < sim::pm::PmType::Dead
                && c.ps.weapon != 0)
                .then_some(n)
        });
        if let Some(n) = found {
            return n;
        }
        s.run_frames(frames(1));
    }
    panic!("no bot spawned on a team within a minute");
}

/// `(shots, last_fire_time, weapon in hand)` of client `n`.
fn record(s: &Server, n: u16) -> (u32, i32, u32) {
    let c = s.game.client(n).expect("the bot is connected");
    (c.shots, c.last_fire_time, c.ps.weapon)
}

#[test]
fn a_bot_fires_on_demand_and_only_unsilenced_shots_mark_it() {
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
        "bots 2",
    ] {
        s.exec_line(line).expect(line);
    }
    let n = live_bot(&mut s);
    s.exec_line("set bot_idle 1").unwrap();
    s.run_frames(frames(2));

    for (weapon, marks) in [("mp5_mp", true), ("mp5_silencer_mp", false)] {
        let index = u32::from(s.game.weapons.index(weapon));
        assert_ne!(index, 0, "{weapon} is not in the weapon table");
        s.exec_line(&format!("devbotfire {n} {weapon} 0")).unwrap();
        s.run_frames(frames(4));
        let before = record(&s, n);
        assert_eq!(
            before.2, index,
            "{weapon}: the bot did not raise it ({before:?})"
        );
        s.exec_line(&format!("devbotfire {n} {weapon} 500"))
            .unwrap();
        s.run_frames(frames(1));
        let after = record(&s, n);
        assert!(
            after.0 > before.0,
            "{weapon}: the bot never fired ({before:?} -> {after:?})"
        );
        assert_eq!(
            after.1 != before.1,
            marks,
            "{weapon}: last_fire_time {} -> {}",
            before.1,
            after.1
        );
    }

    // The frag key held 400 ms, as the `client-hud` check does: the grenade leaves as a missile, the bot lives, and
    // the throw marks nobody.
    let before = record(&s, n);
    s.exec_line(&format!("devbotfire {n} frag 400")).unwrap();
    let mut trace = Vec::new();
    let mut missile = false;
    for _ in 0..frames(3) {
        s.run_frames(1);
        let c = s.game.client(n).expect("the bot is connected");
        let flying = s.game.ents.iter().flatten().any(|e| e.missile.is_some());
        missile |= flying;
        trace.push(format!(
            "t {} state {} weapon {} offhand {} fuse {} buttons {} health {} missile {flying}",
            s.level_time(),
            c.ps.weapon_state,
            c.ps.weapon,
            c.ps.offhand_index,
            c.ps.grenade_time_left,
            c.buttons,
            c.ps.health
        ));
    }
    let after = record(&s, n);
    let alive = s
        .game
        .client(n)
        .is_some_and(|c| c.ps.pm_type < sim::pm::PmType::Dead);
    assert!(
        missile && alive,
        "the frag throw went wrong:\n{}",
        trace.join("\n")
    );
    assert_eq!(
        after.1, before.1,
        "a thrown frag marked the bot for enemy compasses"
    );
}
