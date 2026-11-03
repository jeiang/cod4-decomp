# CoD4 Engine Reimplementation

A new engine that runs Call of Duty 4: Modern Warfare multiplayer from a user-owned copy of the original game files.

## Original game

**Original install**:
The user-owned CoD4 1.7 directory the engine reads content from. The engine never redistributes its contents.
_Avoid_: game files, assets folder

**Original engine**:
IW3 as shipped in `iw3mp.exe` 1.7. It is the behavioral reference, not a dependency.
_Avoid_: the game, stock engine

**Fastfile**:
A compressed zone archive (`.ff`) that holds the original engine's precompiled assets.
_Avoid_: FF, zone file, pak

**Zone**:
The set of assets loaded from one fastfile as a single unit, such as `common_mp` or one map's zone.
_Avoid_: pack, bundle

**IWD**:
A zip archive in `main/` or a mod directory that holds loose files such as images, sounds, and configs.
_Avoid_: pk3, pak

**Asset**:
One typed content item in a zone (map, material, weapon, sound, script, and so on), named by its type and name.
_Avoid_: XAsset (except when referring to the original engine's struct), resource

**Stock content**:
The maps, gametypes, and scripts shipped in the original install, excluding `mods/` and `usermaps/`.
_Avoid_: vanilla, base game

**Mod**:
Content under `mods/` or `usermaps/` that adds to or replaces stock content.

## Runtime

**Server**:
The authoritative simulation of one match. Clients connect to it.
_Avoid_: host

**Headless server**:
A server process with no renderer, audio, or window.
_Avoid_: dedicated server (the original engine's term; acceptable in conversation)

**Client**:
The player-facing process that renders, plays audio, reads input, and talks to a server.

**Bot**:
A server-side simulated player that needs no client.
_Avoid_: test client, AI

**Gametype**:
The rule set of a match (for example TDM, DOM, S&D), defined by stock GSC scripts.
_Avoid_: mode

**GSC**:
The original engine's game scripting language. Stock gameplay rules are written in it.
_Avoid_: script (when ambiguous)

**Tick**:
One fixed-rate step of server simulation.
_Avoid_: frame (a frame is a rendered client image)

**Snapshot**:
The state of the world that the server sends to one client for one tick.

**Logical eye**:
The predicted feet plus the view height: where shots start, the listener hears and the harness measures the player's movement from.
_Avoid_: camera origin

**Render eye**:
The logical eye moved by the camera layer (stair smoothing, bob, lean, landing dip); it is only what the screen draws from.
_Avoid_: eye (say which)

## Verification

**Harness**:
The tool that runs the engine in a scripted scenario and records logs, video, and performance data.

**Run bundle**:
The zip a harness produces for one run. It is sent back from a remote tester.
_Avoid_: report, artifact zip

**Scenario**:
A console command script, with timed waits, that the harness feeds to the engine.
_Avoid_: test case, script (ambiguous with GSC)

**Stage**:
One step of a suite, such as asset load, headless server with bots, client flythrough, or client match. A stage is skipped while the engine cannot run it yet.

**Suite**:
An ordered list of stages that the harness runs. The default suite is what the remote tester runs.

**Control channel**:
The dev-only JSON line channel through which a stage drives a client or server and reads its state document (`docs/control-channel.md`).
_Avoid_: debug API, RPC

**State document**:
What the control channel answers for `state` and `wait`: the client's observable state in named parts. Reading it changes nothing.

**Player's path**:
Reaching a game state the way a person does: keys, mouse clicks on menu items, typed console lines. A stage proves player-facing behaviour only through it.

**Dev hook**:
A named `dev` request that sets up a scenario no key or menu reaches. A stage that uses one cannot prove the player's path for that step.

**Log gate**:
The check that fails a stage whose logs hold a warning or error line that no allow-list entry in `crates/harness/src/logcheck.rs` covers, even when the stage passed.
_Avoid_: log filter

**Allow-list entry**:
One accepted log line in the log gate: a target, the line's text, the stages it covers, a per-stage count limit, and the issue that explains it.

**Slot**:
One of three isolated places on the Linux host where a run can build and run stages beside other runs. Each slot has one owner at a time, held by a slot lease.

**Slot lease**:
The slot guard's record of who may run in a slot. A guarded run whose token does not hold the lease is refused (exit 75); a slot run with no token bypasses the guard and is kept apart only by the slot lock.

**Serial run**:
A run that has the Linux host to itself: no other stage and no build runs beside it. Only a serial run judges timing.
_Avoid_: exclusive run

**Timing set**:
The stages judged only in a serial run because they measure time, frames or ticks: client-load, client-match, client-flythrough and headless-bots-32 (the last is not in the default suite).

**Known flake**:
A failure with an exact signature, issue, owner and rerun allowance in `docs/host-notes.md`. Any other failure is real.
