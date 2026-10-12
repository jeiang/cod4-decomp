// SPDX-License-Identifier: GPL-3.0-only
//! Which files of an install the browser page reads, per language: the page's rules (`web/install.js`) and the
//! development server's index of an install (`web/serve.py`). Each skips when its interpreter is not installed.
use std::path::{Path, PathBuf};
use std::process::Command;

fn web() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web")
}

fn has(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn the_page_reads_the_installs_language() {
    if !has("node") {
        eprintln!("skipped: no node");
        return;
    }
    let out = Command::new("node")
        .arg("--test")
        .arg(web().join("install.test.mjs"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_dev_server_lists_a_non_english_install_by_its_language() {
    if !has("python3") {
        eprintln!("skipped: no python3");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for f in [
        "main/iw_00.iwd",
        "zone/german/ui_mp.ff",
        "zone/german/mp_crash.ff",
        "zone/english/ui_mp.ff",
    ] {
        let p = dir.path().join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    }
    std::fs::write(dir.path().join("localization.txt"), "german\r\n").unwrap();
    let out = Command::new("python3")
        .current_dir(web())
        .args([
            "-c",
            "import json, sys; from pathlib import Path; import serve; \
             print(json.dumps([e['path'] for e in serve.install_index(Path(sys.argv[1]))]))",
        ])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let paths: Vec<String> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        paths,
        [
            "localization.txt",
            "main/iw_00.iwd",
            "zone/german/mp_crash.ff",
            "zone/german/ui_mp.ff"
        ]
    );
}
