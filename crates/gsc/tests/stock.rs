// SPDX-License-Identifier: GPL-3.0-only
//! Compiles every stock MP script from an original install. Skips without `COD4_PATH`.

mod common;

use std::time::Instant;

use gsc::{Builtins, ErrorKind, Options, compile};

use common::stock_mp_scripts;

#[test]
fn all_stock_mp_scripts_compile() {
    let Some(scripts) = stock_mp_scripts() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    assert_eq!(scripts.len(), 210, "research/gsc counts 210 MP scripts");
    let sources: Vec<(&str, &str)> = scripts
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    let builtins = Builtins::stock_mp();
    for developer in [false, true] {
        let t = Instant::now();
        let result = compile(&sources, &builtins, Options { developer });
        let dt = t.elapsed();
        match result {
            Ok(p) => eprintln!(
                "developer={developer}: {} files, {} functions, {} bytes of code, {} strings, {dt:?}",
                p.files.len(),
                p.functions.len(),
                p.functions.iter().map(|f| f.code.len()).sum::<usize>(),
                p.strings.len(),
            ),
            Err(errs) => {
                for e in errs.iter().take(50) {
                    eprintln!("{e}");
                }
                let unknown: std::collections::BTreeSet<_> = errs
                    .iter()
                    .filter(|e| e.kind == ErrorKind::UnknownFunction)
                    .map(|e| e.message.as_str())
                    .collect();
                panic!("{} errors; unknown names: {unknown:?}", errs.len());
            }
        }
    }
}
