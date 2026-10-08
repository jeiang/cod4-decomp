# Original-game reference driver

`orig.sh` runs on the Linux reference host (not the Mac). It launches the
original `iw3mp.exe` through `umu-run` in the live Hyprland session, and
sends keys/mouse (ydotool via `/dev/uinput`, ACL-granted, no root) and takes
window screenshots (grim). Helper tools come from a temporary `nix-shell`.

Environment: `ORIG_DIR` (install copy, default `~/cod4e/original`),
`ORIG_PREFIX` (Wine prefix), `ORIG_OUT` (shots and logs).
Nothing here contains game content; screenshots stay on the host and are never committed.
