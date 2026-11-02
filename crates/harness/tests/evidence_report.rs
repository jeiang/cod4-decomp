// SPDX-License-Identifier: GPL-3.0-only
//! `scripts/evidence-report.py` (`just evidence-report`): its checks against a throwaway git repo and a recorded
//! GitHub. Skips when python3 or git is not installed.
use std::path::Path;
use std::process::Command;

fn has(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn the_evidence_report_reads_bundles_against_the_pr_head() {
    if !has("python3") || !has("git") {
        eprintln!("skipped: no python3 or git");
        return;
    }
    let scripts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts");
    let out = Command::new("python3")
        .current_dir(&scripts)
        .args(["-m", "unittest", "test_evidence_report"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
