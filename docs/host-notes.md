# Host and tooling notes

Facts about the Linux test host, the run recipes and the run bundles that agents kept learning again. Facts about
the engine live next to its code: the control channel in `docs/control-channel.md`, the log allow-list in
`crates/harness/src/logcheck.rs`, a stage's facts in its module doc, terms in `CONTEXT.md`. When you learn a host or
tooling fact, or a flake's status changes, update this file in the same PR.

## Hosts and paths

- The Linux test host is `artemis.jeiang.vpn` (`COD4E_LINUX_HOST`, the justfile's default). `artemis.local` no
  longer answers. ssh to it sometimes resets (`kex_exchange_identification`); retry once.
- Its login shell is fish. Send POSIX shell as a script on stdin: `ssh artemis.jeiang.vpn bash -s <<'EOF' ... EOF`.
  Nested `bash -c '...'` quoting has broken before.
- Project work lives under `~/Projects/cod4e/`:
  - `shared/COD4`: the install every host run uses (`COD4_PATH`).
  - `original-reference/game/iw3mp.exe`: the original 1.7 game under Proton (`just original`). A copy of the
    binary is at `COD4/iw3mp.exe` on the Mac.
  - `kisakcod/src`: KisakCOD (MP 1.8, GPL-3.0), read only. Multiplayer code is mostly under `game_mp`, `cgame_mp`,
    `client_mp`, `server_mp` and `ui_mp`, effects under `EffectsCore` (names are case-sensitive). `grep -rn` over a
    directory works through `bash -s`.
  - `bin/`: GC-rooted tools: `chromium` (`just chromium-install`), `python3`, `slot-guard` (`just slot-guard-install`).
    Without `bin/chromium` (or one on `PATH`) a harness run that selects a web-* stage, the default suite included,
    refuses to start with exit 78 naming `just chromium-install`, unless `--allow-skip` names every such stage (#558).
    An unrooted Chromium in the Nix store is never used.
  - `slot-1` to `slot-3`: each slot's own cargo target dir. `slot-guard/`: the leases.
  - `run-<mac host>-<pid>`: one directory per `remote-linux.sh` run, removed when the run ends. An interrupted run
    leaves its directory behind.
- `objdump`, `rizin` and `python3` are not on the host's PATH. Use `nix shell nixpkgs#binutils -c objdump ...`
  and `~/Projects/cod4e/bin/python3/bin/python3` (`bin/python3` is a GC-rooted store directory, not the interpreter).
- Git on the host is the Nix git 2.56, whose store-level gitconfig sets `commit.gpgsign true`. Commit there with
  `git -c commit.gpgsign=false`.

## 1.7 facts

`iw3mp.exe` 1.7 is the reference (AGENTS.md). Decompile it with REA (Ghidra; `.omp/mcp.json`), or disassemble it
with `objdump` on the local `COD4/iw3mp.exe` or on the host as above. KisakCOD is 1.8: use it for names and order of
operations, and confirm strings, flags and keys in the binary where 1.7 may differ (1.7's `getinfo` sends `hc` and
`od`, which the 1.8 source does not).

## The Mac

- Run `cargo fmt`, git, gh and file reads only. Never build, test, clippy or check Rust on the Mac. The checks
  AGENTS.md asks for before a PR are covered this way: the wasm check by the `windows` CI check (`.github/workflows/
  ci.yml`, `cargo check -p client --target wasm32-unknown-unknown` with `RUSTFLAGS: -D warnings`), and test, clippy
  and fmt by a `slot-full`, `remote` or `serial` run. No host recipe runs the wasm check.
- `nix develop -c just script-tests` (the scripts' Python tests) runs locally. CI does not run it. Scripts must work
  with macOS's bash 3.2, and a `| head` in a pipeline can kill the writer with SIGPIPE.
- `/tmp` is `/private/tmp`: compare canonical paths.
- Raw logs such as `out-slot*.log` are not in `.gitignore`. Check `git status` before a commit.
- Command location is not edit location: the edit, read and write tools resolve relative paths against the main
  checkout, whatever the shell's working directory. Use absolute paths into your worktree.

## Run recipes and what they prove

Every recipe ships the named ref (`git archive`, so uncommitted changes are not included) and runs
`scripts/host-run.sh` in the dev shell with `COD4_PATH` set. Builds and tests run at nice 19.

| Recipe | Runs | Proves |
|---|---|---|
| `just remote <ref>` | build, `cargo test`, clippy, fmt; no stage | the checks only; no harness stage ran |
| `just remote <ref> -- <run options>` | the checks, then the harness as a serial run | as `serial` with the checks |
| `just slot <n> <ref> <stages>` | build and the stages in slot n; no checks | the stages, under contention |
| `just slot-full <n> <ref> <stages>` | the checks, then the stages in slot n | PR evidence: the checks and the stages |
| `just slot-exec <n> <ref> <cmd...>` | release binaries on PATH, cwd the checkout, `out/` copied back | whatever the command checks |
| `just serial <ref> [stages]` | the checks, then the stages (the default suite when empty) with the host to itself | timing-gated stages |
| `just serial-log <ref> [stages]` | `just serial`, with the whole output and exit status kept in `bundles/serial-logs/` (`COD4E_LINUX_CHECKS=1` adds the checks) | as `serial`; the log is the evidence |

`just stages-for [base] [head]` prints the stages a diff can affect, from `scripts/stage-map.txt` (a shared file or
an unmapped path gives the whole default suite; an engine crate adds the timing set), for example
`just slot 1 HEAD "$(just stages-for)"`.

`just merge-pr <n> <sha>` merges PR n only when its head is `sha`, `windows` and every other check passed and it is
MERGEABLE. It prints the diff stat and deleted files first, and is a dry run unless `CONFIRM=yes`: read the deletions
before a confirmed run. `just review-delta <n> <old>` shows what the PR's own change gained since the reviewed head
`old`, with merges from main left out.

- A `slot` bundle does not show that test, clippy or fmt pass. Only `slot-full`, `remote` and `serial` run them
  (`COD4E_LINUX_CHECKS=0` skips them).
- Slots 1 to 3 each have their own target dir, headless 1920x1080@60 display and loopback-only network namespace.
  Up to three slot runs work at once.
- The timing set is `client-load`, `client-match`, `client-flythrough` and `headless-bots-32`, plus any stage that
  fails only on a time, frame or tick budget. It is judged only in the coordinator's serial run: a timing-only failure
  in a slot gets 0 slot reruns. Report it once with its bundle and move on.
- `headless-bots-32` is not in the default suite (`default: false` in `stage.rs`). A serial run that should cover
  it must name it.
- A stage that says `unknown stage` on an older ref shows that the ref lacks the check. It does not show a pass or a
  failure of the behaviour.

## Slots, leases and locks

- Each slot has one owner, named by the coordinator. The lease is checked by `~/Projects/cod4e/bin/slot-guard`
  (`scripts/slot-guard.py`, installed apart from the ref under test). Set `COD4E_LINUX_SLOT_TOKEN` from the token
  file the coordinator names, without printing it. A guarded run whose token does not hold the lease is refused with
  exit status 75 (no lease, wrong token, or a run already live in that slot); a second run under the same lease is
  refused, not queued. A slot run with no token bypasses the guard: only `host-run.sh`'s slot lock keeps it apart.
- `just host-status` shows the pending serial run, the slots, the leases, the registered host runs (oldest first,
  with state and options), harness processes, VRAM, the model servers and disk.
- `host-run.sh` locks:
  - A slot run holds `.slot-<n>.lock` from its build on.
  - Every run holds `.stage-lock` shared from its build on. A serial run then takes it exclusively for its stages, so
    no stage and no compile runs beside them.
  - A serial run writes its PID to `.serial-pending` while it waits and runs. No new run starts its build while that
    marker names a live process. Serial runs queue on `.serial-queue.lock` and start in registration order.
- Host runs queue (#561). Each run registers in `~/Projects/cod4e/queue/`. A waiting run prints what it waits on and
  `queue position <k> of <m>` with each run ahead, at once and every `COD4E_HOST_WAIT_REPORT` s (300). It gives up
  only after `COD4E_HOST_WAIT_TIMEOUT` s of waiting (21600; `0` = no bound; build and test time do not count): exit
  76, `host-run: gave up after <n>s waiting for <what> …`, repeated last by `remote-linux.sh` as `remote: host run
  gave up waiting: …`. Nothing ran after that point; rerun it. A queued guarded slot run keeps its lease busy while it
  waits. Refs older than the queue still drop a run after 1800 s (`timed out waiting for <lock>`, exit 1).

## Logs and bundles

- `scripts/remote-linux.sh` keeps each invocation's whole output in `bundles/logs/remote-<UTC time>-<random>`,
  whatever the caller does with stdout. The log starts with the ref, commit, host, run dir and slot, and ends with
  `exit=<status>`. The path is printed last on stderr (`log: <path>`). Report that exit status: a pipeline ending in
  `tail` or `grep` does not carry it. A reader that closes early (`| head`) stops the log too. The coordinator's
  serial logs (`bundles/serial-logs/serial-<sha12>-<time>.log`) come from a wrapper that is not in the repo yet
  (#562 adds it).
- Bundles come back to `./bundles/` as `cod4e-run-<YYYYMMDD>-<HHMMSS>.zip` (serial and remote runs) or
  `slot-<n>-cod4e-run-...zip` (slot runs). The run prints `local bundle: <path>` for each one. A slot-exec run's files
  come back in `./out/`.
- Inside a bundle:
  - `summary.json`: `ok`, `build`, `skipped`, `unexpected_skips` and per-stage `stages` with status and reason. A
    skipped stage makes `ok` false unless `--allow-skip` names it. Read it with `unzip -p <zip> summary.json`.
  - `manifest.json`: the host, the install, and `build.hash`. Host runs set it to the shipped commit's first 12 hex
    digits (`COD4E_BUILD_HASH`). A local build outside git says `unknown`, and one with uncommitted changes ends in
    `-dirty`.
  - `stages/NN-<stage>/`: `result.json`, `stdout.log`, `stderr.log`, `logs/` (every process's log, which the log
    gate reads), and the stage's own files, such as `client.log`, captures and `control-run.json` (the run's steps).
  - The zip is capped at 100 MB: videos, then images, then traces and CSVs are dropped (largest first) and listed in
    `DROPPED.txt`.
- Evidence is per commit. Record the commit, the complete log with its exit status, and the bundle. A
  `gh pr update-branch` or a merge from main makes a new head, and evidence for the old head no longer covers it.
  After that, `mergeable` reads `UNKNOWN` for 10 to 60 s; poll it and never treat `UNKNOWN` as a pass.
- Keep the bundle of a failing-before run (the negative control) even after you delete its scratch branch.
- `just evidence-report --pr <n> --stage <stages> <bundle.zip>...` reports whether the bundles and the `windows`
  check cover the PR's current head. It is read only and does not approve a merge.

## Shared files

`crates/client/src/netplay.rs`, `crates/client/src/app.rs`, `README.md`, `crates/harness/src/logcheck.rs`,
`crates/harness/src/stage.rs` and `crates/harness/src/stages/mod.rs` are touched by most PRs. Expect conflicts, and
agree on ownership before editing them. A `logcheck.rs` entry change reaches every stage it lists (every stage when
the list is empty).

## Known flakes

A failure whose text matches a row's signature exactly, in the stage the row names, is that flake: name the row and
the bundle in the PR, rerun only as the row allows, and do not investigate it again. A second failure after the
allowed reruns, or any failure that does not match a signature exactly (another stage, another count, another
message), is a real failure. `N` and `M` stand for any number. "Reason" is the stage's reason in `summary.json` and
`result.json`; "log" is a line in the stage's `logs/`, which the log gate reports in the reason.

| Signature | Stage(s) | Issue | Owner | Slot reruns | Serial reruns | Remove when |
|---|---|---|---|---|---|---|
| Reason: `effect names missing: ` with an empty name, `net.fx.missing` `[""]` (a `play_fx` before its effect name reached the client) | client-match | #553 | author of #556 | 0 (timing set) | 1 | #556 merged |
| Reason: `bullet_impact_noblood played 3 times since the mark, want 1` (before #542: `fx:bullet_impact_noblood is +3`); live bots' flesh hits inside the 1 s blood window | client-gfx | #553, fixed by #556 | author of #556 | 1 | 1 | #556 merged |
| Reason: `shell_eject played N times since the mark, want 0` (before #542: `cgis=fx:shell_eject 0` returned N) | client-gfx | #511 (fixed by #513) | the coordinator's 10-run confirmation of #511 | 1 | 1 | 10 consecutive runs pass and #511 closes |
| Log: `the window's surface gave no frame outcome=Validation` | client-gfx | #511 (fixed by #510) | the coordinator's 10-run confirmation of #511 | 1 | 1 | 10 consecutive runs pass and #511 closes |
| Reason: `N of the M other players the server announced were ever drawn` | client-commands | #511 (fixed by #513) | the coordinator's 10-run confirmation of #511 | 1 | 1 | 10 consecutive runs pass and #511 closes |

Fixed and closed, for reference: `ExecutableFileBusy` (ETXTBSY) in the harness's `cargo test` (#538, fixed by #543).
Reported once and not filed: net-match `0 of 300 prediction passes clear`, possibly host load. If it recurs, file it
with the bundle.
