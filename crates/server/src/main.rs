// SPDX-License-Identifier: GPL-3.0-only
//! `cod4e-server`: the headless dedicated server.
//!
//! `cod4e-server [--install dir] [+set name value] [+exec server.cfg] [+map mp_crash]`; the install is
//! found as `install::detect` does (`--install`, `COD4_PATH`, the path `cod4e` remembered, Steam, `./COD4`). The `--webtransport` family of flags turns on the endpoint for
//! browser clients; each is shorthand for setting a cvar. Lines on stdin run as console commands;
//! with `rcon_password` set, `rcon` packets do the same from the network. With `sv_public 1` and `sv_master1`..
//! `sv_master5` set it announces itself to those master servers (`cod4e-master`).

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
    // The game log is on for a dedicated server unless the command line says otherwise.
    let mut all = vec![
        "+set".to_owned(),
        "g_log".to_owned(),
        "games_mp.log".to_owned(),
    ];
    all.extend(sets);
    all.extend(rest);
    Ok(all)
}

/// Removes `--install dir` (or `--install=dir`) from the arguments and returns the directory.
fn take_install(args: Vec<String>) -> Result<(Vec<String>, Option<PathBuf>), String> {
    let mut rest = Vec::new();
    let mut dir = None;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if a == "--install" {
            dir = Some(it.next().ok_or("--install needs a value")?.into());
        } else if let Some(v) = a.strip_prefix("--install=") {
            dir = Some(v.into());
        } else {
            rest.push(a);
        }
    }
    Ok((rest, dir))
}

fn main() -> ExitCode {
    let (args, explicit) = match take_install(std::env::args().skip(1).collect()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let args = match expand_flags(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "usage: cod4e-server [--install dir] [+set name value] [+exec file.cfg] [+map name]\n\
             \x20      [--webtransport [host:]port [--wt-cert cert.pem --wt-key key.pem] [--wt-info info.json]]\n\
             install: --install, $COD4_PATH, the folder cod4e remembered, Steam, ./COD4"
        );
        return ExitCode::SUCCESS;
    }
    let found = install::detect(&install::Options {
        explicit: explicit.as_deref(),
        flag: "--install",
        prompt: false,
        exe_dir: std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(PathBuf::from))
            .as_deref(),
    });
    let Some(root) = found.install.as_ref().map(|i| i.path.clone()) else {
        eprintln!("{}", found.not_found_message("cod4e-server"));
        return ExitCode::FAILURE;
    };
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
    s.announce_to_masters();
    // Ctrl-C and `kill` end the loop like `quit`, so the masters get the final `flatline`.
    #[cfg(unix)]
    for sig in [libc::SIGINT, libc::SIGTERM] {
        extern "C" fn stop(_: libc::c_int) {
            server::server::request_stop();
        }
        // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
        unsafe { libc::signal(sig, stop as *const () as libc::sighandler_t) };
    }
    s.run_forever();
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn install_flag_is_taken_out_of_the_arguments() {
        let (rest, dir) = take_install(args("+set a b --install /game +map mp_crash")).unwrap();
        assert_eq!(dir, Some(PathBuf::from("/game")));
        assert_eq!(rest, args("+set a b +map mp_crash"));
        let (_, dir) = take_install(args("--install=/x")).unwrap();
        assert_eq!(dir, Some(PathBuf::from("/x")));
        assert!(take_install(args("+map x --install")).is_err());
    }
}
