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

## Verification

**Harness**:
The tool that runs the engine in a scripted scenario and records logs, video, and performance data.

**Run bundle**:
The zip a harness produces for one run. It is sent back from a remote tester.
_Avoid_: report, artifact zip
