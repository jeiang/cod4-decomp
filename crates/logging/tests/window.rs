// SPDX-License-Identifier: GPL-3.0-only
//! A new window (map change) makes a repeating warning log again. Alone in its process: the window is global.

use logging::Hit;

#[test]
fn a_new_window_logs_again() {
    static D: logging::Dedup = logging::Dedup::new();
    let before: Vec<_> = (0..3).map(|_| D.hit("w")).collect();
    assert_eq!(before, [Hit::Log(1), Hit::Log(2), Hit::Skip]);
    logging::new_window();
    assert_eq!(D.hit("w"), Hit::Log(1));
}
