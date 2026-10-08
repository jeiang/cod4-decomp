// SPDX-License-Identifier: GPL-3.0-only
//! The browser client: `web/serve.py` serves the built page and the install, a headless Chromium loads it with
//! `?dev-install=/install&autostart&flythrough&report=N`, and the page posts its overlay values (backend, frame
//! times, memory) back to the server's log. Skipped unless the wasm is built (`web/build.sh`), a Chromium is found
//! (`COD4E_CHROME`, or a well-known name) and the install is there. WebGL2 must render; WebGPU is recorded when the
//! headless browser has it.
use crate::stage::{StageCtx, StageReport, Status};
use serde_json::Value;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const NAME: &str = "web-client";
const FRAMES: u32 = 120;
const PORT: u16 = 18_090;

pub(super) fn find_web_dir() -> Option<PathBuf> {
    let ok = |d: &Path| d.join("serve.py").is_file() && d.join("pkg/cod4e_bg.wasm").is_file();
    if let Some(d) = std::env::var_os("COD4E_WEB_DIR").map(PathBuf::from) {
        return ok(&d).then_some(d);
    }
    let start = std::env::current_exe().ok()?;
    start
        .ancestors()
        .map(|a| a.join("web"))
        .chain(std::env::current_dir().ok().map(|c| c.join("web")))
        .find(|d| ok(d))
}

pub(super) fn find_chrome() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("COD4E_CHROME") {
        return Some(p.into());
    }
    const NAMES: &[&str] = &[
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "chrome",
    ];
    let path = std::env::var_os("PATH")?;
    let found = std::env::split_paths(&path)
        .flat_map(|d| NAMES.iter().map(move |n| d.join(n)))
        .find(|p| p.is_file());
    found.or_else(|| {
        let mac = PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
        mac.is_file().then_some(mac)
    })
}

pub(super) struct Kill(pub Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One headless page load with `backend`; the posted report, or why there is none.
fn run_backend(
    chrome: &Path,
    rx: &mpsc::Receiver<String>,
    backend: &str,
    profile: &Path,
) -> Result<Value, String> {
    let url = format!(
        "http://localhost:{PORT}/?dev-install=/install&autostart&flythrough&report={FRAMES}&backend={backend}"
    );
    let _browser = Kill(
        Command::new(chrome)
            .args([
                "--headless=new",
                "--no-first-run",
                "--enable-unsafe-webgpu",
                "--ignore-gpu-blocklist",
                "--use-angle=swiftshader",
                "--enable-unsafe-swiftshader",
                "--window-size=1280,720",
            ])
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(&url)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", chrome.display()))?,
    );
    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left.max(Duration::from_millis(1))) {
            Ok(line) => {
                let v: Value =
                    serde_json::from_str(&line).map_err(|e| format!("bad report: {e}"))?;
                if v["overlay"]["backend"]
                    .as_str()
                    .is_some_and(|b| backend == "webgpu" || !b.contains("webgpu"))
                {
                    return Ok(v);
                }
            }
            Err(_) => return Err(format!("no report within 240 s on {backend}")),
        }
    }
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
    let mut server = Command::new("python3")
        .arg(web.join("serve.py"))
        .arg(PORT.to_string())
        .arg("--install")
        .arg(&install)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = server
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no server output"))?;
    let _server = Kill(server);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(json) = line.strip_prefix("REPORT ") {
                let _ = tx.send(json.to_owned());
            }
        }
    });
    std::thread::sleep(Duration::from_millis(800));

    let mut r = StageReport::new(NAME, Status::Failed);
    for backend in ["webgl", "webgpu"] {
        // Outside the stage's directory: the browser leaves sockets in its profile that a bundle cannot hold.
        let profile =
            std::env::temp_dir().join(format!("cod4e-web-{}-{backend}", std::process::id()));
        let result = run_backend(&chrome, &rx, backend, &profile);
        let _ = std::fs::remove_dir_all(&profile);
        match result {
            Ok(v) => {
                let o = &v["overlay"];
                let num = |k: &str| {
                    o[k].as_f64()
                        .or_else(|| o[k].as_str().and_then(|s| s.parse().ok()))
                        .unwrap_or(f64::NAN)
                };
                r.metrics.insert(
                    format!("{backend}_load_ms"),
                    num("load, play click to first frame (ms)"),
                );
                r.metrics
                    .insert(format!("{backend}_cpu_ms"), num("CPU ms/frame (last 60)"));
                r.metrics
                    .insert(format!("{backend}_wasm_mib"), num("wasm memory MiB"));
                r.metrics
                    .insert(format!("{backend}_surfaces"), num("surfaces drawn"));
                r.notes.push(format!("{backend}: {}", o["backend"]));
                if backend == "webgl" && num("surfaces drawn") > 0.0 {
                    r.status = Status::Passed;
                }
            }
            Err(e) if backend == "webgl" => {
                r.reason = Some(e);
                return Ok(r);
            }
            Err(e) => r.notes.push(format!("{backend}: {e}")),
        }
    }
    if r.status != Status::Passed {
        r.reason = Some("WebGL2 drew no surfaces".into());
    }
    Ok(r)
}
