// SPDX-License-Identifier: GPL-3.0-only
//! Rate limiting for events that repeat (per frame, per message): a key logs on its 1st, 2nd, 4th, 8th ... hit with
//! the count so far, so a warning that fires every frame costs a handful of lines, not thousands.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

static WINDOW: AtomicU64 = AtomicU64::new(0);

/// Starts a new counting window for every limiter: a map change or a new session, so a warning that belongs to each
/// load logs again instead of staying quiet after its first few hits.
pub fn new_window() {
    WINDOW.fetch_add(1, Ordering::Relaxed);
}

struct Seen {
    window: u64,
    counts: HashMap<Box<str>, u64>,
}

/// Most distinct keys one [`Dedup`] remembers; at the limit it forgets all of them and starts a window of its own, so a
/// pathological stream of keys costs bounded memory and still logs (the callers' keys are asset or command names).
const MAX_KEYS: usize = 4096;

pub struct Dedup {
    seen: Mutex<Option<Seen>>,
}

impl Dedup {
    pub const fn new() -> Self {
        Self {
            seen: Mutex::new(None),
        }
    }

    /// Counts one more hit of `key`. Returns the count when it is time to log (a power of two), else `None`.
    /// Allocates only the first time a key is seen.
    pub fn hit(&self, key: &str) -> Option<u64> {
        let mut guard = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let window = WINDOW.load(Ordering::Relaxed);
        let seen = guard.get_or_insert_with(|| Seen {
            window,
            counts: HashMap::new(),
        });
        if seen.window != window || seen.counts.len() >= MAX_KEYS {
            seen.window = window;
            seen.counts.clear();
        }
        let n = match seen.counts.get_mut(key) {
            Some(n) => {
                *n += 1;
                *n
            }
            None => {
                seen.counts.insert(key.into(), 1);
                1
            }
        };
        n.is_power_of_two().then_some(n)
    }
}

impl Default for Dedup {
    fn default() -> Self {
        Self::new()
    }
}

/// Logs at `$level` the first time, and each time the count of this `$key` doubles, with a `seen` field. The key is
/// any `&str` (an asset or command name); the limiter is one per call site. Costs nothing when the level is off.
#[macro_export]
macro_rules! dedup {
    ($level:ident, $key:expr, $($arg:tt)+) => {{
        static LIMITER: $crate::Dedup = $crate::Dedup::new();
        if $crate::tracing::enabled!($crate::tracing::Level::$level) {
            if let Some(seen) = LIMITER.hit($key) {
                $crate::tracing::event!($crate::tracing::Level::$level, seen, $($arg)+);
            }
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_on_powers_of_two_per_key() {
        let d = Dedup::new();
        let hits: Vec<_> = (0..9).map(|_| d.hit("a")).collect();
        assert_eq!(
            hits,
            [
                Some(1),
                Some(2),
                None,
                Some(4),
                None,
                None,
                None,
                Some(8),
                None
            ]
        );
        assert_eq!(d.hit("b"), Some(1));
    }

    #[test]
    fn too_many_keys_start_over_instead_of_going_silent() {
        let d = Dedup::new();
        for i in 0..MAX_KEYS {
            d.hit(&i.to_string());
        }
        assert_eq!(d.hit("fresh"), Some(1));
        assert_eq!(d.hit("0"), Some(1));
    }
}
