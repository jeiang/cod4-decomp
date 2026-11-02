// SPDX-License-Identifier: GPL-3.0-only
//! The script builtins on a running stock level: a check script, compiled against the stock builtin table, runs
//! on the live game after the level's own scripts have initialised. It exercises the developer-only builtins
//! (print channels, asserts, the debug drawings, the file functions, `g_no_script_spam`, the developer-statement
//! compile rules with and without `developer_script`; #409) and the precache rules, their unknown-asset warnings
//! and the `bots` diagnostics (#374), and every line must give the stock result (KisakCOD
//! `game_mp/g_scr_main_mp.cpp`, `script/scr_compiler.cpp`). It also checks the other paths these changed: a
//! malformed UI server command is logged, a bullet into a corpse strikes flesh, and `sv_botsPressAttackBtn 0`
//! keeps a bot's attack button up; and a listen client must draw the `line`/`print3d` its server ran
//! (`script_builtins_listen`).
use crate::stage::{StageCtx, StageReport, Status};
use gsc::{Builtins, CallOutcome, Options, Value, Vm, compile};
use server::client::Team;
use server::script::{Dispatch, ScriptHost};
use server::server::Server;
use sim::weapon::{WeaponEvent, WeaponOut};
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
t_files()
{
	/#
	r = [];
	r[0] = isdefined(openfile("harness.txt", "read"));
	r[1] = isdefined(closefile(0));
	r[2] = isdefined(fprintln(0, "harness"));
	r[3] = isdefined(fprintfields(0, "harness"));
	r[4] = isdefined(freadln(0));
	r[5] = isdefined(fgetarg(0, 0));
	return r;
	#/
}
t_print() { println("harness_spam"); }
t_precache_absent()
{
	precacheshader("harness_absent_shader");
	precachemenu("harness_absent_menu");
	precachestatusicon("harness_absent_icon");
	loadfx("harness/absent_fx");
}
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
    // The file functions do nothing in the release build and return nothing (#409).
    ("t_files", Expect::Ints(&[0, 0, 0, 0, 0, 0])),
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
    for line in [
        "set g_gametype war",
        "set scr_war_timelimit 0",
        "set scr_war_scorelimit 0",
        "map mp_crash",
        "bots 2",
    ] {
        if let Err(e) = server.exec_line(line) {
            return Ok(StageReport::new(NAME, Status::Failed).with_reason(format!("{line}: {e}")));
        }
    }
    server.run_for(Duration::from_secs(2));
    // The corpse and attack-button checks need both bots spawned on a team.
    for _ in 0..60 {
        if live_bots(&server).len() >= 2 {
            break;
        }
        server.run_frames(30);
    }

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
    let print_f = prog.find("harness", "t_print");
    let absent_f = prog.find("harness", "t_precache_absent");
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
    developer_compile_rules(&mut server, &mut failures);
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
    let mut call = |server: &mut Server, f: gsc::bytecode::FuncId| match vm.call(
        &mut ScriptHost {
            game: &mut server.game,
            dispatch: &dispatch,
        },
        f,
        None,
        &[],
    ) {
        Ok(CallOutcome::Finished(v)) => Ok(v),
        Ok(CallOutcome::Pending) => Err("waited".to_owned()),
        Err(e) => Err(e.message),
    };
    // `print`/`println` are silent under `g_no_script_spam` (KisakCOD `game_mp/g_scr_main_mp.cpp:526,539`).
    match print_f {
        Some(f) => {
            let mut seen = Vec::new();
            for spam in ["1", "0"] {
                server.game.cvars.set("g_no_script_spam", spam);
                server.game.printed.clear();
                let r = call(&mut server, f);
                seen.push((r.is_ok(), std::mem::take(&mut server.game.printed)));
            }
            let want = vec![
                (true, Vec::new()),
                (true, vec!["harness_spam\n".to_owned()]),
            ];
            if seen != want {
                failures.push(format!(
                    "g_no_script_spam 1 then 0: printed {seen:?}, want {want:?}"
                ));
            }
        }
        None => failures.push("t_print: not in the check script".to_owned()),
    }
    // While the level initializes, an asset the zones lack still registers and is warned about once (#374).
    match absent_f {
        Some(f) => {
            server.game.level.initializing = true;
            let (r, events) = logging::capture(|| call(&mut server, f));
            server.game.level.initializing = false;
            let warned: Vec<_> = events
                .iter()
                .filter(|e| e.level == tracing::Level::WARN)
                .filter_map(|e| Some((e.field("builtin")?.to_owned(), e.field("name")?.to_owned())))
                .collect();
            let want: Vec<_> = [
                ("precacheshader", "harness_absent_shader"),
                ("precachemenu", "harness_absent_menu"),
                ("precachestatusicon", "harness_absent_icon"),
                ("loadfx", "harness/absent_fx"),
            ]
            .map(|(b, n)| (b.to_owned(), n.to_owned()))
            .to_vec();
            if r.is_err() || warned != want {
                failures.push(format!(
                    "unknown precaches: got {r:?} and warnings {warned:?}, want {want:?}"
                ));
            }
        }
        None => failures.push("t_precache_absent: not in the check script".to_owned()),
    }
    malformed_server_command(&mut failures);
    bot_count_diagnostics(&mut server, &mut failures);
    corpse_strikes_flesh(&mut server, &mut vm, &mut failures);
    bots_attack_button(&mut server, &mut failures);
    // The in-process server is done with; the listen client below brings its own.
    drop(server);
    let listen = match super::script_builtins_listen::listen_debug_draw(ctx) {
        Ok(m) => m,
        Err(e) => {
            failures.push(format!("listen client debug drawing: {e}"));
            Vec::new()
        }
    };

    let mut report = StageReport::new(NAME, Status::Passed);
    report
        .metrics
        .insert("checks".into(), CHECKS.len() as f64 + 9.0);
    report.metrics.extend(listen);
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

/// The bots alive on a team.
fn live_bots(server: &Server) -> Vec<u16> {
    server
        .game
        .connected_clients()
        .filter(|(_, c)| {
            c.bot
                && matches!(c.team, Team::Axis | Team::Allies)
                && c.ps.pm_type < sim::pm::PmType::Dead
                && c.ps.weapon != 0
        })
        .map(|(n, _)| n)
        .collect()
}

/// The developer-statement compile rules (KisakCOD `script/scr_compiler.cpp:468-563,871-891,1000-1022`): the value
/// of a developer-only call outside `/# ... #/` is an error whatever `developer_script` says, and without
/// `developer_script` such calls compile and are then dropped, so nothing prints or draws (#409).
fn developer_compile_rules(server: &mut Server, failures: &mut Vec<String>) {
    const ACCESS: &str =
        "return value of developer command can not be accessed if not in a /# ... #/ comment";
    let b = Builtins::stock_mp();
    for developer in [false, true] {
        for src in [
            "t_ret() { e = getentbynum(1); }",
            "t_ret() { x = self devaddyaw(1); }",
        ] {
            match compile(&[("harness_ret.gsc", src)], &b, Options { developer }) {
                Err(e) if e.first().is_some_and(|e| e.message == ACCESS) => {}
                other => failures.push(format!(
                    "{src} with developer_script {}: want the developer return-value error, got {:?}",
                    u8::from(developer),
                    other.err()
                )),
            }
        }
    }
    // `self` is undefined here, so the dropped method call would fail if it ran.
    let src = "t_nodev() { println(\"harness_nodev\"); assertex(0, \"harness_nodev\"); \
               line((0, 0, 0), (0, 0, 64), (1, 0, 0), 1, 0, 30); self devaddyaw(1); return 1; }";
    let prog = match compile(&[("harness_nodev.gsc", src)], &b, Options::default()) {
        Ok(p) => p,
        Err(e) => {
            failures.push(format!("developer_script 0 check script: {e:?}"));
            return;
        }
    };
    let Some(f) = prog.find("harness_nodev", "t_nodev") else {
        failures.push("t_nodev: not in its script".to_owned());
        return;
    };
    let dispatch = Dispatch::new(&prog);
    let mut vm = match Vm::new(prog) {
        Ok(v) => v,
        Err(e) => {
            failures.push(format!("developer_script 0 vm: {e:?}"));
            return;
        }
    };
    server.game.printed.clear();
    let lines = server.game.debug_draw.shown_lines.len();
    let mut host = ScriptHost {
        game: &mut server.game,
        dispatch: &dispatch,
    };
    let got = match vm.call(&mut host, f, None, &[]) {
        Ok(CallOutcome::Finished(v)) => Ok(v),
        Ok(CallOutcome::Pending) => Err("waited".to_owned()),
        Err(e) => Err(e.message),
    };
    let printed = std::mem::take(&mut server.game.printed);
    server.run_frames(1);
    let now = server.game.debug_draw.shown_lines.len();
    if !matches!(got, Ok(Value::Int(1))) || !printed.is_empty() || now > lines {
        failures.push(format!(
            "developer_script 0 kept developer calls: got {got:?}, printed {printed:?}, lines {lines} -> {now}"
        ));
    }
}

/// A UI server command whose arguments do not parse is logged with its word and argument count; a word that is
/// not a UI command is not.
fn malformed_server_command(failures: &mut Vec<String>) {
    // Other stages in this process may have used up the limiter's early hits for `stat`.
    logging::new_window();
    let (parsed, events) = logging::capture(|| {
        [
            net::ui::ServerCmd::parse("stat harness_index 3"),
            net::ui::ServerCmd::parse("harness_not_ui 1 2"),
        ]
    });
    let warned: Vec<_> = events
        .iter()
        .filter(|e| e.level == tracing::Level::WARN)
        .map(|e| (e.field("command"), e.field("args")))
        .collect();
    if parsed.iter().any(Option::is_some) || warned != [(Some("stat"), Some("2"))] {
        failures.push(format!(
            "malformed server commands: parsed {parsed:?}, warnings {warned:?}, want one for stat with 2 args"
        ));
    }
}

/// `bots` says why it refuses a count that is not a number or is out of range (#374).
fn bot_count_diagnostics(server: &mut Server, failures: &mut Vec<String>) {
    for (line, want) in [
        ("bots harness", "is not a number"),
        ("bots 99", "is out of range"),
    ] {
        match server.exec_line(line) {
            Err(e) if e.contains(want) => {}
            other => failures.push(format!(
                "{line}: want an error saying {want:?}, got {other:?}"
            )),
        }
    }
}

/// A bullet into a player corpse strikes flesh, as into the player (KisakCOD `game/bullet.cpp:58-67`): one bot
/// leaves a corpse and steps out of the line of fire, and the other fires straight down onto it from just above.
fn corpse_strikes_flesh(server: &mut Server, vm: &mut Vm, failures: &mut Vec<String>) {
    use server::tempev::ev;
    let live = live_bots(server);
    let [shooter, victim, ..] = live[..] else {
        failures.push(format!("corpse impact: want two live bots, have {live:?}"));
        return;
    };
    let g = &mut server.game;
    let Some(body) = g.clone_player(vm, victim, 5000, "pb_death_run_onfront") else {
        failures.push("corpse impact: clone_player made no corpse".to_owned());
        return;
    };
    let at = g.ent(body).map_or([0.0; 3], |e| e.origin);
    if let Some(e) = g.ent_mut(victim) {
        e.contents = 0;
    }
    g.relink(victim);
    let weapon = g.weapons.index("mp5_mp");
    let Some(c) = g.client_mut(shooter) else {
        return;
    };
    // A cleared gun motion has no aim of its own yet, so the shot leaves along the view angles set here.
    let kept = (
        c.ps.origin,
        c.ps.viewangles,
        c.ps.weapon,
        std::mem::take(&mut c.gun),
    );
    c.ps.origin = [at[0], at[1], at[2] + 30.0];
    c.ps.viewangles = [89.0, 0.0, 0.0];
    c.ps.weapon = u32::from(weapon);
    let mut out = WeaponOut::default();
    out.push(WeaponEvent::Fire {
        weapon,
        shot: 1,
        first: true,
        ads: false,
        burst: false,
        last_round: false,
    });
    g.weapon_events(vm, shooter, &out);
    let now = g.level.time;
    let hits: Vec<_> = g
        .tempev
        .live(now)
        .filter(|e| {
            e.event == ev::BULLET_IMPACT
                && (e.origin[0] - at[0]).abs() < 24.0
                && (e.origin[1] - at[1]).abs() < 24.0
        })
        .map(|e| (e.event_parm, e.origin))
        .collect();
    if let Some(c) = g.client_mut(shooter) {
        (c.ps.origin, c.ps.viewangles, c.ps.weapon, c.gun) = kept;
    }
    g.set_client_contents(victim);
    g.relink(victim);
    if weapon == 0 || !hits.iter().any(|(surface, _)| *surface == 7) {
        failures.push(format!(
            "a bullet into the corpse at {at:?} struck {hits:?}, want flesh (surface 7; mp5_mp index {weapon})"
        ));
    }
}

/// With `sv_botsPressAttackBtn 0` a bot's attack button stays up, so a held `devbotfire` fires nothing until it
/// is set back to 1.
fn bots_attack_button(server: &mut Server, failures: &mut Vec<String>) {
    let Some(&n) = live_bots(server).first() else {
        failures.push("sv_botsPressAttackBtn: no live bot".to_owned());
        return;
    };
    let shots = |s: &Server| s.game.client(n).map_or(0, |c| c.shots);
    let at = server.game.client(n).map_or([0.0; 3], |c| c.ps.origin);
    let setup = [
        "set developer 1".to_owned(),
        "set bot_idle 1".to_owned(),
        format!("devtele {n} {} {} {}", at[0] + 96.0, at[1], at[2]),
        format!("devbotfire {n} mp5_mp 0"),
    ];
    for line in &setup {
        if let Err(e) = server.exec_line(line) {
            failures.push(format!("sv_botsPressAttackBtn: {line}: {e}"));
            return;
        }
    }
    server.run_frames(7 * 30);
    let mut seen = Vec::new();
    for press in ["0", "1"] {
        let before = shots(server);
        for line in [
            format!("set sv_botsPressAttackBtn {press}"),
            format!("devbotfire {n} mp5_mp 500"),
        ] {
            if let Err(e) = server.exec_line(&line) {
                failures.push(format!("sv_botsPressAttackBtn: {line}: {e}"));
                return;
            }
        }
        server.run_frames(3 * 30);
        seen.push(shots(server).saturating_sub(before));
    }
    if seen[0] != 0 || seen[1] == 0 {
        failures.push(format!(
            "shots with sv_botsPressAttackBtn 0 then 1: {seen:?}, want none then some"
        ));
    }
}
