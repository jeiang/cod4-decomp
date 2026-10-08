// SPDX-License-Identifier: GPL-3.0-only
//! Remote console (`rcon <password> <command>`): who may run a command, and how often.
//!
//! Behaviour after `SVC_RemoteCommand` (KisakCOD, `src/server_mp/sv_main_pc_mp.cpp`, GPL-3.0;
//! facts only, no code taken): one request per 500 ms for the whole server, answered or not, so a
//! password cannot be guessed at wire speed.

use std::time::{Duration, Instant};

/// Requests closer together than this are dropped without an answer.
pub const MIN_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Default)]
pub struct Throttle {
    last: Option<Instant>,
}

impl Throttle {
    /// Whether a request at `now` is served; serving it starts the next interval.
    pub fn allow(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|t| now.duration_since(t) < MIN_INTERVAL)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The server has no `rcon_password`: remote console is off.
    Disabled,
    Granted,
    /// A password was given and is wrong.
    Wrong,
    /// No password was given.
    Missing,
}

/// Checks `given` against the `rcon_password` cvar. An empty configured password never matches.
pub fn check(configured: &str, given: &str) -> Verdict {
    if configured.is_empty() {
        Verdict::Disabled
    } else if given.is_empty() {
        Verdict::Missing
    } else if same(configured.as_bytes(), given.as_bytes()) {
        Verdict::Granted
    } else {
        Verdict::Wrong
    }
}

/// Equality that takes as long for a near miss as for a far one.
fn same(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for (i, x) in a.iter().enumerate() {
        diff |= usize::from(x ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_password_disables_rcon_even_for_an_empty_guess() {
        assert_eq!(check("", ""), Verdict::Disabled);
        assert_eq!(check("", "anything"), Verdict::Disabled);
    }

    #[test]
    fn only_the_exact_password_is_granted() {
        assert_eq!(check("s3cret", "s3cret"), Verdict::Granted);
        assert_eq!(check("s3cret", "s3cre"), Verdict::Wrong);
        assert_eq!(check("s3cret", "s3crets"), Verdict::Wrong);
        assert_eq!(check("s3cret", "S3CRET"), Verdict::Wrong);
        assert_eq!(check("s3cret", ""), Verdict::Missing);
    }

    #[test]
    fn requests_inside_the_interval_are_dropped_even_when_the_first_failed() {
        let mut t = Throttle::default();
        let t0 = Instant::now();
        assert!(t.allow(t0));
        assert!(!t.allow(t0 + Duration::from_millis(499)));
        assert!(t.allow(t0 + MIN_INTERVAL));
        assert!(!t.allow(t0 + MIN_INTERVAL + Duration::from_millis(1)));
    }
}
