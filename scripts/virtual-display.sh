#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Usage: scripts/virtual-display.sh <slot> -- <cmd...>
# Runs <cmd> inside a private headless sway compositor (one 1920x1080@60 output, Xwayland on) so harness slots on
# the Linux host each get their own display. The compositor renders on the host GPU; audio still goes to the host's
# PipeWire. The compositor and its runtime dir are removed on exit; the exit status is <cmd>'s.
set -euo pipefail
slot=${1:?usage: virtual-display.sh <slot> -- <cmd...>}
shift
[ "${1:-}" = -- ] && shift
uid=$(id -u)
rt=/run/user/$uid/cod4e-slot-$slot
rm -rf "$rt"
mkdir -m 0700 -p "$rt"
cat >"$rt/sway.cfg" <<'EOF'
output HEADLESS-1 mode 1920x1080@60Hz
for_window [all] floating enable
default_border none
xwayland enable
EOF
env -u WAYLAND_DISPLAY -u DISPLAY XDG_RUNTIME_DIR="$rt" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=vulkan WLR_RENDER_DRM_DEVICE=/dev/dri/renderD128 \
  sway -c "$rt/sway.cfg" >"$rt/sway.log" 2>&1 &
sway=$!
trap 'kill "$sway" 2>/dev/null; wait "$sway" 2>/dev/null; rm -rf "$rt"' EXIT
for _ in $(seq 100); do
  [ -S "$rt/wayland-1" ] && break
  sleep 0.1
done
if [ ! -S "$rt/wayland-1" ]; then
  echo "virtual display $slot did not start; see $rt/sway.log" >&2
  cat "$rt/sway.log" >&2
  exit 1
fi
status=0
env -u DISPLAY XDG_RUNTIME_DIR="$rt" WAYLAND_DISPLAY=wayland-1 PIPEWIRE_RUNTIME_DIR="/run/user/$uid" \
  PULSE_RUNTIME_PATH="/run/user/$uid/pulse" "$@" || status=$?
exit $status
