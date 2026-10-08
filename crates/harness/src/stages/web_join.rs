// SPDX-License-Identifier: GPL-3.0-or-later
//! The browser joining a server: a native server (in this process, WebTransport on a free port, ten bots in a team
//! deathmatch on mp_crash) and `web/serve.py`; a headless Chromium opens the built page with
//! `?connect=127.0.0.1:<port>&cert=<hash>&report=N` and posts its overlay once N frames have run after the server
//! gave the player a body. Passes when the page says it is `spawned`, snapshots arrived, surfaces were drawn, and the
//! server itself lists a person (not a bot) playing. Skipped without a built `web/pkg`, a Chromium or an install.
use super::web_client::{Kill, find_chrome, find_web_dir};
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::Value;
use server::client::Session;
use server::server::Server;
use std::io::{self, BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const NAME: &str = "web-join";
const PORT: u16 = 18_091;
const BOTS: u32 = 9;
/// Frames the page runs after the spawn before it reports (WebGL2 in software is slow).
const FRAMES: u32 = 15;
const LIMIT: Duration = Duration::from_secs(400);

fn fail(mut r: StageReport, why: impl Into<String>) -> StageReport {
    r.status = Status::Failed;
    r.reason = Some(why.into());
    r
}

/// The page's overlay field `k` as a number (the overlay stringifies some).
fn num(o: &Value, k: &str) -> f64 {
    o[k].as_f64()
        .or_else(|| o[k].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(f64::NAN)
}

/// "open sent 120 received 340 streams ..." -> the number after `word`.
fn after(s: &str, word: &str) -> f64 {
    let mut it = s.split_whitespace();
    it.find(|w| *w == word);
    it.next().and_then(|n| n.parse().ok()).unwrap_or(f64::NAN)
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let skip = |why: &str| Ok(StageReport::new(NAME, Status::Skipped).with_reason(why));
    let Some(web) = find_web_dir() else {
        return skip("web/pkg is not built (web/build.sh) or web/ is not next to the harness");
    };
    let Some(chrome) = find_chrome() else {
        return skip("no Chromium found (set COD4E_CHROME)");
    };
    let Some(install) = ctx.install.clone() else {
        return skip("no original install");
    };
    let info = ctx.dir.join("wt-info.json");
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
    for line in [
        "set g_gametype war",
        "set scr_war_timelimit 0",
        "set scr_war_scorelimit 0",
        "set sv_mapRotation \"gametype war map mp_crash\"",
        "map mp_crash",
    ] {
        if let Err(e) = server.exec_line(line) {
            return Ok(StageReport::new(NAME, Status::Failed).with_reason(format!("{line}: {e}")));
        }
    }
    if let Err(e) = server.exec_line(&format!("bots {BOTS}")) {
        return Ok(StageReport::new(NAME, Status::Failed).with_reason(e));
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
    // The hash is base64: '+', '/' and '=' must survive the query string.
    let hash = hash
        .replace('+', "%2B")
        .replace('/', "%2F")
        .replace('=', "%3D");

    let mut py = Command::new("python3")
        .arg(web.join("serve.py"))
        .arg(PORT.to_string())
        .arg("--install")
        .arg(&install)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = py
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no server output"))?;
    let _py = Kill(py);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(json) = line.strip_prefix("REPORT ") {
                let _ = tx.send(json.to_owned());
            }
        }
    });
    std::thread::sleep(Duration::from_millis(800));

    let profile = std::env::temp_dir().join(format!("cod4e-webjoin-{}", std::process::id()));
    let page = format!(
        "http://localhost:{PORT}/?dev-install=/install&autostart&debug&backend=webgl\
         &connect=127.0.0.1:{wt_port}&cert={hash}&report={FRAMES}&args=--autoplay%20--duration%20100000%20--no-sound"
    );
    let started = Instant::now();
    let browser = Kill(
        Command::new(&chrome)
            .args([
                "--headless=new",
                "--no-first-run",
                "--ignore-gpu-blocklist",
                "--use-angle=swiftshader",
                "--enable-unsafe-swiftshader",
                "--window-size=1280,720",
            ])
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(&page)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| io::Error::other(format!("cannot start {}: {e}", chrome.display())))?,
    );

    // The server runs here while the page loads; note when it first has a person playing.
    let mut human_at: Option<f64> = None;
    let mut report: Option<Value> = None;
    let mut max_humans = 0usize;
    while started.elapsed() < LIMIT && report.is_none() {
        server.run_for(Duration::from_millis(100));
        let humans = server
            .game
            .connected_clients()
            .filter(|(_, c)| !c.bot)
            .count();
        max_humans = max_humans.max(humans);
        let playing = server
            .game
            .connected_clients()
            .any(|(_, c)| !c.bot && matches!(c.session, Session::Playing | Session::Dead));
        if playing && human_at.is_none() {
            human_at = Some(started.elapsed().as_secs_f64() * 1000.0);
        }
        if let Ok(line) = rx.try_recv() {
            report = serde_json::from_str(&line).ok();
        }
    }
    let humans_now = server
        .game
        .connected_clients()
        .filter(|(_, c)| !c.bot)
        .count();
    let net = server.net_stats();
    drop(browser);
    let _ = std::fs::remove_dir_all(&profile);

    let mut r = StageReport::new(NAME, Status::Passed);
    let Some(v) = report else {
        return Ok(fail(
            r,
            format!(
                "no page report within {} s; server saw {max_humans} people, one playing: {}",
                LIMIT.as_secs(),
                human_at.is_some()
            ),
        ));
    };
    let o = &v["overlay"];
    let page_net = o["net"].as_str().unwrap_or("");
    let received = after(page_net, "received");
    let snapshots = num(o, "net snapshots");
    let m = &mut r.metrics;
    m.insert("connect_ms".into(), num(o, "spawned at (ms)"));
    m.insert("snapshots".into(), snapshots);
    m.insert("page_datagrams_in".into(), received);
    m.insert("cpu_ms".into(), num(o, "CPU ms/frame (last 60)"));
    m.insert("wasm_mib".into(), num(o, "wasm memory MiB"));
    m.insert("frames".into(), num(o, "frames"));
    m.insert("surfaces".into(), num(o, "surfaces drawn"));
    m.insert("server_people".into(), humans_now as f64);
    if let Some(n) = net {
        m.insert("server_snapshots_out".into(), n.snapshots_out as f64);
        m.insert("server_joins".into(), n.joins as f64);
    }
    r.notes.push(format!(
        "{}; page: {page_net}; server saw a person playing at {:.0} ms",
        o["backend"],
        human_at.unwrap_or(f64::NAN)
    ));
    let mut why = Vec::new();
    if o["net phase"] != "spawned" {
        why.push(format!("the page's net phase is {}", o["net phase"]));
    }
    if !page_net.starts_with("open") {
        why.push(format!("the WebTransport state is `{page_net}`"));
    }
    if snapshots.is_nan() || snapshots < f64::from(FRAMES) {
        why.push(format!("only {snapshots} snapshots reached the page"));
    }
    if num(o, "surfaces drawn").is_nan() || num(o, "surfaces drawn") <= 0.0 {
        why.push("the page drew no surfaces".into());
    }
    if human_at.is_none() {
        why.push(format!(
            "the server never had a person playing ({max_humans} connected)"
        ));
    }
    if humans_now == 0 {
        why.push("the server lost the browser's client".into());
    }
    if net.is_none_or(|n| n.snapshots_out == 0) {
        why.push("the server sent no snapshots".into());
    }
    if !why.is_empty() {
        return Ok(fail(r, why.join("; ")));
    }
    Ok(r)
}
