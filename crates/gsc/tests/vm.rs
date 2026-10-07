// SPDX-License-Identifier: GPL-3.0-or-later
//! VM behavior and the scheduler's ordering rules (research/gsc-sched, ticket #20).
//!
//! Scripts log through `println`; the test host joins the arguments with spaces.

use std::collections::HashMap;

use gsc::bytecode::{File, Function, Op};
use gsc::{
    Builtins, CallOutcome, EntClass, EntRef, Host, Obj, Options, Program, Value, Vm, VmErrorKind,
    compile,
};

struct TestHost {
    log: Vec<String>,
    functions: Vec<String>,
    methods: Vec<String>,
    origins: HashMap<u16, Value>,
    /// Result of the last `spawn(function)` re-entrant call.
    spawned: Vec<String>,
}

fn show(v: &Value) -> String {
    match v {
        Value::Str(s) => s.to_string(),
        Value::Undefined => "undefined".into(),
        Value::Float(f) => gsc::ops::format_g(*f),
        Value::Vector(v) => gsc::ops::format_vector(v),
        other => format!("{other:?}"),
    }
}

impl Host for TestHost {
    fn call_function(&mut self, vm: &mut Vm, index: u16, args: &[Value]) -> Result<Value, String> {
        match self.functions[usize::from(index)].as_str() {
            "println" => {
                let line = args.iter().map(show).collect::<Vec<_>>().join(" ");
                self.log.push(line);
                Ok(Value::Undefined)
            }
            "isdefined" => Ok(Value::Int(i32::from(!matches!(args[0], Value::Undefined)))),
            "spawnstruct" => Ok(Value::Object(Obj::new_struct())),
            "gettime" => Ok(Value::Int(vm.time() as i32)),
            // `spawn(function)`: an engine call made from inside a builtin.
            "spawn" => {
                let Value::Func(f) = args[0] else {
                    return Err("spawn wants a function".into());
                };
                let r = match vm.call(self, f, None, &[]) {
                    Ok(_) => "ok".to_string(),
                    Err(e) => e.message,
                };
                self.spawned.push(r);
                Ok(Value::Undefined)
            }
            "assert" => Err("assertion failed".into()),
            other => Err(format!("unexpected builtin {other}")),
        }
    }

    fn call_method(
        &mut self,
        vm: &mut Vm,
        index: u16,
        ent: EntRef,
        _args: &[Value],
    ) -> Result<Value, String> {
        match self.methods[usize::from(index)].as_str() {
            "delete" => {
                vm.free_entity(ent.num);
                Ok(Value::Undefined)
            }
            other => Err(format!("unexpected method {other}")),
        }
    }

    fn get_field(&mut self, ent: EntRef, name: &str) -> Option<Value> {
        (name == "origin").then(|| {
            self.origins
                .get(&ent.num)
                .cloned()
                .unwrap_or(Value::Undefined)
        })
    }

    fn set_field(&mut self, ent: EntRef, name: &str, v: &Value) -> Result<bool, String> {
        if name == "origin" {
            self.origins.insert(ent.num, v.clone());
            return Ok(true);
        }
        Ok(false)
    }
}

struct Env {
    vm: Vm,
    host: TestHost,
}

fn program(src: &str) -> Program {
    compile(&[("a.gsc", src)], &Builtins::stock_mp(), Options::default())
        .unwrap_or_else(|e| panic!("{e:?}"))
}

fn env_of(p: Program) -> Env {
    let host = TestHost {
        log: Vec::new(),
        functions: p
            .builtins
            .function_names()
            .iter()
            .map(|s| s.to_string())
            .collect(),
        methods: p
            .builtins
            .method_names()
            .iter()
            .map(|s| s.to_string())
            .collect(),
        origins: HashMap::new(),
        spawned: Vec::new(),
    };
    Env {
        vm: Vm::new(p).unwrap(),
        host,
    }
}

fn env(src: &str) -> Env {
    env_of(program(src))
}

impl Env {
    fn func(&self, name: &str) -> u32 {
        self.vm.program().find("a", name).unwrap()
    }

    fn call(&mut self, name: &str) -> CallOutcome {
        let f = self.func(name);
        self.vm.call(&mut self.host, f, None, &[]).unwrap()
    }

    fn call_on(&mut self, name: &str, this: &Obj) -> CallOutcome {
        let f = self.func(name);
        self.vm
            .call(&mut self.host, f, Some(this.clone()), &[])
            .unwrap()
    }

    /// Drains the current bucket without advancing the tick.
    fn drain(&mut self) {
        let errs = self.vm.run_current_threads(&mut self.host);
        assert!(errs.is_empty(), "{errs:?}");
    }

    fn ticks(&mut self, n: u32) {
        for _ in 0..n {
            let errs = self.vm.inc_time(&mut self.host);
            assert!(errs.is_empty(), "{errs:?}");
        }
    }

    fn take(&mut self) -> Vec<String> {
        std::mem::take(&mut self.host.log)
    }
}

/// Runs `main` to completion on a fresh VM and returns the log.
fn run(src: &str) -> Vec<String> {
    let mut e = env(src);
    e.call("main");
    e.ticks(200);
    e.take()
}

fn ent(e: &mut Env, num: u16) -> Obj {
    e.vm.entity(num, EntClass::Entity)
}

// ---- scheduling order ----

#[test]
fn a_bucket_runs_last_inserted_first() {
    // Each thread re-files itself every tick, so the order flips each round.
    let log = run(r#"
        main() { thread t("a"); thread t("b"); thread t("c"); }
        t(n) { for (i = 0; i < 3; i++) { println(n); wait 0.05; } }
    "#);
    assert_eq!(log.join(""), "abccbaabc");
}

#[test]
fn thread_calls_run_to_their_first_yield_before_returning() {
    let log = run(r#"
        main() { println("before"); thread t(); println("after"); }
        t() { println("t1"); wait 0.05; println("t2"); }
    "#);
    assert_eq!(log, ["before", "t1", "after", "t2"]);
}

#[test]
fn notify_wakes_oldest_first_and_runs_newest_registration_first() {
    let mut e = env(r#"
        main() { thread w("1"); thread w("2"); thread w("3"); }
        w(n) { level waittill("go"); println(n); }
        fire() { level notify("go"); }
    "#);
    e.call("main");
    e.call("fire");
    // Nothing runs inside notify; the woken threads wait in the current bucket.
    assert!(e.take().is_empty());
    e.drain();
    assert_eq!(e.take(), ["3", "2", "1"]);
}

#[test]
fn a_notify_inside_a_drain_runs_its_waiters_in_the_same_drain() {
    let mut e = env(r#"
        main() { thread w("1"); thread w("2"); thread fire(); }
        w(n) { level waittill("go"); println(n); }
        fire() { wait 0.05; level notify("go"); println("fired"); }
    "#);
    e.call("main");
    e.ticks(1);
    assert!(e.take().is_empty());
    e.ticks(1);
    assert_eq!(e.take(), ["fired", "2", "1"]);
}

#[test]
fn woken_threads_receive_the_payload_padded_with_undefined() {
    let mut e = env(r#"
        main() { thread w(); }
        w() { level waittill("e", a, b, c); println(a, b, isdefined(c)); }
        fire() { level notify("e", 1, "s"); }
    "#);
    e.call("main");
    e.call("fire");
    e.drain();
    assert_eq!(e.take(), ["1 s 0"]);
}

#[test]
fn an_extra_payload_value_is_dropped() {
    let mut e = env(r#"
        main() { thread w(); }
        w() { level waittill("e", a); println(a); }
        fire() { level notify("e", 7, 8, 9); }
    "#);
    e.call("main");
    e.call("fire");
    e.drain();
    assert_eq!(e.take(), ["7"]);
}

#[test]
fn a_notify_wakes_a_waiter_once() {
    let mut e = env(r#"
        main() { thread w(); }
        w() { level waittill("e"); println("woke"); }
        fire() { level notify("e"); level notify("e"); }
    "#);
    e.call("main");
    e.call("fire");
    e.drain();
    assert_eq!(e.take(), ["woke"]);
    assert_eq!(e.vm.thread_count(), 0);
}

#[test]
fn waittillmatch_skips_non_matching_payloads_and_stays_registered() {
    let mut e = env(r#"
        main() { thread w(); }
        w() { level waittillmatch("m", "b"); println("matched"); }
        fire(v) { level notify("m", v); }
        a() { level notify("m", "a"); }
        b() { level notify("m", "b"); }
    "#);
    e.call("main");
    e.call("a");
    e.drain();
    assert!(e.take().is_empty());
    assert_eq!(e.vm.thread_count(), 1);
    e.call("b");
    e.drain();
    assert_eq!(e.take(), ["matched"]);
}

#[test]
fn endon_on_the_first_frame_ends_the_thread() {
    let mut e = env(r#"
        main() { thread t(); }
        t() { level endon("stop"); for (;;) { println("tick"); wait 0.05; } }
        stop() { level notify("stop"); }
    "#);
    e.call("main");
    e.ticks(3);
    assert_eq!(e.take(), ["tick", "tick", "tick"]);
    e.call("stop");
    e.ticks(3);
    assert!(e.take().is_empty());
    assert_eq!(e.vm.thread_count(), 0);
}

#[test]
fn endon_on_an_inner_frame_resumes_the_caller_with_undefined() {
    let mut e = env(r#"
        main() { thread w(); thread v(); }
        w() { r = inner(); println("w resumed", isdefined(r)); }
        inner() { level endon("stop"); level waittill("never"); println("inner after"); return 5; }
        v() { level waittill("stop"); println("v"); }
        fire() { level notify("stop"); }
    "#);
    e.call("main");
    e.call("fire");
    assert!(e.take().is_empty());
    e.drain();
    // The endon entry is older, so w is filed first and v (woken second) runs ahead of it.
    assert_eq!(e.take(), ["v", "w resumed 0"]);
}

#[test]
fn endon_hit_during_a_wait_removes_the_thread_from_its_bucket() {
    let mut e = env(r#"
        main() { thread t(); }
        t() { level endon("stop"); wait 1; println("late"); }
        stop() { level notify("stop"); }
    "#);
    e.call("main");
    e.call("stop");
    e.ticks(40);
    assert!(e.take().is_empty());
    assert_eq!(e.vm.thread_count(), 0);
}

#[test]
fn a_notifier_with_endon_for_the_same_event_ends_itself() {
    let mut e = env(r#"
        main() { thread n(); }
        n() { level endon("x"); thread w(); level notify("x"); println("unreachable"); }
        w() { level waittill("x"); println("waiter"); }
    "#);
    e.call("main");
    e.drain();
    assert_eq!(e.take(), ["waiter"]);
    assert_eq!(e.vm.thread_count(), 0);
}

#[test]
fn a_notifier_dying_in_an_inner_frame_resumes_its_caller_in_the_bucket() {
    let mut e = env(r#"
        main() { outer(); }
        outer() { inner(); println("outer resumed"); }
        inner() { level endon("x"); level notify("x"); println("unreachable"); }
    "#);
    // The thread was refiled in the current bucket, not continued.
    assert!(matches!(e.call("main"), CallOutcome::Pending));
    assert!(e.take().is_empty());
    e.drain();
    assert_eq!(e.take(), ["outer resumed"]);
}

#[test]
fn a_dead_frames_endon_does_not_outlive_it() {
    let mut e = env(r#"
        main() { thread t(); }
        t() { f(); level waittill("later"); println("alive"); }
        f() { level endon("x"); }
        fire() { level notify("x"); level notify("later"); }
    "#);
    e.call("main");
    e.call("fire");
    e.drain();
    assert_eq!(e.take(), ["alive"]);
}

#[test]
fn waittillframeend_runs_after_everything_else_in_the_bucket() {
    let mut e = env(r#"
        main() { thread s(); }
        s() { wait 0.05; thread fe("1"); thread w0(); thread fe("2"); println("s"); }
        fe(n) { println("fe" + n); waittillframeend; println("end" + n); }
        w0() { println("w0"); wait 0; println("w0 again"); }
    "#);
    e.call("main");
    e.ticks(1);
    assert!(e.take().is_empty());
    e.ticks(1);
    assert_eq!(
        e.take(),
        ["fe1", "w0", "fe2", "s", "w0 again", "end1", "end2"]
    );
}

#[test]
fn wait_zero_runs_in_the_same_drain_ahead_of_pending_threads() {
    let mut e = env(r#"
        main() { thread a(); thread b(); }
        a() { wait 0.05; println("a"); }
        b() { wait 0.05; println("b"); wait 0; println("b again"); }
    "#);
    e.call("main");
    e.ticks(2);
    // b runs first (last inserted), its wait 0 goes to the head, so it finishes before a.
    assert_eq!(e.take(), ["b", "b again", "a"]);
}

// ---- entity lifetime ----

#[test]
fn freeing_an_entity_discards_its_waiters_without_events() {
    let mut e = env(r#"
        main() { thread w(); thread u(); self delete(); }
        w() { self waittill("death"); println("w woke"); }
        u() { self endon("death"); self waittill("x"); println("u woke"); }
    "#);
    let p = ent(&mut e, 1);
    e.call_on("main", &p);
    // The free is deferred: nothing is cancelled until the tick ends.
    assert_eq!(e.vm.thread_count(), 2);
    e.ticks(1);
    assert_eq!(e.vm.thread_count(), 0);
    e.ticks(5);
    assert!(e.take().is_empty());
}

#[test]
fn threads_woken_before_the_free_still_run_and_a_reused_slot_is_a_new_object() {
    let mut e = env(r#"
        main() { thread w(); thread v(); }
        w() { self waittill("disconnect"); println("w ran"); }
        v() { self waittill("other"); println("v ran"); }
    "#);
    let p = ent(&mut e, 4);
    e.call_on("main", &p);
    e.vm.notify_entity(4, "disconnect", &[]);
    e.vm.free_entity(4);
    assert!(p.is_dead());
    e.ticks(1);
    assert_eq!(e.take(), ["w ran"]);
    assert_eq!(e.vm.thread_count(), 0);
    assert!(p.entity().is_none());
    let fresh = e.vm.entity(4, EntClass::Entity);
    assert!(fresh != p);
    assert!(fresh.entity().is_some());
}

#[test]
fn a_dead_entity_is_not_a_method_receiver() {
    let mut e = env(r#"
        main() { self delete(); }
        again() { self delete(); }
    "#);
    let p = ent(&mut e, 2);
    e.call_on("main", &p);
    let f = e.func("again");
    let err = e.vm.call(&mut e.host, f, Some(p.clone()), &[]).unwrap_err();
    assert_eq!(err.kind, VmErrorKind::Script);
    assert!(err.message.contains("is not an entity"), "{err}");
}

#[test]
fn engine_fields_go_through_the_host() {
    let mut e = env(r#"
        main() { self.origin = (1, 2, 3); self.origin += (1, 1, 1); self.tag = 5; println(self.origin[2], self.tag); }
    "#);
    let p = ent(&mut e, 3);
    e.call_on("main", &p);
    assert_eq!(e.take(), ["4 5"]);
    assert!(matches!(e.host.origins[&3], Value::Vector(v) if v == [2.0, 3.0, 4.0]));
}

// ---- limits ----

fn recursion_depth(src: &str) -> (usize, String) {
    let mut e = env(src);
    let f = e.func("main");
    let err = e.vm.call(&mut e.host, f, None, &[]).unwrap_err();
    (e.take().len(), err.message)
}

#[test]
fn the_call_depth_limit_is_thirty_one_frames() {
    let (printed, msg) = recursion_depth("main() { f(0); } f(n) { println(n); f(n + 1); }");
    // `main` is frame 1, so `f(29)` is frame 31 and its call is refused.
    assert_eq!(printed, 30);
    assert_eq!(
        msg,
        "script stack overflow (too many embedded function calls)"
    );
}

#[test]
fn thread_calls_count_against_the_depth_limit() {
    let (printed, msg) = recursion_depth("main() { f(0); } f(n) { println(n); thread f(n + 1); }");
    assert_eq!(printed, 30);
    assert_eq!(
        msg,
        "script stack overflow (too many embedded function calls)"
    );
}

#[test]
fn engine_calls_from_a_builtin_are_refused_beyond_depth_29() {
    // `spawn` re-enters the VM; `f(n)` is frame n + 1.
    let mut e = env(r#"
        main() { f(0); }
        f(n) { if (n == 27) spawn(::cb); else f(n + 1); }
        g(n) { if (n == 28) spawn(::cb); else g(n + 1); }
        main2() { g(0); }
        cb() {}
    "#);
    e.call("main");
    assert_eq!(e.host.spawned, ["ok"]);
    e.call("main2");
    assert_eq!(
        e.host.spawned[1],
        "script stack overflow (too many embedded function calls)"
    );
}

fn raw_program(code: Vec<u8>) -> Program {
    Program {
        files: vec![File {
            name: "a".into(),
            functions: 0..1,
        }],
        functions: vec![Function {
            name: "main".into(),
            file: 0,
            param_count: 0,
            local_count: 0,
            code,
            lines: Vec::new(),
        }],
        strings: Vec::new(),
        builtins: Builtins::new(),
    }
}

fn pushes(n: usize) -> Vec<u8> {
    let mut code = Vec::new();
    for _ in 0..n {
        code.push(Op::PushInt as u8);
        code.extend_from_slice(&0i32.to_le_bytes());
    }
    code.push(Op::ReturnUndefined as u8);
    code
}

#[test]
fn the_value_stack_holds_2046_entries() {
    let mut e = env_of(raw_program(pushes(gsc::vm::MAX_STACK)));
    assert!(matches!(
        e.call("main"),
        CallOutcome::Finished(Value::Undefined)
    ));

    let mut e = env_of(raw_program(pushes(gsc::vm::MAX_STACK + 1)));
    let f = e.func("main");
    let err = e.vm.call(&mut e.host, f, None, &[]).unwrap_err();
    assert_eq!(err.kind, VmErrorKind::Script);
    assert_eq!(err.message, "Internal script stack overflow");
    assert_eq!(e.vm.thread_count(), 0);
}

#[test]
fn the_stack_limit_is_shared_by_a_thread_and_the_caller_that_started_it() {
    // 1500 values wait in the caller while the thread it started pushes 1000 more.
    let mut p = raw_program(Vec::new());
    let mut main = pushes(1500);
    main.pop();
    main.extend_from_slice(&[Op::CallFunc as u8, 1]);
    main.extend_from_slice(&1u32.to_le_bytes());
    main.push(0);
    main.push(Op::ReturnUndefined as u8);
    p.functions[0].code = main;
    p.functions.push(Function {
        name: "t".into(),
        file: 0,
        param_count: 0,
        local_count: 0,
        code: pushes(1000),
        lines: Vec::new(),
    });
    p.files[0].functions = 0..2;
    let mut e = env_of(p);
    let f = e.func("main");
    let err = e.vm.call(&mut e.host, f, None, &[]).unwrap_err();
    assert_eq!(err.message, "Internal script stack overflow");
    assert_eq!(e.vm.thread_count(), 0);
}

#[test]
fn the_loop_guard_kills_a_thread_that_loops_without_waiting() {
    let mut e = env("main() { for (;;) {} }");
    e.vm.set_loop_timeout(std::time::Duration::from_millis(20));
    let f = e.func("main");
    let err = e.vm.call(&mut e.host, f, None, &[]).unwrap_err();
    assert_eq!(err.kind, VmErrorKind::Script);
    assert_eq!(err.message, "potential infinite loop in script");
    assert_eq!(e.vm.thread_count(), 0);
}

#[test]
fn the_loop_guard_only_warns_while_loading() {
    let mut e = env(r#"main() { for (i = 0; i < 4000000; i++) {} println("done"); }"#);
    e.vm.set_loop_timeout(std::time::Duration::from_millis(1));
    e.vm.set_loading(true);
    assert!(matches!(e.call("main"), CallOutcome::Finished(_)));
    assert_eq!(e.take(), ["done"]);
    assert!(e.vm.guard_warnings() > 0);
}

#[test]
fn a_dead_thread_chain_reports_the_frames_it_lost() {
    let mut e = env("main() { f(); } f() { g(); } g() { x = 1 / 0; }");
    let f = e.func("main");
    let err = e.vm.call(&mut e.host, f, None, &[]).unwrap_err();
    assert_eq!(err.message, "divide by 0");
    assert_eq!(err.trace, ["a::g line 1", "a::f line 1", "a::main line 1"]);
}

#[test]
fn a_runtime_error_in_a_started_thread_kills_the_whole_chain() {
    let mut e = env(r#"
        main() { println("a"); thread bad(); println("unreachable"); }
        bad() { x = undefined + 1; }
    "#);
    let f = e.func("main");
    let err = e.vm.call(&mut e.host, f, None, &[]).unwrap_err();
    assert!(err.message.contains("unmatching types"), "{err}");
    assert_eq!(e.take(), ["a"]);
    assert_eq!(e.vm.thread_count(), 0);
}

#[test]
fn a_drain_continues_after_a_failing_thread() {
    let mut e = env(r#"
        main() { thread bad(); thread good(); }
        bad() { wait 0.05; x = 1 / 0; }
        good() { wait 0.05; println("good"); }
    "#);
    e.call("main");
    e.ticks(1);
    let errs = e.vm.inc_time(&mut e.host);
    assert_eq!(errs.len(), 1);
    // `good` ran first (last inserted), `bad` second; both were dealt with.
    assert_eq!(e.take(), ["good"]);
    assert_eq!(e.vm.thread_count(), 0);
}

// ---- time ----

#[test]
fn waits_are_real_time_at_30_hz() {
    let log = run(r#"
        main() {
            println(gettime());
            wait 0.3; println(gettime());
            wait 1; println(gettime());
            wait 0.01; println(gettime());
            wait 0.034; println(gettime());
            wait 0.067; println(gettime());
            wait 0; println(gettime());
            wait 2; println(gettime());
        }
    "#);
    assert_eq!(log, ["0", "9", "39", "40", "41", "43", "43", "103"]);
}

#[test]
fn every_hundredth_of_a_second_converts_like_exact_arithmetic() {
    for k in 1..=3000u32 {
        let x = k as f32 / 100.0;
        let want = ((k * 3) / 10).max(1);
        assert_eq!(
            gsc::vm::wait_ticks(&Value::Float(x)),
            Ok(u64::from(want)),
            "wait {x}"
        );
    }
}

#[test]
fn bad_waits_are_errors() {
    for (v, msg) in [
        (Value::Int(-1), "negative wait is not allowed"),
        (Value::Float(-0.5), "negative wait is not allowed"),
        (Value::Int(600_000), "wait is too long"),
        (Value::str("x"), "string is not a number"),
    ] {
        assert_eq!(gsc::vm::wait_ticks(&v), Err(msg.to_string()));
    }
    assert_eq!(gsc::vm::wait_ticks(&Value::Int(558_000)), Ok(16_740_000));
}

#[test]
fn the_tick_wraps_at_24_bits() {
    let mut e = env(r#"main() { for (;;) { wait 100000; println(gettime()); } }"#);
    e.call("main");
    // 100000 s is 3,000,000 ticks: 5 rounds pass 2^24 = 16,777,216 and wrap.
    for _ in 0..16_777_216u32 {
        let errs = e.vm.inc_time(&mut e.host);
        assert!(errs.is_empty());
    }
    assert_eq!(e.vm.time(), 0);
    assert_eq!(
        e.take(),
        ["3000000", "6000000", "9000000", "12000000", "15000000"]
    );
    for _ in 0..=(18_000_000u32 - 16_777_216) {
        e.vm.inc_time(&mut e.host);
    }
    assert_eq!(e.take(), ["1222784"]);
}

// ---- language semantics the scheduler tests rely on ----

#[test]
fn arguments_evaluate_right_to_left() {
    let log = run(r#"
        main() { f(p("a"), p("b"), p("c")); }
        f(x, y, z) { println(x, y, z); }
        p(s) { println("eval", s); return s; }
    "#);
    assert_eq!(log, ["eval c", "eval b", "eval a", "a b c"]);
}

#[test]
fn missing_arguments_are_undefined_and_extra_ones_are_dropped() {
    let log = run(r#"
        main() { f(1); f(1, 2, 3); }
        f(a, b) { println(a, isdefined(b)); }
    "#);
    assert_eq!(log, ["1 0", "1 1"]);
}

#[test]
fn logical_operators_yield_integers_and_short_circuit() {
    let log = run(r#"
        main() {
            println("x" == "x" && 5);
            println(0 || "s" == "s");
            println(0 && boom());
            println(1 || boom());
        }
        boom() { println("boom"); }
    "#);
    assert_eq!(log, ["1", "1", "0", "1"]);
}

#[test]
fn arrays_are_values_and_structs_are_shared() {
    let log = run(r#"
        main() {
            a = []; a[0] = 1; b = a; b[0] = 2; println(a[0], b[0]);
            s = spawnstruct(); s.x = 1; t = s; t.x = 2; println(s.x);
            m = []; m["k"]["j"] = 5; println(m["k"]["j"], m.size);
            f(a); println(a[0]);
            a[0] += 10; println(a[0]);
            level.arr[2] = "z"; println(level.arr[2], level.arr.size);
        }
        f(x) { x[0] = 99; }
    "#);
    assert_eq!(log, ["1 2", "2", "5 1", "1", "11", "z 1"]);
}

#[test]
fn storing_undefined_removes_an_element_or_field() {
    let log = run(r#"
        main() {
            a = []; a["x"] = 1; a["y"] = 2; a["x"] = undefined; println(a.size);
            level.f = 1; level.f = undefined; println(isdefined(level.f));
        }
    "#);
    assert_eq!(log, ["1", "0"]);
}

#[test]
fn integer_division_yields_a_float_and_strings_concatenate() {
    let log = run(r#"
        main() {
            println(5 / 2, 6 / 3, 7 % 4, 1 << 4, -8 >> 1);
            println("a" + "b" + 1 + 1.5);
            println((1, 2, 3) + (1, 1, 1), (1, 2, 3) * 2, "ab"[1], "abc".size);
        }
    "#);
    assert_eq!(log, ["2.5 2 3 16 -4", "ab11.5", "(2, 3, 4) (2, 4, 6) b 3"]);
}

#[test]
fn comparisons_and_equality() {
    let log = run(r#"
        main() {
            println(1 == 1.0, 0.1 + 0.2 == 0.3, "a" == "A", (1, 2, 3) == (1, 2, 3));
            println(1 < 2, 2.5 >= 2, "x" != "y", level == level, level == spawnstruct());
        }
    "#);
    assert_eq!(log, ["1 1 0 1", "1 1 1 1 0"]);
}

#[test]
fn mismatched_types_are_runtime_errors() {
    for (src, msg) in [
        ("main() { x = \"a\" < \"b\"; }", "unmatching types"),
        ("main() { x = 1 == \"a\"; }", "unmatching types"),
        ("main() { x = 1 / 0; }", "divide by 0"),
        ("main() { x = 1 % 0; }", "divide by 0"),
        (
            "main() { if (undefined) {} }",
            "cannot cast undefined to bool",
        ),
        ("main() { x = ~1.5; }", "~ cannot be applied"),
        (
            "main() { x = (1, 2, 3)[5]; }",
            "vector index 5 out of range",
        ),
        ("main() { x = 5; x[0] = 1; }", "int is not an array"),
        (
            "main() { x = undefined; x.a = 1; }",
            "undefined is not a field object",
        ),
    ] {
        let mut e = env(src);
        let f = e.func("main");
        let err = e.vm.call(&mut e.host, f, None, &[]).unwrap_err();
        assert!(err.message.contains(msg), "{src}: {err}");
    }
}

#[test]
fn function_pointers_and_methods() {
    let log = run(r#"
        main() {
            f = ::greet;
            [[f]]("one");
            s = spawnstruct(); s.name = "S";
            s [[f]]("two");
            s thread greet("three");
        }
        greet(x) { println(self.name, x); }
    "#);
    // `main` runs with `level` as self, whose `name` is not set.
    assert_eq!(log, ["undefined one", "S two", "S three"]);
}

#[test]
fn switch_loops_and_locals() {
    let log = run(r#"
        main() {
            for (i = 0; i < 5; i++) {
                switch (i) {
                    case 1: println("one"); break;
                    case 3: continue;
                    default: println("d" + i);
                }
            }
            n = 0;
            while (n < 3) n++;
            println(n);
        }
    "#);
    assert_eq!(log, ["d0", "one", "d2", "d4", "3"]);
}
