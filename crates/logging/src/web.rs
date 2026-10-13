// SPDX-License-Identifier: GPL-3.0-only
//! The browser output: events go to the page's console at the matching level.

use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use wasm_bindgen::JsValue;

pub struct ConsoleOut;

impl<S: Subscriber> Layer<S> for ConsoleOut {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let m = event.metadata();
        let text = JsValue::from_str(&format!("{} {}", m.target(), crate::format::line(event)));
        match *m.level() {
            Level::ERROR => web_sys::console::error_1(&text),
            Level::WARN => web_sys::console::warn_1(&text),
            Level::INFO => web_sys::console::info_1(&text),
            _ => web_sys::console::debug_1(&text),
        }
    }
}
