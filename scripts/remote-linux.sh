#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Usage: scripts/remote-linux.sh [git-ref] [-- run-options...]
# Ships a git ref to the Linux test host, runs build/test/clippy/fmt in the devshell with
# COD4_PATH=~/cod4e/COD4, and, when run-options are given, runs
# `cod4e-harness run --out bundles <run-options>` (a bare `--` runs the default
# suite). The harness writes cod4e-run-<date>.zip into bundles/ on that host; that
# directory is copied back to ./bundles/ and the bundles are listed.
# The client is built in release next to the harness so stage 3 can drive it.
# Stage 3 needs a window: set COD4E_LINUX_WAYLAND_DISPLAY (e.g. wayland-1) and
# COD4E_LINUX_XDG_RUNTIME_DIR (e.g. /run/user/1000) to use the host's Wayland
# session; they are exported as WAYLAND_DISPLAY / XDG_RUNTIME_DIR for the run.
# Without them the stage reports `no display` and is skipped.
set -euo pipefail
ref=HEAD
if [ $# -gt 0 ] && [ "$1" != -- ]; then ref=$1; shift; fi
run=0
if [ "${1:-}" = -- ]; then run=1; shift; fi
host=${COD4E_LINUX_HOST:?set COD4E_LINUX_HOST to the ssh host of the Linux test machine}
dir="~/cod4e-run-$(hostname -s)-$$"  # per invocation: parallel runs must not wipe each other
harness=""
display=""
[ -n "${COD4E_LINUX_WAYLAND_DISPLAY:-}" ] && display="WAYLAND_DISPLAY=$(printf '%q' "$COD4E_LINUX_WAYLAND_DISPLAY") "
[ -n "${COD4E_LINUX_XDG_RUNTIME_DIR:-}" ] && display="${display}XDG_RUNTIME_DIR=$(printf '%q' "$COD4E_LINUX_XDG_RUNTIME_DIR") "
[ $run = 1 ] && harness="cargo build --release -p harness -p client && ${display}COD4E_NO_PROMPT=1 cargo run --release -p harness -- run --out bundles $(printf '%q ' "$@")"

git archive "$ref" | ssh "$host" "bash -c 'rm -rf $dir && mkdir -p $dir && tar x -C $dir'"
status=0
ssh "$host" "bash -c 'cd $dir && COD4_PATH=\$HOME/cod4e/COD4 nix develop -c bash -ec \"
cargo build --workspace && cargo test --workspace &&
cargo clippy --workspace -- -D warnings && cargo fmt --check
$harness
\"'" || status=$?
mkdir -p bundles
ssh "$host" "bash -c 'cd $dir && [ -d bundles ] && tar c bundles || true'" | tar x -C . 2>/dev/null || true
ssh "$host" "bash -c 'rm -rf $dir'" || true
ls -l bundles/*.zip 2>/dev/null || true
exit $status
