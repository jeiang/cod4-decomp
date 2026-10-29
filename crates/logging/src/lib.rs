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
mod redact;
#[cfg(target_arch = "wasm32")]
mod web;

pub use dedup::{Dedup, Hit, new_window};
pub use format::Record;
pub use redact::redact;
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

/// What a log file says when it hits its size cap and drops the rest (the harness fails a stage whose log has it).
pub const TRUNCATION_MARK: &str = "[log file reached";

/// How many log files of one app are kept; the oldest go when a run starts.
pub const KEEP: usize = 10;

/// The targets of this workspace: its crates, and its binaries (a binary-only crate logs under the binary's name,
/// `cod4e` for the client, not `client`). A test checks this against the manifests.
const OURS: &[&str] = &[
    "assets",
    "audio",
    "client",
    "cod4e",
    "cod4e_harness",
    "cod4e_master",
    "cod4e_server",
    "devctrl",
    "fx",
    "gsc",
    "harness",
    "install",
    "logging",
    "master",
    "net",
    "render",
    "server",
    "sim",
    "sm3",
];

/// Whether an event target belongs to this workspace (`client::shell`, `cod4e`).
fn ours(target: &str) -> bool {
    let root = target.split("::").next().unwrap_or(target);
    OURS.contains(&root)
}

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

/// What decides the filter: an explicit `log_filter`, else `developer`, else the start-up spec.
struct State {
    developer: i64,
    filter: String,
}

struct Control {
    set: SetFilter,
    /// The spec in force at start (the environment's when it parses, else the default).
    initial: String,
    state: Mutex<State>,
}

static CONTROL: OnceLock<Control> = OnceLock::new();

fn spec_for(level: &str) -> String {
    let mut s = String::from("warn");
    for c in OURS {
        s.push_str(&format!(",{c}={level}"));
    }
    s
}

/// The filter a binary starts with when the environment names none: `info` for our crates, `warn` for the rest.
pub fn default_spec() -> String {
    spec_for("info")
}

/// The environment's filter, or the default; a spec that does not parse is reported (later, once the subscriber is
/// up) and replaced by the default.
fn initial_spec() -> (EnvFilter, String, Option<String>) {
    let env = ["COD4E_LOG", "RUST_LOG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()));
    if let Some(spec) = env {
        match EnvFilter::try_new(&spec) {
            Ok(f) => return (f, spec, None),
            Err(e) => {
                let d = default_spec();
                return (EnvFilter::new(&d), d, Some(format!("{spec:?}: {e}")));
            }
        }
    }
    let d = default_spec();
    (EnvFilter::new(&d), d, None)
}

/// Installs the global subscriber. A second call (a test process, a listen server inside the client) changes
/// nothing and returns a guard without a file.
pub fn init(cfg: Config<'_>) -> Guard {
    let (filter, spec, bad_env) = initial_spec();
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
        state: Mutex::new(State {
            developer: 0,
            filter: String::new(),
        }),
    });
    #[cfg(not(target_arch = "wasm32"))]
    native::log_panics();
    // Debug, not info: stderr stays the program's own messages (a failed `--list-display-modes` is read from it).
    tracing::debug!(app = cfg.app, file = ?path, "logging started");
    if let Some(bad) = bad_env {
        tracing::warn!("COD4E_LOG/RUST_LOG does not parse, using the default filter: {bad}");
    }
    Guard { path }
}

/// Applies what the state says.
fn apply(c: &Control, st: &State) -> Result<(), String> {
    let spec = if !st.filter.trim().is_empty() {
        st.filter.clone()
    } else if st.developer >= 2 {
        spec_for("trace")
    } else if st.developer == 1 {
        spec_for("debug")
    } else {
        c.initial.clone()
    };
    let f = EnvFilter::try_new(&spec).map_err(|e| e.to_string())?;
    if (c.set)(f) {
        Ok(())
    } else {
        Err("logging is shut down".into())
    }
}

/// Sets the `log_filter` (`EnvFilter` syntax), which wins over `developer`. Returns the parse error and keeps the
/// old filter when it does not parse; empty hands the decision back to `developer`.
pub fn set_filter(spec: &str) -> Result<(), String> {
    let Some(c) = CONTROL.get() else {
        return Ok(());
    };
    if !spec.trim().is_empty() {
        EnvFilter::try_new(spec).map_err(|e| e.to_string())?;
    }
    let mut st = c.state.lock().unwrap_or_else(|e| e.into_inner());
    st.filter = spec.to_owned();
    apply(c, &st)
}

/// The stock `developer` levels: 0 is the start-up filter, 1 logs our crates at `debug`, 2 at `trace`.
pub fn set_developer(level: i64) {
    let Some(c) = CONTROL.get() else { return };
    let mut st = c.state.lock().unwrap_or_else(|e| e.into_inner());
    st.developer = level;
    let _ = apply(c, &st);
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
        // Our own problems only: a graphics or window library's warnings are not the player's console's business.
        if level > Level::WARN
            || !FORWARD.load(Ordering::Relaxed)
            || !ours(event.metadata().target())
        {
            return;
        }
        let line = redact(&format::line(event)).into_owned();
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
    fn a_stream_of_distinct_keys_logs_a_bounded_number_of_lines() {
        let (_, events) = capture(|| {
            for i in 0..5000 {
                dedup!(WARN, &i.to_string(), "no such thing");
            }
        });
        assert!(events.len() < 600, "{}", events.len());
        assert!(
            events
                .iter()
                .any(|e| e.message.contains("too many distinct"))
        );
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

    /// Every crate and binary of the workspace logs under a name the filter knows (a binary-only crate logs under
    /// its binary's name), so none is dropped by default.
    #[test]
    fn the_filter_knows_every_workspace_target() {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut seen = 0;
        for dir in std::fs::read_dir(crates).unwrap().flatten() {
            let Ok(text) = std::fs::read_to_string(dir.path().join("Cargo.toml")) else {
                continue;
            };
            let mut section = "";
            for line in text.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    section = if line == "[package]" {
                        "package"
                    } else if line == "[[bin]]" || line == "[lib]" {
                        "target"
                    } else {
                        ""
                    };
                } else if !section.is_empty()
                    && let Some(name) = line
                        .strip_prefix("name = \"")
                        .and_then(|n| n.strip_suffix('"'))
                {
                    seen += 1;
                    let target = name.replace('-', "_");
                    assert!(ours(&target), "{target} is not in OURS");
                }
            }
        }
        assert!(seen > 14, "found only {seen} names");
    }

    #[test]
    fn console_queue_keeps_warnings_only_once_forwarded() {
        // One test owns the process-wide flag; the layer is driven directly with its own subscriber.
        let sub = Registry::default().with(ConsoleLayer);
        tracing::subscriber::with_default(sub, || {
            tracing::warn!("before");
            forward_to_console();
            tracing::warn!(cmd = "foo", "unknown command");
            tracing::warn!(target: "wgpu_hal", "library noise");
            tracing::info!("not a problem");
        });
        let lines = drain_console();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].1.contains("unknown command") && lines[0].1.contains("cmd=foo"));
    }
}
