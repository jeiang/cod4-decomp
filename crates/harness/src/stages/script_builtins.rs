// SPDX-License-Identifier: GPL-3.0-only
//! The script builtins on a running stock level: a check script, compiled against the stock builtin table, runs
//! on the live game after the level's own scripts have initialised. It exercises the developer-only builtins
//! (print channels, asserts, the debug drawings; #409), the precache rules (#374), the client name modes and the
//! per-player team scores (#410) and
//! the laser, aim assist, turret arc, vibrate and grenade effect builtins (#411), and every line must give the stock
//! result (KisakCOD `game_mp/g_scr_main_mp.cpp`, `game/g_scr_mover.cpp`).
use crate::stage::{StageCtx, StageReport, Status};
use gsc::{Builtins, CallOutcome, EntClass, Options, Value, Vm, compile};
use server::script::{Dispatch, ScriptHost};
use server::server::Server;
use server::ui::Dest;
use std::io;
use std::time::Duration;

pub const NAME: &str = "script-builtins";

/// The check script. Each `t_*` returns a value or fails with a script error; `CHECKS` says which.
const SCRIPT: &str = r#"
t_channels()
{
	/#
	createprintchannel("harness");
	r = [];
	r[0] = setprintchannel("harness");
	r[1] = setprintchannel("script");
	return r;
	#/
}
t_channel_permission() { /# x = setprintchannel("error"); #/ }
t_assert() { assertex(0, "harness"); }
t_draw()
{
	line((0, 0, 0), (0, 0, 64), (1, 0, 0), 1, 0, 3);
	print3d((0, 0, 72), "harness", (1, 1, 1), 1, 1, 3);
}
t_loadfx_late() { return loadfx("harness/none"); }
t_precacheshader_late() { precacheshader("white"); }
t_name_mode_bad() { setclientnamemode("sideways"); }
t_update_names_auto() { updateclientnames(); }
t_scores() { self updatescores(); }
t_vibrate_short() { e = spawn("script_model", (0, 0, 0)); e vibrate((0, 1, 0), 1, 1); }
t_aim_assist_model() { e = spawn("script_model", (0, 0, 0)); e enableaimassist(); }
t_arc_not_turret() { e = spawn("script_model", (0, 0, 0)); e setleftarc(10); }
t_laser() { e = spawn("script_model", (0, 0, 0)); e laseron(); return e; }
t_grenade_effect() { grenadeexplosioneffect((0, 0, 0)); }
"#;

enum Expect {
    /// Returns an array of these ints.
    Ints(&'static [i32]),
    Error(&'static str),
    Undefined,
    /// Returns an entity, which the stage checks afterwards.
    Entity,
}

const CHECKS: &[(&str, Expect)] = &[
    // The script channel is 24; the channel opened after the 25 engine channels is 25 (#409).
    ("t_channels", Expect::Ints(&[24, 25])),
    (
        "t_channel_permission",
        Expect::Error("Script does not have permission to print to this channel"),
    ),
    ("t_assert", Expect::Error("assert fail: harness")),
    ("t_draw", Expect::Undefined),
    // After the level's first wait nothing new can be precached (#374).
    (
        "t_loadfx_late",
        Expect::Error("effect \"harness/none\" not precached"),
    ),
    (
        "t_precacheshader_late",
        Expect::Error(
            "precacheShader must be called before any wait statements in the gametype or level script\n",
        ),
    ),
    ("t_name_mode_bad", Expect::Error("Unknown mode")),
    (
        "t_update_names_auto",
        Expect::Error("Only works in [manual_change] mode"),
    ),
    ("t_scores", Expect::Undefined),
    (
        "t_vibrate_short",
        Expect::Error("illegal call to vibrate()\n"),
    ),
    (
        "t_aim_assist_model",
        Expect::Error("Currently only supported on entities with brush models"),
    ),
    ("t_arc_not_turret", Expect::Error("entity is not a turret")),
    ("t_laser", Expect::Entity),
    ("t_grenade_effect", Expect::Undefined),
];

fn ints(v: &Value) -> Option<Vec<i32>> {
    let Value::Array(a) = v else { return None };
    Some(
        (0..)
            .map_while(|i| match a.get(&gsc::Key::Int(i)) {
                Some(Value::Int(n)) => Some(*n),
                _ => None,
            })
            .collect(),
    )
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(install) = ctx.install.as_deref() else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("install not found"));
    };
    let args: Vec<String> = ["+set", "net_port", "0"].map(String::from).to_vec();
    let mut server = match Server::boot(install, &args, false) {
        Ok(s) => s,
        Err(e) => return Ok(StageReport::new(NAME, Status::Failed).with_reason(e)),
    };
    for line in ["set g_gametype dm", "map mp_crash", "bots 1"] {
        if let Err(e) = server.exec_line(line) {
            return Ok(StageReport::new(NAME, Status::Failed).with_reason(format!("{line}: {e}")));
        }
    }
    server.run_for(Duration::from_secs(2));

    let mut failures = Vec::new();
    if !server.script_errors.is_empty() {
        failures.push(format!(
            "the stock scripts failed: {:?}",
            server.script_errors
        ));
    }
    let prog = match compile(
        &[("harness.gsc", SCRIPT)],
        &Builtins::stock_mp(),
        Options { developer: true },
    ) {
        Ok(p) => p,
        Err(e) => {
            return Ok(
                StageReport::new(NAME, Status::Failed).with_reason(format!("check script: {e:?}"))
            );
        }
    };
    let dispatch = Dispatch::new(&prog);
    let funcs: Vec<_> = CHECKS
        .iter()
        .map(|(name, _)| prog.find("harness", name))
        .collect();
    let mut vm = match Vm::new(prog) {
        Ok(v) => v,
        Err(e) => {
            return Ok(StageReport::new(NAME, Status::Failed).with_reason(format!("{e:?}")));
        }
    };
    // A real bot, connected through the level's own callbacks, receives the score commands.
    let Some(score_player) = server
        .game
        .connected_clients()
        .find(|(_, c)| c.bot)
        .map(|(n, _)| n)
    else {
        return Ok(StageReport::new(NAME, Status::Failed).with_reason("bots 1 connected no bot"));
    };
    // Start with both published values already current: `updatescores` must still send both commands (#410).
    server.game.team_score[2] = 17;
    server.game.team_score[1] = 23;
    server
        .game
        .set_configstring(net::ui::cs::SCORES_ALLIES, "17");
    server.game.set_configstring(net::ui::cs::SCORES_AXIS, "23");
    server.game.ui.out.clear();
    let lines_before = server.game.debug_draw.shown_lines.len();
    let mut laser = None;
    for ((name, expect), f) in CHECKS.iter().zip(funcs) {
        let Some(f) = f else {
            failures.push(format!("{name}: not in the check script"));
            continue;
        };
        let mut host = ScriptHost {
            game: &mut server.game,
            dispatch: &dispatch,
        };
        let this = (*name == "t_scores").then(|| vm.entity(score_player, EntClass::Entity));
        let got = match vm.call(&mut host, f, this, &[]) {
            Ok(CallOutcome::Finished(v)) => Ok(v),
            Ok(CallOutcome::Pending) => Err("waited".to_owned()),
            Err(e) => Err(e.message),
        };
        let ok = match (expect, &got) {
            (Expect::Ints(want), Ok(v)) => ints(v).as_deref() == Some(*want),
            (Expect::Error(want), Err(e)) => e == want,
            (Expect::Undefined, Ok(Value::Undefined)) => true,
            (Expect::Entity, Ok(Value::Object(o))) => {
                laser = o.entity().map(|e| e.num);
                laser.is_some()
            }
            _ => false,
        };
        if !ok {
            let want = match expect {
                Expect::Ints(w) => format!("{w:?}"),
                Expect::Error(e) => format!("error {e:?}"),
                Expect::Undefined => "undefined".to_owned(),
                Expect::Entity => "an entity".to_owned(),
            };
            failures.push(format!("{name}: want {want}, got {got:?}"));
        }
    }
    let scores: Vec<_> = server
        .game
        .ui
        .out
        .iter()
        .filter(|o| o.to == Dest::Client(score_player))
        .flat_map(|o| {
            let net::ui::ServerCmd::ConfigStrings(values) = &o.cmd else {
                return Vec::new();
            };
            values
                .iter()
                .filter(|(index, _)| {
                    matches!(
                        *index,
                        net::ui::cs::SCORES_ALLIES | net::ui::cs::SCORES_AXIS
                    )
                })
                .map(|(index, value)| (*index, value.clone()))
                .collect()
        })
        .collect();
    let want_scores = [
        (net::ui::cs::SCORES_ALLIES, "17".to_owned()),
        (net::ui::cs::SCORES_AXIS, "23".to_owned()),
    ];
    if scores != want_scores {
        failures.push(format!(
            "updatescores with already-current values sent {scores:?}, want {want_scores:?}"
        ));
    }
    // The drawings are published by the next frame and last 3 frames; a timed run would catch up several frames
    // after the checks and could age them out, so run exactly one.
    server.run_frames(1);
    let d = &server.game.debug_draw;
    if d.shown_lines.len() <= lines_before || !d.shown_strings.iter().any(|s| s.text == "harness") {
        failures.push(format!(
            "line()/print3d() never reached the debug drawing: {} lines, strings {:?}",
            d.shown_lines.len(),
            d.shown_strings.iter().map(|s| &s.text).collect::<Vec<_>>()
        ));
    }
    // `laseron` flags the entity (#411).
    if !laser.is_some_and(|n| server.game.ent(n).is_some_and(|e| e.x.laser)) {
        failures.push(format!("laseron did not flag entity {laser:?}"));
    }
    // `grenadeexplosioneffect` raised an explosion of the default weapon (#411).
    let default_weapon = server.game.weapons.index("defaultweapon_mp");
    let now = server.game.level.time;
    let exploded = server.game.tempev.live(now).any(|s| {
        s.event == server::tempev::ev::EXPLOSION && s.weapon == default_weapon && s.origin[2] == 1.0
    });
    if default_weapon == 0 || !exploded {
        failures.push(format!(
            "grenadeexplosioneffect raised no defaultweapon_mp explosion (weapon index {default_weapon})"
        ));
    }

    let mut report = StageReport::new(NAME, Status::Passed);
    report
        .metrics
        .insert("checks".into(), CHECKS.len() as f64 + 1.0);
    report
        .metrics
        .insert("failures".into(), failures.len() as f64);
    if failures.is_empty() {
        Ok(report)
    } else {
        report.notes.extend(failures.iter().cloned());
        Ok(report
            .with_status(Status::Failed)
            .with_reason(failures.join("; ")))
    }
}
