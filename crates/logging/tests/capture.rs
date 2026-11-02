// SPDX-License-Identifier: GPL-3.0-only
//! A capture sees its events while a thread without a subscriber (another test) hits the same call sites first, and
//! its rate-limited events count from 1 (#438). Alone in its process: the failure needs the capture to be the only
//! subscriber there is.

use std::sync::mpsc;
use tracing::Level;

fn plain() {
    tracing::warn!(site = "plain", "a plain warning");
}

fn limited() {
    logging::dedup!(WARN, "k", site = "limited", "a limited warning");
}

fn field<'a>(events: &'a [logging::Record], site: &str, name: &str) -> Option<&'a str> {
    events
        .iter()
        .find(|e| e.level == Level::WARN && e.field("site") == Some(site))
        .and_then(|e| e.field(name))
}

#[test]
fn a_capture_sees_call_sites_another_thread_hit_first() {
    let (ready, live) = mpsc::channel();
    let (done, hit) = mpsc::channel();
    let capturing = std::thread::spawn(move || {
        logging::capture(|| {
            ready.send(()).unwrap();
            hit.recv().unwrap();
            plain();
            limited();
        })
        .1
    });
    live.recv().unwrap();
    for _ in 0..3 {
        plain();
        limited();
    }
    done.send(()).unwrap();
    let events = capturing.join().unwrap();
    assert_eq!(field(&events, "plain", "site"), Some("plain"));
    assert_eq!(field(&events, "limited", "seen"), Some("1"));
    // A later capture counts afresh too.
    let ((), events) = logging::capture(limited);
    assert_eq!(field(&events, "limited", "seen"), Some("1"));
}
