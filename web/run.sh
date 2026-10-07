#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the wasm module and serve the page: `nix develop -c web/run.sh [port]`.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release --target wasm32-unknown-unknown -p web
wasm-bindgen --target web --no-typescript --out-dir web/pkg target/wasm32-unknown-unknown/release/web.wasm
wasm-opt -O2 --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext --enable-mutable-globals \
  --enable-multivalue --enable-reference-types -o web/pkg/web_bg.wasm web/pkg/web_bg.wasm
ls -l web/pkg/web_bg.wasm
exec python3 web/serve.py "$@"
