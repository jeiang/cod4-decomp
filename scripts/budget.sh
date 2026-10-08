#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Usage: scripts/budget.sh <map> <gametype> <minutes> [out-dir]
# One cell of the server budget matrix (ticket #56): 32 bots on <map> in <gametype> for
# <minutes>, pinned to CPU $BUDGET_CPU (default 0) with MemoryMax=512M when the host allows it. Run from a repo
# checkout with a release harness built (cargo build --release -p harness) and COD4_PATH set.
set -euo pipefail
map=${1:?map} gt=${2:?gametype} min=${3:?minutes} out=${4:-bundles}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cat > "$tmp/budget-$map-$gt.cfg" <<CFG
set g_gametype $gt
set scr_${gt}_timelimit 0
set scr_${gt}_scorelimit 0
set scr_${gt}_roundlimit 0
map $map
bots 32
wait ${min}m
status
quit
CFG
run=(target/release/cod4e-harness run --no-prompt --out "$out" --timeout $((min * 60 + 300)) --script "$tmp/budget-$map-$gt.cfg")
if command -v systemd-run >/dev/null && [ "$(uname)" = Linux ]; then
  systemd-run --user --scope --quiet -p MemoryMax=512M -p MemorySwapMax=0 taskset -c "${BUDGET_CPU:-0}" "${run[@]}"
else
  "${run[@]}"
fi
