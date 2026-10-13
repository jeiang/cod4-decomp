// SPDX-License-Identifier: GPL-3.0-only
//! A new window (map change) makes a repeating warning log again. Alone in its process: the window is global.

#[test]
fn a_new_window_logs_again() {
    let d = logging::Dedup::new();
    let before: Vec<_> = (0..3).map(|_| d.hit("w")).collect();
    assert_eq!(before, [Some(1), Some(2), None]);
    logging::new_window();
    assert_eq!(d.hit("w"), Some(1));
}
