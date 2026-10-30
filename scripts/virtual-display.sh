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
seat=
cleanup() {
  [ -z "$seat" ] || { kill "$seat" 2>/dev/null || true; wait "$seat" 2>/dev/null || true; }
  kill "$sway" 2>/dev/null || true
  wait "$sway" 2>/dev/null || true
  rm -rf "$rt"
}
trap cleanup EXIT
for _ in $(seq 100); do
  [ -S "$rt/wayland-1" ] && break
  sleep 0.1
done
if [ ! -S "$rt/wayland-1" ]; then
  echo "virtual display $slot did not start; see $rt/sway.log" >&2
  cat "$rt/sway.log" >&2
  exit 1
fi
# The headless backend has no input devices. Keep virtual devices attached so
# wl_seat advertises a keyboard/pointer, mapped windows get focus, and winit can
# request relative pointer lock just as on the real display.
env XDG_RUNTIME_DIR="$rt" WAYLAND_DISPLAY=wayland-1 \
  python3 "$(dirname "$0")/headless-seat.py" "$rt/seat.ready" >"$rt/seat.log" 2>&1 &
seat=$!
for _ in $(seq 100); do
  [ -e "$rt/seat.ready" ] && break
  kill -0 "$seat" 2>/dev/null || break
  sleep 0.1
done
if [ ! -e "$rt/seat.ready" ]; then
  echo "virtual display $slot input seat did not start" >&2
  cat "$rt/seat.log" >&2
  exit 1
fi
status=0
env -u DISPLAY XDG_RUNTIME_DIR="$rt" WAYLAND_DISPLAY=wayland-1 PIPEWIRE_RUNTIME_DIR="/run/user/$uid" \
  PULSE_RUNTIME_PATH="/run/user/$uid/pulse" "$@" || status=$?
exit $status
