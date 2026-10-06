#!/bin/sh
# usage: ./run.sh bin | ./run.sh app   (builds, then launches)
set -e
cd "$(dirname "$0")"
nix shell nixpkgs#cargo nixpkgs#rustc -c cargo build --release
case "$1" in
  bin) exec target/release/macos-raw-mouse-proto ;;
  app)
    A=RawMouseProto.app
    rm -rf $A && mkdir -p $A/Contents/MacOS
    cp Info.plist $A/Contents/Info.plist
    cp target/release/macos-raw-mouse-proto $A/Contents/MacOS/
    codesign --force --sign - $A
    exec open -W -n --stdout /tmp/rawmouse-app.log --env QUIT_AFTER="${QUIT_AFTER:-0}" --env GC_BG="${GC_BG:-}" $A ;;
  *) echo "usage: $0 bin|app"; exit 1 ;;
esac
