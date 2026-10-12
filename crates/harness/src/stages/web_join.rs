// SPDX-License-Identifier: GPL-3.0-only
//! The browser joining a server: a native server (in this process, WebTransport on a free port, ten bots in a team
//! deathmatch on mp_crash) and `web/serve.py`; a headless Chromium opens the built page with
//! `?connect=127.0.0.1:<port>&cert=<hash>&report=N` and posts its overlay once N frames have run after the server
//! gave the player a body. Passes when the page says it is `spawned`, snapshots arrived, surfaces were drawn, and the
//! server itself lists a person (not a bot) playing. Skipped without a built `web/pkg`, a Chromium or an install.
//!
//! Stage `web-download` is the same join to a server whose map the browser's install lacks: a custom map (the stock
//! crash zone under another name, in the server's `usermaps/`) that the page must download from the server before it
//! can load it. The page is opened twice with one browser profile: the first visit downloads the map and keeps it in
//! the browser, the second finds it there and downloads nothing.
use super::web_client::{Kill, find_chrome, find_web_dir};
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::Value;
use server::client::Session;
use server::server::Server;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const PORT: u16 = 18_091;
const BOTS: u32 = 9;
/// Frames the page runs after the spawn before it reports (WebGL2 in software is slow).
const FRAMES: u32 = 15;
const LIMIT: Duration = Duration::from_secs(400);

/// What a run of the stage joins and how many times the page is opened.
struct Scenario {
    name: &'static str,
    /// A custom map the server has and the browser's install lacks, else the stock `mp_crash`.
    custom_map: bool,
}

const JOIN: Scenario = Scenario {
    name: "web-join",
    custom_map: false,
};
const DOWNLOAD: Scenario = Scenario {
    name: "web-download",
    custom_map: true,
};
/// `cg_fov` in the config the page starts with.
const SEEDED_FOV: f64 = 100.0;
/// The custom map's name: the stock crash zone under another one.
const CUSTOM: &str = "mp_crashcopy";

/// A scratch install for the server: the real content linked in and the custom map in `usermaps/`.
fn custom_install(real: &Path, dir: &Path) -> io::Result<PathBuf> {
    let root = dir.join("install");
    let map_dir = root.join("usermaps").join(CUSTOM);
    std::fs::create_dir_all(&map_dir)?;
    for entry in std::fs::read_dir(real)? {
        let entry = entry?;
        let name = entry.file_name();
        if name != "usermaps" {
            link(&entry.path(), &root.join(name))?;
        }
    }
    let zone = real.join("zone/english/mp_crash.ff");
    link(&zone, &map_dir.join(format!("{CUSTOM}.ff")))?;
    Ok(root)
}

/// A folder this stage made outside its own, removed when the stage ends.
struct Tmp(PathBuf);

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn link(from: &Path, to: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(from, to)
}

#[cfg(not(unix))]
fn link(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::copy(from, to).map(|_| ())
}

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
    join(ctx, &JOIN)
}

pub fn run_download(ctx: &StageCtx) -> io::Result<StageReport> {
    join(ctx, &DOWNLOAD)
}

fn join(ctx: &StageCtx, sc: &Scenario) -> io::Result<StageReport> {
    let (name, map) = (sc.name, if sc.custom_map { CUSTOM } else { "mp_crash" });
    let skip = |why: &str| Ok(StageReport::new(name, Status::Skipped).with_reason(why));
    let Some(web) = find_web_dir() else {
        return skip("web/pkg is not built (web/build.sh) or web/ is not next to the harness");
    };
    let Some(chrome) = find_chrome() else {
        return skip("no Chromium found (set COD4E_CHROME)");
    };
    let Some(install) = ctx.install.clone() else {
        return skip("no original install");
    };
    // The server's install: the real one, or with the custom map the browser's install will lack. It lies outside the
    // stage's folder, which goes into the bundle (links to the whole install).
    let scratch = Tmp(std::env::temp_dir().join(format!("cod4e-webdl-{}", std::process::id())));
    let server_install = if sc.custom_map {
        if !install.join("zone/english/mp_crash.ff").is_file() {
            return skip("the install has no zone/english/mp_crash.ff to copy as a custom map");
        }
        custom_install(&install, &scratch.0)?
    } else {
        install.clone()
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
    let mut server = match Server::boot(&server_install, &args, false) {
        Ok(s) => s,
        Err(e) => return Ok(StageReport::new(name, Status::Failed).with_reason(e)),
    };
    for line in [
        "set g_gametype war",
        "set scr_war_timelimit 0",
        "set scr_war_scorelimit 0",
        "set sv_allowDownload 1",
        &format!("set sv_mapRotation \"gametype war map {map}\""),
        &format!("map {map}"),
    ] {
        if let Err(e) = server.exec_line(line) {
            return Ok(StageReport::new(name, Status::Failed).with_reason(format!("{line}: {e}")));
        }
    }
    if let Err(e) = server.exec_line(&format!("bots {BOTS}")) {
        return Ok(StageReport::new(name, Status::Failed).with_reason(e));
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
        return Ok(StageReport::new(name, Status::Failed)
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
    // The browser starts with a kept config_mp.cfg that sets the field of view: the client must read it back.
    let seed = format!(
        "%7B%22players%2Fprofiles%2Fdefault%2Fconfig_mp.cfg%22%3A%22seta%20cg_fov%20%5C%22{SEEDED_FOV}%5C%22%5Cn%22%7D"
    );
    let page = format!(
        "http://localhost:{PORT}/?dev-install=/install&autostart&debug&backend=webgl&seed={seed}\
         &connect=127.0.0.1:{wt_port}&cert={hash}&report={FRAMES}&args=--autoplay%20--duration%20100000%20--no-sound"
    );
    let open = |server: &mut Server| visit(server, &chrome, &page, &profile, &rx);
    let mut seen = open(&mut server)?;
    // The page's second visit, in the same browser: what the first one kept is there.
    let revisit = if sc.custom_map && seen.report.is_some() {
        Some(open(&mut server)?)
    } else {
        None
    };
    let _ = std::fs::remove_dir_all(&profile);
    let (human_at, max_humans, humans_now, net) =
        (seen.human_at, seen.max_humans, seen.humans_now, seen.net);

    let mut r = StageReport::new(name, Status::Passed);
    let Some(v) = seen.report.take() else {
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
    // The seeded config was read: the horizontal field of view is `cg_fov` at 4:3 widened for the canvas (Hor+).
    let (cw, ch) = o["canvas"]
        .as_str()
        .and_then(|c| c.split_once('x'))
        .and_then(|(w, h)| Some((w.parse::<f64>().ok()?, h.parse::<f64>().ok()?)))
        .unwrap_or((f64::NAN, f64::NAN));
    let want_fov = 2.0
        * ((SEEDED_FOV.to_radians() / 2.0).tan() * 0.75 * cw / ch)
            .atan()
            .to_degrees();
    let fov = num(o, "fov_x");
    r.metrics.insert("fov_x".into(), fov);
    if fov.is_nan() || want_fov.is_nan() || (fov - want_fov).abs() >= 0.2 {
        why.push(format!(
            "the field of view is {fov}, not the {want_fov:.1} the kept settings (cg_fov {SEEDED_FOV}) give on a {cw}x{ch} canvas"
        ));
    }
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
    if let Some(second) = revisit {
        let saved_first = saved_files(&v);
        let want = format!("usermaps/{CUSTOM}/{CUSTOM}.ff");
        if !saved_first.contains(&want) {
            why.push(format!(
                "the first visit did not download {want} (it kept {saved_first:?})"
            ));
        }
        match second.report {
            None => why.push("the second visit gave no report".into()),
            Some(v2) => {
                if v2["overlay"]["net phase"] != "spawned" {
                    why.push(format!(
                        "the second visit's net phase is {}",
                        v2["overlay"]["net phase"]
                    ));
                }
                let again = saved_files(&v2);
                if !again.is_empty() {
                    why.push(format!(
                        "the second visit downloaded {again:?} instead of finding it kept in the browser"
                    ));
                }
            }
        }
    }
    if !why.is_empty() {
        return Ok(fail(r, why.join("; ")));
    }
    Ok(r)
}

/// What one opening of the page showed.
struct Visit {
    report: Option<Value>,
    human_at: Option<f64>,
    max_humans: usize,
    humans_now: usize,
    net: Option<server::netsv::NetStats>,
}

/// The files the page says the client had it keep during a visit.
fn saved_files(report: &Value) -> Vec<String> {
    report["saved"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| f.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Opens `page` in a fresh browser process on `profile` (so what an earlier visit stored is there) while `server`
/// runs, until the page's report arrives or [`LIMIT`] passes.
fn visit(
    server: &mut Server,
    chrome: &Path,
    page: &str,
    profile: &Path,
    rx: &mpsc::Receiver<String>,
) -> io::Result<Visit> {
    let started = Instant::now();
    let browser = Kill(
        Command::new(chrome)
            .args([
                "--headless=new",
                "--no-first-run",
                "--ignore-gpu-blocklist",
                "--use-angle=swiftshader",
                "--enable-unsafe-swiftshader",
                "--window-size=1280,720",
            ])
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(page)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| io::Error::other(format!("cannot start {}: {e}", chrome.display())))?,
    );
    // The server runs here while the page loads; note when it first has a person playing.
    let mut v = Visit {
        report: None,
        human_at: None,
        max_humans: 0,
        humans_now: 0,
        net: None,
    };
    while started.elapsed() < LIMIT && v.report.is_none() {
        server.run_for(Duration::from_millis(100));
        let humans = server
            .game
            .connected_clients()
            .filter(|(_, c)| !c.bot)
            .count();
        v.max_humans = v.max_humans.max(humans);
        let playing = server
            .game
            .connected_clients()
            .any(|(_, c)| !c.bot && matches!(c.session, Session::Playing | Session::Dead));
        if playing && v.human_at.is_none() {
            v.human_at = Some(started.elapsed().as_secs_f64() * 1000.0);
        }
        if let Ok(line) = rx.try_recv() {
            v.report = serde_json::from_str(&line).ok();
        }
    }
    v.humans_now = server
        .game
        .connected_clients()
        .filter(|(_, c)| !c.bot)
        .count();
    v.net = server.net_stats();
    drop(browser);
    Ok(v)
}
