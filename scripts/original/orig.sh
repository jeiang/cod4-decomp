#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Driver for the original game run (via umu-run/Proton) on the Linux reference host.
# Runs ON the host. Needs: nix-shell, hyprland session, /dev/uinput access (ACL), umu-run.
# Contains no game content; the install path is supplied by the environment.
set -u
: "${ORIG_DIR:=$HOME/cod4e/original}"
: "${ORIG_PREFIX:=$HOME/cod4e/umu-prefix}"
: "${ORIG_OUT:=$HOME/cod4e/shots}"
export WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-wayland-1}"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/1000}"
if [ -z "${HYPRLAND_INSTANCE_SIGNATURE:-}" ]; then
  HYPRLAND_INSTANCE_SIGNATURE=$(ls "$XDG_RUNTIME_DIR/hypr" | head -n1); export HYPRLAND_INSTANCE_SIGNATURE
fi
mkdir -p "$ORIG_OUT" "$ORIG_PREFIX"

tools() { # re-exec inside a nix-shell that provides the helper tools
  if [ -z "${ORIG_IN_SHELL:-}" ]; then
    ORIG_IN_SHELL=1 exec nix-shell -p grim wtype ydotool jq --run "$(printf '%q ' "$0" "$@")"
  fi
}

# Linux input keycodes for ydotool
declare -A KC=([esc]=1 [1]=2 [2]=3 [3]=4 [4]=5 [5]=6 [6]=7 [7]=8 [8]=9 [9]=10 [0]=11 [enter]=28 [space]=57
  [up]=103 [down]=108 [left]=105 [right]=106 [tab]=15 [backspace]=14 [grave]=41 [shift]=42 [ctrl]=29
  [w]=17 [a]=30 [s]=31 [d]=32 [f]=33 [g]=34 [h]=35 [m]=50 [y]=21 [n]=49 [b]=48 [e]=18 [q]=16 [r]=19 [t]=20 [u]=22 [i]=23 [o]=24 [p]=25
  [j]=36 [k]=37 [l]=38 [z]=44 [x]=45 [c]=46 [v]=47 [f1]=59 [f2]=60 [f3]=61 [f4]=62 [f5]=63 [f10]=68 [f12]=88)

ydo_start() {
  export YDOTOOL_SOCKET="$XDG_RUNTIME_DIR/.ydotool_socket"
  pgrep -u "$USER" -x ydotoold >/dev/null || { ydotoold --socket-path="$YDOTOOL_SOCKET" --socket-perm=0600 >/dev/null 2>&1 & 
    for _ in $(seq 50); do [ -S "$YDOTOOL_SOCKET" ] && break; sleep 0.1; done; }
}

win_json() { hyprctl clients -j | jq -c '[.[]|select((.class|test("iw3mp|cod|steam_app|umu";"i")) or (.title|test("Call of Duty";"i")))][0]'; }

# Hyprland 0.56 uses the Lua dispatcher syntax. Another window taking focus makes the game translucent
# and drops key input, so re-focus before every input action.
do_focus() { hyprctl dispatch 'hl.dsp.focus({window="class:iw3mp.exe"})' >/dev/null; }

cmd="${1:-}"; shift || true
case "$cmd" in
  launch) # launch [extra game args...]; logs to $ORIG_OUT/game.log
    cd "$ORIG_DIR" || exit 1
    export WINEPREFIX="$ORIG_PREFIX" GAMEID="${GAMEID:-umu-cod4-ref}" PROTONPATH="${PROTONPATH:-GE-Proton}"
    export PROTON_LOG=1 PROTON_LOG_DIR="$ORIG_OUT" WINEDLLOVERRIDES="mss32=n,b;binkw32=n,b"
    nohup umu-run iw3mp.exe +set r_fullscreen 0 +set r_mode 1920x1080 +set sv_pure 0 "$@" >"$ORIG_OUT/game.log" 2>&1 &
    echo "pid $!" ;;
  win) tools "$cmd" "$@"; win_json ;;
  shot) tools "$cmd" "$@" # shot [name]: screenshot of the game window (whole output if not found)
    name="${1:-shot-$(date +%H%M%S)}"; g=$(win_json)
    if [ "$g" != null ] && [ -n "$g" ]; then
      geo=$(echo "$g" | jq -r '"\(.at[0]),\(.at[1]) \(.size[0])x\(.size[1])"'); grim -c -g "$geo" "$ORIG_OUT/$name.png"
    else grim "$ORIG_OUT/$name.png"; fi
    echo "$ORIG_OUT/$name.png" ;;
  key) tools "$cmd" "$@"; do_focus; ydo_start # key esc down enter ...
    for k in "$@"; do c=${KC[$k]:?unknown key $k}; ydotool key "$c:1" "$c:0"; sleep 0.25; done ;;
  type) tools "$cmd" "$@"; do_focus; ydo_start; ydotool type --key-delay 40 "$*" ;;
  cmd) tools "$cmd" "$@"; do_focus; ydo_start # cmd 'devmap mp_crash': run a console command (opens/closes the console)
    c=${KC[grave]}; ydotool key "$c:1" "$c:0"; sleep 0.4; ydotool type --key-delay 30 "$*"; ydotool key 28:1 28:0; sleep 0.3; ydotool key "$c:1" "$c:0" ;;
  # Mouse: Hyprland 0.56 absolute move via the Lua dispatcher, then a 1px wiggle through uinput so the game
  # (which reads relative deltas) sees real motion and updates its own cursor/hover. Coordinates are
  # relative to the game window. Screenshots need `grim -c` to include the cursor (the game cursor is
  # drawn by the game itself, the OS cursor is hidden/identical).
  move) tools "$cmd" "$@"; ydo_start; g=$(win_json); ox=$(echo "$g"|jq .at[0]); oy=$(echo "$g"|jq .at[1])
    hyprctl dispatch "hl.dsp.cursor.move({x=$((ox+$1)),y=$((oy+$2))})" >/dev/null
    ydotool mousemove -x 1 -y 0; sleep 0.1; ydotool mousemove -x -1 -y 0; sleep 0.25 ;;
  click) tools "$cmd" "$@"; ydo_start # click [left|right] [x y]
    b=${1:-left}; [ $# -ge 3 ] && { ORIG_IN_SHELL=1 "$0" move "$2" "$3"; }
    case $b in left) ydotool click 0xC0;; right) ydotool click 0xC1;; esac ;;
  focus) tools "$cmd" "$@"; do_focus ;;
  kill) tools "$cmd" "$@"; pid=$(win_json | jq -r '.pid'); [ "$pid" != null ] && kill "$pid" ;;
  *) echo "usage: $0 launch|cmd TEXT|win|shot [name]|key K..|type TEXT|move X Y|click [left|right] [x y]|focus|kill" >&2; exit 2 ;;
esac
