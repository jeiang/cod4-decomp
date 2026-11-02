// SPDX-License-Identifier: GPL-3.0-only
//! The script builtins on a running stock level: a check script, compiled against the stock builtin table, runs
//! on the live game after the level's own scripts have initialised. It exercises the developer-only builtins
//! (print channels, asserts, the debug drawings; #409) and the precache rules (#374), and every line must give
//! the stock result (KisakCOD `game_mp/g_scr_main_mp.cpp`).
use crate::stage::{StageCtx, StageReport, Status};
use gsc::{Builtins, CallOutcome, Options, Value, Vm, compile};
use server::script::{Dispatch, ScriptHost};
use server::server::Server;
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
"#;

enum Expect {
    /// Returns an array of these ints.
    Ints(&'static [i32]),
    Error(&'static str),
    Undefined,
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
    for line in ["set g_gametype dm", "map mp_crash"] {
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
    let lines_before = server.game.debug_draw.shown_lines.len();
    for ((name, expect), f) in CHECKS.iter().zip(funcs) {
        let Some(f) = f else {
            failures.push(format!("{name}: not in the check script"));
            continue;
        };
        let mut host = ScriptHost {
            game: &mut server.game,
            dispatch: &dispatch,
        };
        let got = match vm.call(&mut host, f, None, &[]) {
            Ok(CallOutcome::Finished(v)) => Ok(v),
            Ok(CallOutcome::Pending) => Err("waited".to_owned()),
            Err(e) => Err(e.message),
        };
        let ok = match (expect, &got) {
            (Expect::Ints(want), Ok(v)) => ints(v).as_deref() == Some(*want),
            (Expect::Error(want), Err(e)) => e == want,
            (Expect::Undefined, Ok(Value::Undefined)) => true,
            _ => false,
        };
        if !ok {
            let want = match expect {
                Expect::Ints(w) => format!("{w:?}"),
                Expect::Error(e) => format!("error {e:?}"),
                Expect::Undefined => "undefined".to_owned(),
            };
            failures.push(format!("{name}: want {want}, got {got:?}"));
        }
    }
    // The drawings are published by the next frame and outlive it (duration 3 frames).
    server.run_for(Duration::from_millis(100));
    let d = &server.game.debug_draw;
    if d.shown_lines.len() <= lines_before || !d.shown_strings.iter().any(|s| s.text == "harness") {
        failures.push(format!(
            "line()/print3d() never reached the debug drawing: {} lines, strings {:?}",
            d.shown_lines.len(),
            d.shown_strings.iter().map(|s| &s.text).collect::<Vec<_>>()
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
