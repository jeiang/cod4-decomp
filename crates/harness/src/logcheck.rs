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
    /// The most events of this entry one stage may log (measured, with a margin); more fails the stage, so a
    /// regression that makes the same warning fire far more often is not hidden by the entry.
    pub max: Option<usize>,
}

/// The reviewed list. Keep it short: fix the cause when the fix is small. `max` is per stage, from the default suite
/// measured on the Linux host (the largest stage; each entry says its measurement and date) plus a margin: 10% for the
/// material classes, 25% or more for the rest. It catches a gross regression, not a one-off.
pub const ALLOWED: &[Allow] = &[
    // Known gaps: the code is right to say so; the cause is not fixed yet.
    Allow {
        contains: &["render::material", "no usable technique", "_l_hsm_"],
        reason: "materials on lit techsets (`*_l_hsm_*`: decals, signs, glass, trash) have no technique we translate yet; their draws are skipped",
        stages: &[],
        max: Some(2200), // measured 1985 on the Linux host, 2026-10-14,
    },
    Allow {
        contains: &["render::material", "no usable technique", "_unlit"],
        reason: "materials on unlit techsets (`*_unlit*`: stains, graffiti, godrays, light bulbs) have no technique we translate yet; their draws are skipped",
        stages: &[],
        max: Some(1310), // measured 1185 on the Linux host, 2026-10-14,
    },
    Allow {
        contains: &["render::material", "no usable technique", "mc_effect"],
        reason: "materials on effect techsets (`mc_effect*`: floodlight beams, lens flares, tight glow) have no technique we translate yet; their draws are skipped",
        stages: &[],
        max: Some(190), // measured 171 on the Linux host, 2026-10-14,
    },
    Allow {
        contains: &["render::material", "no usable technique", "mc_shadowcaster"],
        reason: "the tree shadow-caster techset has no technique we translate yet; its draws are skipped",
        stages: &[],
        max: Some(66), // measured 60 on the Linux host, 2026-10-14,
    },
    Allow {
        contains: &["render::material", "no usable technique", "wc_sky"],
        reason: "the sky techset has no technique we translate yet; its draws are skipped",
        stages: &[],
        max: Some(72), // measured 65 on the Linux host, 2026-10-14,
    },
    Allow {
        contains: &["render::material", "no usable technique"],
        reason: "any other techset (sun, default3d, reflex sights, flags, laptop screens) has no technique we translate yet; draws skipped",
        stages: &[],
        max: Some(53), // measured 48 on the Linux host, 2026-10-14,
    },
    Allow {
        contains: &["sim::skel::anim", "animation data read out of range"],
        reason: "random_data_short reads past the table for some takeoff and ads animations (#387)",
        stages: &[],
        max: Some(40),
    },
    Allow {
        contains: &["server::server", "unknown command", "con_showchannel"],
        reason: "the stock configs run con_showchannel, which we do not implement (#365)",
        stages: &[],
        max: Some(5), // measured 4 on the Linux host, 2026-10-14,
    },
    Allow {
        contains: &["fx: model element skipped: its model did not resolve"],
        reason: "effects with a model element whose xmodel is not in the loaded zones (#389)",
        stages: &[],
        max: Some(20),
    },
    Allow {
        contains: &[
            "cod4e::netplay::attach",
            "attachment tag did not resolve",
            "model=\"head_mp_",
        ],
        reason: "head models do not find their attach tag (#388); seen: head_mp_arab_regular_{headwrap,ski_mask,suren,asad,sadiq}, head_mp_usmc_{shaved_head,tactical_mich,tactical_mich_stripes_nomex}; any head_mp_ model is the same bug",
        stages: &[],
        max: Some(60),
    },
    Allow {
        contains: &["cod4e::models", "held model's tag bone missing"],
        reason: "a held model whose tag bone is not in the rig (#389)",
        stages: &[],
        max: Some(3), // measured 2 on the Linux host, 2026-10-14,
    },
    Allow {
        contains: &[
            "cod4e::ui::assets",
            "localized string is missing",
            "key=\"\"",
        ],
        reason: "menus show an empty `@` reference (#372)",
        stages: &[],
        max: Some(9), // measured 7 on the Linux host, 2026-10-14,
    },
    // Timing of the harness's own clients and bots.
    Allow {
        contains: &["server::netsv", "packet from an address with no client"],
        reason: "a client's packet before it connected or after the server dropped it",
        stages: &[],
        max: Some(16),
    },
    Allow {
        contains: &["server::client", "usercmd time out of range; clamped"],
        reason: "a client that stalled (map load, window stall) catches its clock up",
        stages: &[],
        max: Some(20),
    },
    // Stages that provoke the event on purpose.
    Allow {
        contains: &["net::connect", "Server is full"],
        reason: "net-hardening fills the server to check the refusal",
        stages: &["net-hardening"],
        max: Some(2), // measured 1 on the Linux host, 2026-10-14,
    },
    Allow {
        contains: &["server::server", "rcon", "a bad password"],
        reason: "net-rcon sends a wrong password on purpose",
        stages: &["net-rcon"],
        max: Some(2), // measured 1 on the Linux host, 2026-10-14,
    },
    Allow {
        contains: &["server::server", "rcon", "dropped by the throttle"],
        reason: "net-rcon floods rcon to check the throttle",
        stages: &["net-rcon"],
        max: Some(2), // measured 1 on the Linux host, 2026-10-14,
    },
    Allow {
        contains: &["net::connect", "You are banned from this server"],
        reason: "net-rcon bans a client and reconnects it",
        stages: &["net-rcon"],
        max: Some(2), // measured 1 on the Linux host, 2026-10-14,
    },
    Allow {
        contains: &["server::netsv", "usercmd queue full"],
        reason: "net-rcon holds a client's packets back while it is kicked",
        stages: &["net-rcon"],
        max: Some(9), // measured 7 on the Linux host, 2026-10-14,
    },
    Allow {
        contains: &["net::client", "dropped by the server", "Player kicked"],
        reason: "net-rcon and net-vote kick a client on purpose",
        stages: &["net-rcon", "net-vote"],
        max: Some(3), // measured 2 on the Linux host, 2026-10-14,
    },
    Allow {
        contains: &["server::server", "unknown client command", "frobnicate"],
        reason: "net-vote sends an unknown client command to check the answer",
        stages: &["net-vote"],
        max: Some(2), // measured 1 on the Linux host, 2026-10-14,
    },
    Allow {
        contains: &[
            "server::netsv",
            "file request refused: banned or over budget",
        ],
        reason: "web-download exceeds the download budget on purpose",
        stages: &["web-download"],
        max: Some(9), // measured 7 on the Linux host, 2026-10-14
    },
    Allow {
        contains: &[
            "cod4e::ui",
            "script names a menu that is not loaded",
            "menu=\"options_",
        ],
        reason: "the options screens name menus (options_view, options_defaults, options_driverinfo, options_credits) the loaded UI zones lack (#372)",
        stages: &["client-ui"],
        max: Some(10), // measured 8 on the Linux host, 2026-10-14,
    },
    Allow {
        contains: &[
            "cod4e::display",
            "exclusive fullscreen is not available on Wayland",
        ],
        reason: "client-gfx asks for exclusive fullscreen; Wayland has none, borderless is used",
        stages: &["client-gfx"],
        max: Some(2), // measured 1 on the Linux host, 2026-10-14,
    },
    Allow {
        contains: &["render::gpu", "graphics device lost"],
        reason: "client-gfx destroys the device to check recovery",
        stages: &["client-gfx"],
        max: Some(2), // measured 1 on the Linux host, 2026-10-14,
    },
    Allow {
        contains: &["cod4e::app", "recovering from the lost graphics device"],
        reason: "client-gfx destroys the device to check recovery",
        stages: &["client-gfx"],
        max: Some(2), // measured 1 on the Linux host, 2026-10-14,
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

fn allowed(list: &[Allow], stage: &str, e: &Event) -> Option<usize> {
    list.iter().position(|a| {
        (a.stages.is_empty() || a.stages.contains(&stage))
            && a.contains.iter().all(|c| e.text.contains(c))
    })
}

/// What reading a stage's logs found: the events nothing allows, and how often each allowed entry fired.
#[derive(Default, Debug)]
pub struct Findings {
    pub unexpected: Vec<Event>,
    /// Events per allow-list entry (by index).
    pub allowed: BTreeMap<usize, usize>,
}

/// Reads `<dir>/logs/*.log` and sorts its `WARN`/`ERROR` events against `list`.
///
/// The gate vouches for the logs it read, so a `logs/` that is missing, unreadable or empty, and a log file it cannot
/// read, are findings of their own: every process of a stage writes its log there, so none means the gate could not
/// check what the stage did (issue #396).
pub fn scan(dir: &Path, stage: &str, list: &'static [Allow]) -> Findings {
    let mut f = Findings::default();
    let err = |text: String| Event { error: true, text };
    let rd = match std::fs::read_dir(dir.join("logs")) {
        Ok(rd) => rd,
        Err(e) => {
            f.unexpected.push(err(format!(
                "logs/ cannot be read ({e}), so no warn or error line was checked"
            )));
            return f;
        }
    };
    let mut files: Vec<_> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "log"))
        .collect();
    files.sort();
    if files.is_empty() {
        f.unexpected.push(err(
            "logs/ holds no log file, so no warn or error line was checked".into(),
        ));
        return f;
    }
    for p in files {
        let name = p
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let text = match std::fs::read(&p) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => {
                f.unexpected.push(err(format!(
                    "{name} cannot be read ({e}), so its warn and error lines were not checked"
                )));
                continue;
            }
        };
        if text.contains(logging::TRUNCATION_MARK) {
            // Events after the cap are lost, so nothing in this log can be vouched for.
            f.unexpected.push(err(format!(
                "{name} stopped at its size cap, later events are lost"
            )));
        }
        for e in events(&text) {
            match allowed(list, stage, &e) {
                Some(i) => *f.allowed.entry(i).or_default() += 1,
                None => f.unexpected.push(e),
            }
        }
    }
    for (&i, &n) in &f.allowed {
        if let Some(max) = list[i].max.filter(|&m| n > m) {
            f.unexpected.push(Event {
                error: false,
                text: format!(
                    "allow-list entry fired {n} times, more than its max {max}: {}",
                    list[i].reason
                ),
            });
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
    for (&i, n) in &f.allowed {
        report
            .notes
            .push(format!("allowed log events: {n} x {}", list[i].reason));
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
            max: None,
        },
        Allow {
            contains: &["only in a stage"],
            reason: "narrow",
            stages: &["a"],
            max: None,
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

    const CAPPED: &[Allow] = &[Allow {
        contains: &["render::material"],
        reason: "capped",
        stages: &[],
        max: Some(2),
    }];

    #[test]
    fn an_entry_over_its_max_fails_the_stage() {
        let line = "2026-10-13T17:34:38.1Z  WARN render::material: x\n";
        let mut r = StageReport::new("a", Status::Passed);
        apply_with(&mut r, stage_dir(&line.repeat(2)).path(), "a", CAPPED);
        assert_eq!(r.status, Status::Passed);
        let mut r = StageReport::new("a", Status::Passed);
        apply_with(&mut r, stage_dir(&line.repeat(3)).path(), "a", CAPPED);
        assert_eq!(r.status, Status::Failed);
        assert!(r.reason.unwrap().contains("more than its max 2"));
    }

    #[test]
    fn a_log_that_hit_its_size_cap_fails_the_stage() {
        let log = format!(
            "# x\n{} 64 MiB: the rest is dropped]\n",
            logging::TRUNCATION_MARK
        );
        let mut r = StageReport::new("a", Status::Passed);
        apply_with(&mut r, stage_dir(&log).path(), "a", LIST);
        assert_eq!(r.status, Status::Failed);
        assert!(r.reason.unwrap().contains("size cap"));
    }

    #[test]
    fn a_missing_logs_dir_fails_a_passed_stage() {
        // No process wrote a log, so the gate can vouch for nothing (a skip is not a pass either).
        let d = tempfile::tempdir().unwrap();
        let mut r = StageReport::new("a", Status::Passed);
        apply_with(&mut r, d.path(), "a", LIST);
        assert_eq!(r.status, Status::Failed);
        assert!(r.reason.unwrap().contains("logs/ cannot be read"));
        let mut r = StageReport::new("a", Status::Crashed);
        apply_with(&mut r, d.path(), "a", LIST);
        assert_eq!(r.status, Status::Crashed);
        assert!(r.notes.iter().any(|n| n.contains("logs/ cannot be read")));
    }

    #[test]
    fn an_unreadable_logs_dir_fails_a_passed_stage() {
        // `logs` is a file, so the directory cannot be opened (the same on every platform).
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("logs"), b"not a directory").unwrap();
        let mut r = StageReport::new("a", Status::Passed);
        apply_with(&mut r, d.path(), "a", LIST);
        assert_eq!(r.status, Status::Failed);
        assert!(r.reason.unwrap().contains("logs/ cannot be read"));
    }

    #[test]
    fn an_empty_logs_dir_for_a_stage_that_started_processes_fails_it() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("logs")).unwrap();
        let mut r = StageReport::new("a", Status::Passed);
        apply_with(&mut r, d.path(), "a", LIST);
        assert_eq!(r.status, Status::Failed);
        assert!(r.reason.unwrap().contains("holds no log file"));
    }

    #[test]
    fn an_unreadable_log_file_fails_a_passed_stage() {
        // A directory named `x.log`: reading it fails on every platform.
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("logs/x.log")).unwrap();
        let mut r = StageReport::new("a", Status::Passed);
        apply_with(&mut r, d.path(), "a", LIST);
        assert_eq!(r.status, Status::Failed);
        let why = r.reason.unwrap();
        assert!(why.contains("x.log cannot be read"), "{why}");
    }
}
