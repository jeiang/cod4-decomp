# Server budget (ticket #56)

Target: 32 players at 30 Hz on 1 vCPU and 512 MB, x86-64 and ARM64. The tick budget is 33.3 ms,
so the headroom criterion is p99 <= 16.7 ms.

## Method

`scripts/budget.sh <map> <gametype> <minutes>` plays 32 server-side bots for 15 minutes with
the round, time and score limits off, pinned to one CPU with `MemoryMax=512M` and no swap
(`systemd-run --user --scope`, `taskset`). `scripts/budget-table.py` summarises the bundles.
Every tick is recorded in `ticks.csv` (total, per-subsystem, process RSS sampled once a second);
the peak is the kernel high-water mark (`getrusage`), steady is the median of the second half of
the samples. Every server stage fails the run if peak RSS is over 512 MiB.

- x86-64: Ryzen 7 7800X3D host, six cells run at once, each pinned to its own core.
- ARM64: the nixpkgs `darwin.linux-builder` NixOS VM (aarch64, QEMU with HVF acceleration on an
  Apple Silicon Mac, `-smp 1`, 3 GiB RAM), one cell at a time, release build with rustc 1.98.1.
  It runs natively on a single vCPU, so it stands in for a Graviton-class core, not for a Pi.

## Results (15 min, 32 bots)

| map | gametype | arch | p50 ms | p99 ms | max ms | bot share | peak RSS MiB | steady RSS MiB |
|---|---|---|---|---|---|---|---|---|
| mp_carentan | war | x86-64 | 0.51 | 1.59 | 15.4 | 12.9% | 102 | 83 |
| mp_carentan | sd | x86-64 | 0.06 | 0.43 | 9.5 | 7.5% | 102 | 83 |
| mp_creek | war | x86-64 | 0.80 | 3.03 | 14.3 | 11.3% | 102 | 83 |
| mp_creek | sd | x86-64 | 0.12 | 0.86 | 151.2 (23.9 on a solo rerun) | 8.4% | 102 | 83 |
| mp_crash | war | x86-64 | 0.60 | 2.05 | 11.0 | 12.1% | 102 | 83 |
| mp_cargoship | war | x86-64 | 0.50 | 1.37 | 8.6 | 11.0% | 102 | 83 |
| mp_carentan | war | aarch64 | 0.62 | 2.20 | 27.4 | 13.2% | 103 | 83 |
| mp_carentan | sd | aarch64 | 0.20 | 1.23 | 13.0 | 8.1% | 103 | 83 |
| mp_creek | war | aarch64 | 1.71 | 5.30 | 24.6 | 11.1% | 103 | 84 |
| mp_creek | sd | aarch64 | 0.32 | 1.80 | 29.0 | 8.1% | 103 | 83 |
| mp_crash | war | aarch64 | 1.69 | 4.47 | 20.8 | 12.0% | 103 | 83 |
| mp_cargoship | war | aarch64 | 1.53 | 3.84 | 16.0 | 10.0% | 102 | 83 |

Worst p99 is 5.3 ms (ARM64, mp_creek war), 16% of the budget, so headroom is 84%. Peak RSS is
the map-load spike (about 102 MiB); steady RSS is 83 MiB whatever the map. Search and Destroy is
cheaper than team deathmatch in these runs. The single 151 ms
x86 tick (gsc 138 ms) happened with six runs sharing the host and did not reproduce when
mp_creek S&D ran alone (max 23.9 ms).

Known gap: bot navigation ignores doors and movers, so mp_cargoship's stern is unreachable and
its numbers understate bot movement there.
