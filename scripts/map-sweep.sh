#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Plays every stock map in every gametype with the windowed client (--listen --autoplay) and prints one row per
# combination: did it start, spawn, see the bots, run its scripts without errors, and find every effect, vision and
# shock file. Needs a display, so run it on a machine with one (see scripts/remote-linux.sh).
#
#   scripts/map-sweep.sh <cod4e binary> <install dir> <out dir> [seconds per run, default 30]
#
# MAPS and GAMETYPES (space separated) narrow the sweep; by default every mp_* zone in the install and
# war dm dom koth sab sd. The exit status is the number of combinations that failed. Each run's client.json and
# stderr are kept under <out dir>/<map>-<gametype>/.
set -u
client=${1:?cod4e binary}
install=${2:?install dir}
out=${3:?out dir}
secs=${4:-30}
maps=${MAPS:-$(cd "$install/zone/english" && ls mp_*.ff | sed 's/\.ff$//' | grep -v '_load$')}
gametypes=${GAMETYPES:-war dm dom koth sab sd}
mkdir -p "$out"
failed=0
printf '| map | gametype | result | players | shots | events | fx played | notes |\n|---|---|---|---|---|---|---|---|\n'
for map in $maps; do
  for gt in $gametypes; do
    dir="$out/$map-$gt"
    mkdir -p "$dir"
    timeout $((secs + 120)) "$client" --install "$install" --map "$map" --gametype "$gt" --listen --bots 5 \
      --autoplay --duration "$secs" --size 960x540 --out "$dir" >"$dir/stdout.txt" 2>"$dir/stderr.txt"
    code=$?
    row=$(python3 - "$dir" "$map" "$gt" "$code" <<'PY'
import json, sys
d, m, g, code = sys.argv[1:5]
try:
    r = json.load(open(f"{d}/client.json"))
except Exception as e:
    print(f"| {m} | {g} | FAIL | | | | | no report (exit {code}) |")
    sys.exit(1)
n = r.get("net") or {}
s = r.get("server") or {}
fx = n.get("fx") or {}
bad = []
if int(code) != 0: bad.append(f"exit {code}")
if not n.get("connected"): bad.append("not connected")
if not n.get("spawned"): bad.append("never spawned")
if (n.get("snapshots") or 0) < 50: bad.append("few snapshots")
if (n.get("players_seen_max") or 0) < 1: bad.append("saw no player")
if s.get("script_errors"): bad.append(f"{s['script_errors']} script errors")
if fx.get("missing"): bad.append("effects missing: " + ", ".join(sorted(fx["missing"])[:3]))
if fx.get("look_missing"): bad.append("look files missing: " + ", ".join(fx["look_missing"][:3]))
if n.get("weapons_without_models"): bad.append("weapons without models: " + ", ".join(list(n["weapons_without_models"])[:3]))
ev = sum((n.get("events") or {}).values())
played = sum((fx.get("played") or {}).values())
person = s.get("person") or {}
note = "; ".join(bad) if bad else ""
print(f"| {m} | {g} | {'FAIL' if bad else 'ok'} | {n.get('players_seen_max')} | {person.get('shots')} | {ev} | {played} | {note} |")
sys.exit(1 if bad else 0)
PY
)
    st=$?
    echo "$row"
    [ $st -ne 0 ] && failed=$((failed + 1))
  done
done
echo
echo "$failed combinations failed"
exit "$failed"
