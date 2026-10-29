# AGENTS.md

A GPL-3.0-only Rust reimplementation of the Call of Duty 4 1.7 multiplayer
engine. The target is stock multiplayer that plays and looks close to the
original engine (`iw3mp.exe` 1.7) on macOS, Linux, Windows and the browser,
plus a headless server. The plan is the wayfinder map, issue #1.

Read first: `README.md` (what the engine does, feature by feature),
`CONTEXT.md` (the terms; use them, avoid the listed aliases), `docs/adr/`
(decisions that constrain changes).

## Environment

- Run every repository command through the flake: `nix develop -c <cmd>`.
  `rust-toolchain.toml` is the one toolchain pin.
- Repository tools are `just` recipes (`just --list`); run them as
  `nix develop -c just <recipe>`. Parallel runs on the Linux host use slots
  1–3 (`just slot`, `just slot-exec`); each slot has its own headless
  display, network namespace and target dir. `just serial` runs
  timing-gated stages with the host to itself.
- The original install is `./COD4/` (or `COD4_PATH`). It is copyrighted and
  user-owned: never commit, upload, paste or quote its files, and never put
  its content in an issue, a bundle or a log. Tests that need it skip
  without it.
- Linux checks run on the Linux test host: `just remote <ref> -- <harness
  run options>` or `just slot <n> <ref> <stages>` (`COD4E_LINUX_HOST`,
  default `artemis.jeiang.vpn`). On that host, project
  work lives under `~/Projects/cod4e/`: `shared/COD4` (install),
  `original-reference/` (the original game under Proton), and one folder per
  task.
- Windows is reached only through GitHub Actions and the remote tester's run
  bundle. The open Windows defects (#290, #331, #46) cannot be worked on
  locally; do not try to close them without a Windows run.
- `just review` reviews a bundle's screenshots and videos with
  vision models (`COD4E_EMBED_URL`, `COD4E_REVIEW_URL`). Its findings are
  leads to check by eye, not evidence.

## Sources and licensing

- ADR 0003: code may be translated only from sources whose license allows
  GPL-3.0-only distribution (KisakCOD, OpenAssetTools, gsc-tool, ioquake3,
  permissive code). A file that translates code names the project, source
  files and copyright holders in a comment under its SPDX line.
- CoD4X (AGPL) is reference-only: read it for facts, never copy or translate
  its code. Code from `iw3mp.exe` is never copied; functional format
  constants (tables, enum values, layouts) may be committed (ADR 0001).
- `iw3mp.exe` 1.7 is the behaviour reference. Decompile it with REA
  (Ghidra; `.omp/mcp.json`). KisakCOD is a guide to names and order of
  operations; check it against the binary where behaviour matters.
- Every research note records the license of each source it uses.

## Behaviour

- A difference from the original engine's behaviour is a defect, unless the
  README lists it as intentional (for example Bink video, or the native wire
  protocol, voice, demo and download formats). State which reference
  (function in `iw3mp.exe`, KisakCOD file, stock script or config) a fix
  follows.
- No silent fallbacks. A stock dvar, command, builtin, asset or menu action
  that the engine accepts but does not implement logs a `warn` naming it,
  and has an issue. Do not turn a missing feature into a quiet no-op.
- Logging goes through `tracing` (crate `logging`); `print_stdout` and
  `print_stderr` are linted. Levels follow the README Logging section.
- The headless server has no GPU dependencies and runs one thread.
  Movement and weapon simulation lives in `sim` and is shared by the server
  and client prediction; change it there, not in a copy.

## Verification

- Before a PR: `cargo fmt --check`, `cargo clippy --workspace -- -D
  warnings`, `cargo test --workspace`, and
  `cargo check -p client --target wasm32-unknown-unknown`. CI runs the same
  on Windows.
- A PR that adds or changes runtime behaviour adds or extends a harness stage
  or scenario in the default suite that asserts pass or fail, not only
  records (issue #1). Drive it through the player's path (input, menus,
  console) where one exists, not internal hooks (#376).
- A skipped stage proves nothing. Report which stages ran, on which machine.
- `crates/harness/src/logcheck.rs` is a reviewed allow-list. Add an entry
  only for a known, tracked gap: give the reason and the issue, and a
  per-stage `max` measured on the default suite. Fix the cause instead when
  the fix is small.
- Prefer measured numbers (frame times, tick times, RSS) from a run bundle
  over estimates, and say which host and build produced them.

## Documentation

- The README describes what the engine does. Update it in the same PR as
  the behaviour, including what is not done.
- A decision with lasting effect gets an ADR in `docs/adr/`. New terms go in
  `CONTEXT.md`.

## Git, issues and PRs

- Work in a worktree under `.worktrees/`, one per branch, branched from the
  latest `origin/main`.
- Conventional Commits. Reference the issue in the subject:
  `(Resolves #N)` when the PR closes it, `(part of #N)` otherwise.
- PRs are squash-merged by hand (auto-merge is off). `main` requires the
  `windows` check. Before merging, run `git diff --stat origin/main...HEAD`
  and confirm nothing unintended is deleted. A red `main` is fixed forward
  at once.
- Tickets carry wayfinder labels (`wayfinder:task`, `:research`,
  `:prototype`, `:grilling`), a `severity:*` label for gaps, and the
  `Ticket type`, `Area` and `Mode` fields on the GitHub Project. A closed
  ticket adds one line to "Decisions so far" in issue #1.
