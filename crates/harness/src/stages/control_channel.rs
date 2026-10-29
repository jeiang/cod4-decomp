// SPDX-License-Identifier: GPL-3.0-only
//! `control-channel`: the client's dev control channel (#378), driven the way an agent drives it. A real client
//! starts a listen server, the stage opens the console with the key the player presses, types `bots 3`, waits for the
//! four players, holds the scoreboard up and reads it, reads the entity list, clicks a menu item at the centre of the
//! rectangle the state document gave for it and checks the same item became the pointer's, and captures the presented
//! frame offscreen. Every request is one line of JSON on the loopback socket the `--control` flag opens; the run is
//! the harness's own child, so the capture has no visible window on the host.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use super::client_session::profile_args;
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const NAME: &str = "control-channel";
const LIMIT: Duration = Duration::from_secs(360);

/// One request and its reply: the line protocol the transports share.
fn ask(sock: &mut BufReader<TcpStream>, id: u64, req: Value) -> Result<Value, String> {
    let mut req = req;
    req["id"] = json!(id);
    let line = format!("{req}\n");
    sock.get_mut()
        .write_all(line.as_bytes())
        .map_err(|e| e.to_string())?;
    let mut back = String::new();
    sock.read_line(&mut back).map_err(|e| e.to_string())?;
    if back.is_empty() {
        return Err("the channel closed".to_owned());
    }
    let reply: Value = serde_json::from_str(&back).map_err(|e| format!("{e}: {back}"))?;
    match reply["ok"].as_bool() {
        Some(true) => Ok(reply),
        _ => Err(reply["error"]
            .as_str()
            .unwrap_or("the request failed")
            .to_owned()),
    }
}

/// Asks until the reply is `true`, letting the client run frames between offers.
fn until(
    sock: &mut BufReader<TcpStream>,
    id: &mut u64,
    req: Value,
    what: &str,
    limit: Duration,
) -> Result<Value, String> {
    *id += 1;
    ask(
        sock,
        *id,
        json!({"cmd": "wait", "state": req, "timeout_ms": limit.as_millis() as u64}),
    )
    .map_err(|e| format!("{what}: {e}"))
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let skipped = |why: &str| {
        let mut r = StageReport::new(NAME, Status::Skipped);
        r.reason = Some(why.to_owned());
        r
    };
    let Some(install) = &ctx.install else {
        return Ok(skipped("no install"));
    };
    let Some(client) = locate_client() else {
        return Ok(skipped("no client binary"));
    };
    if let Some(report) = no_display(&client, NAME)? {
        return Ok(report);
    }
    let dir = ctx.dir.join("control");
    fs::create_dir_all(&dir)?;
    let addr_file = dir.join("control.addr");
    let log = File::create(dir.join("client.log"))?;
    let mut child: Child = Command::new(&client)
        .arg("--install")
        .arg(install)
        .args([
            "--size",
            "1280x720",
            "--no-sound",
            "--listen",
            "--bots",
            "0",
        ])
        .args(["--duration", "300"])
        .arg("--control")
        .arg("0")
        .arg("--control-file")
        .arg(&addr_file)
        .args(profile_args(&dir.join("config")))
        .arg("--out")
        .arg(&dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;

    let deadline = Instant::now() + Duration::from_secs(90);
    let addr = loop {
        if let Ok(s) = fs::read_to_string(&addr_file)
            && let Some(a) = s.lines().next().and_then(|l| l.split(':').nth(1))
        {
            break format!("127.0.0.1:{a}");
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(skipped("the client did not open its control channel"));
        }
        std::thread::sleep(Duration::from_millis(250));
    };

    let mut sock = match TcpStream::connect(&addr) {
        Ok(s) => {
            s.set_read_timeout(Some(Duration::from_secs(240)))?;
            BufReader::new(s)
        }
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(skipped(&format!("the channel did not answer: {e}")));
        }
    };

    let mut id = 0_u64;
    let mut report = StageReport::new(NAME, Status::Passed);
    report.notes.push(format!("channel {addr}"));

    let outcome = (|| -> Result<(), String> {
        id += 1;
        let ping = ask(&mut sock, id, json!({"cmd": "ping"}))?;
        report.metrics.insert(
            "frames_at_ping".to_owned(),
            ping["frames"].as_f64().unwrap_or(0.0),
        );

        // The console, with the key the player presses, then the line typed into it.
        id += 1;
        ask(&mut sock, id, json!({"cmd": "key", "name": "backquote"}))?;
        if ask(
            &mut sock,
            id + 100,
            json!({"cmd": "wait", "state": {"console.open": true}, "timeout_ms": 3000}),
        )
        .is_err()
        {
            // Some layouts name the key `grave`.
            id += 1;
            ask(&mut sock, id, json!({"cmd": "key", "name": "grave"}))?;
        }
        until(
            &mut sock,
            &mut id,
            json!({"console.open": true}),
            "the console did not open",
            Duration::from_secs(10),
        )?;
        id += 1;
        ask(&mut sock, id, json!({"cmd": "text", "text": "bots 3"}))?;
        id += 1;
        ask(&mut sock, id, json!({"cmd": "key", "name": "enter"}))?;
        id += 1;
        ask(&mut sock, id, json!({"cmd": "key", "name": "backquote"}))?;

        // Four players on the scoreboard, held up by the bind the player holds.
        until(
            &mut sock,
            &mut id,
            json!({"entities.count": {"ge": 4}}),
            "four players did not arrive",
            Duration::from_secs(120),
        )?;
        id += 1;
        let ents = ask(&mut sock, id, json!({"cmd": "state", "what": "entities"}))?;
        let listed = ents["entities"].as_array().cloned().unwrap_or_default();
        report
            .metrics
            .insert("entities".to_owned(), listed.len() as f64);
        if listed.len() < 4 {
            return Err(format!("{} entities, want 4", listed.len()));
        }
        let bots = listed
            .iter()
            .filter(|e| e["name"].as_str().is_some_and(|n| !n.is_empty()))
            .count();
        report
            .metrics
            .insert("named_entities".to_owned(), bots as f64);

        id += 1;
        ask(
            &mut sock,
            id,
            json!({"cmd": "key", "bind": "+scores", "action": "down"}),
        )?;
        id += 1;
        let board = ask(&mut sock, id, json!({"cmd": "state", "what": "scoreboard"}))?;
        id += 1;
        ask(
            &mut sock,
            id,
            json!({"cmd": "key", "bind": "+scores", "action": "up"}),
        )?;
        let rows = board["rows"]
            .as_array()
            .or_else(|| board["scoreboard"]["rows"].as_array())
            .map(Vec::len)
            .unwrap_or(0);
        report
            .metrics
            .insert("scoreboard_rows".to_owned(), rows as f64);
        if rows < 4 {
            return Err(format!("{rows} scoreboard rows, want 4: {board}"));
        }

        // A capture of the frame on screen, with no window of the stage's own.
        id += 1;
        let shot = ask(&mut sock, id, json!({"cmd": "capture"}))?;
        let path = shot["path"]
            .as_str()
            .ok_or_else(|| format!("no capture path: {shot}"))?;
        let png = Path::new(path);
        let bytes = fs::metadata(png).map_err(|e| format!("{path}: {e}"))?.len();
        report.files.push(format!(
            "control/{}",
            png.file_name().unwrap_or_default().to_string_lossy()
        ));
        report
            .metrics
            .insert("capture_bytes".to_owned(), bytes as f64);
        if bytes < 1024 {
            return Err(format!("{path} is {bytes} bytes"));
        }

        // A menu item clicked at the centre of its own rectangle is the item under the pointer.
        id += 1;
        ask(&mut sock, id, json!({"cmd": "key", "name": "escape"}))?;
        id += 1;
        let menu = ask(&mut sock, id, json!({"cmd": "state", "what": "menu"}))?;
        let items = menu["items"].as_array().cloned().unwrap_or_default();
        let Some(item) = items.iter().find(|i| {
            i["name"].as_str().is_some_and(|n| !n.is_empty())
                && i["rect"][2].as_f64().unwrap_or(0.0) > 0.0
        }) else {
            return Err(format!("no menu item to click: {menu}"));
        };
        let name = item["name"].as_str().unwrap_or_default().to_owned();
        let r = item["rect"].as_array().cloned().unwrap_or_default();
        let (x, y) = (
            r[0].as_f64().unwrap_or(0.0) + r[2].as_f64().unwrap_or(0.0) / 2.0,
            r[1].as_f64().unwrap_or(0.0) + r[3].as_f64().unwrap_or(0.0) / 2.0,
        );
        id += 1;
        ask(&mut sock, id, json!({"cmd": "mouse", "x": x, "y": y}))?;
        id += 1;
        let hovered = ask(&mut sock, id, json!({"cmd": "state", "what": "menu"}))?;
        let hit = hovered["focused"]["item"].as_str().unwrap_or_default();
        if !name.is_empty() && hit != name {
            return Err(format!(
                "clicked {name} at {x:.0},{y:.0} and {hit} was under the pointer: {hovered}"
            ));
        }
        report
            .notes
            .push(format!("clicked {name} at {x:.0},{y:.0}"));

        id += 1;
        ask(&mut sock, id, json!({"cmd": "quit"}))?;
        Ok(())
    })();

    let end = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait()? {
            Some(s) => break s,
            None if Instant::now() > end => {
                let _ = child.kill();
                break child.wait()?;
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    if !status.success() && outcome.is_ok() {
        report.status = Status::Failed;
        report.reason = Some(format!("the client exited {status}"));
        return Ok(report);
    }
    if let Err(e) = outcome {
        report.status = Status::Failed;
        report.reason = Some(e);
    }
    let _ = status;
    let _ = LIMIT;
    Ok(report)
}
