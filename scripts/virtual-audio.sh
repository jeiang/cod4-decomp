#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Usage: scripts/virtual-audio.sh <slot> -- <cmd...>
# Runs <cmd> with a private PulseAudio server so a harness slot has its own sound devices: a null sink for playback
# and a 440 Hz sine source as the microphone (#488). The client's cpal opens ALSA's default device; ALSA_CONFIG_PATH
# names only this script's configuration, so that device is PulseAudio's ALSA plugin (COD4E_ALSA_PLUGINS, from the
# dev shell) on this server, not the host's sound server. The socket is a file, so it is reachable from inside
# scripts/netns.sh's namespace. COD4E_TEST_MIC=1 tells the harness a microphone is there. The server and its
# directory are removed on exit; the exit status is <cmd>'s.
set -euo pipefail
slot=${1:?usage: virtual-audio.sh <slot> -- <cmd...>}
shift
[ "${1:-}" = -- ] && shift
plugins=${COD4E_ALSA_PLUGINS:?COD4E_ALSA_PLUGINS is not set (run inside nix develop)}
dir=/run/user/$(id -u)/cod4e-audio-$slot
rm -rf "$dir"
mkdir -m 0700 -p "$dir"
cat >"$dir/slot.pa" <<EOF
load-module module-native-protocol-unix auth-anonymous=1 socket=$dir/native
load-module module-null-sink sink_name=cod4e_out
load-module module-sine-source source_name=cod4e_tone frequency=440 rate=48000
set-default-sink cod4e_out
set-default-source cod4e_tone
EOF
cat >"$dir/asound.conf" <<EOF
pcm_type.pulse { lib "$plugins/libasound_module_pcm_pulse.so" }
ctl_type.pulse { lib "$plugins/libasound_module_ctl_pulse.so" }
pcm.!default { type pulse; server "unix:$dir/native" }
ctl.!default { type pulse; server "unix:$dir/native" }
EOF
# No session bus (the host's would see a second server's name) and no realtime request (rtkit is on that bus).
env -u DBUS_SESSION_BUS_ADDRESS HOME="$dir" XDG_RUNTIME_DIR="$dir" \
  pulseaudio -n -F "$dir/slot.pa" --daemonize=no --exit-idle-time=-1 --use-pid-file=no --system=no \
  --realtime=no --high-priority=no --log-target=file:"$dir/pulse.log" &
pulse=$!
cleanup() {
  kill "$pulse" 2>/dev/null || true
  wait "$pulse" 2>/dev/null || true
  rm -rf "$dir"
}
trap cleanup EXIT
for _ in $(seq 100); do
  [ -S "$dir/native" ] && break
  kill -0 "$pulse" 2>/dev/null || break
  sleep 0.1
done
if [ ! -S "$dir/native" ]; then
  echo "virtual audio $slot did not start; see its log:" >&2
  cat "$dir/pulse.log" >&2 || true
  exit 1
fi
status=0
env -u PULSE_RUNTIME_PATH ALSA_CONFIG_PATH="$dir/asound.conf" PULSE_SERVER="unix:$dir/native" COD4E_TEST_MIC=1 \
  "$@" || status=$?
exit $status
