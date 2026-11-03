#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Prints `COD4E_CHROME=<path>` for the web-* harness stages when a Chromium is already here (on PATH, the GC-rooted one
# `just chromium-install` puts on the Linux host, else in the Nix store); prints nothing otherwise, so the stages skip.
# It never builds or fetches one.
for n in chromium chromium-browser google-chrome google-chrome-stable; do
  if p=$(command -v "$n"); then echo "COD4E_CHROME=$p"; exit 0; fi
done
p=~/Projects/cod4e/bin/chromium/bin/chromium
if [ -x "$p" ]; then echo "COD4E_CHROME=$p"; exit 0; fi
for p in $(ls -d /nix/store/*-chromium-[0-9]*/bin/chromium 2>/dev/null | sort | tail -n 1); do
  [ -x "$p" ] && echo "COD4E_CHROME=$p"
done
exit 0
