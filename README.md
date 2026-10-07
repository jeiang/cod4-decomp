# cod4-decomp

A GPL-3.0-or-later Rust reimplementation of the Call of Duty 4: Modern Warfare (1.7) multiplayer engine. It runs on macOS, Linux, and Windows, and includes a headless server. It contains no game content: you supply your own copy of the game.

Planning and decisions: [wayfinder map](https://github.com/jeiang/cod4-decomp/issues/1). Terms: [CONTEXT.md](CONTEXT.md). Decisions with lasting effect: [docs/adr](docs/adr).

## Build

```sh
nix develop -c cargo build --workspace
nix develop -c cargo test --workspace
```

Without Nix, install rustup; `rust-toolchain.toml` selects the toolchain.

## Original install

The engine reads a CoD4 1.7 install directly: `main/*.iwd`, `zone/<language>/*.ff`, and `localization.txt`. Set `COD4_PATH` to the install directory. The default is `./COD4`, which is gitignored. Never commit or upload anything from the install. Tests that need the install skip when it is missing.

## Harness

`cod4e-harness` runs the staged suite under a watchdog (each stage in a child process; crashes, hangs and panics are caught) and writes `cod4e-run-<date>.zip` (at most 100 MB, user and host names scrubbed) with `manifest.json`, `summary.json`, logs, CSVs, a Perfetto-readable `trace.json` and minidumps.

```sh
nix develop -c cargo run --release -p harness -- run --out bundles   # default suite
nix develop -c cargo run --release -p harness -- stages              # list stages
nix develop -c cargo run --release -p harness -- diff a.zip b.zip    # compare two bundles
```

Stages that need engine commands read console scripts (`scenarios/*.cfg`, or `--script file`); commands the engine lacks yet are reported as skipped. Double-clicking the program runs the suite with the zip next to it. The CI `windows` job uploads that portable folder and smoke-tests it with no install. `scripts/artemis.sh <ref> -- <run options>` runs it on artemis and copies `bundles/` back.

## Layout

| crate | role |
|---|---|
| `assets` | VFS, IWD, fastfile decoding, IWI, typed assets |
| `gsc` | GSC compiler and bytecode VM |
| `sim` | collision traces, player movement, weapons |
| `net` | netchan, snapshot delta codec, transport trait |
| `server` | match simulation and `cod4e-server`; no GPU, window, or audio dependencies |
| `sm3` | D3D9 SM3 bytecode to WGSL |
| `render` | wgpu renderer |
| `client` | `cod4e`, the player-facing client |
| `harness` | `cod4e-harness`, scenario runner and run bundles |
