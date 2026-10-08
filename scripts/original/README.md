# Original-game reference driver

`orig.sh` runs on the Linux reference host (not the Mac). It launches the
original `iw3mp.exe` through `umu-run` in the live Hyprland session, and
sends keys/mouse (ydotool via `/dev/uinput`, ACL-granted, no root) and takes
window screenshots (grim). Helper tools come from a temporary `nix-shell`.

Environment: `ORIG_DIR` (install copy, default `~/cod4e/original`),
`ORIG_PREFIX` (Wine prefix), `ORIG_OUT` (shots and logs).
Nothing here contains game content; screenshots stay on the host and are never committed.

## Prerequisites
- Hyprland (0.56 Lua dispatcher syntax), `HYPRLAND_INSTANCE_SIGNATURE`, `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR` (the script derives them).
- `ydotoold` running (`--socket-path=$XDG_RUNTIME_DIR/.ydotool_socket`) with `/dev/uinput` access via an ACL.
- The game window focused (`orig.sh focus`); other windows taking focus drops key input.
- Mouse: `move X Y` / `click [left|right] [X Y]` use window-relative coordinates. An absolute Hyprland
  cursor move alone does not reach the game; the 1px ydotool wiggle does. ydotool relative moves
  are close to 1:1 here (no accel profile change needed; `hyprctl keyword device[...]` is rejected on 0.56).
- Screenshots must use `grim -c` to show the game's cursor.
- `h.js` is a Bun/eval helper that drives the same steps over ssh from the Mac (set ORIG_HOST, ORIG_YDOTOOL, ORIG_GRIM).
