// SPDX-License-Identifier: GPL-3.0-only
//! The watchdog parent: launches each stage as a child process, captures its
//! output, exit status and minidump, enforces the timeout, and keeps going
//! after a crash so the bundle always gets written.
use crate::child::{RESULT_FILE, TRACE_FILE};
use crate::stage::{StageReport, Status};
use minidumper::{LoopAction, MinidumpBinary, Server, ServerHandler, SocketName};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Which stage a child runs.
#[derive(Clone, Debug)]
pub enum Selection {
    Stage(&'static str),
    Script(PathBuf),
}

/// Receives minidumps from crashing children and writes them into whichever
/// stage directory is current.
pub struct DumpServer {
    pub socket: String,
    dir: Arc<Mutex<PathBuf>>,
    shutdown: Arc<AtomicBool>,
    socket_file: PathBuf,
}

struct Handler(Arc<Mutex<PathBuf>>);

impl ServerHandler for Handler {
    fn create_minidump_file(&self) -> Result<(File, PathBuf), io::Error> {
        let path = self.0.lock().unwrap().join("crash.dmp");
        Ok((File::create(&path)?, path))
    }
    fn on_minidump_created(&self, result: Result<MinidumpBinary, minidumper::Error>) -> LoopAction {
        match result {
            Ok(mut dump) => {
                use std::io::Write;
                let _ = dump.file.flush();
            }
            Err(e) => eprintln!("minidump failed: {e}"),
        }
        LoopAction::Continue
    }
    fn on_message(&self, _: u32, _: Vec<u8>) {}
}

impl DumpServer {
    pub fn start(scratch: &Path) -> Result<Self, String> {
        let socket_file = scratch.join(format!("dump-{}.sock", std::process::id()));
        let socket = socket_file.to_string_lossy().into_owned();
        let mut server = Server::with_name(SocketName::path(&socket)).map_err(|e| e.to_string())?;
        let dir = Arc::new(Mutex::new(scratch.to_owned()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (handler, flag) = (Handler(dir.clone()), shutdown.clone());
        std::thread::Builder::new()
            .name("minidump-server".into())
            .spawn(move || {
                if let Err(e) = server.run(Box::new(handler), &flag, None) {
                    eprintln!("minidump server stopped: {e}");
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            socket,
            dir,
            shutdown,
            socket_file,
        })
    }

    fn set_dir(&self, dir: &Path) {
        *self.dir.lock().unwrap() = dir.to_owned();
    }
}

impl Drop for DumpServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = fs::remove_file(&self.socket_file);
    }
}

pub struct StageRun<'a> {
    pub exe: &'a Path,
    pub selection: &'a Selection,
    pub name: &'a str,
    /// The stage directory; files land here.
    pub dir: &'a Path,
    /// Bundle-relative prefix of `dir`.
    pub rel: &'a str,
    pub install: Option<&'a Path>,
    pub timeout: Duration,
    pub dumps: Option<&'a DumpServer>,
}

fn copy_to_file(
    mut from: impl io::Read + Send + 'static,
    path: PathBuf,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        if let Ok(mut f) = File::create(path) {
            let _ = io::copy(&mut from, &mut f);
        }
    })
}

#[cfg(unix)]
fn died_abnormally(status: &std::process::ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(|s| format!("killed by signal {s}"))
}

#[cfg(windows)]
fn died_abnormally(status: &std::process::ExitStatus) -> Option<String> {
    // NTSTATUS exception codes have the two high bits set.
    status
        .code()
        .map(|c| c as u32)
        .filter(|c| c & 0xC000_0000 == 0xC000_0000)
        .map(|c| format!("exception {c:#010x}"))
}

/// Run one stage in a child and report what happened, whatever happened.
pub fn run_stage(run: &StageRun) -> StageReport {
    let StageRun { name, dir, .. } = *run;
    let fail = |status, reason: String| StageReport::new(name, status).with_reason(reason);
    if let Err(e) = fs::create_dir_all(dir) {
        return fail(
            Status::Failed,
            format!("cannot create {}: {e}", dir.display()),
        );
    }
    if let Some(d) = run.dumps {
        d.set_dir(dir);
    }
    let mut cmd = Command::new(run.exe);
    cmd.arg("__child");
    match run.selection {
        Selection::Stage(s) => cmd.args(["--stage", s]),
        Selection::Script(p) => cmd.arg("--script").arg(p),
    };
    cmd.arg("--dir").arg(dir);
    if let Some(i) = run.install {
        cmd.arg("--install").arg(i);
    }
    if let Some(d) = run.dumps {
        cmd.args(["--dump-socket", &d.socket]);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(Status::Failed, format!("cannot launch child: {e}")),
    };
    let readers = [
        child
            .stdout
            .take()
            .map(|s| copy_to_file(s, dir.join("stdout.log"))),
        child
            .stderr
            .take()
            .map(|s| copy_to_file(s, dir.join("stderr.log"))),
    ];
    let mut hung = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) if started.elapsed() >= run.timeout => {
                hung = true;
                let _ = child.kill();
                break child.wait();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => break Err(e),
        }
    };
    for r in readers.into_iter().flatten() {
        let _ = r.join();
    }
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let status = match status {
        Ok(s) => s,
        Err(e) => return fail(Status::Failed, format!("waiting for child: {e}")),
    };

    let abnormal = died_abnormally(&status);
    if abnormal.is_some() && run.dumps.is_some() {
        // The dump is written by the watchdog's server, possibly just after
        // the child is gone.
        let give_up = Instant::now() + Duration::from_secs(5);
        while !dir.join("crash.dmp").exists() && Instant::now() < give_up {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let child_report = fs::read(dir.join(RESULT_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<StageReport>(&b).ok());

    let mut report = if hung {
        let mut r = child_report.unwrap_or_else(|| StageReport::new(name, Status::Hung));
        r.status = Status::Hung;
        r.reason = Some(format!(
            "no result after {} s; killed",
            run.timeout.as_secs()
        ));
        r
    } else if let Some(why) = abnormal {
        let mut r = child_report.unwrap_or_else(|| StageReport::new(name, Status::Crashed));
        r.status = Status::Crashed;
        r.reason = Some(why);
        r
    } else if let Some(r) = child_report.filter(|_| status.success()) {
        r
    } else {
        let code = status.code().map_or("none".into(), |c| c.to_string());
        let why = if status.code() == Some(101) {
            "child panicked (exit code 101); see stderr.log".to_owned()
        } else {
            format!("child exited with code {code} and wrote no result")
        };
        fail(Status::Failed, why)
    };
    report.name = name.to_owned();
    report.wall_ms = wall_ms;
    report.exit_code = status.code();
    if dir.join("crash.dmp").exists() {
        report.notes.push("minidump captured: crash.dmp".into());
    }
    report.files = [
        "stdout.log",
        "stderr.log",
        RESULT_FILE,
        "crash.dmp",
        TRACE_FILE,
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(report.files.iter().cloned())
    .filter(|f| dir.join(f).exists())
    .collect::<std::collections::BTreeSet<_>>()
    .into_iter()
    .map(|f| format!("{}/{f}", run.rel))
    .collect();
    report
}
