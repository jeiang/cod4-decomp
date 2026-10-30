// SPDX-License-Identifier: GPL-3.0-only
//! `web-match`: the browser client plays a match the way the native client does, driven through the dev control
//! channel's browser transport (#378, #429). A native server (in this process, WebTransport on a free port, three
//! bots in a team deathmatch on mp_crash) and `web/serve.py` run; a headless Chromium opens the built page with
//! `?connect=…&control` and no `--autoplay`, so the page is the player: the stage picks a team and a class by clicking
//! the stock menus' items, walks, turns, fires, throws a grenade, holds the scoreboard, is killed by a bot and sees the
//! killcam, ends the match on a score limit and follows the server into the next map, then leaves through the Escape
//! menu. Each step is asserted on the state document, and a frame is captured at each.
//!
//! Every request goes `POST /control` to `serve.py`, which the page long-polls: the same JSON as the native channel's
//! socket (`control_channel.rs`). The browser hosts no server, so the stage also opens Start New Server from the main
//! menu before the join and asserts the popup that says so (`listen_web.rs`).
//!
//! Parity: after the browser match the native client plays the same map, gametype and bot count; the stage compares
//! its measured movement, shots and HUD ownerdraw set with the browser's. A difference is a note, and the three a
//! match cannot do without (ammo, weapon name, compass map) are assertions.
use super::client_flythrough::locate_client;
use super::web_client::{Kill, find_chrome, find_web_dir};
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::{Value, json};
use server::client::Session;
use server::server::Server;
use std::collections::BTreeSet;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const NAME: &str = "web-match";
const PORT: u16 = 18_092;
const BOTS: u32 = 3;
/// The whole run, the browser's load and the map change included (software WebGL2 is slow).
const LIMIT: Duration = Duration::from_secs(1200);
/// One `wait` request: `serve.py` answers 504 after 30 s, so a longer wait is asked for in pieces.
const WAIT_PIECE: Duration = Duration::from_secs(20);
const PLAYER: &str = "webplayer";
/// What the browser's Start New Server popup says (`crates/client/src/listen_web.rs`).
const NO_HOSTING: &str = "Starting a server is not available in the browser. Use Join Game.";

/// `EV_FIRE_WEAPON` and `EV_FIRE_WEAPON_LASTSHOT` (`sim::pm::ev`), the own events a shot delivers.
const EV_FIRE_WEAPON: u8 = 0x1A;
const EV_FIRE_WEAPON_LASTSHOT: u8 = 0x1B;

/// The ownerdraws a match cannot be played without: the ammo counter, the weapon's name and the compass map.
const MUST_DRAW: &[i64] = &[119, 81, 159];

/// One HTTP POST to `serve.py` on loopback, read to the end: `(status, body)`.
fn http_post(path: &str, body: &str) -> Result<(u16, String), String> {
    let mut s = TcpStream::connect(("127.0.0.1", PORT)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(45)))
        .map_err(|e| e.to_string())?;
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{PORT}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&raw);
    let (head, rest) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| format!("no HTTP response: {text}"))?;
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("bad HTTP status line: {head}"))?;
    Ok((status, rest.to_owned()))
}

/// The page as the channel's other end: each request is posted on a thread while the server keeps running frames
/// here, because the page's reply needs the server's snapshots to arrive in the meantime.
struct Page {
    id: u64,
    requests: u64,
}

impl Page {
    fn ask(&mut self, server: &mut Server, mut req: Value) -> Result<Value, String> {
        self.id += 1;
        self.requests += 1;
        req["id"] = json!(self.id);
        let line = req.to_string();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(http_post("/control", &line));
        });
        let answer = loop {
            server.run_for(Duration::from_millis(50));
            match rx.try_recv() {
                Ok(r) => break r?,
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err("the request thread ended without an answer".to_owned());
                }
            }
        };
        let (status, body) = answer;
        if status != 200 {
            return Err(format!("{} -> HTTP {status}: {}", req["cmd"], body.trim()));
        }
        let reply: Value =
            serde_json::from_str(body.trim()).map_err(|e| format!("bad reply {body:?}: {e}"))?;
        match reply["ok"].as_bool() {
            Some(true) => Ok(reply),
            _ => Err(format!(
                "{}: {}",
                req["cmd"],
                reply["error"].as_str().unwrap_or("the request failed")
            )),
        }
    }

    /// Waits until the state document matches `state`, in pieces `serve.py`'s own timeout allows, or fails with
    /// `what` and what was last observed.
    fn until(
        &mut self,
        server: &mut Server,
        state: Value,
        what: &str,
        limit: Duration,
    ) -> Result<Value, String> {
        let end = Instant::now() + limit;
        let mut last = String::new();
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!("{what} within {} s: {last}", limit.as_secs()));
            }
            let piece = left.min(WAIT_PIECE);
            match self.ask(
                server,
                json!({"cmd": "wait", "state": state, "timeout_ms": piece.as_millis() as u64}),
            ) {
                Ok(v) => return Ok(v),
                Err(e) => last = e,
            }
        }
    }

    fn state(&mut self, server: &mut Server, what: &str) -> Result<Value, String> {
        self.ask(server, json!({"cmd": "state", "what": what}))
            .map(|v| v[what].clone())
    }

    fn bind(&mut self, server: &mut Server, bind: &str, action: &str) -> Result<(), String> {
        self.ask(
            server,
            json!({"cmd": "key", "bind": bind, "action": action}),
        )
        .map(|_| ())
    }

    /// Clicks the visible item of the top open menu whose name or shown text is `want` (case-insensitive), at the
    /// centre of the rectangle the state document gives: the pointer moves there, then the button goes down and up.
    fn click(&mut self, server: &mut Server, want: &str) -> Result<(), String> {
        let menu = self.state(server, "menu")?;
        let top = menu["open"]
            .as_array()
            .and_then(|o| o.last())
            .and_then(Value::as_str)
            .ok_or("no menu is open")?
            .to_owned();
        let item = menu["items"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|it| {
                it["menu"] == top
                    && it["visible"] == true
                    && (it["name"]
                        .as_str()
                        .is_some_and(|n| n.eq_ignore_ascii_case(want))
                        || it["text"]
                            .as_str()
                            .is_some_and(|t| t.eq_ignore_ascii_case(want)))
            })
            .ok_or_else(|| format!("{top} shows no item {want:?}: {}", menu["items"]))?;
        let r = item["rect"].as_array().cloned().unwrap_or_default();
        if r.len() != 4 {
            return Err(format!("{want} has no rectangle: {item}"));
        }
        let (x, y) = (
            r[0].as_f64().unwrap_or(0.0) + r[2].as_f64().unwrap_or(0.0) / 2.0,
            r[1].as_f64().unwrap_or(0.0) + r[3].as_f64().unwrap_or(0.0) / 2.0,
        );
        self.ask(server, json!({"cmd": "mouse", "x": x, "y": y}))?;
        self.ask(server, json!({"cmd": "step", "frames": 1}))?;
        self.button(server, "mouse1", "tap")?;
        Ok(())
    }

    fn button(&mut self, server: &mut Server, name: &str, action: &str) -> Result<(), String> {
        self.ask(
            server,
            json!({"cmd": "button", "name": name, "action": action}),
        )
        .map(|_| ())
    }

    /// Waits until a menu whose name starts with `prefix` is open (the class menu is `changeclass_<team>`).
    fn until_menu(
        &mut self,
        server: &mut Server,
        prefix: &str,
        what: &str,
        limit: Duration,
    ) -> Result<Vec<String>, String> {
        let end = Instant::now() + limit;
        loop {
            let open: Vec<String> = self.state(server, "menu")?["open"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| m.as_str().map(str::to_owned))
                .collect();
            if open.iter().any(|m| m.starts_with(prefix)) {
                return Ok(open);
            }
            if Instant::now() > end {
                return Err(format!(
                    "{what} within {} s: open menus {open:?}",
                    limit.as_secs()
                ));
            }
            self.ask(server, json!({"cmd": "step", "frames": 5}))?;
        }
    }

    /// Reads `path` of section `what` every few frames until `good` says so, or `limit` passes.
    fn poll(
        &mut self,
        server: &mut Server,
        what: &str,
        path: &str,
        good: impl Fn(f64) -> bool,
        why: &str,
        limit: Duration,
    ) -> Result<f64, String> {
        let end = Instant::now() + limit;
        loop {
            let v = num(&self.state(server, what)?, path);
            if good(v) {
                return Ok(v);
            }
            if Instant::now() > end {
                return Err(format!(
                    "{why} within {} s: {what}.{path} is {v}",
                    limit.as_secs()
                ));
            }
            self.ask(server, json!({"cmd": "step", "frames": 3}))?;
        }
    }

    /// Captures the frame on screen into `dir/<step>.png` and lists it in the report.
    fn capture(
        &mut self,
        server: &mut Server,
        report: &mut StageReport,
        dir: &Path,
        step: &str,
    ) -> Result<f64, String> {
        let shot = self.ask(server, json!({"cmd": "capture", "base64": true}))?;
        let b64 = shot["png_base64"]
            .as_str()
            .ok_or_else(|| format!("the capture has no image: {shot}"))?;
        let bytes = decode_base64(b64).ok_or("the capture is not base64")?;
        if bytes.len() < 1024 {
            return Err(format!("the capture of {step} is {} bytes", bytes.len()));
        }
        let path = dir.join(format!("{step}.png"));
        std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        report.files.push(format!("web-match/{step}.png"));
        let lit = shot["lit_fraction"].as_f64().unwrap_or(f64::NAN);
        report.metrics.insert(format!("lit_{step}"), lit);
        // A blank canvas is a capture taken before the frame reached it, or a page that draws nothing.
        if !(lit > 0.01) {
            return Err(format!(
                "the capture of {step} is blank (lit fraction {lit})"
            ));
        }
        Ok(lit)
    }
}

/// Standard base64 (what the page's `toDataURL` writes) to bytes; `None` for a character outside the alphabet.
fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if c == b'=' {
            break;
        }
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// A number anywhere in the state document, by dotted path.
fn num(v: &Value, path: &str) -> f64 {
    devctrl::lookup(v, path)
        .and_then(Value::as_f64)
        .unwrap_or(f64::NAN)
}

/// The browser's human slot on the server, if it is connected.
fn human(server: &Server) -> Option<(u16, Session)> {
    server
        .game
        .connected_clients()
        .find(|(_, c)| !c.bot)
        .map(|(n, c)| (n, c.session))
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let skip = |why: &str| Ok(StageReport::new(NAME, Status::Skipped).with_reason(why));
    let Some(web) = find_web_dir() else {
        return skip("web/pkg is not built (just web-build) or web/ is not next to the harness");
    };
    let Some(chrome) = find_chrome() else {
        return skip("no Chromium found (set COD4E_CHROME)");
    };
    let Some(client) = locate_client() else {
        return skip("cod4e client binary not found next to the harness");
    };
    let Some(install) = ctx.install.clone() else {
        return skip("no original install");
    };
    let dir = ctx.dir.join("web-match");
    std::fs::create_dir_all(&dir)?;
    let info = dir.join("wt-info.json");
    let args: Vec<String> = [
        "+set",
        "net_port",
        "0",
        "+set",
        "net_wt",
        "127.0.0.1:0",
        "+set",
        "net_wt_info",
    ]
    .map(String::from)
    .into_iter()
    .chain([info.display().to_string()])
    .collect();
    let mut server = match Server::boot(&install, &args, false) {
        Ok(s) => s,
        Err(e) => return Ok(StageReport::new(NAME, Status::Failed).with_reason(e)),
    };
    // Two maps in the rotation, so the end of the match leads into another level the browser must load.
    for line in [
        "set g_gametype war",
        "set scr_war_timelimit 0",
        "set scr_war_scorelimit 0",
        // The rotation starts at its first entry when the first match ends (`SV_MapRotate_f`), so that is the other map.
        "set sv_mapRotation \"gametype war map mp_backlot gametype war map mp_crash\"",
        "map mp_crash",
        &format!("bots {BOTS}"),
    ] {
        if let Err(e) = server.exec_line(line) {
            return Ok(StageReport::new(NAME, Status::Failed).with_reason(format!("{line}: {e}")));
        }
    }
    let published: Option<Value> = std::fs::read(&info)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let (Some(url), Some(hash)) = (
        published.as_ref().and_then(|v| v["url"].as_str()),
        published
            .as_ref()
            .and_then(|v| v["certHashSha256Base64"].as_str()),
    ) else {
        return Ok(StageReport::new(NAME, Status::Failed)
            .with_reason("the server did not publish a WebTransport endpoint (--wt-info)"));
    };
    let wt_port = url
        .rsplit(':')
        .next()
        .and_then(|p| p.trim_end_matches('/').parse::<u16>().ok())
        .unwrap_or(0);
    let hash = hash
        .replace('+', "%2B")
        .replace('/', "%2F")
        .replace('=', "%3D");

    let py = Command::new("python3")
        .arg(web.join("serve.py"))
        .arg(PORT.to_string())
        .arg("--install")
        .arg(&install)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let _py = Kill(py);
    std::thread::sleep(Duration::from_millis(800));

    // No `--autoplay` and no autojoin: the menus are the stage's to answer, as a person's. `auto` picks WebGPU where
    // the browser has it and WebGL2 otherwise; the backend used goes in the notes.
    let profile = std::env::temp_dir().join(format!("cod4e-webmatch-{}", std::process::id()));
    let page = format!(
        "http://localhost:{PORT}/?dev-install=/install&autostart&debug&control&name={PLAYER}\
         &connect=127.0.0.1:{wt_port}&cert={hash}&args=--no-autojoin%20--duration%20100000"
    );
    let browser_log = std::fs::File::create(dir.join("browser.log"))?;
    let mut command = Command::new(&chrome);
    command
        .env_remove("LD_LIBRARY_PATH")
        .args([
            "--headless=new",
            "--no-first-run",
            "--ignore-gpu-blocklist",
            "--use-angle=swiftshader",
            "--enable-unsafe-swiftshader",
            "--autoplay-policy=no-user-gesture-required",
            "--window-size=1280,720",
            "--enable-logging=stderr",
            "--log-level=0",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(&page)
        .stdout(Stdio::null())
        .stderr(browser_log);
    let mut cdp = super::browser_cdp::pipe(&mut command)?;
    let browser = Kill(
        command
            .spawn()
            .map_err(|e| io::Error::other(format!("cannot start {}: {e}", chrome.display())))?,
    );

    let started = Instant::now();
    let mut report = StageReport::new(NAME, Status::Passed);
    let mut page = Page { id: 0, requests: 0 };
    let outcome = play(&mut server, &mut page, &mut cdp, &mut report, &dir, started)
        .and_then(|()| native_parity(&client, &install, &dir, &mut report));
    drop(browser);
    let _ = std::fs::remove_dir_all(&profile);
    report.files.push("web-match/browser.log".into());
    report
        .metrics
        .insert("requests".into(), page.requests as f64);
    report
        .metrics
        .insert("seconds".into(), started.elapsed().as_secs_f64());
    if let Err(e) = outcome {
        report.status = Status::Failed;
        report.reason = Some(e);
    }
    Ok(report)
}

/// Runs the native client on the same map, gametype and bot count, then records the measured state/HUD differences.
/// The browser has left its dedicated server by now, so this reference uses the native client's own listen server.
fn native_parity(
    client: &Path,
    install: &Path,
    dir: &Path,
    report: &mut StageReport,
) -> Result<(), String> {
    let out = dir.join("native");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let log = std::fs::File::create(out.join("client.log")).map_err(|e| e.to_string())?;
    let mut child = Command::new(client)
        .arg("--install")
        .arg(install)
        .args(["--map", "mp_crash", "--listen", "--bots"])
        .arg(BOTS.to_string())
        .args([
            "--autoplay",
            "--duration",
            "30",
            "--size",
            "1280x720",
            "--out",
        ])
        .arg(&out)
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log)
        .spawn()
        .map_err(|e| e.to_string())?;
    let end = Instant::now() + Duration::from_secs(180);
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if Instant::now() > end {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the native parity client did not finish within 180 s".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if !status.success() {
        return Err(format!("the native parity client exited with {status}"));
    }
    let native: Value =
        serde_json::from_slice(&std::fs::read(out.join("client.json")).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let browser: Value = serde_json::from_slice(
        &std::fs::read(dir.join("browser-parity.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let ids = |v: &Value, path: &str| -> BTreeSet<i64> {
        devctrl::lookup(v, path)
            .and_then(|v| match v {
                Value::Array(a) => Some(a.iter().filter_map(Value::as_i64).collect()),
                Value::Object(o) => Some(o.keys().filter_map(|k| k.parse().ok()).collect()),
                _ => None,
            })
            .unwrap_or_default()
    };
    let native_ids = ids(&native, "hud.drawn");
    let browser_ids = ids(&browser, "hud_drawn");
    let missing: Vec<i64> = native_ids.difference(&browser_ids).copied().collect();
    let extra: Vec<i64> = browser_ids.difference(&native_ids).copied().collect();
    for (name, value) in [
        ("native_moved", num(&native, "net.moved")),
        ("native_path", num(&native, "net.path")),
        ("native_shots", num(&native, "net.shots")),
        ("native_hud_drawn", native_ids.len() as f64),
        ("parity_hud_missing", missing.len() as f64),
        ("parity_hud_extra", extra.len() as f64),
    ] {
        report.metrics.insert(name.into(), value);
    }
    report.notes.push(format!(
        "measured native/browser parity (mp_crash war, {BOTS} bots): native moved {:.1}, path {:.1}, shots {:.0}; browser moved {:.1}, fire events {:.0}; HUD native {native_ids:?}, browser {browser_ids:?}, missing {missing:?}, extra {extra:?}",
        num(&native, "net.moved"),
        num(&native, "net.path"),
        num(&native, "net.shots"),
        num(&browser, "moved"),
        num(&browser, "fire_events"),
    ));
    report.files.extend([
        "web-match/native/client.json".into(),
        "web-match/native/client.log".into(),
    ]);
    Ok(())
}

/// The match, step by step; the first failed assertion ends it.
fn play(
    server: &mut Server,
    page: &mut Page,
    cdp: &mut super::browser_cdp::BrowserPipe,
    report: &mut StageReport,
    dir: &Path,
    started: Instant,
) -> Result<(), String> {
    let since = |started: Instant| started.elapsed().as_secs_f64() * 1000.0;
    let left = |started: Instant| LIMIT.saturating_sub(started.elapsed());
    // The page answers once the module runs: the first request waits for the wasm to load and draw.
    page.until(
        server,
        json!({"client.frames": {"ge": 1}}),
        "the page never drew a frame",
        Duration::from_secs(240),
    )?;
    report
        .metrics
        .insert("first_frame_ms".into(), since(started));
    let client = page.state(server, "client")?;
    report.notes.push(format!(
        "gpu {} ({}), canvas {}",
        client["gpu"]["backend"], client["gpu"]["name"], client["size"]
    ));

    // The join: the connection is up, and the stock team menu the server's script opens is on screen.
    page.until(
        server,
        json!({"net.connected": true}),
        "the browser did not connect",
        Duration::from_secs(300),
    )?;
    report.metrics.insert("connect_ms".into(), since(started));
    page.until_menu(
        server,
        "team_marinesopfor",
        "the team menu did not open",
        Duration::from_secs(120),
    )?;
    page.capture(server, report, dir, "01-team-menu")?;
    // Marines by the text it shows, then a class by its: the clicks land at the centre of each item's rectangle.
    page.click(server, "Marines")?;
    page.until_menu(
        server,
        "changeclass",
        "the class menu did not open after the team was picked",
        Duration::from_secs(30),
    )?;
    page.capture(server, report, dir, "02-class-menu")?;
    page.click(server, "Assault")?;
    page.until(
        server,
        json!({"player.alive": true, "menu.open": {"len": 0}}),
        "the player did not spawn after the class was picked",
        Duration::from_secs(120),
    )?;
    report.metrics.insert("spawn_ms".into(), since(started));
    page.until(
        server,
        json!({"entities.count": {"ge": 4}}),
        "the bots never showed in the snapshots",
        Duration::from_secs(60),
    )?;
    let (slot, session) = human(server).ok_or("the server has no person connected")?;
    if !matches!(session, Session::Playing) {
        return Err(format!(
            "the server has the browser's client as {session:?}, not playing"
        ));
    }
    page.capture(server, report, dir, "03-spawned")?;
    let p0 = page.state(server, "player")?;
    let team = num(&p0, "team");
    if team != 2.0 {
        return Err(format!(
            "the player is on team {team}, not the Marines (2) that were clicked"
        ));
    }

    // The pointer: a real click through Chromium's input pipeline grants the page the pointer lock, as a player's
    // click does (a headless page gets it no other way); from then on the mouse deltas go through the same
    // control-channel request as native (`CL_MouseMove`), and the yaw the server echoes follows them.
    cdp.click()?;
    page.ask(server, json!({"cmd": "step", "frames": 2}))?;
    let yaw0 = num(&p0, "angles.1");
    let turn = |y: f64| ((y - yaw0 + 540.0).rem_euclid(360.0) - 180.0).abs();
    page.ask(server, json!({"cmd": "mouse", "dx": 120, "dy": 0}))?;
    let turned = page.poll(
        server,
        "player",
        "angles.1",
        |y| turn(y) >= 2.0,
        "the control-channel mouse delta did not turn the view after pointer capture",
        Duration::from_secs(15),
    )?;
    report.metrics.insert("turned_deg".into(), turn(turned));

    // Moving: W (the `+forward` bind) held moves the origin at least 64 units. Held until it has, not for a wall
    // time: a software-rendered page runs a few frames a second right after the spawn (its pipelines build in the
    // frame), and a command's time is capped, so a second of the wall is less than a second of movement there. A
    // spawn can face a wall (the 2026-10-30 run `slot-1-cod4e-run-20261030-040632` walked 25 units in 30 s into
    // one): when a second of walking gains nothing the view turns a quarter, as `net-match`'s walker does.
    let origin0 = [num(&p0, "origin.0"), num(&p0, "origin.1")];
    page.bind(server, "+forward", "down")?;
    let t_move = Instant::now();
    let end = Instant::now() + Duration::from_secs(45);
    let mut dist = 0.0;
    let mut p1 = p0.clone();
    let mut last_gain = (Instant::now(), 0.0);
    let mut turns = 0;
    while dist < 64.0 && Instant::now() < end {
        page.ask(server, json!({"cmd": "step", "frames": 3}))?;
        p1 = page.state(server, "player")?;
        dist = (num(&p1, "origin.0") - origin0[0]).hypot(num(&p1, "origin.1") - origin0[1]);
        if dist > last_gain.1 + 4.0 {
            last_gain = (Instant::now(), dist);
        } else if last_gain.0.elapsed() > Duration::from_secs(2) {
            page.ask(server, json!({"cmd": "mouse", "dx": 820, "dy": 0}))?;
            turns += 1;
            last_gain = (Instant::now(), dist);
        }
    }
    page.bind(server, "+forward", "up")?;
    report.metrics.insert("moved".into(), dist);
    report.metrics.insert("walk_turns".into(), f64::from(turns));
    report
        .metrics
        .insert("walk_ms".into(), t_move.elapsed().as_secs_f64() * 1000.0);
    report.notes.push(format!(
        "walk timing: server level {}, snapshot {}, client command {}, server command {}; alive {}; turned {turns} times",
        server.level_time(),
        p1["server_time"],
        p1["client_command_time"],
        p1["command_time"],
        p1["alive"]
    ));
    if !(dist >= 64.0) {
        return Err(format!(
            "W held for 45 s moved the player {dist:.0} units (from {origin0:?} to {}), want 64",
            p1["origin"]
        ));
    }
    page.capture(server, report, dir, "04-moved")?;
    // The clip as it is now: the trusted click above went through the window's path too, as a player's would.
    let clip0 = num(&page.state(server, "hud")?, "clip_ammo");

    // Firing: mouse1 lowers the clip and the own fire event is delivered.
    page.button(server, "mouse1", "down")?;
    page.ask(server, json!({"cmd": "step", "frames": 6}))?;
    page.button(server, "mouse1", "up")?;
    page.until(
        server,
        json!({"hud.clip_ammo": {"lt": clip0}}),
        &format!("the clip stayed at {clip0} after firing"),
        Duration::from_secs(10),
    )?;
    let events = page.state(server, "events")?;
    let shots = [EV_FIRE_WEAPON, EV_FIRE_WEAPON_LASTSHOT]
        .iter()
        .map(|ev| num(&events, &format!("own.{ev}")))
        .filter(|n| !n.is_nan())
        .sum::<f64>();
    report.metrics.insert("fire_events".into(), shots);
    if shots < 1.0 {
        return Err(format!(
            "the clip went down but no fire event was delivered: {events}"
        ));
    }
    page.capture(server, report, dir, "05-fired")?;

    // A grenade: the throw takes a frag and the explosion arrives as a world event.
    let frag0 = num(&page.state(server, "hud")?, "report.frag");
    page.bind(server, "+frag", "down")?;
    page.ask(server, json!({"cmd": "step", "frames": 2}))?;
    page.bind(server, "+frag", "up")?;
    page.until(
        server,
        json!({"hud.report.frag": {"lt": frag0}}),
        &format!("the grenade count stayed at {frag0} after +frag"),
        Duration::from_secs(10),
    )?;
    page.until(
        server,
        json!({"events.world.explosion": {"ge": 1}}),
        "the grenade never exploded in the snapshots",
        Duration::from_secs(15),
    )?;
    page.capture(server, report, dir, "06-grenade")?;

    // The HUD: what the browser drew against what the native client draws, and a sound voice started.
    let hud = page.state(server, "hud")?;
    let drawn: BTreeSet<i64> = hud["report"]["drawn"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, _)| k.parse().ok())
        .collect();
    report
        .notes
        .push(format!("browser HUD ownerdraws drawn {drawn:?}"));
    report
        .metrics
        .insert("hud_drawn".into(), drawn.len() as f64);
    let browser_parity = json!({
        "hud_drawn": drawn.iter().copied().collect::<Vec<_>>(),
        "moved": dist,
        "fire_events": shots,
        "sound_started": num(&page.state(server, "sound")?, "started"),
    });
    std::fs::write(
        dir.join("browser-parity.json"),
        serde_json::to_vec_pretty(&browser_parity).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    report.files.push("web-match/browser-parity.json".into());
    let lacking: Vec<i64> = MUST_DRAW
        .iter()
        .copied()
        .filter(|id| !drawn.contains(id))
        .collect();
    if !lacking.is_empty() {
        return Err(format!(
            "the HUD never drew ownerdraws {lacking:?} (ammo 119, weapon name 81, compass map 159); drawn {drawn:?}"
        ));
    }
    let sound = page.state(server, "sound")?;
    report
        .metrics
        .insert("sound_started".into(), num(&sound, "started"));
    report
        .metrics
        .insert("sound_active".into(), num(&sound, "active"));
    if !(num(&sound, "started") >= 1.0) {
        return Err(format!("no sound voice started: {sound}"));
    }

    // The scoreboard, held up with the bind the player holds.
    page.bind(server, "+scores", "down")?;
    page.until(
        server,
        json!({"scoreboard.rows.0": {"exists": true}, "hud.scoreboard_shown": true}),
        "the scoreboard did not show",
        Duration::from_secs(20),
    )?;
    let board = page.state(server, "scoreboard")?;
    page.capture(server, report, dir, "07-scoreboard")?;
    page.bind(server, "+scores", "up")?;
    let rows = board["rows"].as_array().cloned().unwrap_or_default();
    report
        .metrics
        .insert("scoreboard_rows".into(), rows.len() as f64);
    if rows.len() < BOTS as usize + 1 {
        return Err(format!(
            "{} scoreboard rows, want {}: {board}",
            rows.len(),
            BOTS + 1
        ));
    }
    if !rows.iter().any(|r| r.to_string().contains(PLAYER)) {
        return Err(format!("the scoreboard lists no {PLAYER}: {board}"));
    }

    // Killed by a bot (the server's `devkill`, as `net-killcam` does): the killcam plays, the kill line arrives,
    // and the player is alive again afterwards.
    let bot = server
        .game
        .connected_clients()
        .find(|(_, c)| c.bot && c.session == Session::Playing)
        .map(|(n, _)| n)
        .ok_or("no bot is playing to kill the player")?;
    server
        .exec_line(&format!("devkill {slot} {bot}"))
        .map_err(|e| format!("devkill: {e}"))?;
    page.until(
        server,
        json!({"player.killcam": true}),
        "the killcam did not start after the kill",
        Duration::from_secs(30),
    )?;
    page.until(
        server,
        json!({"hud.stats.killcam_frames": {"ge": 1}}),
        "the killcam drew no frame",
        Duration::from_secs(10),
    )?;
    page.capture(server, report, dir, "08-killcam")?;
    page.until(
        server,
        json!({"hud.stats.obituaries": {"ge": 1}}),
        "no kill line arrived",
        Duration::from_secs(10),
    )?;
    page.until(
        server,
        json!({"player.alive": true, "player.killcam": false}),
        "the player did not respawn after the killcam",
        Duration::from_secs(60),
    )?;
    report.metrics.insert("respawn_ms".into(), since(started));

    // The end of the match: a score limit the bots have passed ends it (the stock script re-reads the limit), the
    // end-of-game scoreboard shows, and the rotation's next map is loaded and played in the same page. The limit
    // goes back to none once the match has ended, so the next map (whose scripts read the limit afresh) is played and
    // not ended by the first kill: the 2026-10-30 run `slot-1-cod4e-run-20261030-053052` cycled the rotation every
    // minute until the browser, loading map after map, overflowed its reliable commands and was dropped.
    server
        .exec_line("set scr_war_scorelimit 1")
        .map_err(|e| format!("scorelimit: {e}"))?;
    page.until(
        server,
        json!({"hud.intermission": true}),
        "the match did not end on the score limit",
        Duration::from_secs(90),
    )?;
    server
        .exec_line("set scr_war_scorelimit 0")
        .map_err(|e| format!("scorelimit: {e}"))?;
    page.capture(server, report, dir, "09-intermission")?;
    page.until(
        server,
        json!({"client.map": "mp_backlot", "client.in_world": true, "client.loading": false}),
        "the browser did not follow the server into mp_backlot",
        left(started).min(Duration::from_secs(400)),
    )?;
    report.metrics.insert("next_map_ms".into(), since(started));
    // The new level keeps the team: the class menu alone comes back, and the player is in the world again.
    page.until_menu(
        server,
        "changeclass",
        "the class menu did not open in the next map",
        Duration::from_secs(120),
    )?;
    page.click(server, "Assault")?;
    page.until(
        server,
        json!({"player.alive": true, "menu.open": {"len": 0}}),
        "the player did not spawn in the next map",
        Duration::from_secs(120),
    )?;
    page.capture(server, report, dir, "10-next-map")?;

    // Leaving through the Escape menu: the server drops the browser's client.
    page.bind(server, "togglemenu", "tap")?;
    page.until(
        server,
        json!({"menu.open.0": {"exists": true}}),
        "the Escape menu did not open",
        Duration::from_secs(10),
    )?;
    page.click(server, "Leave Game")?;
    page.until_menu(
        server,
        "popup_leavegame",
        "the leave popup did not open",
        Duration::from_secs(10),
    )?;
    page.capture(server, report, dir, "11-leave")?;
    page.click(server, "Yes")?;
    let gone = Instant::now() + Duration::from_secs(30);
    while human(server).is_some() {
        if Instant::now() > gone {
            return Err(
                "the server still has the browser's client 30 s after Leave Game".to_owned(),
            );
        }
        server.run_for(Duration::from_millis(100));
    }
    report
        .notes
        .push("the server dropped the browser's client after Leave Game".into());

    // Back in the main menu, Start New Server says the browser cannot host (`listen_web.rs`).
    page.until(
        server,
        json!({"client.in_world": false}),
        "the world did not go away after leaving",
        Duration::from_secs(60),
    )?;
    page.until_menu(
        server,
        "main",
        "the main menu did not come back after leaving",
        Duration::from_secs(60),
    )?;
    page.click(server, "Start New Server")?;
    page.until_menu(
        server,
        "error_popmenu",
        "Start New Server opened no popup in the browser",
        Duration::from_secs(10),
    )?;
    let message = page.ask(server, json!({"cmd": "cvar", "name": "com_errorMessage"}))?;
    let text = message["value"].as_str().unwrap_or_default().to_owned();
    if text != NO_HOSTING {
        return Err(format!("the Start New Server popup says {text:?}"));
    }
    page.capture(server, report, dir, "12-no-hosting")?;
    report.notes.push(format!("Start New Server: {text}"));
    page.ask(server, json!({"cmd": "quit"}))?;
    Ok(())
}
