// SPDX-License-Identifier: GPL-3.0-only
//! Runs the real binary: the watchdog must survive each way a child can fail
//! and still write a readable bundle. Needs no install.
use harness::diff;
use harness::stage::Status;
use std::path::Path;
use std::process::Command;

/// A bundle's `summary.json`.
fn summary(zip: &Path) -> serde_json::Value {
    let mut z = zip::ZipArchive::new(std::fs::File::open(zip).unwrap()).unwrap();
    serde_json::from_reader(z.by_name("summary.json").unwrap()).unwrap()
}

fn run(out: &Path, args: &[&str]) -> (i32, diff::Bundle, std::path::PathBuf) {
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
    (st.code().unwrap(), diff::open(&zip).unwrap(), zip)
}

#[test]
fn no_install_still_writes_a_bundle_that_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let (code, b, _) = run(dir.path(), &["--cod4", "/definitely/not/here"]);
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
    // Nothing else fails because the install is missing: stages that need it are skipped, and the
    // ones that do not (net-loopback) still run.
    assert!(b.stages[1..].iter().all(|s| s.status != Status::Failed));
}

#[test]
fn a_stage_that_logs_an_unreviewed_warning_does_not_pass() {
    let dir = tempfile::tempdir().unwrap();
    let (code, b, _) = run(dir.path(), &["--stage", "selftest-warn"]);
    assert_eq!(code, 1);
    assert_eq!(b.stages[0].status, Status::Failed);
    let why = b.stages[0].reason.as_deref().unwrap();
    assert!(why.contains("injected an unexpected warning"), "{why}");
    // The warning is in the stage's own log file in the bundle.
    assert!(
        b.logs
            .iter()
            .any(|(n, t)| n.contains("logs/") && t.contains("injected"))
    );
}

#[test]
fn crash_hang_and_panic_are_caught_and_the_suite_continues() {
    let dir = tempfile::tempdir().unwrap();
    let (code, b, _) = run(
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
    let cmd = script.to_str().unwrap();
    let (code, b, zip) = run(dir.path(), &["--script", cmd]);
    // `map` does not exist, so the script is skipped as a whole after echo/wait ran; a skip nothing allows
    // fails the run (#375).
    assert_eq!(code, 1);
    assert_eq!(b.stages[0].name, "script:mine");
    assert_eq!(b.stages[0].status, Status::Skipped);
    assert_eq!(b.stages[0].commands.len(), 4);
    assert!(
        summary(&zip)["unexpected_skips"]
            .as_array()
            .is_some_and(|a| a.iter().any(|n| n == "script:mine")),
        "the skip is not reported as unexpected: {}",
        summary(&zip)
    );
    // Named on the command line, the same skip passes.
    let allowed = tempfile::tempdir().unwrap();
    let (code, b, zip) = run(
        allowed.path(),
        &["--script", cmd, "--allow-skip", "script:mine"],
    );
    assert_eq!(code, 0, "{:?}", b.stages[0].reason);
    assert_eq!(b.stages[0].status, Status::Skipped);
    assert!(
        summary(&zip)["allowed_skips"]
            .as_array()
            .is_some_and(|a| a.iter().any(|n| n == "script:mine")),
        "--allow-skip did not record the skip: {}",
        summary(&zip)
    );
}
