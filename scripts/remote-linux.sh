#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Usage: scripts/remote-linux.sh [git-ref] [-- run-options... | -- command...]
# Ships a git ref to the Linux test host and runs scripts/host-run.sh there in the devshell with
# COD4_PATH=~/Projects/cod4e/shared/COD4: build/test/clippy/fmt and, when run-options are given,
# `cod4e-harness run --out bundles <run-options>` (a bare `--` runs the default suite). The host's bundles/ and
# out/ are copied back to ./bundles/ (a slot run's zips named slot-<n>-<zip>) and ./out/ and the bundles are listed.
# The release client and server are built next to the harness so the client stages can drive them; the browser
# client (web/pkg) is built when a web-* stage (or the whole suite) is run, and a Chromium already on the host is
# used (scripts/find-chromium.sh).
# Environment:
#   COD4E_LINUX_HOST              ssh host of the Linux test machine (required).
#   COD4E_LINUX_SLOT=1..3         run in a harness slot: own target dir, headless display and network namespace;
#                                 up to three slots run at once.
#   COD4E_LINUX_EXEC=1            (slot only) run the command after `--` in the slot instead of the harness, with
#                                 the release binaries on PATH; it writes into out/.
#   COD4E_LINUX_CHECKS=0          skip test/clippy/fmt (the build still runs).
#   COD4E_LINUX_WAYLAND_DISPLAY,
#   COD4E_LINUX_XDG_RUNTIME_DIR   (serial runs) the host's Wayland session for the client stages, e.g. wayland-1
#                                 and /run/user/1000; without them those stages report `no display` and skip.
# Without a slot the run is serial: it takes the host-wide stage lock exclusively, after the running slots finish
# and before new ones start, so timing checks see a quiet host. Builds and tests run at nice 19.
set -euo pipefail
ref=HEAD
if [ $# -gt 0 ] && [ "$1" != -- ]; then ref=$1; shift; fi
host=${COD4E_LINUX_HOST:?set COD4E_LINUX_HOST to the ssh host of the Linux test machine}
dir="~/Projects/cod4e/run-$(hostname -s)-$$"  # per invocation: parallel runs must not wipe each other
vars="COD4_PATH=\$HOME/Projects/cod4e/shared/COD4 "
[ -n "${COD4E_LINUX_SLOT:-}" ] && vars="${vars}COD4E_SLOT=$(printf '%q' "$COD4E_LINUX_SLOT") "
[ -n "${COD4E_LINUX_WAYLAND_DISPLAY:-}" ] && vars="${vars}WAYLAND_DISPLAY=$(printf '%q' "$COD4E_LINUX_WAYLAND_DISPLAY") "
[ -n "${COD4E_LINUX_XDG_RUNTIME_DIR:-}" ] && vars="${vars}XDG_RUNTIME_DIR=$(printf '%q' "$COD4E_LINUX_XDG_RUNTIME_DIR") "
opts=""
[ "${COD4E_LINUX_CHECKS:-1}" = 0 ] && opts="--no-checks "
[ "${COD4E_LINUX_EXEC:-0}" = 1 ] && opts="${opts}--exec "
if [ "${1:-}" = -- ]; then shift; opts="${opts}-- $(printf '%q ' "$@")"; fi

git archive "$ref" | ssh "$host" "bash -c 'rm -rf $dir && mkdir -p $dir && tar x -C $dir'"
status=0
ssh "$host" "bash -c 'cd $dir && ${vars}nix develop -c scripts/host-run.sh $opts'" || status=$?
mkdir -p bundles out
# Slot bundles get a slot-<n>- prefix: runs in two slots can start in the same second and name their zips alike.
tmp=$(mktemp -d)
ssh "$host" "bash -c 'cd $dir && [ -d bundles ] && tar c bundles || true'" | tar x -C "$tmp" 2>/dev/null || true
for z in "$tmp"/bundles/*; do
  [ -e "$z" ] && mv "$z" "bundles/${COD4E_LINUX_SLOT:+slot-$COD4E_LINUX_SLOT-}$(basename "$z")"
done
rm -rf "$tmp"
ssh "$host" "bash -c 'cd $dir && [ -d out ] && tar c out || true'" | tar x -C . 2>/dev/null || true
ssh "$host" "bash -c 'rm -rf $dir'" || true
ls -l bundles/*.zip 2>/dev/null || true
exit $status
