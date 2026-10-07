#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Summarise budget-run bundles: python3 scripts/budget-table.py <arch> <bundle.zip>..."""
import json, sys, zipfile

arch = sys.argv[1]
print("| map | gametype | arch | ticks | p50 ms | p99 ms | max ms | bot share | peak RSS MiB | steady RSS MiB | cgroup peak MiB |")
print("|---|---|---|---|---|---|---|---|---|---|---|")
for path in sys.argv[2:]:
    z = zipfile.ZipFile(path)
    for n in z.namelist():
        if not n.endswith("result.json"):
            continue
        r = json.loads(z.read(n))
        m, t = r["metrics"], r["series"].get("tick.ms")
        if not t:
            continue
        cfg = r.get("environment", {}).get("cvars", {})
        mp = next((k[len("map_load_ms."):] for k in m if k.startswith("map_load_ms.")), "?")
        mib = lambda k: f"{m[k] / 1048576:.0f}" if k in m else "-"
        print(f"| {mp} | {cfg.get('g_gametype', '?')} | {arch} | {t['n']} | {t['p50']:.2f} | {t['p99']:.2f} | {t['max']:.1f} | "
              f"{m.get('tick.bot_share', 0) * 100:.1f}% | {mib('rss.peak_bytes')} | {mib('rss.steady_bytes')} | {mib('rss.cgroup_peak_bytes')} |")
