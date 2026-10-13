// SPDX-License-Identifier: GPL-3.0-only
//! Rate limiting for events that repeat (per frame, per message): a key logs on its 1st, 2nd, 4th, 8th ... hit with
//! the count so far, so a warning that fires every frame costs a handful of lines, not thousands.

use std::collections::HashMap;
use std::sync::Mutex;

/// Most distinct keys one [`Dedup`] remembers; beyond it new keys are dropped silently (the callers' keys are asset or
/// command names, so this only bounds a pathological stream).
const MAX_KEYS: usize = 4096;

pub struct Dedup {
    seen: Mutex<Option<HashMap<Box<str>, u64>>>,
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
        let map = guard.get_or_insert_with(HashMap::new);
        let full = map.len() >= MAX_KEYS;
        let n = match map.get_mut(key) {
            Some(n) => {
                *n += 1;
                *n
            }
            None if !full => {
                map.insert(key.into(), 1);
                1
            }
            None => return None,
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
}
