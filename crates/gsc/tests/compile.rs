// SPDX-License-Identifier: GPL-3.0-or-later
use gsc::{Builtins, ErrorKind, Options, Program, compile};

fn build(sources: &[(&str, &str)]) -> Result<Program, Vec<gsc::CompileError>> {
    compile(sources, &Builtins::stock_mp(), Options::default())
}

fn ok(src: &str) -> Program {
    build(&[("a.gsc", src)]).unwrap_or_else(|e| panic!("{e:?}"))
}

fn dis(p: &Program, name: &str) -> Vec<String> {
    let id = p.find("a", name).unwrap();
    p.functions[id as usize]
        .disassemble(p)
        .into_iter()
        // Drop the offset column so tests do not depend on instruction sizes.
        .map(|l| l.split_once(' ').unwrap().1.to_string())
        .collect()
}

fn err(sources: &[(&str, &str)]) -> gsc::CompileError {
    build(sources).unwrap_err().remove(0)
}

#[test]
fn arguments_are_pushed_last_first_and_object_on_top() {
    let p = ok("f(a, b) { self g(1, 2); } g(x, y) {}");
    assert_eq!(
        dis(&p, "f"),
        [
            "PushInt 2",
            "PushInt 1",
            "PushSelf",
            "CallFunc 4 1 2",
            "Pop",
            "ReturnUndefined"
        ]
    );
}

#[test]
fn thread_forms_set_flags() {
    let p = ok("f() { thread g(); level childthread g(); } g() {}");
    let d = dis(&p, "f");
    assert_eq!(d[0], "CallFunc 1 1 0");
    assert_eq!(d[3], "CallFunc 7 1 0");
}

#[test]
fn identifiers_are_case_insensitive_but_strings_are_not() {
    let p = ok("Main() { X = \"Hello\"; y = x; } ");
    assert_eq!(p.find("a", "MAIN"), p.find("a", "main"));
    let d = dis(&p, "main");
    assert_eq!(d[0], "PushStr 0 \"Hello\"");
    assert_eq!(d[2], "GetLocal 0");
}

#[test]
fn includes_resolve_after_own_functions() {
    let p = build(&[
        (
            "maps/mp/b.gsc",
            "helper() { return 1; } shared() { return 2; }",
        ),
        (
            "a.gsc",
            "#include maps\\mp\\b; shared() { return 3; } f() { helper(); shared(); }",
        ),
    ])
    .unwrap();
    let f = p.find("a", "f").unwrap();
    let code = p.functions[f as usize].disassemble(&p);
    let helper = p.find("maps/mp/b", "helper").unwrap();
    let shared = p.find("a", "shared").unwrap();
    assert!(code[0].ends_with(&format!("CallFunc 0 {helper} 0")));
    assert!(code[2].ends_with(&format!("CallFunc 0 {shared} 0")));
}

#[test]
fn qualified_call_and_function_pointer() {
    let p = build(&[
        ("maps/mp/b.gsc", "g() {}"),
        ("a.gsc", "f() { maps\\mp\\b::g(); p = ::local; p2 = maps\\mp\\b::g; [[ p ]](1); self [ [ p ] ](); } local() {}"),
    ])
    .unwrap();
    let d = dis(&p, "f");
    assert!(d.contains(&"CallPtr 0 1".to_string()));
    assert!(d.contains(&"CallPtr 4 0".to_string()));
    assert_eq!(d.iter().filter(|l| l.starts_with("PushFuncPtr")).count(), 2);
}

#[test]
fn builtins_bind_by_index_and_methods_need_an_object() {
    let b = Builtins::stock_mp();
    let p = ok("f() { isdefined(x); self giveweapon(\"ak47_mp\"); }");
    let d = dis(&p, "f");
    assert_eq!(
        d[1],
        format!("CallBuiltin {} 1", b.function("isdefined").unwrap())
    );
    assert_eq!(
        d[5],
        format!("CallBuiltinMethod {} 1", b.method("giveweapon").unwrap())
    );
    // `spawn` is both a function and a method.
    assert!(b.function("spawn").is_some() && b.method("spawn").is_some());
    let e = err(&[("a.gsc", "f() { giveweapon(\"x\"); }")]);
    assert_eq!(
        (e.kind, e.message.as_str()),
        (ErrorKind::UnknownFunction, "unknown function `giveweapon`")
    );
}

#[test]
fn unknown_names_are_errors_listing_the_name() {
    let e = err(&[("a.gsc", "f() {\n\n nosuchthing(1); }")]);
    assert_eq!(e.kind, ErrorKind::UnknownFunction);
    assert!(e.message.contains("nosuchthing"));
    assert_eq!((e.file.as_str(), e.line), ("a", 3));
    let e = err(&[("a.gsc", "#include nofile; f() {}")]);
    assert_eq!(e.kind, ErrorKind::UnknownFile);
    let e = err(&[("a.gsc", "f() { nofile::g(); }")]);
    assert_eq!(e.kind, ErrorKind::UnknownFile);
}

#[test]
fn custom_builtin_tables_are_honored() {
    let mut b = Builtins::new();
    let i = b.add_function("Custom");
    assert_eq!(b.add_function("custom"), i);
    let p = compile(&[("a.gsc", "f() { custom(); }")], &b, Options::default()).unwrap();
    assert_eq!(dis(&p, "f")[0], format!("CallBuiltin {i} 0"));
}

#[test]
fn prof_statements_are_dropped_and_not_builtins() {
    let p = ok("f() { prof_begin(\" x\"); prof_end(\" x\"); }");
    assert_eq!(dis(&p, "f"), ["ReturnUndefined"]);
    assert!(Builtins::stock_mp().function("prof_begin").is_none());
}

#[test]
fn developer_blocks_compile_only_in_developer_mode() {
    let src = "f() { /# nosuchbuiltin(); #/ }";
    let p = ok(src);
    assert_eq!(dis(&p, "f"), ["ReturnUndefined"]);
    let e = compile(
        &[("a.gsc", src)],
        &Builtins::stock_mp(),
        Options { developer: true },
    )
    .unwrap_err();
    assert_eq!(e[0].kind, ErrorKind::UnknownFunction);
}

#[test]
fn events_compile_to_stack_forms() {
    let p = ok(
        "f() { self endon(\"d\"); level waittill(\"e\", a, b); self notify(\"n\", 1, 2); wait 0.05; waittillframeend; }",
    );
    assert_eq!(
        dis(&p, "f"),
        [
            "PushStr 0 \"d\"",
            "PushSelf",
            "Endon",
            "PushStr 1 \"e\"",
            "PushLevel",
            "Waittill 2",
            "SetLocal 0",
            "SetLocal 1",
            "PushInt 2",
            "PushInt 1",
            "PushStr 2 \"n\"",
            "PushSelf",
            "Notify 2",
            "PushFloat 0.05",
            "Wait",
            "WaittillFrameEnd",
            "ReturnUndefined",
        ]
    );
}

#[test]
fn assignments_to_fields_and_arrays_use_references() {
    let p = ok("f() { level.a[\"k\"] = 1; self.n += 2; game[\"x\"]++; }");
    assert_eq!(
        dis(&p, "f"),
        [
            "PushInt 1",
            "PushLevel",
            "RefField 0 \"a\"",
            "PushStr 1 \"k\"",
            "RefIndex",
            "Store",
            "PushSelf",
            "RefField 2 \"n\"",
            "LoadRef",
            "PushInt 2",
            "Add",
            "Swap",
            "Store",
            "RefGame",
            "PushStr 3 \"x\"",
            "RefIndex",
            "LoadRef",
            "PushInt 1",
            "Add",
            "Swap",
            "Store",
            "ReturnUndefined",
        ]
    );
}

#[test]
fn logical_operators_short_circuit_to_ints() {
    let p = ok("f(a, b) { return a && b; }");
    let d = dis(&p, "f");
    // The jump lands on `Return`, past `ToBool`.
    let end = p.functions[0].code.len() - 2;
    assert_eq!(
        d[..4],
        [
            "GetLocal 0".to_string(),
            format!("AndJump ->{end}"),
            "GetLocal 1".into(),
            "ToBool".into()
        ]
    );
}

#[test]
fn loops_jump_back_and_break_continue_patch() {
    let p = ok("f() { for (i = 0; i < 3; i++) { if (i == 1) continue; if (i == 2) break; } }");
    let code = &p.functions[0].code;
    let d = dis(&p, "f");
    let back = d.iter().filter(|l| l.starts_with("Jump ->")).count();
    assert!(back >= 3);
    // The loop's closing jump goes backwards.
    let jumps_back = d.iter().any(|l| {
        l.strip_prefix("Jump ->")
            .is_some_and(|t| t.parse::<usize>().unwrap() < code.len() / 2)
    });
    assert!(jumps_back);
}

#[test]
fn switch_dispatches_on_constants_with_default_and_fallthrough() {
    let p =
        ok("f(x) { switch (x) { case 1: case \"a\": y = 1; break; default: y = 2; } return y; }");
    let d = dis(&p, "f");
    assert_eq!(d.iter().filter(|l| *l == "Eq").count(), 2);
    assert_eq!(d.iter().filter(|l| l.starts_with("JumpIfTrue")).count(), 2);
    let e = err(&[("a.gsc", "f(x, z) { switch (x) { case z: break; } }")]);
    assert_eq!(e.kind, ErrorKind::Semantic);
}

#[test]
fn negative_literals_fold_and_big_ints_become_floats() {
    let p = ok("f() { a = -5; b = -0.5; c = 100000000000; d = 0xFF; }");
    let d = dis(&p, "f");
    assert_eq!(d[0], "PushInt -5");
    assert_eq!(d[2], "PushFloat -0.5");
    assert_eq!(d[4], "PushFloat 100000000000");
    assert_eq!(d[6], "PushInt 255");
}

#[test]
fn vectors_localized_strings_and_animtrees() {
    let p = ok(
        "#using_animtree(\"mp\"); f() { v = (1, 2, 3); s = &\"LOC\"; a = %idle; t = #animtree; e = []; }",
    );
    let d = dis(&p, "f");
    assert_eq!(
        &d[..4],
        ["PushInt 3", "PushInt 2", "PushInt 1", "PushVector"]
    );
    assert!(d.iter().any(|l| l.starts_with("PushLocStr")));
    assert!(d.iter().any(|l| l.starts_with("PushAnimRef")));
    assert!(d.iter().any(|l| l.starts_with("PushAnimTree")));
    assert!(d.contains(&"PushEmptyArray".to_string()));
    let e = err(&[("a.gsc", "f() { a = %idle; }")]);
    assert_eq!(e.kind, ErrorKind::Semantic);
}

#[test]
fn syntax_errors_report_file_and_position() {
    let e = err(&[("maps/x.gsc", "f() {\n  a = ;\n}")]);
    assert_eq!(
        (e.kind, e.file.as_str(), e.line),
        (ErrorKind::Syntax, "maps\\x", 2)
    );
    assert_eq!(err(&[("a.gsc", "f() { 1 + 2; }")]).kind, ErrorKind::Syntax);
    assert_eq!(err(&[("a.gsc", "f() {} F() {}")]).kind, ErrorKind::Semantic);
    assert_eq!(
        err(&[("a.gsc", "f() { break; }")]).kind,
        ErrorKind::Semantic
    );
}

#[test]
fn comments_and_line_endings() {
    let p = ok("// c\r\n/* x\r\n y */ f() { // d\r\n return 1; }\r\n");
    assert_eq!(dis(&p, "f"), ["PushInt 1", "Return", "ReturnUndefined"]);
    assert_eq!(p.functions[0].line_at(0), 4);
}

#[test]
fn stock_inventory_has_295_builtins_plus_two_statements() {
    let b = Builtins::stock_mp();
    let both = b
        .function_names()
        .iter()
        .filter(|n| b.method(n).is_some())
        .count();
    assert_eq!(both, 5);
    assert_eq!(
        b.function_names().len() + b.method_names().len() - both,
        295
    );
}
