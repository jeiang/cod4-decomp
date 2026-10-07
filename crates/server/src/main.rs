// SPDX-License-Identifier: GPL-3.0-or-later
//! `cod4e-server`: the headless dedicated server.
//!
//! `cod4e-server [+set name value] [+exec server.cfg] [+map mp_crash]`; the install comes from
//! `COD4_PATH` (default `./COD4`).

use server::server::Server;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let root = PathBuf::from(std::env::var_os("COD4_PATH").unwrap_or_else(|| "COD4".into()));
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "usage: cod4e-server [+set name value] [+exec file.cfg] [+map name]\ninstall: $COD4_PATH (default ./COD4)"
        );
        return ExitCode::SUCCESS;
    }
    let mut s = match Server::boot(&root, &args, true) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    s.run_forever();
    ExitCode::SUCCESS
}
