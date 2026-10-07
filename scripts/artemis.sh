#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Usage: scripts/artemis.sh [git-ref] [-- harness args...]
# Ships a git ref to artemis, runs build/test/clippy/fmt in the devshell with
# COD4_PATH=~/cod4e/COD4, optionally runs `cod4e-harness <args>`, and copies
# any bundles/ directory back to ./bundles/.
set -euo pipefail
ref=HEAD
if [ $# -gt 0 ] && [ "$1" != -- ]; then ref=$1; shift; fi
[ "${1:-}" = -- ] && shift
host=artemis.jeiang.vpn
dir='~/cod4e-run'
harness=""
[ $# -gt 0 ] && harness="cargo run --release -p harness -- $(printf '%q ' "$@")"

git archive "$ref" | ssh "$host" "bash -c 'rm -rf $dir && mkdir -p $dir && tar x -C $dir'"
ssh "$host" "bash -c 'cd $dir && COD4_PATH=\$HOME/cod4e/COD4 nix develop -c bash -ec \"
cargo build --workspace && cargo test --workspace &&
cargo clippy --workspace -- -D warnings && cargo fmt --check
$harness
\"'"
mkdir -p bundles
ssh "$host" "bash -c 'cd $dir && [ -d bundles ] && tar c bundles || true'" | tar x -C . 2>/dev/null || true
