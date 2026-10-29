# SPDX-License-Identifier: GPL-3.0-only
# Repository tools. Run them in the dev shell: `nix develop -c just <recipe>`; `just --list` shows them all.

set shell := ["bash", "-euo", "pipefail", "-c"]
set positional-arguments

host := env("COD4E_LINUX_HOST", "artemis.jeiang.vpn")
export COD4E_EMBED_URL := env("COD4E_EMBED_URL", "http://artemis.jeiang.vpn:8189")
export COD4E_REVIEW_URL := env("COD4E_REVIEW_URL", "http://artemis.jeiang.vpn:8190")

# List the recipes
default:
    @just --list

# Build and run

# The CI checks: fmt, clippy, tests and the wasm client
check:
    cargo fmt --check
    cargo clippy --workspace -- -D warnings
    cargo test --workspace
    cargo check -p client --target wasm32-unknown-unknown

# Run the cod4e client (release)
client *args:
    cargo run --release -p client -- "$@"

# Run the cod4e-server dedicated server (release)
server *args:
    cargo run --release -p server -- "$@"

# Run the cod4e-master master server (release)
master *args:
    cargo run --release -p master -- "$@"

# Run cod4e-harness (release), e.g. `just harness run --out bundles`
harness *args:
    cargo run --release -p harness -- "$@"

# Build the browser client into web/pkg
web-build *args:
    web/build.sh "$@"

# Serve web/ with the headers the page needs, e.g. `just web-serve 8080`
web-serve *args:
    python3 web/serve.py "$@"

# Tools

# Review a bundle's frames with the vision models on the Linux host
review *args:
    python3 scripts/footage-review.py "$@"

# Play every stock map and gametype with the windowed client
map-sweep *args:
    scripts/map-sweep.sh "$@"

# Run the 32-bot server budget run
budget *args:
    scripts/budget.sh "$@"

# Summarise budget bundles as a table
budget-table *args:
    python3 scripts/budget-table.py "$@"

# Fetch and build the SM3 cross-check drivers
crosscheck-tools *args:
    scripts/build-crosscheck-tools.sh "$@"

# Generate the wavelet tables from an iw3mp.exe
wavelet-tables exe:
    python3 scripts/gen-wavelet-tables.py "$@"

# Linux test host

# Ship a ref to the Linux host: checks, or `-- <run options>` for a serial harness run
remote ref="HEAD" *args:
    COD4E_LINUX_HOST={{host}} scripts/remote-linux.sh "$@"

# Run comma-separated stages in slot n (1-3) on the Linux host, build only
slot n ref stages:
    COD4E_LINUX_HOST={{host}} COD4E_LINUX_SLOT="$1" COD4E_LINUX_CHECKS=0 scripts/remote-linux.sh "$2" -- --stage "$3"

# Run comma-separated stages in slot n after test/clippy/fmt (PR evidence)
slot-full n ref stages:
    COD4E_LINUX_HOST={{host}} COD4E_LINUX_SLOT="$1" scripts/remote-linux.sh "$2" -- --stage "$3"

# Run a command in slot n with the release binaries on PATH; out/ is copied back
slot-exec n ref +cmd:
    COD4E_LINUX_HOST={{host}} COD4E_LINUX_SLOT="$1" COD4E_LINUX_CHECKS=0 COD4E_LINUX_EXEC=1 scripts/remote-linux.sh "$2" -- "${@:3}"

# Run stages (default suite when empty) with the host to itself, on a headless display
serial ref stages="":
    COD4E_LINUX_HOST={{host}} scripts/remote-linux.sh "$1" -- ${2:+--stage "$2"}

# Run a bash command on the Linux host in ~/Projects/cod4e
host +cmd:
    @ssh {{host}} bash -s <<< "cd ~/Projects/cod4e && $*"

# Open a bash shell on the Linux host
host-shell:
    ssh -t {{host}} 'cd ~/Projects/cod4e; exec bash -l'

# Show slots, serial lock, harness processes, VRAM, model servers and disk on the Linux host
host-status:
    #!/usr/bin/env bash
    set -euo pipefail
    ssh {{host}} bash -s <<'EOF'
    r=~/Projects/cod4e
    if [ -e $r/.serial-pending ]; then echo "serial run pending"; else echo "no serial run pending"; fi
    for n in 1 2 3; do
      if flock -n $r/.slot-$n.lock true; then echo "slot $n free"; else echo "slot $n busy"; fi
    done
    pgrep -af 'cod4e-harness|cod4e-server|/cod4e |sway|chromium|iw3mp' || echo "no harness processes"
    echo "VRAM used: $(( $(cat /sys/class/drm/card1/device/mem_info_vram_used) / 1048576 )) MiB"
    echo "embed: $(curl -s -m 5 http://127.0.0.1:8189/health || echo down)"
    echo "review: $(curl -s -m 5 http://127.0.0.1:8190/health || echo down)"
    df -h $r
    EOF

# Run the original game (orig.sh) on the host's real session, serially
original *args:
    #!/usr/bin/env bash
    set -euo pipefail
    ssh {{host}} bash -s -- $(printf '%q ' "$@") <<'EOF'
    r=~/Projects/cod4e
    touch $r/.serial-pending
    trap 'rm -f $r/.serial-pending' EXIT
    flock -x $r/.stage-lock $r/original-reference/orig.sh "$@"
    EOF
