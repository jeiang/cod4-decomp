#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Usage: scripts/host-run.sh [--no-checks] [--exec] [-- <harness run options> | -- <command...>]
# Runs on the Linux test host, inside a checkout and the dev shell (scripts/remote-linux.sh calls it). Builds the
# workspace and, unless --no-checks, runs test/clippy/fmt. With `--` it then runs `cod4e-harness run --out bundles
# <options>` (a bare `--` runs the default suite); with --exec it runs <command...> instead, with the release
# binaries on PATH, writing into out/.
# COD4E_SLOT=1..3 selects a harness slot: its own target dir, headless display (scripts/virtual-display.sh) and
# loopback-only network namespace (scripts/netns.sh). Slot runs hold the host-wide stage lock shared, so up to three
# run at once. Without a slot the run is serial: it holds the stage lock exclusively, so timing-gated stages see a
# quiet host, and runs on its own headless display like a slot (the desktop session's state cannot stall its
# windows). A pending serial run (~/Projects/cod4e/.serial-pending) keeps new slot runs from starting.
set -euo pipefail
root=$HOME/Projects/cod4e
checks=1
exec_mode=0
while [ $# -gt 0 ]; do
  case $1 in
    --no-checks) checks=0; shift ;;
    --exec) exec_mode=1; shift ;;
    *) break ;;
  esac
done
run=0
if [ "${1:-}" = -- ]; then run=1; shift; fi
if [ -n "${COD4E_SLOT:-}" ]; then
  case $COD4E_SLOT in 1|2|3) ;; *) echo "COD4E_SLOT must be 1..3" >&2; exit 2 ;; esac
  export CARGO_TARGET_DIR=$root/slot-$COD4E_SLOT/target
  # The slot is held from the build on: two runs in one slot would otherwise rebuild each other's binaries.
  exec 9>"$root/.slot-$COD4E_SLOT.lock"
  flock -x 9
  # The stage lock is shared from the build on too, so a serial run's timing stages never share the host with a
  # slot's compile. A pending serial run keeps new slot runs (and their builds) from starting.
  while [ -e "$root/.serial-pending" ]; do sleep 10; done
  exec 8>>"$root/.stage-lock"
  flock -s 8
elif [ $exec_mode = 1 ]; then
  echo "--exec needs a slot" >&2
  exit 2
fi

nice -n 19 cargo build --workspace
if [ $checks = 1 ]; then
  nice -n 19 cargo test --workspace
  nice -n 19 cargo clippy --workspace -- -D warnings
  cargo fmt --check
fi
[ $run = 1 ] || exit 0

slot_wrap() {
  scripts/virtual-display.sh "$COD4E_SLOT" -- scripts/netns.sh -- "$@"
}

if [ $exec_mode = 1 ]; then
  nice -n 19 cargo build --release -p harness -p client -p server -p master
  mkdir -p out
  slot_wrap env PATH="$CARGO_TARGET_DIR/release:$PATH" "$@"
  exit
fi

# The browser client is built only for a run that has a web-* stage or the full suite (no --stage given).
case " $* " in
  *" --stage "*|*" --stage="*) case " $* " in *web-*) nice -n 19 web/build.sh >/dev/null ;; esac ;;
  *) nice -n 19 web/build.sh >/dev/null ;;
esac
nice -n 19 cargo build --release -p harness -p client -p server
h="${CARGO_TARGET_DIR:-target}/release/cod4e-harness"
chrome=$(scripts/find-chromium.sh)
if [ -n "${COD4E_SLOT:-}" ]; then
  # shellcheck disable=SC2086 # $chrome is an optional COD4E_CHROME=<path> assignment
  slot_wrap env COD4E_NO_PROMPT=1 $chrome "$h" run --out bundles "$@"
else
  touch "$root/.serial-pending"
  trap 'rm -f "$root/.serial-pending"' EXIT
  # shellcheck disable=SC2086
  flock -x "$root/.stage-lock" scripts/virtual-display.sh serial -- \
    env COD4E_NO_PROMPT=1 $chrome "$h" run --out bundles "$@"
fi
