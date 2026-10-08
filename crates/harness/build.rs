// SPDX-License-Identifier: GPL-3.0-only
//! Embeds the git revision so a bundle identifies the build that produced it.
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn main() {
    println!("cargo:rerun-if-env-changed=COD4E_BUILD_HASH");
    for p in ["HEAD", "logs/HEAD", "index"] {
        if let Some(path) = git(&["rev-parse", "--git-path", p]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let hash = std::env::var("COD4E_BUILD_HASH").ok().unwrap_or_else(|| {
        let rev = git(&["rev-parse", "--short=12", "HEAD"]);
        match rev {
            Some(rev) if git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty()) => {
                format!("{rev}-dirty")
            }
            Some(rev) => rev,
            None => "unknown".to_owned(),
        }
    });
    println!("cargo:rustc-env=COD4E_BUILD_HASH={hash}");
    println!(
        "cargo:rustc-env=COD4E_BUILD_PROFILE={}",
        std::env::var("PROFILE").unwrap_or_default()
    );
    println!(
        "cargo:rustc-env=COD4E_BUILD_TARGET={}",
        std::env::var("TARGET").unwrap_or_default()
    );
}
