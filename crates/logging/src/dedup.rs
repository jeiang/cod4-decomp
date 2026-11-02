// SPDX-License-Identifier: GPL-3.0-only
//! Rate limiting for events that repeat (per frame, per message): a key logs on its 1st, 2nd, 4th, 8th ... hit with
//! the count so far, so a warning that fires every frame costs a handful of lines, not thousands.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

static WINDOW: AtomicU64 = AtomicU64::new(0);

/// Starts a new counting window for every limiter: a map change or a new session, so a warning that belongs to each
/// load logs again instead of staying quiet after its first few hits.
pub fn new_window() {
    WINDOW.fetch_add(1, Ordering::Relaxed);
}

static SCOPES: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The [`scoped`] run on this thread, 0 outside one.
    static SCOPE: Cell<u64> = const { Cell::new(0) };
    /// The limiters that run has counted in: its counts there go when it ends.
    static TOUCHED: RefCell<Vec<&'static Dedup>> = const { RefCell::new(Vec::new()) };
}

/// Runs `f` with counts of its own on this thread: every limiter it hits counts from its start, apart from the
/// process-wide counts and from other runs (other tests on other threads, at the same time), and the counts go when
/// it ends. [`new_window`] does not reset them. For [`crate::capture`], whose caller expects the first hit of a key
/// to log.
pub(crate) fn scoped<R>(f: impl FnOnce() -> R) -> R {
    struct Restore {
        scope: u64,
        outer: u64,
        touched: Vec<&'static Dedup>,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            for d in TOUCHED.replace(std::mem::take(&mut self.touched)) {
                if let Some(st) = d.lock().as_mut() {
                    st.scopes.retain(|(s, _)| *s != self.scope);
                }
            }
            SCOPE.set(self.outer);
        }
    }
    let scope = SCOPES.fetch_add(1, Ordering::Relaxed) + 1;
    let _restore = Restore {
        scope,
        outer: SCOPE.replace(scope),
        touched: TOUCHED.take(),
    };
    f()
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

#[derive(Default)]
struct Counts {
    counts: HashMap<Box<str>, u64>,
    overflow: u64,
}

impl Counts {
    fn hit(&mut self, key: &str) -> Hit {
        let full = self.counts.len() >= MAX_KEYS;
        let n = match self.counts.get_mut(key) {
            Some(n) => {
                *n += 1;
                *n
            }
            None if full => {
                self.overflow += 1;
                return if self.overflow.is_power_of_two() {
                    Hit::Overflow(self.overflow)
                } else {
                    Hit::Skip
                };
            }
            None => {
                self.counts.insert(key.into(), 1);
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

struct State {
    /// The [`new_window`] the process-wide counts belong to.
    window: u64,
    global: Counts,
    /// The counts of each [`scoped`] run that has hit this limiter and not ended yet.
    scopes: Vec<(u64, Counts)>,
}

pub struct Dedup {
    state: Mutex<Option<State>>,
}

impl Dedup {
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(None),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<State>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Counts one more hit of `key`: when to log (a power of two) and with what count. Allocates only the first time
    /// a key is seen.
    pub fn hit(&'static self, key: &str) -> Hit {
        let scope = SCOPE.get();
        let window = WINDOW.load(Ordering::Relaxed);
        let mut guard = self.lock();
        let st = guard.get_or_insert_with(|| State {
            window,
            global: Counts::default(),
            scopes: Vec::new(),
        });
        if scope == 0 {
            if st.window != window {
                st.window = window;
                st.global = Counts::default();
            }
            return st.global.hit(key);
        }
        let i = match st.scopes.iter().position(|(s, _)| *s == scope) {
            Some(i) => i,
            None => {
                TOUCHED.with_borrow_mut(|t| t.push(self));
                st.scopes.push((scope, Counts::default()));
                st.scopes.len() - 1
            }
        };
        st.scopes[i].1.hit(key)
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

    fn counts(d: &'static Dedup, key: &str, n: usize) -> Vec<Option<u64>> {
        (0..n)
            .map(|_| match d.hit(key) {
                Hit::Log(n) => Some(n),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn logs_on_powers_of_two_per_key() {
        static D: Dedup = Dedup::new();
        assert_eq!(
            counts(&D, "a", 9),
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
        assert_eq!(counts(&D, "b", 1), [Some(1)]);
    }

    #[test]
    fn too_many_keys_are_counted_together_and_said_so() {
        static D: Dedup = Dedup::new();
        for i in 0..MAX_KEYS {
            assert!(matches!(D.hit(&i.to_string()), Hit::Log(1)));
        }
        // New keys past the cap share one counter that logs on its own doublings; kept keys still count.
        assert!(matches!(D.hit("fresh-1"), Hit::Overflow(1)));
        assert!(matches!(D.hit("fresh-2"), Hit::Overflow(2)));
        assert!(matches!(D.hit("fresh-3"), Hit::Skip));
        assert!(matches!(D.hit("0"), Hit::Log(2)));
    }

    #[test]
    fn a_scoped_run_counts_apart_and_its_counts_go_when_it_ends() {
        static D: Dedup = Dedup::new();
        assert_eq!(counts(&D, "k", 2), [Some(1), Some(2)]);
        let inner = scoped(|| {
            let first = counts(&D, "k", 2);
            // A nested run counts apart from the one around it, which goes on where it was.
            let nested = scoped(|| counts(&D, "k", 1));
            (first, nested, counts(&D, "k", 2))
        });
        assert_eq!(
            inner,
            (vec![Some(1), Some(2)], vec![Some(1)], vec![None, Some(4)])
        );
        assert!(D.lock().as_ref().unwrap().scopes.is_empty());
        assert_eq!(counts(&D, "k", 2), [None, Some(4)]);
    }
}
