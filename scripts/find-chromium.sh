#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Prints the path of a Chromium for the web-* harness stages: one on PATH, else nixpkgs' (built or fetched on first use).
for n in chromium chromium-browser google-chrome google-chrome-stable; do
  if p=$(command -v "$n"); then echo "$p"; exit 0; fi
done
out=$(nix build nixpkgs#chromium --no-link --print-out-paths 2>/dev/null | head -n 1)
[ -n "$out" ] && echo "$out/bin/chromium"
