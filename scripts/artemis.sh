#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Usage: scripts/artemis.sh [git-ref] [-- run-options...]
# Ships a git ref to artemis, runs build/test/clippy/fmt in the devshell with
# COD4_PATH=~/cod4e/COD4, and, when run-options are given, runs
# `cod4e-harness run --out bundles <run-options>` (a bare `--` runs the default
# suite). The harness writes cod4e-run-<date>.zip into bundles/ on artemis; that
# directory is copied back to ./bundles/ and the bundles are listed.
set -euo pipefail
ref=HEAD
if [ $# -gt 0 ] && [ "$1" != -- ]; then ref=$1; shift; fi
run=0
if [ "${1:-}" = -- ]; then run=1; shift; fi
host=artemis.jeiang.vpn
dir='~/cod4e-run'
harness=""
[ $run = 1 ] && harness="COD4E_NO_PROMPT=1 cargo run --release -p harness -- run --out bundles $(printf '%q ' "$@")"

git archive "$ref" | ssh "$host" "bash -c 'rm -rf $dir && mkdir -p $dir && tar x -C $dir'"
status=0
ssh "$host" "bash -c 'cd $dir && COD4_PATH=\$HOME/cod4e/COD4 nix develop -c bash -ec \"
cargo build --workspace && cargo test --workspace &&
cargo clippy --workspace -- -D warnings && cargo fmt --check
$harness
\"'" || status=$?
mkdir -p bundles
ssh "$host" "bash -c 'cd $dir && [ -d bundles ] && tar c bundles || true'" | tar x -C . 2>/dev/null || true
ls -l bundles/*.zip 2>/dev/null || true
exit $status
