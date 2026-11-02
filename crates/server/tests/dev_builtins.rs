// SPDX-License-Identifier: GPL-3.0-only
//! The developer-only builtins driven through GSC: printing and its channels, asserts, the debug drawings and the
//! file functions (KisakCOD `game_mp/g_scr_main_mp.cpp:467-548,717-836`; issue #409).

use gsc::{Builtins, CallOutcome, Options, Value, Vm, compile};
use server::content::Content;
use server::cvar::Cvars;
use server::game::Game;
use server::script::{Dispatch, ScriptHost};

const SCRIPT: &str = r#"
t_print() { println("a", 1, " ", 2.5); print("b"); }
t_assert_ok() { assert(1); assertex(1, "x"); }
t_assert() { assert(0); }
t_assertex() { assertex(0, "why"); }
t_assertmsg() { assertmsg("always"); }
t_channels()
{
	/#
	createprintchannel("mine");
	createprintchannel("MINE");
	r = [];
	r[0] = setprintchannel("mine");
	r[1] = setprintchannel("script");
	r[2] = setprintchannel(24);
	return r;
	#/
}
t_bad_channel() { /# x = setprintchannel("nosuch"); #/ }
t_no_permission() { /# x = setprintchannel("error"); #/ }
t_bad_create() { createprintchannel(); }
t_draw()
{
	line((0, 0, 0), (10, 0, 0));
	line((0, 0, 0), (0, 10, 0), (1, 0, 0), 0.5, 1, 3);
	print3d((0, 0, 64), "hello");
	print3d((0, 0, 64), "red", (1, 0, 0), 1, 2, 2);
}
t_bad_line() { line((0, 0, 0)); }
t_bad_print3d() { print3d((0, 0, 0), "a", (1, 1, 1), 1, 1, 1, 1); }
t_files()
{
	/#
	r = [];
	r[0] = isdefined(openfile("x", "read"));
	r[1] = isdefined(freadln(0));
	r[2] = isdefined(fgetarg(0, 0));
	return r;
	#/
}
"#;

fn game() -> Game {
    let mut g = Game::new(Cvars::new(), Content::default());
    g.reset_level(8);
    g
}

fn run(g: &mut Game, name: &str) -> Result<Value, String> {
    let prog = compile(
        &[("t.gsc", SCRIPT)],
        &Builtins::stock_mp(),
        Options { developer: true },
    )
    .unwrap();
    let f = prog.find("t", name).unwrap();
    let dispatch = Dispatch::new(&prog);
    let mut vm = Vm::new(prog).unwrap();
    let mut host = ScriptHost {
        game: g,
        dispatch: &dispatch,
    };
    match vm.call(&mut host, f, None, &[]) {
        Ok(CallOutcome::Finished(v)) => Ok(v),
        Ok(CallOutcome::Pending) => Err("waited".into()),
        Err(e) => Err(e.message),
    }
}

fn ints(v: &Value) -> Vec<i32> {
    let Value::Array(a) = v else { panic!("{v:?}") };
    (0..)
        .map_while(|i| match a.get(&gsc::Key::Int(i)) {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        })
        .collect()
}

#[test]
fn print_and_println_write_to_the_console_unless_g_no_script_spam() {
    let mut g = game();
    run(&mut g, "t_print").unwrap();
    assert_eq!(g.printed, ["a1 2.5\n", "b"]);
    g.printed.clear();
    g.cvars.register("g_no_script_spam", "0", 0);
    g.cvars.set("g_no_script_spam", "1");
    run(&mut g, "t_print").unwrap();
    assert!(g.printed.is_empty(), "{:?}", g.printed);
}

#[test]
fn asserts_fail_with_the_stock_messages_whatever_developer_says() {
    let mut g = game();
    run(&mut g, "t_assert_ok").unwrap();
    assert_eq!(run(&mut g, "t_assert").unwrap_err(), "assert fail");
    assert_eq!(run(&mut g, "t_assertex").unwrap_err(), "assert fail: why");
    assert_eq!(
        run(&mut g, "t_assertmsg").unwrap_err(),
        "assert fail: always"
    );
}

#[test]
fn scripts_open_channels_and_print_only_to_their_own() {
    let mut g = game();
    let r = run(&mut g, "t_channels").unwrap();
    let mine = logging::channels::NAMES.len() as i32;
    // Each call returns the channel before it.
    assert_eq!(ints(&r), [24, mine, 24]);
    assert_eq!(g.level.script_print_channel, 24);
    assert_eq!(
        run(&mut g, "t_bad_channel").unwrap_err(),
        "Invalid Print Channel"
    );
    assert_eq!(
        run(&mut g, "t_no_permission").unwrap_err(),
        "Script does not have permission to print to this channel"
    );
    assert_eq!(
        run(&mut g, "t_bad_create").unwrap_err(),
        "illegal call to createprintchannel()"
    );
}

#[test]
fn line_and_print3d_draw_for_their_duration_in_frames() {
    let mut g = game();
    g.debug_draw.flush();
    run(&mut g, "t_draw").unwrap();
    g.debug_draw.publish();
    let d = &g.debug_draw;
    assert_eq!(d.shown_lines.len(), 2);
    assert_eq!(d.shown_lines[1].color, [1.0, 0.0, 0.0, 0.5]);
    assert!(d.shown_lines[1].depth_test && !d.shown_lines[0].depth_test);
    assert_eq!(d.shown_strings.len(), 2);
    assert_eq!(
        (d.shown_strings[0].text.as_str(), d.shown_strings[0].scale),
        ("hello", 1.0)
    );
    assert_eq!(d.shown_strings[1].color, [1.0, 0.0, 0.0, 1.0]);
    // Next frame: the default durations (line 0, print3d 1) are gone, the longer ones stay.
    g.debug_draw.flush();
    g.debug_draw.publish();
    assert_eq!(g.debug_draw.shown_lines.len(), 1);
    assert_eq!(g.debug_draw.shown_strings.len(), 1);
    assert_eq!(
        run(&mut g, "t_bad_line").unwrap_err(),
        "illegal call to line()"
    );
    assert_eq!(
        run(&mut g, "t_bad_print3d").unwrap_err(),
        "illegal call to print3d()"
    );
}

#[test]
fn the_file_functions_return_nothing_as_in_the_release_build() {
    let mut g = game();
    assert_eq!(ints(&run(&mut g, "t_files").unwrap()), [0, 0, 0]);
}
