#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the browser client into web/pkg: `nix develop -c web/build.sh [debug]`, then serve with web/serve.py.
set -euo pipefail
cd "$(dirname "$0")/.."
profile=release
flags=(--release)
if [ "${1:-}" = debug ]; then profile=debug; flags=(); fi
cargo build "${flags[@]}" --target wasm32-unknown-unknown -p client
out="${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/$profile/cod4e.wasm"
wasm-bindgen --target web --no-typescript --out-dir web/pkg "$out"
if [ "$profile" = release ]; then
  wasm-opt -O2 --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext --enable-mutable-globals \
    --enable-multivalue --enable-reference-types -o web/pkg/cod4e_bg.wasm web/pkg/cod4e_bg.wasm
fi
ls -l web/pkg/cod4e_bg.wasm
