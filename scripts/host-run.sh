#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Usage: scripts/host-run.sh [--no-checks] [--exec] [-- <harness run options> | -- <command...>]
# Runs on the Linux test host, inside a checkout and the dev shell (scripts/remote-linux.sh calls it). Builds the
# workspace and, unless --no-checks, runs test/clippy/fmt. With `--` it then runs `cod4e-harness run --out bundles
# <options>` (a bare `--` runs the default suite); with --exec it runs <command...> instead, with the release
# binaries on PATH, writing into out/.
# COD4E_SLOT=1..3 selects a harness slot: its own target dir, headless display (scripts/virtual-display.sh) and
# loopback-only network namespace (scripts/netns.sh). Every run holds the host-wide stage lock shared from its build
# on, so up to three slot runs work at once. Without a slot the run is serial: after its build it holds the stage lock
# exclusively, so timing-gated stages see a quiet host (no stage and no compile beside them), and runs on its own
# headless display like a slot (the desktop session's state cannot stall its windows). A pending serial run
# (~/Projects/cod4e/.serial-pending) keeps new runs from starting.
set -euo pipefail
root=$HOME/Projects/cod4e
# All waits are bounded, including a live but stuck owner. Diagnostics name the kernel lock holders rather than a
# PID file that may have outlived its owner. The marker's short critical sections share fd 6; serial owners queue on
# fd 7 so one owner's EXIT trap cannot clear another owner's pending marker.
lock_timeout=1800
lock_holders() {
  echo "lock holders on $root (PID COMMAND PATH):" >&2
  lslocks --notruncate --output PID,COMMAND,PATH >&2 || true
}
take_lock() {
  local mode=$1 fd=$2 label=$3
  if ! flock -n "$mode" "$fd"; then
    echo "waiting up to ${lock_timeout}s for $label" >&2
    lock_holders
    if ! flock -w "$lock_timeout" "$mode" "$fd"; then
      echo "timed out waiting for $label after ${lock_timeout}s" >&2
      lock_holders
      exit 1
    fi
  fi
}
exec 6>>"$root/.serial-marker.lock"
pending_owner() {
  take_lock -x 6 "serial marker"
  owner=
  if [ -e "$root/.serial-pending" ]; then
    read -r owner < "$root/.serial-pending" || true
    if [[ ! $owner =~ ^[1-9][0-9]*$ ]] || ! kill -0 "$owner" 2>/dev/null; then
      echo "removing orphaned serial marker (owner '${owner:-missing}' is not alive)" >&2
      rm -f "$root/.serial-pending"
      owner=
    fi
  fi
  flock -u 6
}
clear_pending() {
  take_lock -x 6 "serial marker cleanup"
  local owner=
  read -r owner < "$root/.serial-pending" 2>/dev/null || true
  if [ "$owner" = "$$" ]; then rm -f "$root/.serial-pending"; fi
  flock -u 6
}
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
  take_lock -x 9 "slot $COD4E_SLOT"
elif [ $exec_mode = 1 ]; then
  echo "--exec needs a slot" >&2
  exit 2
fi
# Every build holds the stage lock shared, a serial run's included, so a serial run's timing stages never share the
# host with a compile. A pending serial run keeps new runs (and their builds) from starting.
deadline=$((SECONDS + lock_timeout))
pending_owner
if [ -n "$owner" ]; then
  echo "waiting up to ${lock_timeout}s for serial owner PID $owner" >&2
  ps -p "$owner" -o pid=,args= >&2 || true
fi
while [ -n "$owner" ]; do
  if [ "$SECONDS" -ge "$deadline" ]; then
    echo "timed out waiting for serial owner PID $owner after ${lock_timeout}s" >&2
    lock_holders
    exit 1
  fi
  sleep 2
  pending_owner
done
exec 8>>"$root/.stage-lock"
take_lock -s 8 "shared stage lock (build/run)"

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
  flock -u 8
  exec 7>>"$root/.serial-queue.lock"
  take_lock -x 7 "serial queue"
  take_lock -x 6 "serial marker"
  printf '%s\n' "$$" > "$root/.serial-pending"
  flock -u 6
  trap clear_pending EXIT
  take_lock -x 8 "exclusive stage lock (serial run)"
  # shellcheck disable=SC2086
  scripts/virtual-display.sh serial -- \
    env COD4E_NO_PROMPT=1 $chrome "$h" run --out bundles "$@"
fi
