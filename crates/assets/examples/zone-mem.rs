// SPDX-License-Identifier: GPL-3.0-or-later
//! Decode the dedicated-server zone set with `Consumer::Server`, keep every
//! asset alive and print the retained heap and per-type asset counts.
//!
//! `cargo run --release -p assets --example zone-mem` (zones from
//! `$COD4_PATH/zone/english`, default `./COD4`). Wrap in `/usr/bin/time -l`
//! for peak RSS.

use assets::zone::{Consumer, DecodeFilter, XAssetType, Zone};
use std::alloc::{GlobalAlloc, Layout, System};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

/// Counts live heap bytes.
struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let n = LIVE.fetch_add(l.size(), Relaxed) + l.size();
        PEAK.fetch_max(n, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        if new >= l.size() {
            let n = LIVE.fetch_add(new - l.size(), Relaxed) + new - l.size();
            PEAK.fetch_max(n, Relaxed);
        } else {
            LIVE.fetch_sub(l.size() - new, Relaxed);
        }
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

const SET: [&str; 5] = [
    "code_post_gfx_mp",
    "localized_code_post_gfx_mp",
    "common_mp",
    "localized_common_mp",
    "mp_crash",
];

fn main() {
    let root = PathBuf::from(std::env::var_os("COD4_PATH").unwrap_or("COD4".into()));
    let mut kept = Vec::new();
    let mut counts = [0usize; XAssetType::COUNT];
    for name in SET {
        let path = root.join("zone/english").join(format!("{name}.ff"));
        let f = File::open(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let zone = Zone::open(BufReader::new(f)).unwrap_or_else(|e| panic!("{name}: {e}"));
        for (c, n) in counts.iter_mut().zip(zone.counts()) {
            *c += n;
        }
        zone.decode(&Consumer::Server, |a| kept.push(a))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    let retained = LIVE.load(Relaxed);
    println!("retained heap: {:.2} MiB", retained as f64 / 1048576.0);
    println!(
        "peak heap:     {:.2} MiB",
        PEAK.load(Relaxed) as f64 / 1048576.0
    );
    println!("kept assets:   {}", kept.len());
    for ty in XAssetType::all() {
        if counts[ty as usize] > 0 && Consumer::Server.keep(ty) {
            println!("  {:<14}{}", ty.name(), counts[ty as usize]);
        }
    }
}
