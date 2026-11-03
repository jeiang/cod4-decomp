#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Prints `COD4E_CHROME=<path>` for the web-* harness stages when a Chromium is already here: on PATH, else the
# GC-rooted one `just chromium-install` puts on the Linux host. Prints nothing otherwise: elsewhere the stages then
# skip, and on the Linux host scripts/host-run.sh refuses a run that selects them. It never builds or fetches one, and
# never takes an unrooted /nix/store Chromium, which any garbage collection can remove.
for n in chromium chromium-browser google-chrome google-chrome-stable; do
  if p=$(command -v "$n"); then echo "COD4E_CHROME=$p"; exit 0; fi
done
p=~/Projects/cod4e/bin/chromium/bin/chromium
if [ -x "$p" ]; then echo "COD4E_CHROME=$p"; fi
exit 0
