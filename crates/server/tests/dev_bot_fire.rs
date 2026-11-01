// SPDX-License-Identifier: GPL-3.0-only
//! The gated `devbotfire` hook, mirroring the listen-server compass stage's idle, teleport, weapon sequence and waits.
//! A frag must actually become a missile. Skips without COD4_PATH.

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

/// `(shots, weapon in hand)` of client `n`.
fn record(s: &Server, n: u16) -> (u32, u32) {
    let c = s.game.client(n).expect("the bot is connected");
    (c.shots, c.ps.weapon)
}

#[test]
fn a_bot_fires_then_throws_after_the_compass_stage_sequence() {
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
    assert!(
        s.exec_line(&format!("devbotfire {n} mp5_mp 0")).is_err(),
        "hook must require opt-in"
    );
    s.exec_line("set sv_cheats 1").unwrap();
    let at = s.game.client(n).unwrap().ps.origin;
    s.exec_line(&format!("devtele {n} {} {} {}", at[0] + 96.0, at[1], at[2]))
        .unwrap();
    s.run_frames(frames(2));

    for weapon in ["mp5_mp", "mp5_silencer_mp"] {
        let index = u32::from(s.game.weapons.index(weapon));
        assert_ne!(index, 0, "{weapon} is not in the weapon table");
        s.exec_line(&format!("devbotfire {n} {weapon} 0")).unwrap();
        s.run_frames(frames(2));
        s.run_frames(frames(5));
        let before = record(&s, n);
        assert_eq!(
            before.1, index,
            "{weapon}: the bot did not raise it ({before:?})"
        );
        s.exec_line(&format!("devbotfire {n} {weapon} 500"))
            .unwrap();
        s.run_frames(frames(7));
        let after = record(&s, n);
        assert!(
            after.0 > before.0,
            "{weapon}: the bot never fired ({before:?} -> {after:?})"
        );
    }

    // Just like the client stage: allow the old mark to finish, then hold frag for a second.
    s.run_frames(frames(5));
    let before = record(&s, n);
    s.exec_line(&format!("devbotfire {n} frag 1000")).unwrap();
    let mut trace = Vec::new();
    let mut missile = false;
    for _ in 0..frames(4) {
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
    assert_eq!(after.0, before.0, "an offhand throw is not FireWeapon");
}
