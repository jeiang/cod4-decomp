// SPDX-License-Identifier: GPL-3.0-only
//! The child side: runs one stage in its own process so a crash or hang
//! cannot take the watchdog with it.
use crate::stage::{self, Kind, StageCtx, StageReport, Status};
use crate::trace::{self, LogLayer};
use std::path::PathBuf;
use std::time::Duration;
use tracing_subscriber::layer::SubscriberExt;

/// One span window per stage, capped so the trace stays small.
const TRACE_WINDOW: Duration = Duration::from_secs(30);
const TRACE_CAP: usize = 200_000;

pub struct ChildArgs {
    pub stage: Option<String>,
    pub script: Option<PathBuf>,
    pub dir: PathBuf,
    pub install: Option<PathBuf>,
    pub dump_socket: Option<String>,
}

pub const RESULT_FILE: &str = "result.json";
pub const TRACE_FILE: &str = "trace.json";

/// Run the stage; returns the process exit code. The report is written to
/// `result.json` before returning.
pub fn run(args: ChildArgs) -> i32 {
    // Keeps the crash handler attached for the life of the stage.
    let _crash_guard = args.dump_socket.as_deref().and_then(attach_crash_handler);

    let (layer, trace_handle) = trace::chrome_trace(TRACE_WINDOW, TRACE_CAP);
    let subscriber = tracing_subscriber::registry().with(LogLayer).with(layer);
    let _ = tracing::subscriber::set_global_default(subscriber);

    let ctx = StageCtx {
        dir: args.dir.clone(),
        install: args.install,
    };
    let name = args.stage.clone().unwrap_or_else(|| {
        args.script
            .as_deref()
            .map(stage::script_stage_name)
            .unwrap_or_default()
    });
    let started = std::time::Instant::now();
    let report = {
        let _span = tracing::info_span!("stage", name = %name).entered();
        match (&args.stage, &args.script) {
            (Some(s), _) => match stage::find(s) {
                Some(def) => match def.kind {
                    Kind::Builtin(f) => f(&ctx).unwrap_or_else(|e| {
                        StageReport::new(&name, Status::Failed).with_reason(e.to_string())
                    }),
                    Kind::Script(src) => stage::run_script(&name, src),
                    Kind::Server(src) => crate::stages::server::run(&ctx, &name, src),
                },
                None => StageReport::new(&name, Status::Failed).with_reason("unknown stage"),
            },
            (None, Some(path)) => match std::fs::read_to_string(path) {
                Ok(src) if ctx.install.is_some() => crate::stages::server::run(&ctx, &name, &src),
                Ok(src) => stage::run_script(&name, &src),
                Err(e) => StageReport::new(&name, Status::Failed)
                    .with_reason(format!("cannot read script: {e}")),
            },
            (None, None) => StageReport::new(&name, Status::Failed).with_reason("no stage given"),
        }
    };
    let mut report = report;
    report.wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    if !trace_handle.is_empty() && trace_handle.write(&args.dir.join(TRACE_FILE)).is_ok() {
        report.files.push(TRACE_FILE.into());
    }
    match serde_json::to_vec_pretty(&report)
        .map_err(std::io::Error::other)
        .and_then(|j| std::fs::write(args.dir.join(RESULT_FILE), j))
    {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("cannot write {RESULT_FILE}: {e}");
            1
        }
    }
}

fn attach_crash_handler(socket: &str) -> Option<crash_handler::CrashHandler> {
    let client = match minidumper::Client::with_name(minidumper::SocketName::path(socket)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("crash capture unavailable in child: {e}");
            return None;
        }
    };
    // SAFETY: the callback runs inside a crash handler. It only talks to the
    // watchdog over the already-open IPC channel, as minidumper documents.
    let event = unsafe {
        crash_handler::make_crash_event(move |ctx: &crash_handler::CrashContext| {
            crash_handler::CrashEventResult::Handled(client.request_dump(ctx).is_ok())
        })
    };
    match crash_handler::CrashHandler::attach(event) {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("crash handler not attached: {e}");
            None
        }
    }
}
