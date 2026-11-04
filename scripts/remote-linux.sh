#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Usage: scripts/remote-linux.sh [git-ref] [-- run-options... | -- command...]
# Recovery: scripts/remote-linux.sh --attach '~/Projects/cod4e/run-<identity>'
# Ships a git ref to the Linux test host and runs scripts/host-run.sh there in the devshell with
# COD4_PATH=~/Projects/cod4e/shared/COD4: build/test/clippy/fmt and, when run-options are given,
# `cod4e-harness run --out bundles <run-options>` (a bare `--` runs the default suite). The host's bundles/ and
# out/ are copied back to ./bundles/ (a slot run's zips named slot-<n>-<zip>) and ./out/, and this run's copied
# bundles are named (`local bundle: <path>`).
# The release client and server are built next to the harness so the client stages can drive them; the browser
# client (web/pkg) is built when a web-* stage (or the whole suite) is run, and a Chromium already on the host is
# used (scripts/find-chromium.sh); without one such a run refuses to start (exit 78, naming `just chromium-install`).
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
#   COD4E_HOST_WAIT_TIMEOUT       seconds the host run may wait in all for the host's locks and queue before it gives
#                                 up with exit 76 (default 21600; 0 waits without a bound); passed to host-run.sh.
#   COD4E_HOST_WAIT_REPORT        seconds between a waiting host run's queue reports (default 300).
# When the host run gave up waiting, the last stderr line before `log:` is `remote: host run gave up waiting: …`.
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
# Keep log draining outside the worker's temporary command redirections. An EXIT trap entered from a signal
# handler inside `cat ... >&2` otherwise retains bash's saved pipe descriptor and can wait forever for tee.
controller=$$
(
trap - EXIT
scratch=
trap '[ -z "$scratch" ] || rm -rf "$scratch"' EXIT
dir=
interrupted() {
  echo "remote: follower stopped; the host run keeps its lease and locks: $host $dir" >&2
  echo "remote: recover with COD4E_LINUX_HOST=$host scripts/remote-linux.sh --attach '$dir'" >&2
  exit "$1"
}
trap 'interrupted 130' INT
trap 'interrupted 143' TERM
trap 'interrupted 129' HUP
host=${COD4E_LINUX_HOST:?set COD4E_LINUX_HOST to the ssh host of the Linux test machine}
transport="$(cd "$(dirname "$0")" && pwd)/host-ssh.py"
# The helper owns detector-free config and the shared cross-worktree master.
host_ssh() { python3 "$transport" "$host" "$@"; }
retry_ssh() {
  local dest=$1 rc attempt=0 delay
  shift
  while :; do
    kill -0 "$controller" 2>/dev/null || exit 143
    host_ssh "$@" > "$dest" && return 0
    rc=$?
    [ "$rc" = 255 ] || return "$rc"
    if [ "$attempt" = 4 ]; then
      echo "remote: reconnect attempts exhausted; retained $host $dir for recovery" >&2
      return "$rc"
    fi
    case $attempt in 0) delay=5;; 1) delay=10;; 2) delay=20;; 3) delay=30;; esac
    delay="$delay.$((RANDOM % 1000))"
    attempt=$((attempt + 1))
    echo "remote: connection lost; host state unknown, reconnecting in ${delay}s ($dir)" >&2
    sleep "$delay"
  done
}
attach=0
if [ "${1:-}" = --attach ]; then
  [ $# = 2 ] || { echo "usage: $0 --attach <run-directory>" >&2; exit 2; }
  dir=$2
  [[ $dir =~ ^~/Projects/cod4e/run-[a-zA-Z0-9._-]+$ ]] || {
    echo "remote: invalid run directory: $dir" >&2; exit 2;
  }
  attach=1
  shift 2
fi
ref=HEAD
if [ $# -gt 0 ] && [ "$1" != -- ]; then ref=$1; shift; fi
{
  printf 'started=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'ref=%s\n' "$ref"
} >> "$log"
if [ "$attach" = 0 ]; then
# The ref is resolved once: the shipped tree and the build hash name the same commit even if a branch moves.
commit=$(git rev-parse --verify "$ref^{commit}")
dir="~/Projects/cod4e/run-$(hostname -s)-$(date -u +%Y%m%dT%H%M%SZ)-${log##*-}"
slot=${COD4E_LINUX_SLOT:-}
token=${COD4E_LINUX_SLOT_TOKEN:-}
if [ -n "$token" ] && [ -z "$slot" ]; then
  echo "COD4E_LINUX_SLOT_TOKEN needs COD4E_LINUX_SLOT" >&2
  exit 2
fi
# The shipped tree has no .git, so the harness's build script cannot name the commit; pass it in.
vars="COD4_PATH=\$HOME/Projects/cod4e/shared/COD4 COD4E_BUILD_HASH=$(git rev-parse --short=12 "$commit") "
for v in COD4E_HOST_WAIT_TIMEOUT COD4E_HOST_WAIT_REPORT; do
  [ -n "${!v:-}" ] || continue
  if [[ ! ${!v} =~ ^[0-9]+$ ]]; then
    echo "$v must be a whole number of seconds" >&2
    exit 2
  fi
  vars="${vars}$v=$(printf '%q' "${!v}") "
done
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
# `tar x -m` prevents an older ref reusing a slot's newer compiled binaries.
git archive "$commit" | host_ssh "bash -c 'umask 077 && mkdir $dir && tar x -m -C $dir'"
# The command file is private: it may contain the lease token, never printed to logs or process arguments.
printf 'cd %s || exit\n%s\n%s%s\n' "$dir" "$guard_check" "$vars" "$launch" |
  host_ssh "bash -c 'umask 077 && cat > $dir/.host-cmd'"
printf '%s\n' "$slot" | host_ssh "bash -c 'cat > $dir/.host-slot'"
# This transport belongs to the launching checkout, not the ref being tested. The detached session encloses the
# installed guard as well as host-run.sh; queue ownership, inherited lease locks and EXIT cleanup are unchanged.
host_ssh "bash -c 'umask 077 && cat > $dir/.host-transport'" <<'TRANSPORT'
#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
identity_live() {
  local pid start
  [ -f .host-identity ] || return 1
  { read -r pid; IFS= read -r start; } < .host-identity
  [ "$(ps -o lstart= -p "$pid" 2>/dev/null)" = "$start" ]
}
case $1 in
  start)
    command -v setsid >/dev/null && command -v nohup >/dev/null && command -v flock >/dev/null || {
      echo "remote: detachment needs setsid, nohup and flock on the host" >&2; exit 2;
    }
    if [ ! -f .host-identity ] && [ ! -f .host-status ]; then
      nohup setsid bash .host-transport supervise </dev/null >.host-supervisor.log 2>&1 3>&- 4>&- &
    fi
    # Wait only for durable identity, not for the run. Repeating start after a lost acknowledgement is safe.
    for ((i=0; i<50; i++)); do
      [ ! -f .host-identity ] || exit 0
      sleep .1
    done
    echo "remote: supervisor did not publish its identity; inspect .host-supervisor.log" >&2
    exit 2 ;;
  supervise)
    exec 5>.host-launch.lock
    flock -n 5 || exit 0
    [ ! -f .host-identity ] && [ ! -f .host-status ] || exit 0
    : > .host-stdout
    : > .host-stderr
    { printf '%s\n' "$$"; ps -o lstart= -p "$$"; ps -o pgid= -p "$$"; } > .host-identity.tmp
    mv .host-identity.tmp .host-identity
    rc=0
    bash .host-cmd >.host-stdout 2>.host-stderr || rc=$?
    printf '%s\n' "$rc" > .host-status.tmp
    mv .host-status.tmp .host-status ;;
  poll)
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    # Read status BEFORE sizes: completion means the command's final output is already in these files.
    live=0
    identity_live && live=1
    if [ -f .host-status ]; then
      cp .host-status "$tmp/status"
      echo done > "$tmp/state"
    elif [ "$live" = 1 ]; then
      echo running > "$tmp/state"
    else
      echo dead > "$tmp/state"
    fi
    for stream in stdout stderr; do
      offset=$2; shift
      size=$(wc -c < ".host-$stream")
      dd if=".host-$stream" of="$tmp/$stream" bs=65536 iflag=skip_bytes,count_bytes \
        skip="$offset" count="$((size - offset))" status=none
    done
    cp .host-slot "$tmp/slot"
    tar c -C "$tmp" . ;;
  collect)
    [ -f .host-status ] || exit 2
    files=(.host-status)
    for f in bundles out .host-run-gave-up; do [ ! -e "$f" ] || files+=("$f"); done
    tar c "${files[@]}" ;;
  *) exit 2 ;;
esac
TRANSPORT
echo "remote: detached run $host $dir"
retry_ssh /dev/null "bash -c 'bash $dir/.host-transport start'"
else
  printf 'host=%s\nrun_dir=%s\n' "$host" "$dir" >> "$log"
  echo "remote: attaching to $host $dir"
fi

scratch=$(mktemp -d)
out_offset=0 err_offset=0
while :; do
  # Download to a file first: a dropped connection must not print a partial response or advance the offsets.
  retry_ssh "$scratch/snapshot.tar" "bash -c 'bash $dir/.host-transport poll $out_offset $err_offset'"
  mkdir -p "$scratch/snapshot"
  tar xf "$scratch/snapshot.tar" -C "$scratch/snapshot"
  cat "$scratch/snapshot/stdout"
  cat "$scratch/snapshot/stderr" >&2
  out_offset=$((out_offset + $(wc -c < "$scratch/snapshot/stdout")))
  err_offset=$((err_offset + $(wc -c < "$scratch/snapshot/stderr")))
  slot=$(cat "$scratch/snapshot/slot")
  state=$(cat "$scratch/snapshot/state")
  case $state in
    done) status=$(cat "$scratch/snapshot/status"); break ;;
    running) sleep 5 ;;
    *) echo "remote: host supervisor is gone without an exit status; retained $dir for inspection" >&2; exit 255 ;;
  esac
done
[[ $status =~ ^[0-9]+$ ]] && [ "$status" -le 255 ] || {
  echo "remote: invalid host exit status in $dir" >&2; exit 2;
}
printf 'host_status=%s\n' "$status" >> "$log"
mkdir -p bundles out "$scratch/artifacts"
retry_ssh "$scratch/artifacts.tar" "bash -c 'bash $dir/.host-transport collect'"
tar xf "$scratch/artifacts.tar" -C "$scratch/artifacts"
copied=()
for z in "$scratch"/artifacts/bundles/*; do
  [ -e "$z" ] || continue
  dest="bundles/${slot:+slot-$slot-}$(basename "$z")"
  mv "$z" "$dest"
  copied+=("$dest")
done
if [ -d "$scratch/artifacts/out" ]; then cp -R "$scratch/artifacts/out/." out/; fi
gave_up=
if [ -f "$scratch/artifacts/.host-run-gave-up" ]; then gave_up=$(cat "$scratch/artifacts/.host-run-gave-up"); fi
# Only a completed, collected run may be removed. Killing the follower never touches the host process or its files.
kill -0 "$controller" 2>/dev/null || exit 143
retry_ssh /dev/null "bash -c 'rm -rf $dir'"
rm -rf "$scratch"
scratch=
for p in ${copied[@]+"${copied[@]}"}; do echo "local bundle: $p"; done
[ -z "$gave_up" ] || echo "remote: host run gave up waiting: ${gave_up#host-run: }" >&2
exit "$status"
) &
worker=$!
signal_status=0
stop_worker() {
  signal_status=$1
  kill -TERM "$worker" 2>/dev/null || true
}
trap 'stop_worker 130' INT
trap 'stop_worker 143' TERM
trap 'stop_worker 129' HUP
status=0
while :; do
  wait "$worker" && status=0 || status=$?
  # A trapped signal interrupts wait before the worker exits; wait again so its descriptors close before tee.
  kill -0 "$worker" 2>/dev/null || break
done
[ "$signal_status" = 0 ] || status=$signal_status
exit "$status"
