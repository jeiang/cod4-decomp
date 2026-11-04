#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Usage: scripts/host-run.sh [--no-checks] [--exec] [-- <harness run options> | -- <command...>]
#        scripts/host-run.sh --queue
# Runs on the Linux test host, inside a checkout and the dev shell (scripts/remote-linux.sh calls it). Builds the
# workspace and, unless --no-checks, runs test/clippy/fmt. With `--` it then runs `cod4e-harness run --out bundles
# <options>` (a bare `--` runs the default suite); with --exec it runs <command...> instead, with the release
# binaries on PATH, writing into out/. A harness run that selects a web-* stage (the default suite does) refuses to
# start with exit 78, before queueing or building, when scripts/find-chromium.sh finds no Chromium, unless
# --allow-skip names every such stage.
# COD4E_SLOT=1..3 selects a harness slot: its own target dir, headless display (scripts/virtual-display.sh), sound
# server with a test-tone microphone (scripts/virtual-audio.sh) and loopback-only network namespace
# (scripts/netns.sh). Every run holds the host-wide stage lock shared from its build
# on, so up to three slot runs work at once. Without a slot the run is serial: after its build it holds the stage lock
# exclusively, so timing-gated stages see a quiet host (no stage and no compile beside them), and runs on its own
# headless display like a slot (the desktop session's state cannot stall its windows). A pending serial run
# (~/Projects/cod4e/.serial-pending) keeps new runs from starting.
# Queue: every run registers in ~/Projects/cod4e/queue/ from start to exit (PID, process start time, kind, build hash,
# options, state). Serial runs take their turn in registration order (FIFO). A run that has to wait prints what it
# waits on, its queue position and the runs ahead of it, at once and every COD4E_HOST_WAIT_REPORT seconds (default
# 300). It gives up after COD4E_HOST_WAIT_TIMEOUT seconds of waiting in all (default 21600, 6 h; 0 waits without a
# bound) with exit status 76, a `host-run: gave up after ...` line and the file .host-run-gave-up in the checkout.
# `--queue` prints the registered runs, oldest first (`just host-status`); it reads nothing from stdin.
set -euo pipefail
root=$HOME/Projects/cod4e
queue_dir=$root/queue
EX_GAVE_UP=76

# A registry entry is live while its PID runs the process that wrote it: the start time tells a reused PID (after a
# SIGKILL or a reboot) from the owner.
proc_start() {
  local s
  s=$(ps -o lstart= -p "$1" 2>/dev/null) || return 0
  s=${s#"${s%%[![:space:]]*}"}
  printf '%s' "${s%"${s##*[![:space:]]}"}"
}
# Fills the E_* arrays with the live entries, oldest first, and removes entries whose owner is gone.
scan_queue() {
  E_name=() E_pid=() E_kind=() E_hash=() E_state=() E_since=() E_opts=()
  local f pid start kind hash st since opts k v
  for f in "$queue_dir"/*; do
    # Entries are written as <name>.tmp and renamed into place whole.
    [ -f "$f" ] && [[ $f != *.tmp ]] || continue
    pid='' start='' kind='' hash='' st='' since='' opts=''
    while IFS='=' read -r k v; do
      case $k in
        pid) pid=$v ;; start) start=$v ;; kind) kind=$v ;; hash) hash=$v ;; state) st=$v ;;
        since) since=$v ;; options) opts=$v ;;
      esac
    done < "$f" || true
    if [[ ! $pid =~ ^[1-9][0-9]*$ ]] || [ -z "$start" ] || [ "$(proc_start "$pid")" != "$start" ]; then
      rm -f "$f"
      continue
    fi
    E_name+=("${f##*/}") E_pid+=("$pid") E_kind+=("$kind") E_hash+=("$hash") E_state+=("$st")
    E_since+=("${since:-0}") E_opts+=("$opts")
  done
}
describe() {
  local i=$1 now
  now=$(date +%s)
  printf 'PID %s %s %s, %s for %ss' "${E_pid[$i]}" "${E_kind[$i]}" "${E_hash[$i]:-unknown-hash}" "${E_state[$i]}" \
    "$((now - E_since[$i]))"
  [ -z "${E_opts[$i]}" ] || printf ' (%s)' "${E_opts[$i]}"
}

if [ "${1:-}" = --queue ]; then
  scan_queue
  if [ ${#E_name[@]} = 0 ]; then echo "no registered host runs"; exit 0; fi
  echo "registered host runs, oldest first (serial runs start in this order):"
  for i in "${!E_name[@]}"; do printf '  %s\n' "$(describe "$i")"; done
  exit 0
fi

seconds_var() {
  local name=$1 default=$2 min=$3 v
  v=${!name:-$default}
  if [[ ! $v =~ ^[0-9]+$ ]] || [ "$v" -lt "$min" ]; then
    echo "$name must be a whole number of seconds, at least $min (got '$v')" >&2
    exit 2
  fi
  printf '%s' "$((10#$v))"
}
wait_bound=$(seconds_var COD4E_HOST_WAIT_TIMEOUT 21600 0)
report_every=$(seconds_var COD4E_HOST_WAIT_REPORT 300 1)
waited=0

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
elif [ $exec_mode = 1 ]; then
  echo "--exec needs a slot" >&2
  exit 2
fi

# The web-* stages this harness run selects: those --stage names, or with no --stage and no --script the default
# suite's four (crates/harness/src/stage.rs). On this host one that --allow-skip does not name needs a Chromium: the
# run refuses to start without one rather than end, after the whole run, with the stages skipped.
web_stages=()
chrome=
if [ $run = 1 ] && [ $exec_mode = 0 ]; then
  selected=0 named=() allowed=,
  # Match the harness's separate-token options and exact stage names, not equals forms or globs. Unsupported
  # arguments stay unchanged for the harness to reject; preflight is not a second argument validator.
  args=("$@")
  i=0
  while [ $i -lt ${#args[@]} ]; do
    opt=${args[$i]}
    case $opt in
      --stage|--allow-skip|--script) val=${args[$((i + 1))]:-}; i=$((i + 2)) ;;
      *) i=$((i + 1)); continue ;;
    esac
    case $opt in
      --stage) selected=1; IFS=, read -ra parts <<< "$val"; named+=(${parts[@]+"${parts[@]}"}) ;;
      --script) selected=1 ;;
      --allow-skip) allowed="$allowed${val// /},"
    esac
  done
  [ $selected = 1 ] || named=(web-client web-join web-download web-match)
  for s in ${named[@]+"${named[@]}"}; do
    case $s in web-client|web-join|web-download|web-match) web_stages+=("$s") ;; esac
  done
  chrome=$(scripts/find-chromium.sh)
  unaccepted=()
  for s in ${web_stages[@]+"${web_stages[@]}"}; do [[ $allowed == *",$s,"* ]] || unaccepted+=("$s"); done
  if [ -z "$chrome" ] && [ ${#unaccepted[@]} -gt 0 ]; then
    # shellcheck disable=SC2088 # the message names the path as the user would type it
    echo "host-run: no Chromium for ${unaccepted[*]} on this host; run \`just chromium-install\` (a GC-rooted" \
      "~/Projects/cod4e/bin/chromium), or name the stages in --allow-skip to accept their skips" >&2
    exit 78
  fi
fi

# Registration. The entry is written whole and renamed into place; its name orders serial turns.
mkdir -p "$queue_dir"
now_us=${EPOCHREALTIME:-$(date +%s).000000}
entry=$queue_dir/$(printf '%017d' "${now_us/./}")-$$
my_kind=${COD4E_SLOT:+slot $COD4E_SLOT}
my_kind=${my_kind:-serial}
if [ $run = 0 ]; then
  my_opts="checks only"
elif [ $exec_mode = 1 ]; then
  my_opts="exec: $*"
else
  my_opts=${*:-default suite}
fi
my_opts=${my_opts//$'\n'/ }
my_start=$(proc_start $$)
[ -n "$my_start" ] || { echo "host-run: cannot read this run's start time (ps -o lstart)" >&2; exit 1; }
state=starting
write_entry() {
  printf 'pid=%s\nstart=%s\nkind=%s\nhash=%s\nstate=%s\nsince=%s\noptions=%s\n' "$$" "$my_start" \
    "$my_kind" "${COD4E_BUILD_HASH:-}" "$state" "$(date +%s)" "$my_opts" > "$entry.tmp"
  mv -f "$entry.tmp" "$entry"
}
set_state() { state=$1; write_entry; }
marker_set=0
on_exit() {
  rm -f "$entry" "$entry.tmp"
  [ $marker_set = 0 ] || clear_pending
}
trap on_exit EXIT
write_entry

lock_holders() {
  echo "host-run: lock holders on $root (PID COMMAND PATH):" >&2
  { lslocks --notruncate --output PID,COMMAND,PATH 2>/dev/null || true; } | awk -v r="$root/" 'NR == 1 || index($0, r)' >&2
}
# Index of this run in the E_* arrays (after scan_queue).
my_index() {
  local i
  for i in "${!E_name[@]}"; do [ "${E_name[$i]}" != "${entry##*/}" ] || { echo "$i"; return; }; done
  echo -1
}
# Sets pos, of and ahead (E_* indices) for the wait named by $wait_kind.
position() {
  scan_queue
  local me i
  me=$(my_index)
  ahead=() pos=1 of=1
  case $wait_kind in
    turn|slot)
      # FIFO by registration among the runs that queue on the same thing: every serial run, or this slot's runs.
      local n=0
      for i in "${!E_name[@]}"; do
        [ "${E_kind[$i]}" = "$my_kind" ] || continue
        n=$((n + 1))
        [ "$i" -lt "$me" ] && ahead+=("$i")
      done
      pos=$((${#ahead[@]} + 1)) of=$n
      return ;;
    owner)
      for i in "${!E_name[@]}"; do [ "${E_pid[$i]}" != "$owner" ] || ahead+=("$i"); done ;;
    shared)
      # The serial run holding the stage lock exclusively.
      for i in "${!E_name[@]}"; do
        [ "${E_kind[$i]}" != serial ] || [ "${E_state[$i]}" != running ] || ahead+=("$i")
      done ;;
    exclusive)
      # Every other registered run past its waits holds the stage lock shared.
      for i in "${!E_name[@]}"; do
        [ "$i" != "$me" ] || continue
        case ${E_state[$i]} in building|running) ahead+=("$i") ;; esac
      done ;;
  esac
  # Then the runs waiting on the same thing, in registration order.
  local before=0 same=0
  for i in "${!E_name[@]}"; do
    [ "${E_state[$i]}" = "$state" ] || continue
    same=$((same + 1))
    [ "$i" -lt "$me" ] && before=$((before + 1))
  done
  pos=$((${#ahead[@]} + before + 1)) of=$((${#ahead[@]} + same))
  [ "$me" -ge 0 ] || of=$((of + 1))
}
report() {
  local elapsed=$1 i
  if [ "$wait_kind" = owner ] && [ "$what" != "serial run PID $owner" ]; then
    what="serial run PID $owner"
    set_state "waiting for $what"
  fi
  position
  local bound="no wait bound"
  [ "$wait_bound" = 0 ] || bound="gives up after ${wait_bound}s of waiting"
  echo "host-run: waiting ${elapsed}s for $what; queue position $pos of $of ($bound, waited $((waited + elapsed))s in all)" >&2
  for i in ${ahead[@]+"${ahead[@]}"}; do echo "host-run:   ahead: $(describe "$i")" >&2; done
  if [ "$wait_kind" = owner ] && [ ${#ahead[@]} = 0 ]; then
    echo "host-run:   ahead: PID $owner (not registered: $(ps -p "$owner" -o args= 2>/dev/null || echo gone))" >&2
  fi
}
give_up() {
  local msg
  position
  msg="host-run: gave up after ${waited}s waiting for $what (queue position $pos of $of, COD4E_HOST_WAIT_TIMEOUT=$wait_bound); nothing ran after this point"
  echo "$msg" >&2
  printf '%s\n' "$msg" > .host-run-gave-up 2>/dev/null || true
  lock_holders
  exit $EX_GAVE_UP
}
# Seconds the next wait slice may take: the report interval, cut to what is left of the wait bound (0: give up).
slice() {
  local elapsed=$1 left
  if [ "$wait_bound" = 0 ]; then echo "$report_every"; return; fi
  left=$((wait_bound - waited - elapsed))
  [ "$left" -gt 0 ] || left=0
  [ "$left" -lt "$report_every" ] && echo "$left" || echo "$report_every"
}
# wait_lock <mode> <fd> <wait kind> <what>: take a lock, reporting while it waits.
wait_lock() {
  local mode=$1 fd=$2 t0 s first=1
  wait_kind=$3 what=$4
  flock -n "$mode" "$fd" && return 0
  local previous=$state
  set_state "waiting for $what"
  t0=$SECONDS
  while :; do
    s=$(slice $((SECONDS - t0)))
    if [ "$s" = 0 ]; then waited=$((waited + SECONDS - t0)); give_up; fi
    report $((SECONDS - t0))
    [ $first = 0 ] || lock_holders
    first=0
    if flock -w "$s" "$mode" "$fd"; then break; fi
  done
  waited=$((waited + SECONDS - t0))
  set_state "$previous"
}
# wait_until <wait kind> <what> <command...>: wait until the command succeeds (polled every 2 s), reporting.
wait_until() {
  local t0 s next
  wait_kind=$1 what=$2
  shift 2
  "$@" && return 0
  local previous=$state
  set_state "waiting for $what"
  t0=$SECONDS
  next=0
  while ! "$@"; do
    if [ $((SECONDS - t0)) -ge "$next" ]; then
      s=$(slice $((SECONDS - t0)))
      if [ "$s" = 0 ]; then waited=$((waited + SECONDS - t0)); give_up; fi
      report $((SECONDS - t0))
      next=$((next + report_every))
    fi
    s=$(slice $((SECONDS - t0)))
    if [ "$s" = 0 ]; then waited=$((waited + SECONDS - t0)); give_up; fi
    sleep 2
  done
  waited=$((waited + SECONDS - t0))
  set_state "$previous"
}

# The marker's short critical sections share fd 6; serial owners queue on fd 7 so one owner's EXIT trap cannot clear
# another owner's pending marker. Diagnostics name the kernel lock holders rather than a PID file that may have
# outlived its owner.
exec 6>>"$root/.serial-marker.lock"
marker_lock() {
  if ! flock -w 60 -x 6; then
    echo "host-run: the serial marker lock was held for 60 s" >&2
    lock_holders
    exit 1
  fi
}
pending_owner() {
  marker_lock
  owner=
  if [ -e "$root/.serial-pending" ]; then
    read -r owner < "$root/.serial-pending" || true
    if [[ ! $owner =~ ^[1-9][0-9]*$ ]] || ! kill -0 "$owner" 2>/dev/null; then
      echo "host-run: removing orphaned serial marker (owner '${owner:-missing}' is not alive)" >&2
      rm -f "$root/.serial-pending"
      owner=
    fi
  fi
  flock -u 6
}
no_pending_owner() { pending_owner; [ -z "$owner" ]; }
clear_pending() {
  marker_lock
  local o=
  read -r o < "$root/.serial-pending" 2>/dev/null || true
  if [ "$o" = "$$" ]; then rm -f "$root/.serial-pending"; fi
  flock -u 6
}
# A serial run's turn: no live serial run registered before it.
my_turn() {
  scan_queue
  local i me
  me=$(my_index)
  for i in "${!E_name[@]}"; do
    [ "$i" -lt "$me" ] || break
    [ "${E_kind[$i]}" != serial ] || return 1
  done
  return 0
}

if [ -n "${COD4E_SLOT:-}" ]; then
  export CARGO_TARGET_DIR=$root/slot-$COD4E_SLOT/target
  # The slot is held from the build on: two runs in one slot would otherwise rebuild each other's binaries.
  exec 9>"$root/.slot-$COD4E_SLOT.lock"
  wait_lock -x 9 slot "slot $COD4E_SLOT"
fi
# Every build holds the stage lock shared, a serial run's included, so a serial run's timing stages never share the
# host with a compile. A pending serial run keeps new runs (and their builds) from starting.
pending_owner
[ -z "$owner" ] || wait_until owner "serial run PID $owner" no_pending_owner
exec 8>>"$root/.stage-lock"
wait_lock -s 8 shared "the stage lock (shared, for the build)"

set_state building
nice -n 19 cargo build --workspace
if [ $checks = 1 ]; then
  nice -n 19 cargo test --workspace
  nice -n 19 cargo clippy --workspace -- -D warnings
  cargo fmt --check
fi
[ $run = 1 ] || exit 0

slot_wrap() {
  scripts/virtual-display.sh "$COD4E_SLOT" -- scripts/virtual-audio.sh "$COD4E_SLOT" -- scripts/netns.sh -- "$@"
}

if [ $exec_mode = 1 ]; then
  nice -n 19 cargo build --release -p harness -p client -p server -p master
  mkdir -p out
  set_state running
  slot_wrap env PATH="$CARGO_TARGET_DIR/release:$PATH" "$@"
  exit
fi

# The browser client is built only for a run that selects a web-* stage.
[ ${#web_stages[@]} = 0 ] || nice -n 19 web/build.sh >/dev/null
nice -n 19 cargo build --release -p harness -p client -p server
h="${CARGO_TARGET_DIR:-target}/release/cod4e-harness"
if [ -n "${COD4E_SLOT:-}" ]; then
  set_state running
  # shellcheck disable=SC2086 # $chrome is an optional COD4E_CHROME=<path> assignment
  slot_wrap env COD4E_NO_PROMPT=1 $chrome "$h" run --out bundles "$@"
else
  flock -u 8
  set_state built
  wait_until turn "its serial turn" my_turn
  exec 7>>"$root/.serial-queue.lock"
  wait_lock -x 7 turn "the serial queue lock"
  marker_lock
  printf '%s\n' "$$" > "$root/.serial-pending"
  marker_set=1
  flock -u 6
  wait_lock -x 8 exclusive "the stage lock (exclusive, for the serial run)"
  set_state running
  # shellcheck disable=SC2086
  scripts/virtual-display.sh serial -- scripts/virtual-audio.sh serial -- \
    env COD4E_NO_PROMPT=1 $chrome "$h" run --out bundles "$@"
fi
