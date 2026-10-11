// SPDX-License-Identifier: GPL-3.0-only
//! `client-dedicated`: what a Windows tester does with the portable folder. The real `cod4e-server` binary starts on
//! a free UDP port with a map and a few bots; the real windowed `cod4e` client joins it with `--connect`, spawns,
//! walks and shoots for half a minute (`--autoplay`) and quits. The stage reads the client's report for the same
//! frame and clock checks as `client-match`, checks that snapshots arrived, and then asks the server (`status`) whether
//! the player is gone: a clean disconnect frees the slot at once, a dropped connection would keep it until the
//! timeout. Needs a display, the install, and `cod4e-server` next to the harness; skips cleanly without them.
use super::client_flythrough::locate_client;
use super::client_match::{column, hitch_failures, num};
use super::client_models::no_display;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const NAME: &str = "client-dedicated";
const BOTS: usize = 3;
const SECS: u64 = 30;
const NICK: &str = "HarnessDedicated";
/// The server loads the zones and the map, and a slow disk is slow.
const SERVER_UP: Duration = Duration::from_secs(240);
const CLIENT_LIMIT: Duration = Duration::from_secs(300);
/// How long a clean disconnect may take to free the player's slot.
const LEAVE_LIMIT: Duration = Duration::from_secs(20);

/// `cod4e-server` next to the harness (the portable folder, `target/<profile>`), or `COD4E_SERVER`.
pub(crate) fn locate_server() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("COD4E_SERVER") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let exe = std::env::current_exe().ok()?;
    let p = exe
        .parent()?
        .join(format!("cod4e-server{}", std::env::consts::EXE_SUFFIX));
    p.is_file().then_some(p)
}

/// A UDP port nobody holds right now.
fn free_port() -> io::Result<u16> {
    Ok(std::net::UdpSocket::bind(("0.0.0.0", 0))?
        .local_addr()?
        .port())
}

/// The server's console output, collected as lines by a reader thread, with a way to wait for one.
#[derive(Default)]
struct Lines {
    seen: Mutex<Vec<String>>,
    more: Condvar,
}

impl Lines {
    fn len(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    /// The lines from `from` on, once one satisfies `done` (the matching line is in them) or `limit` passes.
    fn wait_for(
        &self,
        from: usize,
        limit: Duration,
        done: impl Fn(&str) -> bool,
    ) -> Option<Vec<String>> {
        let end = Instant::now() + limit;
        let mut seen = self.seen.lock().unwrap();
        loop {
            if seen.iter().skip(from).any(|l| done(l)) {
                return Some(seen[from..].to_vec());
            }
            let left = end.checked_duration_since(Instant::now())?;
            seen = self.more.wait_timeout(seen, left).unwrap().0;
        }
    }
}

/// The running server. Dropping it kills the process, so no run leaves one behind.
struct Dedicated {
    child: Child,
    stdin: ChildStdin,
    lines: Arc<Lines>,
}

impl Dedicated {
    fn start(server: &Path, install: &Path, cwd: &Path, port: u16, log: &Path) -> io::Result<Self> {
        let mut child = Command::new(server)
            .arg("--install")
            .arg(install)
            .args(["+set", "net_port"])
            .arg(port.to_string())
            .args(["+set", "g_gametype", "war", "+set", "bot_count"])
            .arg(BOTS.to_string())
            .args(["+map", "mp_crash"])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take().expect("piped");
        let out = child.stdout.take().expect("piped");
        let lines = Arc::new(Lines::default());
        let (shared, mut file) = (lines.clone(), File::create(log)?);
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = writeln!(file, "{line}");
                shared.seen.lock().unwrap().push(line);
                shared.more.notify_all();
            }
            // The process ended: wake the waiters so they see it.
            shared.seen.lock().unwrap().push(EOF.to_owned());
            shared.more.notify_all();
        });
        Ok(Self {
            child,
            stdin,
            lines,
        })
    }

    /// Runs a console command and returns what it printed. The `echo` after it marks the end of the output.
    fn command(&mut self, line: &str, limit: Duration) -> Option<Vec<String>> {
        let from = self.lines.len();
        let mark = format!("end-of-{}", from);
        writeln!(self.stdin, "{line}\necho {mark}").ok()?;
        self.stdin.flush().ok()?;
        self.lines.wait_for(from, limit, |l| l == mark || l == EOF)
    }

    /// Stops the server and returns its exit status.
    fn quit(mut self) -> Option<std::process::ExitStatus> {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.stdin.flush();
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            match self.child.try_wait() {
                Ok(Some(s)) => return Some(s),
                Ok(None) if Instant::now() < end => {
                    // The reader thread notifies at the end of the output; this is the short wait for the exit.
                    let seen = self.lines.seen.lock().unwrap();
                    let _ = self
                        .lines
                        .more
                        .wait_timeout(seen, Duration::from_millis(100));
                }
                _ => return None,
            }
        }
    }
}

impl Drop for Dedicated {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The end of the server's output; no console line says this.
const EOF: &str = "\u{0}end of output";

/// Whether a `status` listing has a player of this name (a bot's address column says `bot`).
fn lists_player(status: &[String], name: &str) -> bool {
    status
        .iter()
        .any(|l| l.split_whitespace().any(|w| w == name) && !l.contains(" bot "))
}

fn fail(mut r: StageReport, why: String) -> StageReport {
    r.status = Status::Failed;
    r.reason = Some(why);
    r
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(client) = locate_client() else {
        return Ok(StageReport::new(NAME, Status::Skipped)
            .with_reason("cod4e client binary not found next to the harness"));
    };
    let Some(server) = locate_server() else {
        return Ok(StageReport::new(NAME, Status::Skipped)
            .with_reason("cod4e-server binary not found next to the harness"));
    };
    let Some(install) = &ctx.install else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("no install"));
    };
    if let Some(r) = no_display(&client, NAME)? {
        return Ok(r);
    }
    let dir = ctx.dir.join("dedicated");
    let work = dir.join("server");
    fs::create_dir_all(&work)?;
    let port = free_port()?;
    let mut report = StageReport::new(NAME, Status::Passed);
    report.files.extend([
        "dedicated/server.log".into(),
        "dedicated/client.log".into(),
        "dedicated/client.json".into(),
    ]);

    let mut dedicated = Dedicated::start(&server, install, &work, port, &dir.join("server.log"))?;
    let up = dedicated.lines.wait_for(0, SERVER_UP, |l| {
        l.starts_with("map mp_crash loaded") || l == EOF
    });
    match up
        .as_deref()
        .and_then(|l| l.iter().find(|l| l.starts_with("map ")))
    {
        Some(l) if l.contains(" 0 script errors") => {}
        Some(l) => {
            return Ok(fail(
                report,
                format!("the server's map did not load cleanly: {l}"),
            ));
        }
        None => {
            return Ok(fail(
                report,
                "cod4e-server never loaded the map (see dedicated/server.log)".into(),
            ));
        }
    }

    let log = File::create(dir.join("client.log"))?;
    let mut child = Command::new(&client)
        .arg("--install")
        .arg(install)
        .arg("--connect")
        .arg(format!("127.0.0.1:{port}"))
        .args(["--name", NICK, "--autoplay", "--duration"])
        .arg(SECS.to_string())
        .args(["--size", "1280x720", "--no-sound", "--config-dir"])
        .arg(dir.join("config"))
        .arg("--out")
        .arg(&dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    let end = Instant::now() + CLIENT_LIMIT;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(fail(
                report,
                format!("client hung (killed after {} s)", CLIENT_LIMIT.as_secs()),
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if !status.success() {
        return Ok(fail(
            report,
            format!("client exited with {status} (see dedicated/client.log)"),
        ));
    }
    let json: Value = match fs::read(dir.join("client.json"))
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
    {
        Ok(v) => v,
        Err(e) => return Ok(fail(report, format!("no client report: {e}"))),
    };
    let net = &json["net"];
    let mut failures = Vec::new();
    let secs = SECS as f64;
    let snaps = num(net, &["snapshots"]);
    let (predicted, corrected) = (
        num(net, &["predictions"]),
        num(net, &["prediction_corrections"]),
    );
    if net["connected"] != true {
        failures.push("the client never connected".to_owned());
    }
    if net["spawned"] != true {
        failures.push("the server never gave the player a body".to_owned());
    }
    if num(net, &["moved"]) < 100.0 && num(net, &["path"]) < 500.0 {
        failures.push(format!(
            "the player barely moved ({:.0} units walked)",
            num(net, &["path"])
        ));
    }
    if num(net, &["attack_cmds"]) < 1.0 {
        failures.push("the player never fired".into());
    }
    if num(net, &["players_seen_max"]) < 1.0 {
        failures.push(format!("none of the {BOTS} bots was ever seen"));
    }
    // The player is in the world for part of the run only (the load and the join come first).
    if snaps < secs * 10.0 {
        failures.push(format!("only {snaps} snapshots in {SECS} s"));
    }
    if predicted == 0.0 || corrected * 7.0 > predicted {
        failures.push(format!(
            "the server corrected {corrected} of {predicted} predictions"
        ));
    }
    let cpu = fs::read_to_string(dir.join("frames.raw.csv"))
        .map(|csv| column(&csv, 0))
        .unwrap_or_default();
    failures.extend(hitch_failures(&json, &cpu));

    // The client has quit. A clean disconnect has freed its slot; a lost connection would still list it.
    let mut left = false;
    let mut listing = Vec::new();
    let end = Instant::now() + LEAVE_LIMIT;
    while Instant::now() < end {
        let Some(status) = dedicated.command("status", Duration::from_secs(10)) else {
            break;
        };
        left = !lists_player(&status, NICK);
        listing = status;
        if left || status_dead(&listing) {
            break;
        }
    }
    let joined = dedicated
        .lines
        .seen
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.starts_with("MP_CONNECTED") && l.contains(NICK));
    if !joined {
        failures.push("the server never announced the player joining".into());
    }
    if !left {
        failures.push(format!(
            "the server still lists the player {} s after the client quit: not a clean disconnect",
            LEAVE_LIMIT.as_secs()
        ));
    }
    let code = dedicated.quit();
    if code.is_none_or(|c| !c.success()) {
        failures.push(format!(
            "cod4e-server did not exit cleanly on `quit`: {code:?}"
        ));
    }

    let m = &mut report.metrics;
    m.insert("client.snapshots".into(), snaps);
    m.insert("client.bytes_in".into(), num(net, &["bytes_in"]));
    m.insert("client.moved".into(), num(net, &["moved"]));
    m.insert("client.attack_cmds".into(), num(net, &["attack_cmds"]));
    m.insert("client.predictions".into(), predicted);
    m.insert("client.prediction_corrections".into(), corrected);
    m.insert("server.port".into(), f64::from(port));
    if !failures.is_empty() {
        report
            .notes
            .push(format!("server status at the end:\n{}", listing.join("\n")));
        return Ok(fail(report, failures.join("; ")));
    }
    Ok(report)
}

/// The server's output ended: nothing more will come.
fn status_dead(lines: &[String]) -> bool {
    lines.last().is_some_and(|l| l == EOF)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|r| (*r).to_owned()).collect()
    }

    #[test]
    fn a_status_row_of_a_person_counts_and_a_bot_of_the_same_name_does_not() {
        let person =
            rows(&["  3     0   12    0 HarnessDedicated      40 127.0.0.1:5000   1234    -"]);
        assert!(lists_player(&person, "HarnessDedicated"));
        let bot =
            rows(&["  0     0  BOT    0 HarnessDedicated       0 bot                 0    -"]);
        assert!(!lists_player(&bot, "HarnessDedicated"));
        assert!(!lists_player(&rows(&["end-of-1"]), "HarnessDedicated"));
    }

    #[test]
    fn wait_for_returns_the_lines_once_one_matches() {
        let lines = Arc::new(Lines::default());
        let l = lines.clone();
        let t = std::thread::spawn(move || {
            for s in ["a", "b", "done"] {
                l.seen.lock().unwrap().push(s.to_owned());
                l.more.notify_all();
            }
        });
        let got = lines.wait_for(1, Duration::from_secs(10), |l| l == "done");
        t.join().unwrap();
        assert_eq!(got.unwrap(), ["b", "done"]);
        assert!(
            lines
                .wait_for(0, Duration::from_millis(20), |l| l == "never")
                .is_none()
        );
    }
}
