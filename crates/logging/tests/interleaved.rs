// SPDX-License-Identifier: GPL-3.0-only
//! Two captures on two threads hitting one `dedup!` site in turn (A, B, A, B ...) each count their own hits (#438).

use std::sync::{Arc, Barrier};

fn limited() {
    logging::dedup!(WARN, "k", "a limited warning");
}

#[test]
fn interleaved_captures_count_their_own_hits() {
    let turn = Arc::new(Barrier::new(2));
    let run = |first: bool| {
        let turn = Arc::clone(&turn);
        std::thread::spawn(move || {
            logging::capture(|| {
                for _ in 0..4 {
                    if first {
                        limited();
                        turn.wait();
                        turn.wait();
                    } else {
                        turn.wait();
                        limited();
                        turn.wait();
                    }
                }
            })
            .1
            .iter()
            .filter_map(|e| e.field("seen").map(str::to_owned))
            .collect::<Vec<_>>()
        })
    };
    let (a, b) = (run(true), run(false));
    assert_eq!(a.join().unwrap(), ["1", "2", "4"]);
    assert_eq!(b.join().unwrap(), ["1", "2", "4"]);
}
