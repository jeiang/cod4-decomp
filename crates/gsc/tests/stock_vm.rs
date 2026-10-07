// SPDX-License-Identifier: GPL-3.0-or-later
//! Loads every stock MP script into the VM and runs each file's `main` with stub builtins.
//! Skips without `COD4_PATH`.
//!
//! The stubs answer with defaults (empty arrays, empty strings, zeros, `undefined`), so scripts
//! soon hit a script runtime error; those are counted and are fine. A VM fault is not.

mod common;

use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Instant;

use gsc::{Array, Builtins, EntRef, Host, Obj, Options, Value, Vm, VmErrorKind, compile};

use common::stock_mp_scripts;

struct Stub {
    functions: Vec<String>,
    calls: u64,
}

impl Host for Stub {
    fn call_function(&mut self, vm: &mut Vm, index: u16, args: &[Value]) -> Result<Value, String> {
        self.calls += 1;
        Ok(match self.functions[usize::from(index)].as_str() {
            "isdefined" => Value::Int(i32::from(!matches!(args[0], Value::Undefined))),
            "spawnstruct" => Value::Object(Obj::new_struct()),
            "gettime" => Value::Int(vm.time() as i32 * 33),
            "getdvar" | "getdvarint" | "getdvarfloat" if args.len() > 1 => args[1].clone(),
            "getdvar" => Value::str(""),
            "getdvarint" => Value::Int(0),
            "getdvarfloat" => Value::Float(0.0),
            "randomint" | "randomintrange" => Value::Int(0),
            "randomfloat" | "randomfloatrange" => Value::Float(0.0),
            "getentarray"
            | "getnodearray"
            | "getvehiclenodearray"
            | "getarraykeys"
            | "getweaponarray" => Value::Array(Rc::new(Array::new())),
            _ => Value::Undefined,
        })
    }

    fn call_method(
        &mut self,
        _vm: &mut Vm,
        _index: u16,
        _ent: EntRef,
        _args: &[Value],
    ) -> Result<Value, String> {
        self.calls += 1;
        Ok(Value::Undefined)
    }
}

#[test]
fn stock_scripts_load_and_run_without_vm_faults() {
    let Some(scripts) = stock_mp_scripts() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    assert_eq!(scripts.len(), 210);
    let sources: Vec<(&str, &str)> = scripts
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    let prog = compile(&sources, &Builtins::stock_mp(), Options::default()).unwrap();
    let mut host = Stub {
        functions: prog
            .builtins
            .function_names()
            .iter()
            .map(|s| s.to_string())
            .collect(),
        calls: 0,
    };
    let mains: Vec<(String, u32)> = prog
        .files
        .iter()
        .filter_map(|f| prog.find(&f.name, "main").map(|id| (f.name.clone(), id)))
        .collect();
    let files = prog.files.len();
    let t = Instant::now();
    let mut vm = Vm::new(prog).expect("every stock function passes bytecode verification");
    eprintln!("{files} files loaded into the VM in {:?}", t.elapsed());

    let mut script_errors: BTreeMap<String, u32> = BTreeMap::new();
    let mut faults = Vec::new();
    let mut note = |e: gsc::VmError| match e.kind {
        VmErrorKind::Fault => faults.push(e.to_string()),
        VmErrorKind::Script => {
            // Group by message with numbers and quoted names stripped.
            let key: String = e
                .message
                .chars()
                .take_while(|c| *c != '\'' && *c != '"')
                .collect();
            *script_errors.entry(key).or_default() += 1;
        }
    };
    let t = Instant::now();
    for (name, id) in &mains {
        if let Err(e) = vm.call(&mut host, *id, None, &[]) {
            note(e);
        }
        for _ in 0..40 {
            for e in vm.inc_time(&mut host) {
                note(e);
            }
        }
        let _ = name;
    }
    let dt = t.elapsed();
    let total: u32 = script_errors.values().sum();
    eprintln!(
        "{} mains, {} ops, {} builtin calls in {dt:?}; {total} script runtime errors, {} faults; {} threads still parked",
        mains.len(),
        vm.ops_executed(),
        host.calls,
        faults.len(),
        vm.thread_count()
    );
    for (k, n) in &script_errors {
        eprintln!("  {n:5} x {k}");
    }
    assert!(faults.is_empty(), "{faults:#?}");
    assert!(!mains.is_empty());
}
