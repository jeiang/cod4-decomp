// SPDX-License-Identifier: GPL-3.0-or-later
//! Runs the real binary: the watchdog must survive each way a child can fail
//! and still write a readable bundle. Needs no install.
use harness::diff;
use harness::stage::Status;
use std::path::Path;
use std::process::Command;

fn run(out: &Path, args: &[&str]) -> (i32, diff::Bundle) {
    let st = Command::new(env!("CARGO_BIN_EXE_cod4e-harness"))
        .args(["--no-prompt", "--out"])
        .arg(out)
        .args(args)
        .env_remove("COD4_PATH")
        // No Steam library of the machine running the test may be found.
        .env("HOME", out)
        .env("USERPROFILE", out)
        .current_dir(out)
        .status()
        .unwrap();
    let zip = std::fs::read_dir(out)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "zip"))
        .expect("a bundle was written");
    (st.code().unwrap(), diff::open(&zip).unwrap())
}

#[test]
fn no_install_still_writes_a_bundle_that_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let (code, b) = run(dir.path(), &["--cod4", "/definitely/not/here"]);
    assert_eq!(code, 1);
    assert_eq!(b.stages[0].name, "asset-load");
    assert_eq!(b.stages[0].status, Status::Failed);
    assert_eq!(b.stages[0].reason.as_deref(), Some("install not found"));
    assert!(
        b.logs
            .iter()
            .any(|(n, t)| n == "harness.log" && t.contains("install not found"))
    );
    assert_eq!(b.manifest["install"]["found"], false);
    // The rest of the suite is reported skipped, not run.
    assert!(b.stages[1..].iter().all(|s| s.status == Status::Skipped));
}

#[test]
fn crash_hang_and_panic_are_caught_and_the_suite_continues() {
    let dir = tempfile::tempdir().unwrap();
    let (code, b) = run(
        dir.path(),
        &[
            "--stage",
            "selftest-crash,selftest-hang,selftest-panic,selftest-panic",
        ],
    );
    assert_eq!(code, 1);
    let st: Vec<_> = b.stages.iter().map(|s| s.status).collect();
    assert_eq!(
        st,
        [
            Status::Crashed,
            Status::Hung,
            Status::Failed,
            Status::Failed
        ]
    );
    assert!(
        b.stages[0].files.iter().any(|f| f.ends_with("crash.dmp")),
        "no minidump: {:?} {:?}",
        b.stages[0],
        b.manifest["crash_capture"]
    );
    assert!(b.stages[2].reason.as_deref().unwrap().contains("panicked"));
    assert!(b.logs.iter().any(|(_, t)| t.contains("selftest panic")));
}

#[test]
fn script_scenario_runs_and_skips_missing_commands() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("mine.cfg");
    std::fs::write(&script, "echo hello; wait 10ms; map mp_crash; wait 1h").unwrap();
    let (code, b) = run(dir.path(), &["--script", script.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert_eq!(b.stages[0].name, "script:mine");
    // `map` does not exist yet, so the script is skipped as a whole after echo/wait ran.
    assert_eq!(b.stages[0].status, Status::Skipped);
    assert_eq!(b.stages[0].commands.len(), 4);
}
