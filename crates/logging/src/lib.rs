// SPDX-License-Identifier: GPL-3.0-only
//! One `tracing` subscriber setup for every binary (client, server, master, harness, the browser client).
//!
//! * Filter: `COD4E_LOG` (else `RUST_LOG`) in `EnvFilter` syntax; without it, our crates log at `info` and
//!   everything else at `warn`. [`set_developer`] (the `developer` cvar) and [`set_filter`] (the `log_filter` cvar)
//!   change it at run time. A disabled level costs one cached atomic load at the call site.
//! * Output: stderr, and a log file per run in `COD4E_LOG_DIR` or the directory the binary passes (the newest
//!   [`KEEP`] files of an app are kept). In the browser, the page's console.
//! * Console: `warn` and `error` events are queued for the in-game console ([`drain_console`]) once a front end
//!   calls [`forward_to_console`].
//! * [`dedup!`] rate-limits an event that repeats.
//! * [`capture`] collects the events of a closure, for tests.

mod dedup;
mod format;
#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(target_arch = "wasm32")]
mod web;

pub use dedup::Dedup;
pub use format::Record;
pub use tracing;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::Registry;
use tracing_subscriber::reload;

/// How many log files of one app are kept; the oldest go when a run starts.
pub const KEEP: usize = 10;

/// The crates of this workspace: the targets `developer` raises.
const OURS: &[&str] = &[
    "assets", "audio", "client", "fx", "gsc", "harness", "install", "logging", "master", "net",
    "render", "server", "sim", "sm3",
];

/// A boxed extra layer, for a binary's own needs (the harness's Chrome trace).
pub type BoxLayer = Box<dyn Layer<Registry> + Send + Sync>;

pub struct Config<'a> {
    /// Names the log file: `<app>-<unix seconds>-<pid>.log`.
    pub app: &'a str,
    /// Where the log file goes when `COD4E_LOG_DIR` is not set; `None` for no file.
    pub dir: Option<PathBuf>,
    /// Write events to stderr (native only).
    pub stderr: bool,
    pub extra: Vec<BoxLayer>,
}

impl<'a> Config<'a> {
    pub fn new(app: &'a str) -> Self {
        Self {
            app,
            dir: None,
            stderr: true,
            extra: Vec::new(),
        }
    }
}

/// Keeps the run's log file path; drop it last.
pub struct Guard {
    path: Option<PathBuf>,
}

impl Guard {
    /// The log file of this run, when one is written.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

type SetFilter = Box<dyn Fn(EnvFilter) -> bool + Send + Sync>;

struct Control {
    set: SetFilter,
    /// The spec in force at start (the environment's, or the default); `developer 0` returns to it.
    initial: String,
}

static CONTROL: OnceLock<Control> = OnceLock::new();

fn default_spec() -> String {
    let mut s = String::from("warn");
    for c in OURS {
        s.push_str(&format!(",{c}=info"));
    }
    s
}

fn initial_spec() -> String {
    ["COD4E_LOG", "RUST_LOG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
        .unwrap_or_else(default_spec)
}

/// Installs the global subscriber. A second call (a test process, a listen server inside the client) changes
/// nothing and returns a guard without a file.
pub fn init(cfg: Config<'_>) -> Guard {
    let spec = initial_spec();
    let filter = EnvFilter::try_new(&spec).unwrap_or_else(|_| EnvFilter::new(default_spec()));
    let (filter, handle) = reload::Layer::new(filter);
    let mut layers: Vec<BoxLayer> = cfg.extra;
    layers.push(Box::new(ConsoleLayer));
    #[cfg(not(target_arch = "wasm32"))]
    let path = native::layers(cfg.app, cfg.dir, cfg.stderr, &mut layers);
    #[cfg(target_arch = "wasm32")]
    let path = {
        let _ = (cfg.dir, cfg.stderr);
        layers.push(Box::new(web::ConsoleOut));
        None::<PathBuf>
    };
    let subscriber = Registry::default().with(layers).with(filter);
    if tracing::subscriber::set_global_default(subscriber).is_err() {
        return Guard { path: None };
    }
    let _ = CONTROL.set(Control {
        set: Box::new(move |f| handle.reload(f).is_ok()),
        initial: spec,
    });
    #[cfg(not(target_arch = "wasm32"))]
    native::log_panics();
    tracing::info!(app = cfg.app, file = ?path, "logging started");
    Guard { path }
}

/// Replaces the filter with `spec` (`EnvFilter` syntax). Returns the parse error and keeps the old filter when it
/// does not parse; empty restores the start-up filter.
pub fn set_filter(spec: &str) -> Result<(), String> {
    let Some(c) = CONTROL.get() else {
        return Ok(());
    };
    let spec = if spec.trim().is_empty() {
        &c.initial
    } else {
        spec
    };
    let f = EnvFilter::try_new(spec).map_err(|e| e.to_string())?;
    if (c.set)(f) {
        Ok(())
    } else {
        Err("logging is shut down".into())
    }
}

/// The stock `developer` levels: 0 is the normal filter, 1 logs our crates at `debug`, 2 at `trace`.
pub fn set_developer(level: i64) {
    let lvl = match level {
        ..=0 => return drop(set_filter("")),
        1 => "debug",
        _ => "trace",
    };
    let mut s = String::from("warn");
    for c in OURS {
        s.push_str(&format!(",{c}={lvl}"));
    }
    let _ = set_filter(&s);
}

static FORWARD: AtomicBool = AtomicBool::new(false);
static QUEUE: Mutex<VecDeque<(Level, String)>> = Mutex::new(VecDeque::new());
const QUEUE_CAP: usize = 256;

/// A front end with an in-game console calls this once; from then on `warn` and `error` events queue for it.
pub fn forward_to_console() {
    FORWARD.store(true, Ordering::Relaxed);
}

/// The queued `warn` and `error` lines, oldest first. One atomic load when there are none.
pub fn drain_console() -> Vec<(Level, String)> {
    if !FORWARD.load(Ordering::Relaxed) {
        return Vec::new();
    }
    let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    q.drain(..).collect()
}

struct ConsoleLayer;

impl<S: Subscriber> Layer<S> for ConsoleLayer {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let level = *event.metadata().level();
        if level > Level::WARN || !FORWARD.load(Ordering::Relaxed) {
            return;
        }
        let line = format::line(event);
        let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
        if q.len() >= QUEUE_CAP {
            q.pop_front();
        }
        q.push_back((level, line));
    }
}

/// Runs `f` with a subscriber that records every event (all levels), and returns them with `f`'s result. For tests
/// that check an unexpected state is logged at the right level.
pub fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<Record>) {
    let sink = format::Sink::default();
    let subscriber = Registry::default().with(format::CaptureLayer(sink.clone()));
    let r = tracing::subscriber::with_default(subscriber, f);
    (r, sink.take())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_records_level_target_and_fields() {
        let (_, events) = capture(|| {
            let _s = tracing::info_span!("load", map = "mp_crash").entered();
            tracing::warn!(asset = "x", "missing");
            tracing::trace!("detail");
        });
        let w = events.iter().find(|e| e.level == Level::WARN).unwrap();
        assert_eq!(w.message, "missing");
        assert_eq!(w.field("asset"), Some("x"));
        assert!(events.iter().any(|e| e.level == Level::TRACE));
    }

    #[test]
    fn dedup_macro_logs_the_first_and_doubling_hits() {
        let (_, events) = capture(|| {
            for _ in 0..5 {
                dedup!(WARN, "k", "no such thing");
            }
        });
        let seen: Vec<_> = events.iter().filter_map(|e| e.field("seen")).collect();
        assert_eq!(seen, ["1", "2", "4"]);
    }

    #[test]
    fn console_queue_keeps_warnings_only_once_forwarded() {
        // One test owns the process-wide flag; the layer is driven directly with its own subscriber.
        let sub = Registry::default().with(ConsoleLayer);
        tracing::subscriber::with_default(sub, || {
            tracing::warn!("before");
            forward_to_console();
            tracing::warn!(cmd = "foo", "unknown command");
            tracing::info!("not a problem");
        });
        let lines = drain_console();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].1.contains("unknown command") && lines[0].1.contains("cmd=foo"));
    }
}
