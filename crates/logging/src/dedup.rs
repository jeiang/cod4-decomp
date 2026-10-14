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
    overflow: u64,
}

/// Most distinct keys one [`Dedup`] counts apart. Keys are asset or command names, some of them sent by a remote
/// peer, so beyond this the new keys are counted together and logged as such (see [`Hit::Overflow`]): a hostile
/// stream of names costs a handful of lines, not one per name.
const MAX_KEYS: usize = 512;

#[derive(Debug, PartialEq, Eq)]
pub enum Hit {
    /// Log the event; the count of this key so far.
    Log(u64),
    /// A new key past the cap: log that there are too many distinct keys here; the count of such hits so far.
    Overflow(u64),
    Skip,
}

pub struct Dedup {
    seen: Mutex<Option<Seen>>,
}

impl Dedup {
    pub const fn new() -> Self {
        Self {
            seen: Mutex::new(None),
        }
    }

    /// Counts one more hit of `key`: when to log (a power of two) and with what count. Allocates only the first time
    /// a key is seen.
    pub fn hit(&self, key: &str) -> Hit {
        let mut guard = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let window = WINDOW.load(Ordering::Relaxed);
        let seen = guard.get_or_insert_with(|| Seen {
            window,
            counts: HashMap::new(),
            overflow: 0,
        });
        if seen.window != window {
            seen.window = window;
            seen.counts.clear();
            seen.overflow = 0;
        }
        let full = seen.counts.len() >= MAX_KEYS;
        let n = match seen.counts.get_mut(key) {
            Some(n) => {
                *n += 1;
                *n
            }
            None if full => {
                seen.overflow += 1;
                return if seen.overflow.is_power_of_two() {
                    Hit::Overflow(seen.overflow)
                } else {
                    Hit::Skip
                };
            }
            None => {
                seen.counts.insert(key.into(), 1);
                1
            }
        };
        if n.is_power_of_two() {
            Hit::Log(n)
        } else {
            Hit::Skip
        }
    }
}

impl Default for Dedup {
    fn default() -> Self {
        Self::new()
    }
}

/// Logs at `$level` the first time, and each time the count of this `$key` doubles, with a `seen` field. The key is
/// any `&str` (an asset or command name, or a constant); the limiter is one per call site. Costs nothing when the
/// level is off.
#[macro_export]
macro_rules! dedup {
    // The message first, then fields (`"what", asset = name`): tracing wants the fields first, so move the message last.
    ($level:ident, $key:expr, $msg:literal, $($rest:tt)+) => {
        $crate::dedup!(@go $level, $key, ($($rest)+), $msg)
    };
    ($level:ident, $key:expr, $($arg:tt)+) => {
        $crate::dedup!(@raw $level, $key, $($arg)+)
    };
    (@go $level:ident, $key:expr, ($($fields:tt)+), $msg:literal) => {
        $crate::dedup!(@raw $level, $key, $($fields)+, $msg)
    };
    (@raw $level:ident, $key:expr, $($arg:tt)+) => {{
        static LIMITER: $crate::Dedup = $crate::Dedup::new();
        if $crate::tracing::enabled!($crate::tracing::Level::$level) {
            match LIMITER.hit($key) {
                $crate::Hit::Log(seen) => {
                    $crate::tracing::event!($crate::tracing::Level::$level, seen, $($arg)+);
                }
                $crate::Hit::Overflow(seen) => {
                    $crate::tracing::event!(
                        $crate::tracing::Level::$level,
                        seen,
                        "too many distinct keys at this call site; the rest are counted together"
                    );
                }
                $crate::Hit::Skip => {}
            }
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(d: &Dedup, key: &str, n: usize) -> Vec<Option<u64>> {
        (0..n)
            .map(|_| match d.hit(key) {
                Hit::Log(n) => Some(n),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn logs_on_powers_of_two_per_key() {
        let d = Dedup::new();
        assert_eq!(
            counts(&d, "a", 9),
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
        assert_eq!(counts(&d, "b", 1), [Some(1)]);
    }

    #[test]
    fn too_many_keys_are_counted_together_and_said_so() {
        let d = Dedup::new();
        for i in 0..MAX_KEYS {
            assert!(matches!(d.hit(&i.to_string()), Hit::Log(1)));
        }
        // New keys past the cap share one counter that logs on its own doublings; kept keys still count.
        assert!(matches!(d.hit("fresh-1"), Hit::Overflow(1)));
        assert!(matches!(d.hit("fresh-2"), Hit::Overflow(2)));
        assert!(matches!(d.hit("fresh-3"), Hit::Skip));
        assert!(matches!(d.hit("0"), Hit::Log(2)));
    }
}
