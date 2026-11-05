#!/usr/bin/env bash
set -euo pipefail

tmp=$RUNNER_TEMP
client=$tmp/garret/bin/garret
config=$tmp/garret.toml
socket=$tmp/garret.sock
cursor=$tmp/garret-cursor
log=$tmp/garret-watch.log
pidfile=$tmp/garret-watch.pid
outputs=$tmp/garret-outputs

case ${1:-} in
hook)
  cat >"$tmp/garret-hook" <<EOF
#!/bin/sh
[ -x "$client" ] || exit 0
exec "$client" enqueue --socket "$socket"
EOF
  chmod +x "$tmp/garret-hook"
  ;;
watch)
  rev=${GARRET_CLIENT_REV:?}
  nix build --out-link "$tmp/garret" "github:jeiang/garret/${rev}#garret"
  {
    printf 'public_keys = ["%s"]\n' "$GARRET_PUBLIC_KEY"
    cat .ci/garret.toml
  } >"$config"
  nohup "$client" --config "$config" watch-store --cursor "$cursor" --socket "$socket" >"$log" 2>&1 &
  echo $! >"$pidfile"
  for _ in $(seq 30); do
    [ -s "$cursor" ] && exit 0
    sleep 1
  done
  cat "$log" >&2
  exit 1
  ;;
drain)
  if [ -s "$pidfile" ]; then
    pid=$(cat "$pidfile")
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 30); do
      kill -0 "$pid" 2>/dev/null || break
      sleep 1
    done
    tail -n 100 "$log"
  fi
  status=0
  "$client" --config "$config" watch-store --cursor "$cursor" --drain || status=$?
  if [ -s "$outputs" ]; then
    # shellcheck disable=SC2046
    "$client" --config "$config" push $(cat "$outputs") || status=$?
  fi
  exit "$status"
  ;;
*)
  echo "usage: $0 hook|watch|drain" >&2
  exit 64
  ;;
esac
