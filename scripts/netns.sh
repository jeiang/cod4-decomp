#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Usage: scripts/netns.sh -- <cmd...>
# Runs <cmd> in a private network namespace that has only loopback, so the harness's fixed ports (18090, 18091),
# the LAN ports 28960-28963 and LAN broadcasts stay inside one harness slot. Unprivileged: an outer user namespace
# maps us to root to bring up lo, and an inner one maps back to the real uid (Chromium refuses to run as root
# with its sandbox on).
set -euo pipefail
[ "${1:-}" = -- ] && shift
exec unshare --user --map-root-user --net -- sh -c 'ip link set lo up && u=$1 g=$2 && shift 2 && exec unshare --user --map-user="$u" --map-group="$g" -- "$@"' sh "$(id -u)" "$(id -g)" "$@"
