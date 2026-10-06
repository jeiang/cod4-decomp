# iw3mp subsystems: lineage, references, size, and server needs

Ticket: [#4](https://github.com/jeiang/cod4-decomp/issues/4). Scope: stock multiplayer, no network compatibility, no PunkBuster.

## How to read this sheet

Evidence tags:

- **[V]** verified against the real install in `COD4/` (strings and import tables of `iw3mp.exe`, disassembly of a call site, decompressed fastfile contents).
- **[S]** read in published source, used as a *fact only*. CoD4X is AGPL-3.0; nothing from it is copied or translated (ADR 0001).
- **[M]** measured by line-counting a clone of a published source tree.
- **[I]** inference. Not yet confirmed. Listed again under "Open unknowns".

LOC numbers are `wc -l` of `.c/.cpp/.h` files and are a scale, not a plan.

## Sources and licenses

| Source | Revision | License | Use |
|---|---|---|---|
| ioquake3, <https://github.com/ioquake/ioq3> | `83a7762` | **GPL-2.0-or-later** (file headers: "version 2 ... or (at your option) any later version") [M] | May be translated into the GPL-3.0-or-later project. Primary reference for everything that descends from Quake 3. |
| RTCW-MP / Enemy Territory, id Software, <https://github.com/id-Software/RTCW-MP> | GitHub API | **GPL-3.0** (headers: "either version 3 ... or any later version" in `g_main.c`) | May be translated. Closest *GPL* game with a stock MP rule set; not CoD-specific. |
| iortcw, ET:Legacy | GitHub API | GPL-3.0 | Same as above, maintained forks. Not read in depth here. |
| Quake3e (`ec-/Quake3e`), DarkPlaces | GitHub API | GPL-2.0 (SPDX reported; "or later" not checked) | Reference only unless the headers say "or later". |
| OpenAssetTools (OAT), <https://github.com/Laupetin/OpenAssetTools> | GitHub API | **GPL-3.0** (SPDX; whether files say "or later" not checked) | Has IW3 struct definitions, asset field tables and loaders (PhysPreset, XAnimParts, XModel, Material, techset, WeaponDef, MenuList/menuDef, StringTable, RawFile, Font, LocalizeEntry, SndCurve). May be reused if GPL-3-only is acceptable for the touched files. Flag: if a file is GPL-3.0-only, the combined work for that file is effectively GPL-3.0-only. |
| gsc-tool (`xensik/gsc-tool`), <https://github.com/xensik/gsc-tool> | GitHub API | GPL-3.0 (file headers say "GNU GPLv3 license"; "or later" not stated) | GSC lexer/parser/AST/compiler structure for IW5 and later. **No IW3 target** (earliest supported is IW5) [V: README]. Same GPL-3-only flag as OAT. |
| CoD4X server, <https://github.com/callofduty4x/CoD4x_Server> | `ddd65fc` | **AGPL-3.0** (GitHub SPDX, file headers) | **Fact source only.** It is a patched-in extension of the original binary (see below), not a standalone engine. |
| cod4x-docs, <https://github.com/callofduty4x/cod4x-docs> | `54f0676` | none declared (SPDX null) | Fact source only (documentation). |
| CoD4X client (`CoD4x_Client_pub`) | not read | NOASSERTION | Not used. |
| ODE, <https://www.ode.org/> | web page | **LGPL-2.1-or-later OR BSD-3** (dual; stated on ode.org section 3) | Compatible with GPL-3. |
| Rapier (`dimforge/rapier`) | GitHub API | Apache-2.0 | Compatible with GPL-3 (one-way: Apache-2.0 into GPL-3). |
| Original install, `COD4/iw3mp.exe`, `COD4/zone/english/*.ff` | 1.7 | Proprietary | Read for facts only. Nothing from it is committed or pasted. |

Important fact about CoD4X: it is a *binary-patching server extension*. Many of its files are headers or tiny stubs because the real function lives in `iw3mp.exe`. For example `bg_weapons.cpp` is 6 lines, `bg_animation.cpp` 25, `g_active.cpp` 100, `cm_trace.c` 91, `cl_dedicated.cpp` 103, `dobj.c` 93, `bullet.cpp` 158 [M]. `sys_patch.c`, `pe32_parser.c` and `elf32_parser.c` exist for patching the loaded original image [S]. So CoD4X is **not** a drop-in source map of the original engine. Where it has full code (script variable system, message coding, snapshot, netchan, snapshot archive), it is a good fact source. Where it has stubs, we must decompile.

## Facts about the binary that shape the table

- One executable contains client, server and game logic. There is no QVM and no game DLL. Imports are Win32 plus Direct3D 9 (`d3d9.dll`, `d3dx9_34.dll`), DirectSound, Miles (`mss32.dll`), Bink (`binkw32.dll`), WinSock and PunkBuster DLL names (`pbcl*/pbsv*`). No OpenGL, no OpenAL [V]. No `vm_`/QVM strings [V].
- PE sections: `.text` 0x28F429 bytes (about 2.7 MB code), `.data` virtual size 0xD218680 (about 220 MB), but only 0x10000 bytes raw. So roughly 218 MB is zero-initialized static state (server, client, renderer) [V]. Stock memory use is therefore dominated by static tables, not by code.
- Multithreaded client: strings for "render thread", "database thread" (fastfile loading), "Worker0", "Process renderer front end / back end in a separate thread", and thread-affinity cvars [V]. The server frame runs on the main thread [I].
- The engine descends from the Quake 3 line through Call of Duty 1 and 2. Hard evidence of Q3 lineage in strings: OOB commands `getchallenge`, `challengeResponse`, `getstatus`, `statusResponse`, `connectResponse` [V]; netchan fragment messages ("Dropped a message fragment", "illegal fragment length") [V]; `Hunk_*` and `Z_Malloc` [V]; snapshot/entity delta error strings ("Delta from invalid frame", "Delta parseEntitiesNum too old") [V]. Evidence of CoD2 lineage: a BSP version warning that names `cod2map` [V]. "Dvar" naming replaces "cvar" in many messages [V].
- Scripts in fastfiles are **GSC source text**, not bytecode. `common_mp.ff` contains `common_scripts/utility.gsc`, `maps/mp/_createfx.gsc` and about 135 `.gsc` paths, and the script body text is readable [V]. The binary contains `******* script compile error *******`, `Scr_BeginLoadScripts`, `Scr_AnimTreeParse`, `SCR_FUNC_TABLE_SIZE exceeded`, `#include` handling [V]. Conclusion: **the engine compiles GSC at load time**.
- Menus are compiled structs in the fastfile, not text: `common_mp.ff` lists 34 `.menu` asset names and zero `itemDef` text [V]. OAT lists `MenuList`/`menuDef_t` as loadable for IW3 [OAT docs].
- Collision (`clipMap_t`), `ComWorld`, `GameWorldMp`, `MapEnts`, `GfxWorld` are fastfile assets [OAT docs list them for IW3; supports "loadable in memory" but not dump]. The engine does not read `.bsp` files at runtime in the MP install I looked at; [I] that it never does.
- `iw3mp.exe` contains paths to the original ODE sources: `physics\ode\src\ode.cpp`, `odemath.cpp`, `rotation.cpp` [V]. Those three file names match the early (0.5-era) ODE layout [I]. Physics-related names in the binary: `phys_*` cvars (41), `phys_mcv_ragdoll`, `phys_contact_cfm_ragdoll`, script builtins `physicsexplosionsphere`, `physicsexplosioncylinder`, `physicsjolt`, `physicsjitter`, `startragdoll`, `isragdoll` [V]. Entity events `EV_PHYS_EXPLOSION_SPHERE/CYLINDER/JOLT`, `EV_PHYS_JITTER` exist in CoD4X's `bg.h` [S]: the server sends physics *events* to clients [I: simulation is client-side].
- Stock server cvars present in the original binary (strings): `sv_fps`, `sv_maxclients`, `sv_maxRate`, `sv_clientArchive`, `sv_clientSideBullets`, `sv_botsPressAttackBtn`, `sv_padPackets`, `sv_debugRate`, `sv_showAverageBPS`, `sv_voice`, `g_antilag` ("Turn on antilag checks for weapon hits") [V]. So antilag, killcam archive, test clients and voice are **stock**, not CoD4X inventions.
- Cvar counts in the binary, by prefix, as an indicator of surface area: `r_` 205, `cg_` about 197, `ui_` 87, `snd_` 34, `cl_` 43, `fx_` 15, `phys_` 41, `net_` 15, `sv_` about 75 names (strings, may include duplicates) [V].
- `sv_fps` registration: the call-site loads the immediate 0x14 (20) as the default value and the help text "Server frames per second"; an immediate 0x0A (10) is pushed alongside it [V, disassembly of the call site at VA 0x530700 region]. I did not decode the max, and the min/max assignment of 10 is an inference. CoD4X registers it as default 20, range 1..250 [S].

## Subsystem table

Relation to ioquake3: **same** = recognizably the Q3 code with small edits; **changed** = Q3 structure with significant rewrite or new fields; **new** = no Q3 counterpart.

Size: **S** < 2k LOC, **M** 2-6k, **L** 6-15k, **XL** > 15k, as a Rust rewrite estimate [I], scaled from ioq3 [M] and CoD4X where it is a full implementation [S]. Decompile effort is noted separately where it dominates.

"Server" = needed by the headless server.

| # | Subsystem | Relation to ioq3 | Best GPL-3-compatible reference | Size | Server |
|---|---|---|---|---|---|
| 1 | **Frame loop** (`Com_Frame`, `SV_Frame`, time residual, sleep) | **changed**. Same accumulate-and-step shape [S: CoD4X `SV_Frame` uses `timeResidual`, a per-frame budget of `1000000/sv_fps` microseconds, `NET_Sleep` until a packet or deadline]. Changes: microsecond timing, multi-threaded client (render, database, worker). Stock `sv_fps` default 20 [V]. | ioq3 `common.c` + `sv_main.c` (GPL-2.0+) [M: 3801 + 1294 lines] | S (server loop) / M (client loop with threads) | **Yes** (server loop only) |
| 2 | **Server/client split** (`svs`/`sv`, `client_t`, connect, configstrings, dedicated flag) | **changed**. Same split and OOB handshake [V]. Game and cgame are linked into the exe; no QVM trap layer. | ioq3 `sv_client.c`, `sv_init.c`, `sv_main.c` (GPL-2.0+) [M: 2022 lines for `sv_client.c`] | M | **Yes** |
| 3 | **Network channel** (netchan, fragments, reliable commands, OOB, Huffman) | **same lineage**, changed details [V strings; S: CoD4X `netchan.c` 632 lines vs ioq3 690; `msg.c` 4068 vs 1717, mostly field tables]. | ioq3 `net_chan.c`, `msg.c`, `huffman.c`, `net_ip.c` (GPL-2.0+). **Wire compatibility is out of scope**, so no need to reproduce the original byte layout or Huffman tables. | S-M | **Yes** |
| 4 | **Snapshot and delta coding** (`SV_BuildClientSnapshot`, PVS, entity delta, player state, archive) | **changed heavily**. Same delta-against-last-acked-frame idea [V: error strings]. New: per-entity field tables, fog-distance cull (`G_GetFogOpaqueDistSqrd`), 1024 entities (10 bits) [S], `playerState_t` about 12 KB (0x2F64), `clientSnapshot_t` about 12 KB (0x2F84) [S, struct size comments], `PACKET_BACKUP` 32 [S]. Archive and cache for killcam and antilag (rows 14 and 7). | ioq3 `sv_snapshot.c` (685 lines) + `msg.c` delta (GPL-2.0+). The state *contents* (which fields exist) come from decompiling or CoD4X fact reading. | M | **Yes** (largest single server cost; see hotspots) |
| 5 | **Usercmd** (input command, key obfuscation, move quantization) | **changed**. Q3 `usercmd_t` plus extra fields; command bytes are XOR-keyed on the wire [S: `MSG_ReadKey`, `MSG_ReadDeltaKey*`]. | ioq3 `msg.c` `usercmd` delta (GPL-2.0+); field list from decompile. The XOR key is a wire detail and not needed. | S | **Yes** |
| 6 | **Player movement `pmove` and `bg_` physics** | **changed heavily**. Ancestor is Q3 `bg_pmove.c` (2068 lines) [M]. New: prone, ladders, sprint, ADS slowdown, jump handling (`Jump_*`), fall damage, view bob [V: `bg_ladder_yawcap`, `bg_prone_yawcap`, `bg_fallDamage*`, `bg_bob*`, `player_*` cvars, 90 `bg_` strings]. CoD4X contains only `Jump_*` functions (374 lines) [S]. | Structure: ioq3 `bg_pmove.c` (GPL-2.0+), RTCW-MP `bg_pmove.c` as a second GPL-3.0 baseline. Behavior: **decompile only**. | L (decompile-bound) | **Yes** (client prediction and server both run it) |
| 7 | **Hit detection and traces** (`CM_BoxTrace`, entity link/area query, bullet traces, hit locations, penetration, antilag) | **changed**. `CM_BoxTrace` is Q3 lineage; entity world query is Q3 `sv_world.c` (687 lines) vs CoD4X 1524 [M, S]. New: bullet penetration and surface flags, hit-location from skeletal bone parts (OAT ships `partclassification_mp.csv` for IW3 [V: file exists in OAT tree]), antilag. | ioq3 `cm_trace.c` (1470), `cm_load.c`, `cm_patch.c`, `sv_world.c` (GPL-2.0+). Antilag and penetration: **decompile only**. Facts: antilag rewinds client positions up to **400 ms** back [S: CoD4X `G_AntiLagRewindClientPos`], reading them from archived snapshots. | M-L | **Yes** |
| 8 | **Physics (ODE)** | **new** (no Q3 counterpart). Embedded ODE [V: paths]. In MP it is cosmetic (ragdolls, dynamic entities, physics presets, explosions) [I]. | ODE (LGPL-2.1+/BSD-3, dual) or Rapier (Apache-2.0) as a replacement. Behavioral fidelity of ragdoll is a client concern. | M (client) / **S-none (server)** | **No** [I]. The server must load dynamic-entity definitions (`DynEnt_LoadEntities` appears in CoD4X `cm_load.c` [S]) and emit physics events, not simulate. Confirm (see unknowns). |
| 9 | **Animation** (`XAnimTree`, `DObj`, bone matrices, blend, notifies) | **new**. Q3 has only MD3 tag lerp in the renderer. | OAT for `XAnimParts`/`XModel` data layout (GPL-3.0). Evaluation order and blend rules: **decompile**. | L | **Yes, partly**. The server evaluates skeletons for hit boxes, tag queries (`gettagorigin`) and anim notifies [S: `SV_DObjCreateSkelForBone`, `SV_DObjUpdateServerTime`, `SV_DObjGetMatrixArray` in CoD4X `sv_game.c`]. Cost scales with player count (see hotspots). |
| 10 | **GSC VM and compiler** (lexer, parser, compiler, bytecode VM, string table, variable tree, threads/notify/endon, builtin function table, callbacks) | **new** (Q3 has QVM bytecode; this is a different language, a CoD2 lineage [I]). Engine compiles `.gsc` text at load [V]. Script threads are fibers run **sequentially** on one thread [S: cod4x-docs scripting guide]. Builtin surface: CoD4X's table registers 245 functions and 269 methods, a mix of stock and CoD4X additions that I did not separate [S: `scr_vm_main.c`]. | Language reference: community docs and stock scripts (in the zones). Compiler/VM structure: **gsc-tool** (GPL-3.0) for IW5+ parser/AST/compiler/disassembler; its engine tables differ from IW3. CoD4X's `cscr_*`, `scr_vm*` (16k lines, AGPL) = **fact only**. | **XL** (VM + compiler ~8-10k, builtins ~5k+) [I] | **Yes** (all gametype rules run here) |
| 11 | **Menus and UI** (menu structs, item draw, expressions, ownerdraw, script menus) | **changed**. Origin is Team Arena `menuDef_t`/`itemDef_t` (`ui_shared.c`, present in ioq3 `code/ui`, 16,614 lines in `ui/`) [M]. IW3 menus are compiled assets with expression bytecode ("exp") [V]. | ioq3 `code/ui/ui_shared.*` (GPL-2.0+) for the item model; OAT's `MenuList`/`menuDef_t` loader and *decompiler* for the on-disk struct and expression encoding (GPL-3.0). | L | **No**, except accepting `menuresponse` commands and precaching script menu names [I] |
| 12 | **Renderer** (D3D9 only; techsets, materials, shader bytecode, world, models, sky, decals) | **new**. Not related to Q3 renderer. | Separate research ticket. OAT has Material/techset/image definitions (GPL-3.0). | XL | **No** |
| 13 | **Sound** (Miles `mss32.dll`, DirectSound, sound aliases, curves, mp3/wav in zones) | **new**, proprietary backend [V]. | Mixer design only: ioq3 `snd_mem.c`, `snd_dma.c`, `snd_mix.c`, OpenAL path (GPL-2.0+). Alias/curve layout: OAT lists `SndCurve` loadable; `snd_alias_list_t` **unsupported** (❌) in OAT, so decompile or reverse the format. | L | **No** (the server only needs alias names for `playsound`-type events [I]) |
| 14 | **FX** (effect defs, particle elements, impact tables, play events) | **new**. | **Decompile only**: OAT marks `FxEffectDef` and `FxImpactTable` unsupported for IW3. | L | **No**. Server sends fx indices; it needs the index table only [I]. Impact table also affects bullet decals (client). |
| 15 | **Killcam / archived snapshots** | **new**. | Facts only: the server archives each frame's snapshot into a ring of **1200** frames in a **16 MiB** buffer (`NUM_ARCHIVED_FRAMES`, `ARCHIVEDSSBUF_SIZE` 0x1000000) [S]; stock `sv_clientArchive` cvar exists [V]. A killcam can instead be built as a replay of recorded entity states in our own format since wire compatibility is out of scope. | M | **Yes**, if killcam is kept; it is a stock MP feature |
| 16 | **Cvars and commands** (`Dvar_*`, `Cmd_*`, command buffer, config files) | **same** with rename "Dvar" and a **domain struct** (min/max/enum) per registration [V call-site]. | ioq3 `cvar.c` (1464) + `cmd.c` (871) (GPL-2.0+) | S | **Yes** |
| 17 | **Bots / test clients** | **new**. Stock `addtestclient` script builtin exists [V] plus `sv_botsPressAttackBtn` ("Allow testclients to press attack button") [V]. There is **no botlib or AAS** in the original binary [V: zero hits]. CoD4X implements its own A*-based bot movement (`sv_bots.cpp` 401 lines, `sv_bots_astar.h`) plus extra GSC functions `removetestclient`, `removealltestclients`, bot movement/stance control [S]. | Navigation: ioq3 `botlib` (GPL-2.0+, AAS from BSP; needs a compile step that works on `clipMap_t` data) is the only GPL navigation code. Alternatives: own nav mesh from collision. | S (stock test client) / M-L (useful bots) | **Yes** (map says server-side bots are in scope) |

### Subsystems not on the ticket's list that still belong in the architecture

| # | Subsystem | Relation to ioq3 | Best reference | Size | Server |
|---|---|---|---|---|---|
| 18 | **Filesystem and fastfile database** (IWD zip search paths, `fs_game`, zone load, "database thread", XAsset pool limits) | **changed**. Zip search path is Q3 `files.c`; the fastfile DB (`DB_*`) is **new**. CoD4X notes "Support for higher xasset limits" and fixed a "Small zone overflow" [S: CHANGELOG 17.0, 17.3]. | ioq3 `files.c` + `unzip.c` (GPL-2.0+); OAT for zone format and asset structs (GPL-3.0). See the fastfile ticket. | M-L | **Yes** |
| 19 | **Memory** (hunk, zones, `physicalmemory`, `com_memory`) | **changed**. Strings reference `Hunk_*`, `Z_Malloc`, `physicalmemory.cpp`, `com_memory.cpp` [V]. | ioq3 `common.c` hunk/zone (GPL-2.0+). A Rust rewrite does not need to mimic it. | S | **Yes** |
| 20 | **Entity/game logic** (`G_RunFrame`, `g_` entity think, spawn, combat, `g_` cvars, turrets, vehicles/helicopters) | **changed**. Q3 `g_*` lineage. The CoD4X reimplementation is tiny: `g_active.cpp` 100 lines; vehicle files 9-14 lines [M]. | ioq3 `code/game` (46,207 lines) as ancestry; RTCW-MP (GPL-3.0). Behavior: **decompile**. | L | **Yes** |
| 21 | **Voice chat** (`sv_voice`, `sv_voiceQuality`) | **new**. | None GPL; decompile. | S-M | **Yes** if voice is kept; optional |
| 22 | **Localization** (`localized_english_*.iwd`, `stringed`, `LocalizeEntry`) | **new**. | OAT `LocalizeEntry` (GPL-3.0). | S | **Partially** (server strings in GSC `&"..."` references) [I] |
| 23 | **Platform layer** (windowing, input, timing, sockets) | **changed**. | ioq3 `sys_*.c`, `sdl/` (3,235 lines) (GPL-2.0+), but the project uses winit/SDL3 per ADR 0001. | S | **Yes** (sockets, time only) |

## Reference quality summary

| Subsystem group | How much GPL-3-compatible code exists |
|---|---|
| netchan, msg framework, cvar, cmd, files, collision trace, `sv_world`, frame loop, server/client connect | Plenty, ioq3 (GPL-2.0+). Translate directly. |
| pmove, entity game logic, snapshot field set | Skeleton from ioq3/RTCW; **specifics need decompilation**. |
| Asset struct layouts (IW3) | OAT (GPL-3.0) covers most types except sound aliases, clipMap, FX, GfxWorld, ComWorld. |
| GSC | gsc-tool (GPL-3.0) for IW5+; IW3 specifics only from decompile and CoD4X facts. |
| Animation eval, bullet penetration, antilag, killcam, FX playback, sound mixer details | **Decompile only.** |

## Known CoD4 server CPU and memory hotspots

All items below come from CoD4X source and docs (AGPL, fact only), from the original binary, or are marked as inference. There is **no published benchmark** of CPU time per subsystem for CoD4 or CoD4X (cod4x-docs has no performance section, and CoD4X CHANGELOG has none beyond bug fixes). I found only qualitative guidance; numbers need our own harness (this is already listed in the map under "Server performance method").

### CPU

1. **Single-thread frame budget.** The server runs in one frame loop. At `sv_fps` 30 the budget is 33.3 ms per tick; at 20 it is 50 ms. The loop catches up by running several ticks when late [S: CoD4X `SV_Frame` while `timeResidual >= frameUsec`]. Community advice for CoD4X servers is that single-thread speed is what matters and GSC does not use extra cores [S: cod4x-docs scripting guide "executed sequentially"].
2. **GSC execution.** Every gametype rule, every per-player `for(;;){ ...; wait 0.05; }` loop and every notify runs on this one thread. The CoD4X scripting guide says to load-balance heavy threads across server frames with `wait`. Because GSC wake-ups are tied to the tick, cost scales with `sv_fps x players x threads per player` [S, I].
3. **Per-client snapshot build.** For each client each tick the server walks the visible-entity candidate list, and for each entity does a leaf lookup (`CM_BoxLeafnums`) and a PVS bit test against the client's cluster, plus a fog-distance test, then delta-encodes entities and player state, compresses, and sends [S: `SV_AddEntitiesVisibleFromPoint`, `SV_BuildClientSnapshot`]. That is O(clients x entities) per tick, with a large per-client `playerState_t` (about 12 KB) to delta [S]. This is the most obvious scaling cost at 32+ players and is the same shape as Q3's `sv_snapshot.c`, with a heavier state.
4. **Snapshot archive (always on).** Each tick the server also encodes a full snapshot into the ring for killcam and antilag (`SV_ArchiveSnapshot`, 1200 frames, 16 MiB) regardless of how many clients are connected [S]. At 30 Hz this is 40 s of history, at 20 Hz 60 s [derived from 1200 / fps].
5. **Antilag rewind.** For each hitscan shot the server rewinds client positions by decoding archived snapshots back into a cached form (`SV_GetCachedSnapshotInternal`, `MSG_ReadDeltaArchivedEntity`), up to 400 ms back [S]. The decode is delta-chain recursive in CoD4X (a `depth` parameter). This makes shot cost depend on history decode, not a simple array lookup. A rewrite can keep a simple ring of per-client positions instead [I].
6. **Player movement.** `Pmove` runs for every user command received, per client, on the server [I; same as Q3]. Low cost per call but multiplied by commands per frame.
7. **Server-side skeletons.** Bullet hit checks and `gettagorigin`-style calls need bone matrices, so the server builds `DObj` skeletons and updates animation time for entities [S: `SV_DObjUpdateServerTime`, `SV_DObjCreateSkelForBone`]. Cost grows with players and with scripted models [I].
8. **Bots.** CoD4X bots use A* path search on the server [S: `sv_bots_astar.h`]; the stock test client does not (it is a no-AI client; the stock cvar only allows pressing attack) [V cvar; I behavior]. Any navigation we add is a new hotspot.
9. **Connectionless floods.** `getstatus`/`getinfo`/`rcon` handling is rate-limited per address with leaky buckets in CoD4X (`SVC_RateLimitAddress`) [S]. The stock binary has `getstatus`/`getinfo` handlers [V] but I did not verify stock limits.
10. **Logging stalls.** CoD4X guards an error print "to stop error message flooding which can stall the whole server" (`nextArchivedSnapshotErrorTime`) [S]. Console output in a hot loop is a known stall source.
11. **Map load and restart.** Zone inflate (`IWffu100`: zlib) plus GSC compile at load, on the server's thread unless loading is moved off it [I; the original has a separate database thread, [V] strings]. Any "fast restart" path should avoid recompiling GSC [I].

### Memory

Server state structure sizes below come from CoD4X's reconstruction of `svs` and its comments [S]; they are *not* re-measured in the binary. They are consistent with the 218 MB zero-initialized `.data` [V] but that size covers client and renderer too.

| Item | Size estimate | Basis |
|---|---|---|
| Static state in the original exe (all subsystems) | ~218 MB virtual | `.data` virtual size 0xD218680 [V] |
| `snapshotEntities` ring | 0x2A000 x 0xF4 = ~41 MB (172,032 entities x 244 bytes) | [S] |
| Archived snapshot buffer | 16 MiB | `ARCHIVEDSSBUF_SIZE` 0x1000000 [S] |
| Per-client frame history | 64 clients x 32 frames x ~12 KB = ~25 MB | `MAX_CLIENTS` 64 and `PACKET_BACKUP` 32 [S]; scales linearly with client count |
| `svEntities` | 1024 x 0x178 = 0x5E000 (~380 KB) | [S] |
| Cached snapshot frames/entities/clients | 512 frames, 16,384 entities, 4,096 clients | array sizes [S] |
| Fastfile contents | one `common_mp` zone decompresses to ~41 MB (13.5 MB compressed); one map zone (`mp_crash`) to ~63 MB (35 MB compressed) | measured by zlib-inflating `IWffu100` streams at offset 12 [V]. Includes gfx and sound data a headless server may skip, see the fastfile ticket. |

Observations:

- The original 64-slot layout is far larger than a 32-slot 512 MB budget needs; a rewrite should size these by `sv_maxclients` and by actual history depth instead of fixed 64-slot, 1200-frame arrays.
- CoD4X fixed "Memoryleaks" and a "Small zone overflow" (issue #148) [S: CHANGELOG]. These show zone and leak failures were practical problems at long uptime.

## Implications for the architecture ticket

1. The two biggest reference gaps are **GSC** (large, no IW3 open implementation) and **pmove/entity logic** (small ioq3 skeleton, large decompile surface).
2. Because network compatibility is out of scope, rows 3-5 can follow ioq3's design with our own wire format; the only constraint is that the gameplay-visible semantics (snapshot rates, reliable commands, usercmd contents) match.
3. Rows 8, 11-14 are client-only and can come after the headless server milestone. Row 9 (animation) and row 7 (traces) cannot be skipped for the server because hit detection depends on them.
4. Killcam (row 15) and antilag (row 7) both depend on the snapshot history design; they should be one design decision.

## Open unknowns

1. Whether the server simulates any ODE physics (ragdoll, dynamic entities). I only found events and cvars, not server code (CoD4X leaves `startragdoll`/`isragdoll` to the original binary, so I could not read it). Needs decompilation of `GScr_StartRagdoll` and the `DynEnt` update path. This belongs to the map's "Collision and physics needs" fog.
2. Stock `sv_fps` min/max, and whether stock allows 30. The call-site decode gave the default 20 but not the range (see above).
3. Exact stock GSC builtin count and callback list (CoD4X table mixes stock and added functions).
4. Real CPU and memory profile of the stock server per subsystem. Nothing published; this needs the harness and a headless build.
5. Whether `-only` or `-or-later` applies to OAT and gsc-tool source files (license text read only at repository level).
6. Whether servers require the sound-alias table or localization at all (inferred "alias names only").
7. Stock `addtestclient` behavior (does it send any usercmds at all). The CoD4X help text reads only as usage text; behavior unverified.

## Method notes

- `strings -n 5` over `COD4/iw3mp.exe` (20,086 strings) for cvar, command, script-builtin, source-path and import evidence.
- Python `struct` PE parse for sections; Capstone x86-32 disassembly of cvar registration call sites (temporary Nix shell, not committed).
- `zlib.decompressobj().decompress(file[12:])` on `common_mp.ff` and `mp_crash.ff`, then printable-string scan for asset paths. No content kept.
- `git clone --depth 1` of CoD4X, cod4x-docs and ioq3, then `wc -l` and reading specific functions.
- GitHub API for license SPDX identifiers. For ioq3, RTCW-MP and gsc-tool, license headers of individual files were also read.
