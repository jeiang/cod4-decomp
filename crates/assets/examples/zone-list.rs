// SPDX-License-Identifier: GPL-3.0-or-later
//! Print per-type top-level asset counts of fastfiles.
//!
//! `cargo run -p assets --example zone-list -- [--inflate] [--decode] <zone.ff | zone-name>...`
//! A bare name resolves against `$COD4_PATH/zone/english` (default `./COD4`).

use assets::zone::{KeepAll, XAssetType, Zone};
use std::fs::File;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let (mut inflate, mut decode) = (false, false);
    let mut targets = Vec::new();
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--inflate" => inflate = true,
            "--decode" => decode = true,
            _ => targets.push(a),
        }
    }
    if targets.is_empty() {
        eprintln!("usage: zone-list [--inflate] [--decode] <zone.ff | zone-name>...");
        std::process::exit(2);
    }
    let root = PathBuf::from(std::env::var_os("COD4_PATH").unwrap_or("COD4".into()));
    for t in targets {
        let mut path = PathBuf::from(&t);
        if !path.exists() {
            path = root.join("zone/english").join(format!("{t}.ff"));
        }
        let start = Instant::now();
        let zone = match File::open(&path)
            .map_err(Into::into)
            .and_then(|f| Zone::open(std::io::BufReader::new(f)))
        {
            Ok(z) => z,
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                std::process::exit(1);
            }
        };
        let counts = zone.counts();
        println!(
            "{}: {} assets, {} script strings, size {}, external {}  (list in {:?})",
            path.display(),
            zone.asset_types().len(),
            zone.script_strings().len(),
            zone.header().size,
            zone.header().external_size,
            start.elapsed()
        );
        for ty in XAssetType::all() {
            if counts[ty as usize] > 0 {
                println!("  {:<14}{}", ty.name(), counts[ty as usize]);
            }
        }
        let start = Instant::now();
        if decode {
            let mut n = 0usize;
            match zone.decode(&KeepAll, |_| n += 1) {
                Ok(st) => println!("  decoded, {} bytes, {:?}", st.consumed, start.elapsed()),
                Err(e) => println!("  decode stopped after {n} assets: {e}"),
            }
        } else if inflate {
            match zone.inflate_rest() {
                Ok(n) => println!("  inflated {n} bytes in {:?}", start.elapsed()),
                Err(e) => println!("  inflate failed: {e}"),
            }
        }
    }
}
