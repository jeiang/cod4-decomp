// SPDX-License-Identifier: GPL-3.0-only
//! The log gate: a stage that passed but logged a `warn` or `error` that nobody reviewed has not passed.
//!
//! Every process of a stage (the stage child, the clients and servers it starts) writes its log into `<stage>/logs`
//! (the watchdog sets `COD4E_LOG_DIR`). After the stage, [`apply`] reads those files and matches every `WARN` and
//! `ERROR` line against [`ALLOWED`]. A line that matches nothing fails a passed stage, with the first few lines in the
//! reason; allowed lines are counted in the stage's notes with their reasons, so the list stays honest.
//!
//! Adding an entry is a review decision: say why the event is expected (a stock data hole, a known open issue) and
//! keep `contains` narrow, so a different failure of the same code still shows up.

use crate::stage::{StageReport, Status};
use std::collections::BTreeMap;
use std::path::Path;

/// One reviewed, expected `warn`/`error` event.
pub struct Allow {
    /// Substrings the whole log line must all contain (the target path and a key phrase of the message).
    pub contains: &'static [&'static str],
    /// Why the event is expected; an issue number when it is a known gap.
    pub reason: &'static str,
    /// The stages it is expected in; empty means any.
    pub stages: &'static [&'static str],
}

/// The reviewed list. Keep it short: fix the cause when the fix is small.
pub const ALLOWED: &[Allow] = &[
    // Known gaps: the code is right to say so; the cause is not fixed yet.
    Allow {
        contains: &["render::material", "no usable technique"],
        reason: "materials whose techset has no technique we translate yet (decals, glass, unlit and shadow-caster sets); their draws are skipped",
        stages: &[],
    },
    Allow {
        contains: &["sim::skel::anim", "animation data read out of range"],
        reason: "random_data_short reads past the table for some takeoff and ads animations (gap in the animation table decode)",
        stages: &[],
    },
    Allow {
        contains: &["server::server", "unknown command", "con_showchannel"],
        reason: "the stock configs run con_showchannel, which we do not implement (#365)",
        stages: &[],
    },
    Allow {
        contains: &["fx:", "model element skipped"],
        reason: "effects with a model element whose xmodel is not in the loaded zones",
        stages: &[],
    },
    Allow {
        contains: &[
            "cod4e::netplay::attach",
            "attachment tag did not resolve",
            "model=\"head_",
        ],
        reason: "player head models: the tag the server numbered is not in the body's tag table",
        stages: &[],
    },
    Allow {
        contains: &["cod4e::models", "held model's tag bone missing"],
        reason: "a held model whose tag bone is not in the rig",
        stages: &[],
    },
    Allow {
        contains: &[
            "cod4e::ui::assets",
            "localized string is missing",
            "key=\"\"",
        ],
        reason: "menus show an empty `@` reference (#372)",
        stages: &[],
    },
    // Timing of the harness's own clients and bots.
    Allow {
        contains: &["server::netsv", "packet from an address with no client"],
        reason: "a client's packet before it connected or after the server dropped it",
        stages: &[],
    },
    Allow {
        contains: &["server::client", "usercmd time out of range; clamped"],
        reason: "a client that stalled (map load, window stall) catches its clock up",
        stages: &[],
    },
    // Stages that provoke the event on purpose.
    Allow {
        contains: &["net::connect", "Server is full"],
        reason: "net-hardening fills the server to check the refusal",
        stages: &["net-hardening"],
    },
    Allow {
        contains: &["server::server", "rcon", "a bad password"],
        reason: "net-rcon sends a wrong password on purpose",
        stages: &["net-rcon"],
    },
    Allow {
        contains: &["server::server", "rcon", "dropped by the throttle"],
        reason: "net-rcon floods rcon to check the throttle",
        stages: &["net-rcon"],
    },
    Allow {
        contains: &["net::connect", "You are banned from this server"],
        reason: "net-rcon bans a client and reconnects it",
        stages: &["net-rcon"],
    },
    Allow {
        contains: &["server::netsv", "usercmd queue full"],
        reason: "net-rcon holds a client's packets back while it is kicked",
        stages: &["net-rcon"],
    },
    Allow {
        contains: &["net::client", "dropped by the server", "Player kicked"],
        reason: "net-rcon and net-vote kick a client on purpose",
        stages: &["net-rcon", "net-vote"],
    },
    Allow {
        contains: &["server::server", "unknown client command", "frobnicate"],
        reason: "net-vote sends an unknown client command to check the answer",
        stages: &["net-vote"],
    },
    Allow {
        contains: &[
            "server::netsv",
            "file request refused: banned or over budget",
        ],
        reason: "web-download exceeds the download budget on purpose",
        stages: &["web-download"],
    },
    Allow {
        contains: &[
            "cod4e::ui",
            "script names a menu that is not loaded",
            "options_view",
        ],
        reason: "client-ui opens every stock menu; options_view is not in the shipped zones",
        stages: &["client-ui"],
    },
    Allow {
        contains: &[
            "cod4e::display",
            "exclusive fullscreen is not available on Wayland",
        ],
        reason: "client-gfx asks for exclusive fullscreen; Wayland has none, borderless is used",
        stages: &["client-gfx"],
    },
    Allow {
        contains: &["render::gpu", "graphics device lost"],
        reason: "client-gfx destroys the device to check recovery",
        stages: &["client-gfx"],
    },
    Allow {
        contains: &["cod4e::app", "recovering from the lost graphics device"],
        reason: "client-gfx destroys the device to check recovery",
        stages: &["client-gfx"],
    },
];

/// A `WARN` or `ERROR` event read from a log.
#[derive(Debug, PartialEq, Eq)]
pub struct Event {
    pub error: bool,
    /// Everything after the level: `target: message key=value ...` (span prefixes included).
    pub text: String,
}

/// The `WARN`/`ERROR` events of one log file's text. Header, continuation and lower-level lines are skipped.
pub fn events(text: &str) -> Vec<Event> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let stamp = words.next()?;
            if !(stamp.contains('T') && stamp.ends_with('Z')) {
                return None;
            }
            let error = match words.next()? {
                "WARN" => false,
                "ERROR" => true,
                _ => return None,
            };
            let text = line
                .split_once(if error { "ERROR" } else { "WARN" })?
                .1
                .trim();
            Some(Event {
                error,
                text: text.to_owned(),
            })
        })
        .collect()
}

fn allowed<'a>(list: &'a [Allow], stage: &str, e: &Event) -> Option<&'a Allow> {
    list.iter().find(|a| {
        (a.stages.is_empty() || a.stages.contains(&stage))
            && a.contains.iter().all(|c| e.text.contains(c))
    })
}

/// What reading a stage's logs found: the events nothing allows, and how often each allowed entry fired.
#[derive(Default, Debug)]
pub struct Findings {
    pub unexpected: Vec<Event>,
    pub allowed: BTreeMap<&'static str, usize>,
}

/// Reads `<dir>/logs/*.log` and sorts its `WARN`/`ERROR` events against `list`.
pub fn scan(dir: &Path, stage: &str, list: &'static [Allow]) -> Findings {
    let mut f = Findings::default();
    let Ok(rd) = std::fs::read_dir(dir.join("logs")) else {
        return f;
    };
    let mut files: Vec<_> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "log"))
        .collect();
    files.sort();
    for p in files {
        let text = match std::fs::read(&p) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => {
                tracing::warn!(stage, path = %p.display(), "cannot read a stage log: {e}");
                continue;
            }
        };
        for e in events(&text) {
            match allowed(list, stage, &e) {
                Some(a) => *f.allowed.entry(a.reason).or_default() += 1,
                None => f.unexpected.push(e),
            }
        }
    }
    f
}

/// Notes what the logs held on `report`, and turns a passed stage with an unexpected event into a failed one.
pub fn apply(report: &mut StageReport, dir: &Path, stage: &str) {
    apply_with(report, dir, stage, ALLOWED);
}

fn apply_with(report: &mut StageReport, dir: &Path, stage: &str, list: &'static [Allow]) {
    let f = scan(dir, stage, list);
    for (reason, n) in &f.allowed {
        report
            .notes
            .push(format!("allowed log events: {n} x {reason}"));
    }
    if f.unexpected.is_empty() {
        return;
    }
    // Distinct lines, in order, with their counts; long ones cut.
    let mut seen: Vec<(String, usize)> = Vec::new();
    for e in &f.unexpected {
        let level = if e.error { "ERROR" } else { "WARN" };
        let mut line = format!("{level} {}", e.text);
        if line.len() > 200 {
            line.truncate(line.floor_char_boundary(200));
            line.push_str("...");
        }
        match seen.iter_mut().find(|(l, _)| *l == line) {
            Some((_, n)) => *n += 1,
            None => seen.push((line, 1)),
        }
    }
    for (line, n) in &seen {
        report
            .notes
            .push(format!("unexpected log event ({n}x): {line}"));
    }
    if report.status == Status::Passed {
        report.status = Status::Failed;
        let first: Vec<_> = seen.iter().take(3).map(|(l, _)| l.as_str()).collect();
        report.reason = Some(format!(
            "{} unexpected warn/error log events in {} distinct lines (allow-list: harness/src/logcheck.rs); first: {}",
            f.unexpected.len(),
            seen.len(),
            first.join(" | ")
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &[Allow] = &[
        Allow {
            contains: &["render::material", "no usable technique"],
            reason: "decals",
            stages: &[],
        },
        Allow {
            contains: &["only in a stage"],
            reason: "narrow",
            stages: &["a"],
        },
    ];

    const LOG: &str = "# client pid 1 started\n\
2026-10-13T17:34:37.9Z  INFO client: started\n\
2026-10-13T17:34:38.1Z  WARN render::material: material has no usable technique seen=4 material=\"x\"\n\
2026-10-13T17:34:38.2Z ERROR map_load{map=\"mp_crash\"}: client::app: cannot load\n\
  continuation line WARN\n";

    fn stage_dir(log: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("logs")).unwrap();
        std::fs::write(d.path().join("logs/client-1.log"), log).unwrap();
        d
    }

    #[test]
    fn only_warn_and_error_events_are_read() {
        let e = events(LOG);
        assert_eq!(e.len(), 2);
        assert!(!e[0].error && e[0].text.starts_with("render::material:"));
        assert!(e[1].error && e[1].text.contains("cannot load"));
    }

    #[test]
    fn an_unlisted_event_fails_a_passed_stage_and_a_listed_one_does_not() {
        let d = stage_dir(LOG);
        let mut r = StageReport::new("a", Status::Passed);
        apply_with(&mut r, d.path(), "a", LIST);
        assert_eq!(r.status, Status::Failed);
        let why = r.reason.unwrap();
        assert!(
            why.contains("cannot load") && !why.contains("no usable technique"),
            "{why}"
        );
        assert!(r.notes.iter().any(|n| n.contains("1 x decals")));

        let d = stage_dir(
            "2026-10-13T17:34:38.1Z  WARN render::material: material has no usable technique\n",
        );
        let mut r = StageReport::new("a", Status::Passed);
        apply_with(&mut r, d.path(), "a", LIST);
        assert_eq!(r.status, Status::Passed);
    }

    #[test]
    fn an_entry_for_one_stage_does_not_cover_another() {
        let line = "2026-10-13T17:34:38.1Z  WARN x::y: only in a stage\n";
        let mut r = StageReport::new("a", Status::Passed);
        apply_with(&mut r, stage_dir(line).path(), "a", LIST);
        assert_eq!(r.status, Status::Passed);
        let mut r = StageReport::new("b", Status::Passed);
        apply_with(&mut r, stage_dir(line).path(), "b", LIST);
        assert_eq!(r.status, Status::Failed);
    }

    #[test]
    fn a_stage_without_logs_or_already_failed_keeps_its_status() {
        let d = tempfile::tempdir().unwrap();
        let mut r = StageReport::new("a", Status::Passed);
        apply_with(&mut r, d.path(), "a", LIST);
        assert_eq!(r.status, Status::Passed);
        let mut r = StageReport::new("a", Status::Crashed);
        apply_with(&mut r, stage_dir(LOG).path(), "a", LIST);
        assert_eq!(r.status, Status::Crashed);
        assert!(r.notes.iter().any(|n| n.contains("unexpected log event")));
    }
}
