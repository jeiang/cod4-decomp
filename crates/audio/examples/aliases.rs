// SPDX-License-Identifier: GPL-3.0-or-later
//! Lists alias names containing the given words: `cargo run -p audio --example aliases -- <map> <word>...`
use assets::vfs::Vfs;
use audio::bank::Bank;
use std::path::PathBuf;
use std::sync::Arc;

fn main() {
    let mut a = std::env::args().skip(1);
    let map = a.next().expect("map name");
    let words: Vec<String> = a.collect();
    let root = PathBuf::from(std::env::var_os("COD4_PATH").unwrap_or("COD4".into()));
    let vfs = Arc::new(Vfs::open_stock(&root, 0).unwrap());
    let zones: Vec<PathBuf> = ["code_post_gfx_mp", "localized_common_mp", map.as_str()]
        .iter()
        .map(|z| root.join(format!("zone/english/{z}.ff")))
        .collect();
    let bank = Bank::load(vfs, &zones).unwrap();
    let mut names: Vec<&str> = bank
        .names()
        .filter(|n| words.iter().any(|w| n.contains(w.as_str())))
        .collect();
    names.sort_unstable();
    for n in names {
        let al = bank.aliases_of(n);
        let ch = al
            .first()
            .map_or("?", |a| bank.channels[usize::from(a.channel)].name.as_str());
        println!("{n} x{} {ch}", al.len());
    }
    eprintln!("{:?}", bank.stats);
}
