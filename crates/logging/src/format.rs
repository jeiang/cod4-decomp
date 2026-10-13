// SPDX-License-Identifier: GPL-3.0-only
//! Turning an event into text, and the recording layer behind `capture`.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

#[derive(Default)]
struct Fields {
    message: String,
    fields: Vec<(&'static str, String)>,
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            self.fields.push((field.name(), format!("{value:?}")));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            self.fields.push((field.name(), value.to_owned()));
        }
    }
}

/// `message key=value ...`, the text of a console line.
pub fn line(event: &Event<'_>) -> String {
    let mut f = Fields::default();
    event.record(&mut f);
    let mut s = f.message;
    for (k, v) in &f.fields {
        let _ = write!(s, " {k}={v}");
    }
    s
}

/// One recorded event.
#[derive(Clone, Debug)]
pub struct Record {
    pub level: Level,
    pub target: String,
    pub message: String,
    pub fields: Vec<(&'static str, String)>,
}

impl Record {
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Default)]
pub struct Sink(Arc<Mutex<Vec<Record>>>);

impl Sink {
    pub fn take(&self) -> Vec<Record> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

pub struct CaptureLayer(pub Sink);

impl<S: Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let mut f = Fields::default();
        event.record(&mut f);
        let m = event.metadata();
        self.0
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Record {
                level: *m.level(),
                target: m.target().to_owned(),
                message: f.message,
                fields: f.fields,
            });
    }
}
