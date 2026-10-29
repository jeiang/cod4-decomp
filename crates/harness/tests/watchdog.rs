// SPDX-License-Identifier: GPL-3.0-only
//! Runs the real binary: the watchdog must survive each way a child can fail
//! and still write a readable bundle. Needs no install.
use harness::diff;
use harness::stage::Status;
use std::path::Path;
use std::process::Command;

/// A run's exit status, its console output, the bundle and the bundle's path.
fn run_capture(out: &Path, args: &[&str]) -> (i32, String, diff::Bundle, std::path::PathBuf) {
    let cmd = Command::new(env!("CARGO_BIN_EXE_cod4e-harness"))
        .args(["--no-prompt", "--out"])
        .arg(out)
        .args(args)
        .env_remove("COD4_PATH")
        // No Steam library of the machine running the test may be found.
        .env("HOME", out)
        .env("USERPROFILE", out)
        .current_dir(out)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&cmd.stdout).into_owned();
    let zip = std::fs::read_dir(out)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "zip"))
        .expect("a bundle was written");
    (
        cmd.status.code().unwrap(),
        stdout,
        diff::open(&zip).unwrap(),
        zip,
    )
}

fn run(out: &Path, args: &[&str]) -> (i32, diff::Bundle, std::path::PathBuf) {
    let (code, _, b, zip) = run_capture(out, args);
    (code, b, zip)
}

/// A bundle's `summary.json`.
fn summary(zip: &Path) -> serde_json::Value {
    let mut z = zip::ZipArchive::new(std::fs::File::open(zip).unwrap()).unwrap();
    serde_json::from_reader(z.by_name("summary.json").unwrap()).unwrap()
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
fn script_scenario_fails_commands_the_console_lacks() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("mine.cfg");
    std::fs::write(&script, "echo hello; wait 10ms; map mp_crash; wait 1h").unwrap();
    let cmd = script.to_str().unwrap();
    let (code, b, _) = run(dir.path(), &["--script", cmd]);
    // No engine answers `--script`, so `map` is an unknown command: the scenario fails rather than passing on
    // the lines that did run, and --allow-skip cannot turn a failure into a pass (#375).
    assert_eq!(code, 1);
    assert_eq!(b.stages[0].name, "script:mine");
    assert_eq!(b.stages[0].status, Status::Failed);
    assert!(
        b.stages[0].reason.as_deref().unwrap_or("").contains("map"),
        "{:?}",
        b.stages[0].reason
    );
    assert_eq!(b.stages[0].commands.len(), 4);
    let allowed = tempfile::tempdir().unwrap();
    let (code, b, _) = run(
        allowed.path(),
        &["--script", cmd, "--allow-skip", "script:mine"],
    );
    assert_eq!(code, 1);
    assert_eq!(b.stages[0].status, Status::Failed);
}

#[test]
fn a_stage_that_cannot_run_fails_the_suite_and_naming_it_accepts_it() {
    let dir = tempfile::tempdir().unwrap();
    let (code, stdout, b, zip) = run_capture(
        dir.path(),
        &[
            "--stage",
            "client-flythrough",
            "--cod4",
            "/definitely/not/here",
        ],
    );
    assert_eq!(code, 1, "{stdout}");
    assert_eq!(b.stages[0].name, "client-flythrough");
    assert_eq!(b.stages[0].status, Status::Skipped);
    let s = summary(&zip);
    assert_eq!(s["ok"], serde_json::Value::Bool(false));
    assert_eq!(
        s["unexpected_skips"],
        serde_json::json!(["client-flythrough"])
    );
    assert_eq!(s["allowed_skips"], serde_json::json!([]));
    // The failure line says how to accept the skip.
    assert!(
        stdout.contains("--allow-skip client-flythrough"),
        "no way to accept the skip in: {stdout}"
    );

    // The same run, with the skip named, passes and records it apart from the stages that ran.
    let allowed = tempfile::tempdir().unwrap();
    let (code, stdout, b, zip) = run_capture(
        allowed.path(),
        &[
            "--stage",
            "client-flythrough",
            "--cod4",
            "/definitely/not/here",
            "--allow-skip",
            "client-flythrough",
        ],
    );
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(b.stages[0].status, Status::Skipped);
    let s = summary(&zip);
    assert_eq!(s["ok"], serde_json::Value::Bool(true));
    assert_eq!(s["allowed_skips"], serde_json::json!(["client-flythrough"]));
    assert_eq!(s["unexpected_skips"], serde_json::json!([]));
    assert!(
        stdout.contains("skipped as --allow-skip named them: client-flythrough"),
        "the summary did not report the accepted skip in: {stdout}"
    );
}

#[test]
fn an_allow_skip_that_names_no_stage_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cod4e-harness"))
        .args(["--no-prompt", "--out"])
        .arg(dir.path())
        .args(["--allow-skip", "nosuchstage"])
        .env_remove("COD4_PATH")
        .env("HOME", dir.path())
        .env("USERPROFILE", dir.path())
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("nosuchstage"), "{err}");
    assert!(err.contains("--allow-skip"), "{err}");
}
