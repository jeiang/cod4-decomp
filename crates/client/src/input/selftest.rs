// SPDX-License-Identifier: GPL-3.0-or-later
//! `cod4e --input-selftest`: the harness's `client-input` stage. Drives the real [`Input`] with synthetic key, wheel
//! and mouse events (no window, GPU, install or timing involved) and checks what comes out. Prints one line per check;
//! returns the failures.

use super::{Input, buttons};

fn check(out: &mut Vec<String>, name: &str, ok: bool) {
    println!("{} {name}", if ok { "pass" } else { "FAIL" });
    if !ok {
        out.push(name.to_owned());
    }
}

/// Run every check; the names of the failed ones.
pub fn run() -> Vec<String> {
    let mut bad = Vec::new();
    let o = &mut bad;
    let mut i = Input::detached();

    for (key, f) in [("w", 1.0f32), ("s", -1.0)] {
        i.key(key, true);
        let fr = i.frame(0.01);
        check(
            o,
            &format!("default bind {key} -> move_forward {f}"),
            fr.move_forward == f,
        );
        i.key(key, false);
    }
    for (key, bit) in [
        ("mouse1", buttons::ATTACK),
        ("mouse2", buttons::ADS),
        ("r", buttons::RELOAD),
        ("e", buttons::USE),
        ("f", buttons::MELEE),
        ("g", buttons::FRAG),
        ("space", buttons::JUMP),
        ("ctrl", buttons::CROUCH),
        ("z", buttons::PRONE),
        ("shift", buttons::SPRINT),
    ] {
        i.key(key, true);
        let down = i.frame(0.01);
        i.key(key, false);
        let up = i.frame(0.01);
        check(
            o,
            &format!("default bind {key} -> button {bit:#x} press/release edges"),
            down.buttons & bit != 0
                && down.pressed & bit != 0
                && up.buttons & bit == 0
                && up.released & bit != 0,
        );
    }
    i.pulse("mwheelup");
    check(
        o,
        "mwheelup -> weapnext",
        i.frame(0.01).pending_commands == ["weapnext"],
    );
    i.key("tab", true);
    check(
        o,
        "tab holds +scores",
        i.frame(0.01).held_other == ["scores"],
    );
    i.key("tab", false);

    i.exec_line("set sensitivity 2; set m_yaw 0.5; set m_pitch 0.25");
    i.mouse.winit_motion((10.0, 4.0));
    let f = i.frame(0.01);
    check(
        o,
        "mouse look scaled by sensitivity, m_yaw, m_pitch",
        (f.look_delta_yaw, f.look_delta_pitch) == (-10.0, 2.0),
    );

    // Config write-back and reload through a real file.
    let dir = std::env::temp_dir().join(format!("cod4e-input-selftest-{}", std::process::id()));
    let cfg = dir.join("config_mp.cfg");
    let _ = std::fs::remove_dir_all(&dir);
    i.config_path = Some(cfg.clone());
    i.exec_line("seta sensitivity 7.5; bind x \"say \\\"hi; there\\\"\"; unbind w");
    let saved = i.save().is_ok();
    check(o, "config saved to disk", saved && cfg.is_file());
    let mut j = Input::detached();
    if let Ok(t) = std::fs::read_to_string(&cfg) {
        j.exec_text(&t, None, false, 0);
    }
    check(
        o,
        "config reload restores cvars and binds",
        j.cvar("sensitivity") == Some("7.5")
            && j.bound("w").is_none()
            && j.bound("x") == Some("say \"hi; there\"")
            && j.bound("mouse1") == Some("+attack"),
    );
    let _ = std::fs::remove_dir_all(&dir);

    println!(
        "raw mouse source: {} (informational)",
        super::rawmouse::RawMouse::new().source()
    );
    bad
}
