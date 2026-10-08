// SPDX-License-Identifier: GPL-3.0-only
//! `cod4e-server`: the headless dedicated server.
//!
//! `cod4e-server [+set name value] [+exec server.cfg] [+map mp_crash]`; the install comes from
//! `COD4_PATH` (default `./COD4`). The `--webtransport` family of flags turns on the endpoint for
//! browser clients; each is shorthand for setting a cvar. Lines on stdin run as console commands;
//! with `rcon_password` set, `rcon` packets do the same from the network.

use server::server::Server;
use std::path::PathBuf;
use std::process::ExitCode;

/// `--flag value` options and the cvar each sets.
const FLAGS: [(&str, &str); 4] = [
    ("--webtransport", "net_wt"),
    ("--wt-cert", "net_wt_cert"),
    ("--wt-key", "net_wt_key"),
    ("--wt-info", "net_wt_info"),
];

/// Turns the flags into leading `+set cvar value` pairs, leaving the other arguments.
fn expand_flags(args: Vec<String>) -> Result<Vec<String>, String> {
    let mut sets = Vec::new();
    let mut rest = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) => (n.to_owned(), Some(v.to_owned())),
            None => (a.clone(), None),
        };
        match FLAGS.iter().find(|(f, _)| *f == name) {
            Some((f, cvar)) => {
                let v = inline
                    .or_else(|| it.next())
                    .ok_or_else(|| format!("{f} needs a value"))?;
                sets.extend(["+set".to_owned(), (*cvar).to_owned(), v]);
            }
            None => rest.push(a),
        }
    }
    sets.extend(rest);
    Ok(sets)
}

fn main() -> ExitCode {
    let root = PathBuf::from(std::env::var_os("COD4_PATH").unwrap_or_else(|| "COD4".into()));
    let args = match expand_flags(std::env::args().skip(1).collect()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "usage: cod4e-server [+set name value] [+exec file.cfg] [+map name]\n\
             \x20      [--webtransport [host:]port [--wt-cert cert.pem --wt-key key.pem] [--wt-info info.json]]\n\
             install: $COD4_PATH (default ./COD4)"
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
    // Whatever is typed on the terminal runs as a console command (`status`, `kick name`, `map x`).
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    s.attach_console(rx);
    s.run_forever();
    ExitCode::SUCCESS
}
