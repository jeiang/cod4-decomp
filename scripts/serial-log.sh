#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Usage: scripts/serial-log.sh <ref> [stages]   (`just serial-log <ref> [stages]`)
# Runs `just serial <sha> [stages]` (empty stages: the default suite) with the whole output and the exit status kept
# in bundles/serial-logs/serial-<sha12>-<UTC time>.log, which starts with `sha=`, `stages=` and `checks=` lines and
# ends with `exit=<status>`. stdout shows only the progress lines (`[i/n] stage: …`, `bundle:`, `local bundle:`) and
# then `exit=<status> log=<path>`; the exit status is the run's (2 when <ref> does not resolve).
# The ref is fetched and resolved to one commit first, so the log names exactly what ran.
# COD4E_LINUX_CHECKS (default 0) is passed on: 1 also runs cargo test, clippy and fmt on the host.
set -uo pipefail
if [ $# -lt 1 ] || [ $# -gt 2 ]; then echo "usage: $0 <ref> [stages]" >&2; exit 2; fi
ref=$1
stages=${2:-}
checks=${COD4E_LINUX_CHECKS:-0}
git fetch -q origin || echo "serial-log: git fetch origin failed; resolving $ref locally" >&2
sha=$(git rev-parse --verify --quiet "$ref^{commit}") || { echo "serial-log: unknown ref $ref" >&2; exit 2; }
mkdir -p bundles/serial-logs
log="bundles/serial-logs/serial-${sha:0:12}-$(date -u +%Y%m%dT%H%M%SZ).log"
printf 'sha=%s\nstages=%s\nchecks=%s\n' "$sha" "${stages:-<default suite>}" "$checks" > "$log"
echo "sha=$sha stages=${stages:-<default suite>} log=$log"
COD4E_LINUX_CHECKS=$checks just serial "$sha" "$stages" >> "$log" 2>&1 < /dev/null
rc=$?
echo "exit=$rc" >> "$log"
grep -aE '^\[[0-9]+/[0-9]+\] [a-z0-9-]+: |^(local )?bundle:' "$log" || true
echo "exit=$rc log=$log"
exit "$rc"
