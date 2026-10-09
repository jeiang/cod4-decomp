// SPDX-License-Identifier: GPL-3.0-only
//! The mixer callback must not allocate or free: it runs on a realtime thread (and, later, an AudioWorklet).
//! A counting global allocator watches `fill` through a busy mix: plays, replacements, finishing sounds, a
//! stream and a listener that moves.

use audio::channels::ChannelDef;
use audio::curve::Curve;
use audio::eq::{Band, EqType};
use audio::mixer::{Emitter, Listener, Pcm, Play, Source, Stream, mixer};
use audio::ring::ring;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

struct Counting;

thread_local! {
    static WATCH: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        count();
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(p, l, n) }
    }
}

fn count() {
    if WATCH.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCS.try_with(|a| a.set(a.get() + 1));
    }
}

#[global_allocator]
static A: Counting = Counting;

#[test]
fn fill_neither_allocates_nor_frees() {
    let chans: Vec<ChannelDef> = (0..4)
        .map(|i| ChannelDef {
            name: format!("c{i}"),
            priority: i as u8,
            is_3d: i % 2 == 1,
            restricted: i == 3,
            pausable: true,
            max_voices: 6,
        })
        .collect();
    let (mut h, mut m) = mixer(48_000, &chans);
    let pcm = Arc::new(Pcm {
        rate: 22_050,
        channels: 1,
        samples: vec![8000; 3000].into(),
    });
    let (mut tx, rx) = ring(1 << 14);
    tx.push(&vec![0.1; 6000]);
    tx.finish();
    let id = h.next_id();
    h.play(Play::new(
        id,
        Source::Stream(Stream {
            ring: rx,
            rate: 44_100,
            channels: 2,
        }),
        0,
    ));
    for i in 0..40 {
        let id = h.next_id();
        let mut p = Play::new(id, Source::Loaded(pcm.clone()), (i % 4) as u8);
        p.looping = i % 5 == 0;
        p.pitch = 0.8 + 0.01 * i as f32;
        p.entity = i % 3;
        p.emitter = Some(Emitter {
            pos: [10.0 * i as f32, 50.0, 0.0],
            min: 50.0,
            max: 900.0,
            curve: Curve::LINEAR,
        });
        h.play(p);
    }
    let mut out = vec![0.0f32; 1024];
    m.fill(&mut out);
    ALLOCS.with(|a| a.set(0));
    WATCH.with(|w| w.set(true));
    drop(std::hint::black_box(vec![0u8; 16]));
    assert_eq!(ALLOCS.with(Cell::get), 2, "the counter sees allocations");
    ALLOCS.with(|a| a.set(0));
    for i in 0..400 {
        h.set_listener(Listener::from_yaw([i as f32, 0.0, 0.0], i as f32 * 0.1));
        if i % 50 == 0 {
            // Changing the room retunes in place; the EQ and the reverb are part of the busy mix.
            h.set_reverb((i / 50 % 26) as u8, 0.6, 100);
            // Channel volume groups come and go while it mixes.
            h.set_channel_volumes(1 + (i / 50 % 3) as u8, [0.25; 64], 100);
            h.deactivate_channel_volumes(1 + ((i / 50 + 1) % 3) as u8, 100);
            h.set_eq(
                (i / 50 % 4) as u8,
                0,
                (i / 50 % 3) as u8,
                Some(Band {
                    kind: EqType::Bell,
                    gain_db: 6.0,
                    freq: 1000.0,
                    q: 1.0,
                }),
            );
        }
        m.fill(&mut out);
    }
    WATCH.with(|w| w.set(false));
    assert_eq!(ALLOCS.with(Cell::get), 0);
    assert!(out.iter().any(|s| *s != 0.0));
    h.reap(|_, _| {});
}
