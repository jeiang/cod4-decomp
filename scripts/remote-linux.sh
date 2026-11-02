#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Usage: scripts/remote-linux.sh [git-ref] [-- run-options... | -- command...]
# Ships a git ref to the Linux test host and runs scripts/host-run.sh there in the devshell with
# COD4_PATH=~/Projects/cod4e/shared/COD4: build/test/clippy/fmt and, when run-options are given,
# `cod4e-harness run --out bundles <run-options>` (a bare `--` runs the default suite). The host's bundles/ and
# out/ are copied back to ./bundles/ (a slot run's zips named slot-<n>-<zip>) and ./out/, and this run's copied
# bundles are named (`local bundle: <path>`).
# The release client and server are built next to the harness so the client stages can drive them; the browser
# client (web/pkg) is built when a web-* stage (or the whole suite) is run, and a Chromium already on the host is
# used (scripts/find-chromium.sh).
# Every invocation keeps its whole output in its own bundles/logs/remote-<UTC time>-<random> file, early failures
# included, ending with `exit=<status>`; the path is printed last on stderr (`log: <path>`). The exit status is the
# host run's (or the first local failure's), whatever the caller does with the output.
# Environment:
#   COD4E_LINUX_HOST              ssh host of the Linux test machine (required).
#   COD4E_LINUX_SLOT=1..3         run in a harness slot: own target dir, headless display and network namespace;
#                                 up to three slots run at once.
#   COD4E_LINUX_SLOT_TOKEN        (slot only) the slot's lease token: the run starts through the host's installed
#                                 ~/Projects/cod4e/bin/slot-guard (scripts/slot-guard.py), which refuses it (exit 75)
#                                 unless the token holds the slot's lease. Unset, the run is unguarded as before.
#   COD4E_LINUX_EXEC=1            (slot only) run the command after `--` in the slot instead of the harness, with
#                                 the release binaries on PATH; it writes into out/.
#   COD4E_LINUX_CHECKS=0          skip test/clippy/fmt (the build still runs).
# Without a slot the run is serial: it takes the host-wide stage lock exclusively, after the running slots finish
# and before new ones start, so timing checks see a quiet host; it gets its own headless display like a slot.
# Builds and tests run at nice 19.
set -euo pipefail
# The log is written by this script, so it is whole whatever the caller keeps of the output, as long as the caller
# reads it to the end (`| tail`, `| grep`, a file): a reader that closes early (`| head`) stops the copies, and the log
# with them. stdout and stderr stay apart (each copied to the log as well); the EXIT trap waits for both copies before
# it exits with the run's status.
mkdir -p bundles/logs
log=$(mktemp "bundles/logs/remote-$(date -u +%Y%m%dT%H%M%SZ)-XXXXXXXX")
exec 3>&1 4>&2
exec > >(exec tee -i -a "$log")
out_tee=$!
exec 2> >(exec tee -i -a "$log" >&2)
err_tee=$!
finish() {
  local status=$?
  exec 2>&4
  wait "$err_tee" || true
  exec 1>&3
  wait "$out_tee" || true
  printf 'exit=%s\n' "$status" >> "$log"
  printf 'log: %s\n' "$log" >&2
  exit "$status"
}
trap finish EXIT
ref=HEAD
if [ $# -gt 0 ] && [ "$1" != -- ]; then ref=$1; shift; fi
{
  printf 'started=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'ref=%s\n' "$ref"
} >> "$log"
host=${COD4E_LINUX_HOST:?set COD4E_LINUX_HOST to the ssh host of the Linux test machine}
# The ref is resolved once: the shipped tree and the build hash name the same commit even if a branch moves.
commit=$(git rev-parse --verify "$ref^{commit}")
dir="~/Projects/cod4e/run-$(hostname -s)-$$"  # per invocation: parallel runs must not wipe each other
slot=${COD4E_LINUX_SLOT:-}
token=${COD4E_LINUX_SLOT_TOKEN:-}
if [ -n "$token" ] && [ -z "$slot" ]; then
  echo "COD4E_LINUX_SLOT_TOKEN needs COD4E_LINUX_SLOT" >&2
  exit 2
fi
# The shipped tree has no .git, so the harness's build script cannot name the commit; pass it in.
vars="COD4_PATH=\$HOME/Projects/cod4e/shared/COD4 COD4E_BUILD_HASH=$(git rev-parse --short=12 "$commit") "
[ -n "$slot" ] && vars="${vars}COD4E_SLOT=$(printf '%q' "$slot") "
opts=""
[ "${COD4E_LINUX_CHECKS:-1}" = 0 ] && opts="--no-checks "
[ "${COD4E_LINUX_EXEC:-0}" = 1 ] && opts="${opts}--exec "
# A bare `--` must add no argument: printf with no arguments still prints its format once, as `''`.
if [ "${1:-}" = -- ]; then shift; opts="${opts}-- "; [ $# -gt 0 ] && opts="${opts}$(printf '%q ' "$@")"; fi
# The guard is the host's installed copy, never the shipped tree's: the ref under test cannot change the check.
launch="nix develop -c scripts/host-run.sh $opts"
guard_check=
if [ -n "$token" ]; then
  guard='$HOME/Projects/cod4e/bin/slot-guard'
  guard_check="if [ ! -x \"$guard\" ]; then echo \"slot-guard is not installed on this host (just slot-guard-install)\" >&2; exit 75; fi"
  launch="COD4E_SLOT_TOKEN=$(printf '%q' "$token") \"$guard\" run --protocol 1 $(printf '%q' "$slot") -- $launch"
fi
{
  printf 'commit=%s\n' "$commit"
  printf 'host=%s\n' "$host"
  printf 'run_dir=%s\n' "$dir"
  printf 'slot=%s guarded=%s\n' "${slot:-serial}" "$([ -n "$token" ] && echo yes || echo no)"
  printf 'host_run_options=%s\n' "$opts"
} >> "$log"

# `tar x -m` stamps every file with the extraction time: git archive carries commit times, and a slot's target dir
# outlives a run, so an older ref's sources would look older than the last build and cargo would reuse its binaries.
git archive "$commit" | ssh "$host" "bash -c 'rm -rf $dir && mkdir -p $dir && tar x -m -C $dir'"
status=0
# The command goes to the host as a file: the login shell there is fish, and printf %q output for arguments with
# quotes, newlines or other specials ($'...') must be parsed by bash alone. It can carry the lease token, so only its
# owner may read it, and it is removed with the run directory.
printf 'cd %s || exit\n%s\n%s%s\n' "$dir" "$guard_check" "$vars" "$launch" |
  ssh "$host" "bash -c 'umask 077 && cat > $dir/.host-cmd'"
ssh "$host" "bash -c 'bash $dir/.host-cmd'" || status=$?
printf 'host_status=%s\n' "$status" >> "$log"
mkdir -p bundles out
# Slot bundles get a slot-<n>- prefix: runs in two slots can start in the same second and name their zips alike.
tmp=$(mktemp -d)
ssh "$host" "bash -c 'cd $dir && [ -d bundles ] && tar c bundles || true'" | tar x -C "$tmp" 2>/dev/null || true
copied=()
for z in "$tmp"/bundles/*; do
  [ -e "$z" ] || continue
  dest="bundles/${slot:+slot-$slot-}$(basename "$z")"
  mv "$z" "$dest"
  copied+=("$dest")
done
rm -rf "$tmp"
ssh "$host" "bash -c 'cd $dir && [ -d out ] && tar c out || true'" | tar x -C . 2>/dev/null || true
ssh "$host" "bash -c 'rm -rf $dir'" || true
for p in ${copied[@]+"${copied[@]}"}; do echo "local bundle: $p"; done
exit $status
