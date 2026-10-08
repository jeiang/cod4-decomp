// SPDX-License-Identifier: GPL-3.0-only
//! `tracing` plumbing: a layer that logs events to stderr (captured by the
//! watchdog into the bundle) and a layer that records spans for one window
//! as a Chrome trace, which Perfetto opens directly.
use std::fmt::Write as _;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

#[derive(Default)]
struct Fields {
    message: String,
    args: serde_json::Map<String, serde_json::Value>,
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            self.args
                .insert(field.name().to_owned(), format!("{value:?}").into());
        }
    }
}

/// Prints `LEVEL target: message key=value ...` to stderr.
pub struct LogLayer;

impl<S: Subscriber> Layer<S> for LogLayer {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let mut f = Fields::default();
        event.record(&mut f);
        let mut line = format!(
            "{} {}: {}",
            event.metadata().level(),
            event.metadata().target(),
            f.message
        );
        for (k, v) in &f.args {
            let _ = write!(line, " {k}={}", v.as_str().unwrap_or_default());
        }
        eprintln!("{line}");
    }
}

struct Record {
    name: String,
    ts_us: u64,
    dur_us: Option<u64>,
    tid: u64,
    args: serde_json::Map<String, serde_json::Value>,
}

struct Shared {
    t0: Instant,
    window: Duration,
    cap: usize,
    events: Mutex<Vec<Record>>,
}

/// Collected trace; write it with [`TraceHandle::write`].
#[derive(Clone)]
pub struct TraceHandle(Arc<Shared>);

/// Records spans (and events as instants) that start within `window` of
/// creation, capped at `cap` records. Later activity is dropped, so the trace
/// stays small however long the stage runs.
pub struct ChromeTrace(Arc<Shared>);

pub fn chrome_trace(window: Duration, cap: usize) -> (ChromeTrace, TraceHandle) {
    let s = Arc::new(Shared {
        t0: Instant::now(),
        window,
        cap,
        events: Mutex::new(Vec::new()),
    });
    (ChromeTrace(s.clone()), TraceHandle(s))
}

fn tid() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    thread_local!(static TID: u64 = NEXT.fetch_add(1, Ordering::Relaxed));
    TID.with(|t| *t)
}

struct Started(Instant, serde_json::Map<String, serde_json::Value>);

impl Shared {
    fn open(&self, at: Instant) -> bool {
        at.duration_since(self.t0) < self.window && self.events.lock().unwrap().len() < self.cap
    }
    fn push(&self, r: Record) {
        let mut e = self.events.lock().unwrap();
        if e.len() < self.cap {
            e.push(r);
        }
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for ChromeTrace {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let now = Instant::now();
        if !self.0.open(now) {
            return;
        }
        let mut f = Fields::default();
        attrs.record(&mut f);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(Started(now, f.args));
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let Some(Started(start, args)) = span.extensions_mut().remove::<Started>() else {
            return;
        };
        self.0.push(Record {
            name: span.name().to_owned(),
            ts_us: start.duration_since(self.0.t0).as_micros() as u64,
            dur_us: Some(start.elapsed().as_micros() as u64),
            tid: tid(),
            args,
        });
    }

    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let now = Instant::now();
        if !self.0.open(now) {
            return;
        }
        let mut f = Fields::default();
        event.record(&mut f);
        self.0.push(Record {
            name: f.message,
            ts_us: now.duration_since(self.0.t0).as_micros() as u64,
            dur_us: None,
            tid: tid(),
            args: f.args,
        });
    }
}

impl TraceHandle {
    /// Number of records so far.
    pub fn len(&self) -> usize {
        self.0.events.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Write the Chrome trace event JSON.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let mut events = self.0.events.lock().unwrap();
        events.sort_by_key(|r| r.ts_us);
        let list: Vec<_> = events
            .iter()
            .map(|r| {
                let mut e = serde_json::json!({
                    "name": r.name, "cat": "cod4e", "pid": 1, "tid": r.tid,
                    "ts": r.ts_us, "args": r.args,
                });
                match r.dur_us {
                    Some(d) => {
                        e["ph"] = "X".into();
                        e["dur"] = d.into();
                    }
                    None => {
                        e["ph"] = "i".into();
                        e["s"] = "t".into();
                    }
                }
                e
            })
            .collect();
        let doc = serde_json::json!({ "traceEvents": list, "displayTimeUnit": "ms" });
        std::fs::write(path, serde_json::to_vec(&doc)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn spans_and_window_cap() {
        let (layer, handle) = chrome_trace(Duration::from_secs(60), 3);
        let sub = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(sub, || {
            for i in 0..5 {
                let _s = tracing::info_span!("zone", i).entered();
                tracing::info!("inside");
            }
        });
        assert_eq!(handle.len(), 3);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("trace.json");
        handle.write(&p).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
        let ev = v["traceEvents"].as_array().unwrap();
        assert_eq!(ev.len(), 3);
        assert!(ev.iter().any(|e| e["ph"] == "X" && e["name"] == "zone"));
        assert!(ev.iter().any(|e| e["ph"] == "i" && e["name"] == "inside"));
    }

    #[test]
    fn closed_window_records_nothing() {
        let (layer, handle) = chrome_trace(Duration::ZERO, 100);
        let sub = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(sub, || {
            let _s = tracing::info_span!("late").entered();
        });
        assert!(handle.is_empty());
    }
}
