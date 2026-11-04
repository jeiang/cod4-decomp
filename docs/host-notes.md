# Host and tooling notes

Facts about the Linux test host, the run recipes and the run bundles that agents kept learning again. Facts about
the engine live next to its code: the control channel in `docs/control-channel.md`, the log allow-list in
`crates/harness/src/logcheck.rs`, a stage's facts in its module doc, terms in `CONTEXT.md`. When you learn a host or
tooling fact, or a flake's status changes, update this file in the same PR.

## Hosts and paths

- The Linux test host is `artemis.jeiang.vpn` (`COD4E_LINUX_HOST`, the justfile's default). `artemis.local` no
  longer answers. Use `scripts/host-ssh.py`, not raw ssh, for this host (see SSH transport below).
- Its login shell is fish. Send POSIX shell on stdin: `scripts/host-ssh.py artemis.jeiang.vpn bash -s <<'EOF' ... EOF`.
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
  - `run-<mac host>-<UTC time>-<random>`: private detached run directory. Removed only after the host command
    finishes and logs/bundles/output have been collected; stopping the Mac follower leaves it recoverable.
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
  `tail` or `grep` does not carry it. A reader that closes early (`| head`) stops the local log/follower, not the host
  run. `just serial-log` keeps its additional log in `bundles/serial-logs/serial-<sha12>-<time>.log`.
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

### SSH transport

Every Linux-host recipe and detached follower uses `scripts/host-ssh.py`. Run it in the dev shell (Python3 is
required). The default `-F /dev/null` skips **both** ~/.ssh/config and system ssh_config/Includes, avoiding
NetBird's `Match exec ... ssh detect` banner probes (#590). `ProxyCommand=none` alone does not avoid Match exec.
The agent (`SSH_AUTH_SOCK`), default identity filenames and default known_hosts verification still work;
host-key checking is never disabled. What is lost is config-only Host/HostName aliases, User/Port, IdentityFile,
IdentitiesOnly, IdentityAgent (including 1Password routing), certificates, ProxyJump/ProxyCommand, custom
known_hosts and host-key settings, and other user/system configuration. Use the real FQDN by default.

For required nondefault settings, set `COD4E_LINUX_SSH_CONFIG=/absolute/path/to/detector-free-config` on every
recipe. This separate OpenSSH config and every Include it uses must omit the NetBird detector. The repository
does not edit/copy/filter ~/.ssh/config or change artemis. `-F` applies to effective-config lookup, master startup,
control checks and command channels. Do not pass multiplexing/proxy overrides as wrapper `-o` arguments.

The socket lives at `/tmp/cod4e-ssh-<uid>/<connection-hash>.sock` (`/private/tmp` on the Mac), in an owner-checked
0700 directory, not a checkout or omp directory. The hash includes selected config path and effective host, user,
port, identity and routing settings, not tty mode. First use takes an OS file lock, checks the master and starts
`ControlMaster=yes -Nf` with `ControlPersist=600`. Never combine that option with `-M`: OpenSSH promotes the
second master request to ask mode. Concurrent starts cannot create competing masters. Eight flock-backed channel
tokens leave headroom under sshd's default MaxSessions=10. A separate local keeper owns each token until its SSH
child exits, even when the invoking wrapper is killed and OpenSSH closes inherited descriptors. Master creation
is not locked for a command's duration, so slots/serials share channels concurrently.
Do not remove lock/token files: replacing their inode breaks cross-process locking.

Followers never stop the shared master; OpenSSH expires it after 600 idle seconds. A dead/stale master is
replaced by the next invocation under the creation lock. Command channels use `ControlMaster=no` and
`ProxyCommand=false`: a master dying between check and use returns 255 rather than silently creating a new
connection. Master death interrupts active channels, not detached host jobs. The wrapper never replays commands;
only the follower retries idempotent start/poll/collect/remove operations. Uploads and arbitrary host commands
fail normally. Restart/attach after an exhausted disconnect; never rerun the whole job blindly.

File transfers currently use tar over this wrapper, not scp/rsync. If needed, use
`rsync -e scripts/host-ssh.py ... artemis.jeiang.vpn:...` with the same config environment.
The wrapper is not an scp `-S` adapter (scp supplies additional policy/subsystem options).
Do not introduce a raw-SSH parallel transport.

### Detached runs: recovery and cancellation

`remote-linux.sh` starts a host-owned `nohup setsid` session around the installed slot guard and the whole
`host-run.sh` command. The queue PID/start identity, EXIT cleanup, FIFO/lock order, lease lifetime, Chromium
preflight (78), queue give-up (76), and virtual audio path are unchanged. No daemon or new host installation is
needed; detachment requires the host's `setsid`, `nohup` and `flock` (absence is an error, not a foreground fallback).

The private run directory keeps `.host-stdout`, `.host-stderr`, `.host-identity` (supervisor PID, exact `ps -o lstart=`
start time, process group), `.host-status` (atomically published command exit status), and `.host-slot`. Its
`.host-cmd` may contain a lease token: never print or copy it into an issue/log. SSH has `ConnectTimeout=10`,
`ServerAliveInterval=10`, `ServerAliveCountMax=3` and batch mode. Successful running polls sleep 5 seconds. Only
transport exit 255 retries: four sleeps of 5/10/20/30 s plus 0–1 s jitter, reset after success, then the real 255
and SSH diagnostic, with the directory retained. Sleep totals stay below 69 s per operation; connection timeout,
keepalive detection and command runtime are additional. Only complete snapshots advance offsets. A command's exit 255 is read from `.host-status`,
not confused with SSH's 255. A reachable host with a dead supervisor and no status is reported as incomplete
(255) and retained for inspection.

**Ctrl-C, TERM, HUP, a coordinator's `proc kill` of the local job, or a killed Mac stops only the follower.** The
host run continues holding its lease and locks, including while queued. The local log already contains the host
and run directory even when SIGKILL prevents an interruption message. Resume from any checkout with:

```sh
COD4E_LINUX_HOST=artemis.jeiang.vpn nix develop -c scripts/remote-linux.sh --attach '~/Projects/cod4e/run-<identity>'
```

Use the exact directory from the log. Attach neither ships a ref nor starts another command nor needs a token.
A fresh attach replays the host logs from their beginning. Artifact transfers are retried before extraction, and
the host directory is retained until collection succeeds; a killed follower during collection can attach again.

For **intentional host cancellation**, first stop local followers (otherwise an attach may race inspection).
Use the exact logged run directory, and send POSIX commands to bash, not the host's fish login shell:

```sh
scripts/host-ssh.py artemis.jeiang.vpn bash -s <<'EOF'
run=$HOME/Projects/cod4e/run-<identity>
{ read -r pid; IFS= read -r started; read -r pgid; } < "$run/.host-identity"
test "$(ps -o lstart= -p "$pid")" = "$started" || exit 1
test "$pid" = "$pgid" || exit 1
# Inspect the command, entire descendant tree and lock holders BEFORE signalling anything.
ps -eo pid,ppid,pgid,sid,args --forest
lslocks --notruncate --output PID,COMMAND,PATH
# After verifying that this is the intended run, signal its group, not just the supervisor or queue PID.
kill -TERM -- "-$pgid"
EOF
```

Save the verified run's descendant PIDs/groups from that tree first: commands that create their own process
groups/sessions must also be terminated (for example `kill -TERM -- -<verified-descendant-pgid>`). Reinspect
with the same `ps`/`lslocks` commands; if a verified group/tree survives TERM, use `kill -KILL -- -<verified-pgid>`
for that group and its verified escaped descendants, not a broad name-based `pkill`. Do not reuse stale PIDs
without rechecking their start identities. Killing only the supervisor/registry PID can strand children holding
inherited lease and stage locks. From the Mac run `nix develop -c just host-status` and inspect host `lslocks`
again: confirm this run, its descendants and its slot/stage/lease lock holders are gone **before releasing or
reassigning the slot**. A killed supervisor may leave no `.host-status`; retain that directory's logs for
diagnostics rather than treating cancellation as a completed harness result.


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
