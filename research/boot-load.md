# Boot and map-load: how the original engine finds and loads files

Ticket: [#3](https://github.com/jeiang/cod4-decomp/issues/3). Status: resolved, with named unknowns at the end.

## 0. Method, evidence labels, and licenses

- **[V]** = verified. Read directly from the real files in `COD4/` or from disassembly of `iw3mp.exe` 1.7 (sha256 `e41fcd59…a1fa893`, PE32 x86, image base `0x400000`).
- **[I]** = inference. Reasoned from verified facts, not directly observed.
- Addresses like `0x55e670` are virtual addresses in `iw3mp.exe`. They are given so the Ghidra baseline (ticket #10) can find the same code. No decompiled code, assets, or script text is reproduced here. Names are original-engine function guesses written by this researcher (the binary is stripped).
- Disassembly came from `objdump -D` on the raw file (nix `binutils`) plus Ghidra (REA) for function boundaries. Ghidra's decompiler dropped register-passed arguments (`edi`) in the FS code, so the assembly was the authority.

| Source | License | Use |
|---|---|---|
| `COD4/iw3mp.exe` 1.7, IWDs, fastfiles, cfgs | Proprietary (user-owned install) | Primary evidence. Facts only; nothing copied. |
| Quake III Arena `code/qcommon/files.c` (id Software) | GPL-2.0-or-later ("version 2 … or (at your option) any later version") | **GPL-3-compatible. Code may be reused** (with attribution). The CoD4 FS is a direct descendant (same `searchpath_t` list, same pak-sort-then-prepend, same `FS_Startup` shape). |
| CoD4x_Server `src/filesystem.c` (callofduty4x) | AGPL-3.0-or-later | **Fact source only; never copy or translate.** Used once to cross-check `FS_Startup` order. Differences from stock are listed in §3.4. |
| A web-search summary (aggregating war24/moddb/CoD4X docs) | Secondary, unattributed | Not relied on. It agrees with the alphabetical-override rule, which §3 verifies from the binary. |

## 1. Install layout that matters (verified against `COD4/`)

- `main/` holds 21 IWDs: `iw_00…iw_13.iwd` (14 files, 150 MB–168 MB each except `iw_12` 5.6 MB and `iw_13` 28.6 MB) and `localized_english_iw00…iw06.iwd` (7 files). Total 24,079 unique entry names. **[V]**
  - `iw_00`–`iw_05`, `iw_12`, `iw_13`: `images/*.iwi` (6,561 unique images across all IWDs).
  - `iw_00`: also 125 `.cfg` files and 2 `.csv` (`configure.csv`, `configure_mp.csv`).
  - `iw_06`–`iw_11`: `sound/*.wav|mp3`, `weapons/{mp,sp}/*` (extension-less weapon def files), `accuracy/*.accu`.
  - `localized_english_iw00`: `default.cfg`, `default_mp.cfg`, `default_mp_controls.cfg`, a few `images/`, and 2,867 `sound/` files. `iw01…iw06` are only `sound/*.wav` (battle-chatter, voice-overs).
  - All entries are Deflate (method 8), except one stored entry in `iw_00`. Directory entries (`images/`, `sound/`) exist. **[V]**
- `zone/english/` holds the fastfiles: `code_post_gfx_mp.ff`, `localized_code_post_gfx_mp.ff`, `common_mp.ff`, `localized_common_mp.ff`, `ui_mp.ff`, and 21 MP maps each as `<map>.ff` + `<map>_load.ff` (`mp_backlot, mp_bloc, mp_bog, mp_broadcast, mp_carentan, mp_cargoship, mp_citystreets, mp_convoy, mp_countdown, mp_crash, mp_crash_snow, mp_creek, mp_crossfire, mp_farm, mp_killhouse, mp_overgrown, mp_pipeline, mp_shipment, mp_showdown, mp_strike, mp_vacant`). Plus SP zones. Every `.ff` header is `IWffu100` + version 5 (little-endian u32). **[V]**
  - `*_load.ff` are tiny (255–318 bytes compressed, ~920 bytes inflated). `mp_crash_load` inflates to two image/material records (names `loadscreen_mp_crash` and menu backdrops, DXT1 headers, **no pixel data**). The pixels are `images/loadscreen_mp_crash.iwi` in an IWD. **[V]**
  - The `zone/<language>/` directory name is read from line 1 of `COD4/localization.txt` (`english`) by a startup routine at `0x5761a0`, stored in a global (`0xcc147d4`). **[V]** (`zone` is **not** under `fs_basepath`; see §4.)
- `players/profiles/active.txt` (7 bytes, the profile name), `players/profiles/<name>/config_mp.cfg` (text), `players/profiles/<name>/mpdata` (8,476 bytes). **[V]**
- `mods/<mod>/…`, `usermaps/<map>/…` exist in this install (mods: `RoZo_0.5.021-1`, `PeZBOT`, `kh`, …). These are the user's, not stock content. A `cod4-client-manualinstall_21.1/` folder (CoD4X client) is also present but unused by the original engine.
- `main/` has **no** `config_mp.cfg`, `autoexec*.cfg` (no `autoexec` string exists in the exe), or `server.cfg`. Servers supply their own via `+exec`. **[V]**

## 2. Ordered boot sequence, process start to main menu / listen-server-ready

Entry: `Com_Init` at `0x4ff1c0` (prints `CoD4 MP 1.7 build 568 …`). Order below is the call order inside it, **[V]** unless marked.

1. Parse command line into tokens, then init memory, command buffer, command table (`0x4fd610`, `0x517f10`, `0x5719b0`, `0x4f8d40`, `0x4f9dd0`).
2. `Com_StartupVariable(NULL)` (`0x4fd740`). Executes only `set`/`seta` lines from the command line so that early cvars (`fs_*`, `dedicated`, `useFastFile`, `loc_language`) are set before use. All other `+cmd` items are held.
3. Register core dvars (`0x4fea80`): `dedicated`, `com_maxfps`, **`useFastFile`** (description: "Enables loading data from fast files. Only tools can run without"), `sys_lockThreads`, etc. **[V]**
4. If `useFastFile`: print `begin $init`, reserve a 128 MiB block (`VirtualAlloc 0x8000000`, then a time marker `$init`), create the **database (zone-loader) thread** (`0x50b480`; failure text `Failed to create database thread`).
5. `FS_InitFilesystem` (`0x55ebf0`):
   1. `Com_StartupVariable` for `fs_cdpath, fs_basepath, fs_homepath, fs_game, fs_copyfiles, fs_restrict, loc_language`.
   2. `loc_language` and `loc_*` dvars registered (`0x5386e0`).
   3. `FS_Startup("main")` (`0x55e670`). Search-path rules in §3.
   4. `0x538860` scans the new list for localized nodes and marks which of the 15 languages are available; falls back to the first available, and errors `No languages available because no localized assets were found` if none.
   5. `0x503e00`, then it checks `fileSysCheck.cfg` can be opened; else error `Couldn't load fileSysCheck.cfg. Make sure Call of Duty is run from the correct folder.` (`fileSysCheck.cfg` is an empty file in `iw_00`.) **[V]**
   6. Records `fs_basepath` and `fs_game` values as the "last started" copies (used by `FS_ConditionalRestart`).
6. Register `bind`/`unbind`, console, and stat commands (`0x467d20`, `0x476fc0`, `0x579f80`).
7. `Com_InitPlayerProfiles` (`0x4fae30`), registers `ui_playerProfileAlreadyChosen` and `com_playerProfile`. It reads `profiles/active.txt` through the FS (§5). Then:
   - **No profile selected** → `0x4ff130` with no profile cfg.
   - **Profile selected** → `0x4fac60`: set `com_playerProfile`, build `profiles/<name>/config_mp.cfg`, call `0x4ff130`.
8. `0x4ff130` queues `exec default_mp.cfg`, `exec language.cfg`, then (if any) `exec profiles/<name>/config_mp.cfg`; flushes the command buffer (`Cbuf_Execute`, `0x4f9280`); if the `safe` start-up flag is set (`0x4fd660`: `safe` / `dvar_restart` on the command line, or improper-quit detection) queues `exec safemode_mp.cfg` and flushes again. **[V]**
   - At this point **no zone is loaded**, so these `exec`s come from the FS (IWDs), see §6.
9. `Cbuf_Execute` again (`0x4f9280`), then `com_recommendedSet` check (`0x4fe930`): if not `1`, run the hardware-profile routine (`0x4fe440`) that picks a `configure_mp.csv` row by CPU GHz / system MB / GPU string and queues `exec configure_mp.cfg` (sets `r_*` defaults). `com_recommendedSet` is then set to 1. **[V]**
10. Net/sound/client init: Winsock init, `0x46d4e0` (client dvars), `0x46fd40` (`----- Client Initialization -----`), render-thread creation (`0x50b3a0`), sound (`0x5c6b90`).
11. **Initial zone load** (§7). Dedicated servers call `0x46cd60` (no `ui_mp`). Clients load via `R_Init` `0x46ccb0`.
12. `end $init %d ms`, `--- Common Initialization Complete ---`, then `Com_AddStartupCommands` (`0x4fd850`) runs the command-line `+exec server.cfg`, `+map …` items. Then the client enters the menu/`map` flow. A helper at `0x500200` (`{ui_mp, tag 8, free 0x68}`, gated on `useFastFile` only) reloads `ui_mp`; its callers are the init wrapper, disconnect/error cleanup and server restart paths (§7.1). **[V]**
13. Config writes happen from `writeconfig` (`0x4ffbb0`): requires the archive-dirty bit and a selected profile; writes `players/profiles/<name>/config_mp.cfg` (header line, `unbindall`, bind lines, `seta` archive cvars). **[V]** (§5)

### 2.1 `.cfg` execution order summary

| Order | Cfg | Source it is read from | Condition |
|---|---|---|---|
| 1 | `default_mp.cfg` (which `exec`s `default_mp_controls.cfg`, `default_mp_gamesettings.cfg`, `server_map.cfg`) | FS → `localized_english_iw00.iwd` (and `iw_00.iwd` for the three it pulls in) | always |
| 2 | `language.cfg` | FS (`iw_00.iwd`; **0 bytes**, stored uncompressed, a placeholder a mod can override) | always |
| 3 | `profiles/<name>/config_mp.cfg` | **Disk only** (see §6) via the `players/` game dir | profile selected |
| 4 | `safemode_mp.cfg` | FS (`iw_00.iwd`) | safe-mode flag |
| 5 | `configure_mp.cfg` | FS (`iw_00.iwd`) | `com_recommendedSet != 1` |
| 6 | command-line `+exec server.cfg`, `+set`, `+map` | disk / FS | after init |

`default_mp.cfg` itself is 17 lines and holds exactly three `exec` lines plus comments. **[V]** `default_mp.cfg`, `default_mp_controls.cfg`, `server_map.cfg` and `default_mp_gamesettings.cfg` also exist as rawfile assets in zone `localized_code_post_gfx_mp`. Later `exec`s prefer those (§6). `autoexec_dev_mp.cfg` in `iw_00` is a dev-build artifact; the shipping exe never references it. **[V]**

## 3. File system: search path construction and override rules

### 3.1 Cvars (registered at `0x55e390`) **[V]**

| cvar | default / notes |
|---|---|
| `fs_basepath` | current working directory (`_getcwd` at register time). **Not** the exe directory. |
| `fs_homepath` | same as `fs_basepath` unless overridden |
| `fs_cdpath` | "" |
| `fs_basegame` | "" |
| `fs_game` | "" ; validated by `0x55e230` (see below) |
| `fs_usedevdir` | off (dev-only `devraw*`/`raw*` directories) |
| `fs_restrict`, `fs_copyfiles`, `fs_debug`, `fs_ignoreLocalized`, `loc_language` (0 = `english`), `loc_forceEnglish`, `loc_translate` | |

`fs_game` validator (`0x55e230`): value must be empty, or **start with `mods`** followed by `/` or `\` (length ≥ 6), and must not contain `..` or `::`. Else `ERROR: Invalid server value '%s' for 'fs_game'`. On change the value is lower-cased and `\` converted to `/` (`0x55e2f0`).

### 3.2 `FS_Startup("main")` order **[V]**

`AddGameDirectory(path, dir)` prepends to a single linked list. **Later additions are searched first.** Calls in order:

1. `fs_basepath`: *(if `fs_usedevdir`: `devraw_shared`, `devraw`, `raw_shared`, `raw`)*, then **`players`**.
2. `fs_homepath` (if ≠ basepath): *(dev dirs, only if `fs_usedevdir`)*.
3. `fs_cdpath` (if set and ≠ basepath): *(dev dirs)*, then `main`.
4. `fs_basepath`: `main_shared`, `main`.
5. `fs_homepath` (if ≠ basepath): `main_shared`, `main`.
6. If `fs_basegame` set and ≠ `main`: `cdpath/basegame`, `basepath/basegame`, `homepath/basegame`.
7. If `fs_game` set and ≠ `main`: `cdpath/fs_game`, `basepath/fs_game`, `homepath/fs_game`.
8. Then `0x4fde70`, `0x503c70`, and `FS_Path_f`-style dump (`0x55d510`), prints `%d files in iwd files`.

Highest priority first, therefore: `homepath/<fs_game>` > `basepath/<fs_game>` > `cdpath/<fs_game>` > `homepath/<basegame>` … > `homepath/main` > `basepath/main` > `basepath/main_shared` > `cdpath/main` > `basepath/players`.

Each `AddGameDirectory` (`0x55e020`) expands to **16 calls** of the real worker (`0x55dd80`): 15 times with `localized=1` for subfolder `<dir>/<language>` for every language in the table below, then once with `localized=0` for `<dir>` itself.

15-language table (`0x724710`): `english french german italian spanish british russian polish korean taiwanese japanese chinese thai leet czech`. **[V]**

### 3.3 Inside one game directory **[V]**

`0x55dd80` creates a directory node, inserts it, then calls `0x55d8b0` to scan `<path>/<dir>/*.iwd`:

1. List `.iwd` files, cap **1024** (`0x400`). Warning `Exceeded max number of iwd files` beyond that.
2. Names starting `localized_` have their 10-byte prefix temporarily replaced by 10 spaces so they sort **before** all other names. Comparator (`0x55d7b0`): case-insensitive (`A-Z` folded to upper), `\` and `:` folded to `/`, plain byte order (`0x55d3f0`). Among two localized names, a language of `english` sorts first (so **non-English localized IWDs end up higher priority than English ones**), ties fall to the normal compare.
3. `localized_<language>_iwd#.iwd` must name a language in the table, else `WARNING: Localized assets iwd file … has invalid name` (and the supported-languages list is printed once). The language index is stored on the node.
4. **Basepath `main/` restriction:** when the directory is `main` **and** the path equals `fs_basepath`, every non-localized IWD whose name does not start `iw_` is **rejected** with `WARNING: Invalid IWD %s in \main.` (so `zz_*.iwd` in `basepath/main` is ignored, but accepted in `homepath/main` if it differs). If `main` has no IWDs at all in basepath: error `No IWD files found in /main`. **[V]** (`0x55d956`–`0x55d9dc`).
5. Each IWD becomes a node. **Non-localized IWDs are prepended to the list head** (after the directory node, so they come before the loose-file dir node). **Localized IWD/folder nodes are inserted before the first existing localized node**, which means they sit **behind every non-localized node**.

So the final list is: *[all non-localized nodes, last-added first; inside each directory: IWDs in reverse sorted order, then loose files]* then *[all localized nodes, by the same rule]*. I.e. a localized IWD only wins over a non-localized one if no non-localized node has the file. **[V]** Example in this install: `images/specialty_new.iwi` exists in `iw_13` and `localized_english_iw00` (bit-identical, CRC equal), so the choice is invisible there; `iw_13` is found first. **[I]** that is intended.

**Lookup** (`FS_FOpenFileRead`, `0x55b960`): walk the list from the head. A localized node is skipped if `fs_ignoreLocalized` is set or its language index ≠ current `loc_language`. For an IWD node, hash-lookup (case-insensitive, `\`→`/`); for a directory node, `fopen("<path>/<dir>/<qpath>", "rb")`. First hit wins. **[V]**

**Same name inside one IWD:** the zip central-directory is read in order and each entry is **prepended** to its hash bucket (`0x55ca46`–`0x55ca62`), so the **last** entry for a name in the central directory wins. Stock `iw_00` has 10 `.cfg` names that appear twice (`avatar_dev, chad, createfx, default_mp_gamesettings, jake, jiesang, massey, robotg, roger, test`); the later copy is used. E.g. `default_mp_gamesettings.cfg` has two copies differing in 3 lines. **[V]** (listing + disassembly).

**Across stock IWDs**, only 4 names collide (15 raw collisions incl. the in-IWD ones): `default_filter.cfg` (`iw_00`, `iw_13`), `images/compass_map_mp_broadcast.iwi` and `images/loadscreen_mp_broadcast.iwi` (`iw_01`, `iw_13`), plus the two `localized_english_iw00` image copies above. `iw_13` (Jun 2008, 1.7 patch) therefore overrides `iw_00`/`iw_01`: this is exactly what the patch intends. **[V]**

**Mod IWDs** (`mods/<mod>/*.iwd`) are not subject to the `iw_` filter; the same sort applies, so `zz_images.iwd` beats `z_svr_rozo_maps.iwd` beats `<mod>.iwd`. **[V]** by code; mod IWD names observed in `mods/RoZo_0.5.021-1`: `zz_images, zz_radio, zz_sounds, zz_weapons, z_svr_rozo_maps`.

### 3.4 Differences from CoD4X (reference-only)

CoD4X's `FS_Startup` (AGPL, fact source only) gives the same core order (`basepath/players`, `cdpath/main`, `basepath/main_shared|main`, `homepath/main_shared|main`, then basegame, then `fs_game`), re-adds `basepath/main_shared` (a no-op through the duplicate check), and adds `usermaps/<mapname>` (keyed on the `mapname` cvar) **inside `FS_Startup` before the `fs_game` directories**, i.e. at *lower* priority than the mod. The **original** adds usermaps dynamically at map load via `0x55dd30` (§3.5), **after** `fs_game`, i.e. at the **highest** priority. The stock engine is the reference here, not CoD4X.

### 3.5 `usermaps/<map>` and `mods/<mod>/mod.ff`

- `usermaps/<mapname>/*.iwd` is added by `0x55dd30` as a **dynamic extra search-path** with path `"."` (current directory, not `fs_basepath`) and dir `usermaps/<map>`. The caller (`0x55dd30` ← `0x45be80`, `0x46a800`, `0x52f210`) only does it when **`fs_game` is non-empty** *and* `usermaps/<map>/<map>.ff` exists (check `0x48b9b0(map, 2)`). Added last, so **its IWDs outrank everything**, including `fs_game` IWDs. Every `FS_Restart` (each map load) wipes and rebuilds the list, and the usermaps dir is re-added afterward. **[V]** The install's `usermaps/mp_c4s_minecraft/` has only `*.ff.tmp` (not loadable as-is) and `zz_minecraft_textures.iwd` (35 `images/*.iwi`).
- `mods/<mod>/mod.ff` is a **fastfile** in the mod folder. It is a zone named `mod`, resolved as `<exe dir>\<fs_game>\mod.ff` (§4). Observed: `mods/RoZo…/mod.ff` (24.7 MB), `mods/PeZBOT/mod.ff` (1.5 MB), `mods/kh/mod.ff` (7.5 KB), all `IWffu100` v5. **[V]**

## 4. Zone (fastfile) path resolution (`DB_FindFile`, `0x48a8f0`; builders `0x489f20`, `0x48a7a0`) **[V]**

Name → file, tried in this order:

1. Name `mp_patch`: `update:\mp_patch.ff`, else `<exe dir>\zone\<lang>\mp_patch.ff`. (`mp_patch` is not shipped in 1.7; irrelevant.)
2. If `fs_game` is non-empty:
   - Name `mod` → `<exe dir>\<fs_game>\mod.ff`.
   - Other name: if the **stock** `<exe dir>\zone\<lang>\<name>.ff` exists → skip (stock wins); else `<exe dir>\usermaps\<name-without-_load>\<name>.ff`.
3. Stock: `<exe dir>\zone\<lang>\<name>.ff`, where `<exe dir>` = directory of the running exe (`0x572ce0`, from `GetModuleFileName`) and `<lang>` = `localization.txt` line 1 (`english`).
   - Not found and name ends `_load` → `WARNING: Could not find zone '%s'` (soft; the load screen is optional).
   - Not found otherwise → `ERROR: Could not find zone '%s'` (fatal).

Header check (messages seen in `0x2d3328…`): magic/version `IWffu100` v5; "out of date (version %d, expecting %d)" / "newer than client executable". The zlib stream follows the 12-byte header. (Container details belong to ticket #2.)

Note: zones resolve from the **exe directory**, IWDs/cfgs from **fs_basepath (cwd)**. They differ if the exe is launched from another cwd; a re-implementation can treat both as "the install root".

## 5. `players/`, profiles, `servercache.dat`, `hunkusage.dat`

### 5.1 `players/` **[V]**

- `FS_Startup` always adds `fs_basepath` + `players` as a read search dir (even when homepath differs). Profile listing (`profiles` dir enumerated through the FS, `0x4fabb0`), `profiles/active.txt` (read via FS, `0x4fadc0`; written by `0x4fac60` path), `profiles/<name>/config_mp.cfg` and `profiles/<name>/mpdata` are **relative paths under `players/`**.
- **Writes** go through `FS_BuildOSPath(fs_homepath, "players", qpath)` (`0x55b4d0`, `0x4ffabe`, `0x579bb0`). The write target is `<fs_homepath>/players/profiles/<name>/…`.
- Quirk **[I]**: when `fs_homepath ≠ fs_basepath`, `players/` is read only from `basepath` but written under `homepath`. Not exercised here.
- `config_mp.cfg` is the only cfg `exec` refuses to take from a zone: `Cmd_Exec` (`0x4f9470`) tests the basename and goes straight to disk. Written format: a header comment, `unbindall`, one `bind` line per key, then `seta <cvar> "<value>"` for every dvar with the `ARCHIVE` flag (observed in the install's 11.9 KB file). **[V]**
- `mpdata` is a binary stats blob: 8,476 bytes = `0x211c`. First `0x2000` bytes are the stats buffer (a 4-byte checksum, then `0x1ffc` bytes; compared at load; mismatch → reset with `MENU_RESETCUSTOMCLASSES` notice), then `0x11c` bytes of trailer. Out-of-scope for byte-exact reproduction: the project only needs *a* persistent stats store. **[V]** sizes from code; field semantics **[I]**.
- `active.txt` = bare profile name, no newline (`Lagahoo`, 7 bytes). **[V]**

### 5.2 `servercache.dat` **[V]**

Server-browser cache, read/written by `0x4764d0` / `0x476540` through the FS. Layout (little-endian): `u32 version=3`, `u32 count`, `u32 count2`, `u32 payloadSize=0x2fe980`, then **20,000 slots × 156 bytes** (`0x9c`; `0x2f9b80` bytes) of server records (observed strings: server hostname, `mp_*` map, gametype), then `0x4e00` bytes of a second table. File size 3,139,984 = 16 + 0x2fe980. This is master-server browser state. **Not needed** for the project (network compatibility is out of scope).

### 5.3 `hunkusage.dat` **[V]**

Plain text, one record per line: `maps/<name>.d3dbsp <bytes>` (observed: 21 lines, **SP map names** like `maps/killhouse.d3dbsp 45192`; CRLF at the end). Used only when **`useFastFile` is 0** (loose-BSP path; the `maps/mp/%s.d3dbsp` caller at `0x52f4e7` is behind `if !useFastFile`): `0x52eec0` looks up the BSP name and stores the number (`0x1435d38`) as the expected hunk size for the progress bar. Written by the dev command `updatehunkusage` (`0x45bab0`, lines `"%s %i\n"`). **Irrelevant for a fastfile-based reimplementation.** The stock file is stale (SP only).

## 6. How loose files override or supplement zone assets **[V unless noted]**

Two **independent** mechanisms.

### 6.1 Zone-to-zone asset override (`0x489b00`)

- Assets live in a hash keyed by (type, name). Each zone records a numeric **tag** (the `allocFlags` passed when loaded). On a name clash, the asset whose zone has the **higher tag wins; equal tag → the later-loaded zone wins**; the loser is kept as a shadow in a chain.
- Error `Attempting to override asset '%s' from zone '%s' with zone '%s'` (ERR_DROP) is raised for most asset types, but **not** for rawfile (type `0x1f`) or type `0xf`. **[I]** for the exact set (the check is "type has no registered name-string or is 0x1f/0xf"); to confirm in #10.
- Zone tags (alloc flag values observed): `localized_common_mp=1`, `code_post_gfx_mp=2`, `common_mp=4`, `ui_mp=8`, `<map>=8`, `mod=0x10`, `<map>_load=0x20`, a `0x40`-tag zone loaded in dev builds (`0xe62a40`). So **`mod.ff` (0x10) overrides anything in stock zones**. `localized_code_post_gfx_mp` has tag 0 (its cfg/rawfiles have no clashing names).

### 6.2 IWD content that fastfiles reference by name (data lives in IWDs, not zones)

- **Images**: zone image assets hold the name plus a DXT/format header; the **pixels are loaded at runtime from `images/<name>.iwi`** through the FS (`0x571670` builds `images/%s.iwi`, 63-char cap, `0x64238e` caller). Evidence: `mp_crash_load.ff` carries `loadscreen_mp_crash` (DXT1 header, no pixels) and `images/loadscreen_mp_crash.iwi` is an IWD entry; of 6,561 IWD image names, 882 appear as asset names inside `mp_crash.ff` alone. So **a mod/usermap IWD with the same `images/x.iwi` replaces the pixels** of that zone image (FS order, §3). New images need a zone asset naming them.
- **Sounds**: zone sound-alias records hold the wave filename (e.g. `mortar_dirt01.wav`, 163 `.wav` refs in `mp_crash.ff`), and the bytes come from `sound/<path>` in IWDs (`sound/%s/%s`, `sound/%s`; `iw_06…iw_11` and `localized_english_iw0*`). **[V]** for name/refs, **[I]** for exact per-asset loader (belongs to the audio ticket).
- **Weapons**: weapon defs are loose text files `weapons/<mp|sp>/<name>` (extension-less; `"WEAPONFILE"` header string at `0x6c5214`, loader at `0x41d270`). `iw_11` holds 128 `weapons/mp/*`; `mods/RoZo…/zz_weapons.iwd` holds 121 more. **[V]** that they are IWD entries and that a loader reads `weapons/%s/%s` via FS; **[I]** that this path is the one that supplies runtime weapon data in fastfile mode (zones carry only name references: `ak47_mp` appears inside `common_mp` only as part of asset names, `WEAPONFILE` string appears nowhere in the zones).
- **`.accu`** accuracy files: `accuracy/%s/%s` through FS.
- **`mp/playeranim.script`, `mp/playeranimtypes.txt`, `mp/*.csv`**: stringtables and anim scripts in zones (`common_mp` has 1,152 refs to `mp/statstable.csv`; `ui_mp` has `mp/*Table*.csv`). They are **zone rawfiles/stringtables**; mods that ship them in an IWD (e.g. PeZBOT's `mp/playeranim.script`) take effect only on code paths that read them via FS. **[I]**.
- **GSC scripts** (`maps/mp/gametypes/*.gsc`, `maps/mp/mp_<map>.gsc`) are **rawfile assets in zones** (`common_mp` and each map zone), not IWD entries. A mod overrides them with its own `mod.ff` (tag 0x10) or, for `usermaps`, the map zone. The RoZo IWD `z_svr_rozo_maps.iwd` ships `maps/…/*.gsc` in an IWD, which works only if the engine's script loader also consults the FS; whether it does is **unverified** (see unknowns).

### 6.3 `exec` of a cfg: fastfile first, then disk (`Cmd_Exec`, `0x4f9470`)

1. Append `.cfg` if missing; normalise.
2. If the basename is `config_mp.cfg` → **disk (FS) only**.
3. Else if `useFastFile` **and** the "cfg rawfiles are ready" flag `0xd5ec423` is set (set when zone `localized_code_post_gfx_mp` finishes loading, `0x478147`) → look up rawfile (type `0x1f`) by name across loaded zones → print `execing %s from fastfile`.
4. Else FS lookup (IWDs/dirs) → `execing %s from disk`.
5. Else `couldn't exec %s`.

Consequences:
- Boot-time execs (§2 steps 7–9) happen **before** the flag is set, so they read **IWD** copies. After the zone is loaded, later `exec default_mp.cfg` etc. use the **zone** rawfile copies.
- `default_mp.cfg` and `default_mp_controls.cfg` in `localized_code_post_gfx_mp`/`localized_english_iw00` are byte-identical (verified). `default_mp_gamesettings.cfg` and `server_map.cfg` are **not** identical between the zone and `iw_00` (zone copy not located by its header text; differences uncharacterised). A re-implementation must decide which copy is authoritative for each phase.
- Zone rawfile cfgs: `code_post_gfx_mp` has `default_mp_gamesettings.cfg`, `devgui_*.cfg`, `hardcore_*.cfg`, `oldschool_*.cfg`, `options_graphics*.cfg`, `ragdoll.cfg`; `ui_mp`/`common_mp` have `default*.cfg`, `options_graphics*.cfg`, `createfx.cfg`, etc.; `localized_code_post_gfx_mp` has the four `default_mp*/server_map` cfgs. Gametype scripts (`hardcore_settings.cfg` etc.) are `exec`ed from zone rawfiles, **not** from IWDs. **[V]** names; **[I]** consumers.

## 7. Zone loading API and exact zone sequences

`DB_LoadXAssets(XZoneInfo* zones, count, sync)` at `0x48a2b0`; `XZoneInfo = { name, allocFlags, freeFlags }` (12 bytes). Behaviour **[V]**:

1. Mark every loaded zone whose `tag & freeFlags != 0` and unload them (free-bit order `0x40, 0x20, 0x10, 0x8, 0x4, 0x2, 0x1`, iterating zones newest-first). `freeFlags = 0` frees nothing.
2. Load the listed fastfiles with `allocFlags` as their tag, in listed order (database thread; `sync=0` is followed by an explicit wait: `0x5f78a0`/`0x5f78f0`/`0x48a120`).
3. Max 32 zones loaded (`Max zone count exceeded`).

### 7.1 Startup zones (`0x5f3c00`, config built by `0x46ccb0` client / `0x46cd60` dedicated) **[V]**

Call A (all names `\<exe dir>\zone\<lang>\…` per §4):

| # | Zone | alloc tag | free |
|---|---|---|---|
| 1 | `code_post_gfx_mp` | 2 | 0 |
| 2 | `localized_code_post_gfx_mp` | 0 | 0 |
| 3 | `mod` (only if `fs_game` set and `<exe dir>\<fs_game>\mod.ff` exists, `0x48ba10`) | 0x10 | 0 |

then **wait for load to finish**, then Call B:

| # | Zone | alloc tag | free |
|---|---|---|---|
| 4 | `ui_mp` (**omitted when `dedicated`**) | 8 | 0 |
| 5 | `common_mp` | 4 | 0 |
| 6 | `localized_common_mp` | 1 | 0 |

So the effective **load order** is: `code_post_gfx_mp` → `localized_code_post_gfx_mp` → [`mod`] → `ui_mp` → `common_mp` → `localized_common_mp`. This matches the order guessed in the ticket; the additions are the `mod` zone (between the first group and `ui_mp`), the synchronisation point between the two groups, and the tag/free-mask system.

`ui_mp` is also (re)loaded by `0x500200` (`{ui_mp, tag 8, free 0x68}`) from `0x4fce00` (error/disconnect cleanup, after `0x46caf0`), `0x5335d0` (`EXE_SERVERKILLED`), `0x533210` (server restart: it then queues `map <name>`) and `0x500290` (an SEH-wrapped init entry; no static caller found). Freeing `0x68` = tags `0x40|0x20|0x8` = the last map, any `_load` zone, and the old `ui_mp`. **[V]** flags and callers; **[I]** reasoning. Note `0x500200` is gated by `useFastFile` only, **not** by `dedicated`, so a dedicated server may load `ui_mp` on those cleanup paths (unverified; see §10).

### 7.2 Map load, server side (`SV_SpawnServer`, `0x52f210`) **[V]**

1. Wait for the DB thread (`0x500740`).
2. **Non-dedicated only:** load `<map>_load` as `{name, alloc 0x20, free 0x60}` (frees the previous `_load` and previous-map tags `0x40`/`0x20`; **`0x60` does not include tag 8**, so the previous map stays until step 6).
3. Update `loadingnewmap\n<map>\n<gametype>` UI text to connected clients; set `mapname`.
4. Print `------ Server Initialization ------`; init game systems.
5. **`FS_Restart`** (`0x55ed10`): shut the FS down, `FS_Startup("main")` again, re-check `default_mp.cfg` exists. This **re-scans all IWDs**, so new/removed IWDs appear at each map load. If the previous config came from a different `fs_game`, `config_mp.cfg` is `exec`ed again.
6. If `fs_game` is set and `usermaps/<map>/<map>.ff` exists → add `usermaps/<map>` IWD dir (§3.5).
7. Load the map zone `{<map>, alloc 8, free 8}`. `free 8` frees **the previous map and `ui_mp` (both tag 8)**, then loads the new `<map>.ff`. (Dedicated servers never had `ui_mp`.) **[V]**
8. If `!useFastFile`: `maps/mp/<map>.d3dbsp` + `maps/mp/<map>.csv` + `hunkusage.dat` lookup instead (dev path). **[V]**
9. Spawn game script, `map_restart` etc. (not this ticket).

### 7.3 Map load, client side

- **Joining a server / server changing map** (`0x470580`, `0x46aa88` area): `Server changing map %s, gametype %s` → set `mapname`/`g_gametype` (`0x5442f0`, which also records whether a `_load` zone exists) → `0x46a800`: add usermaps dir (if applicable), load `<map>_load` `{0x20, 0x60}` → `0x45bef0`/`0x45be80`: add usermaps dir again if applicable, load `<map>` `{8, 8}`.
- **Download complete** (`CL_DownloadsComplete`, `0x46a925`): `FS_ConditionalRestart(checksumFeed)` (`0x55ed10`) + `vid_restart`-style renderer reinit (`0x46a180`) → `donedl` → state `CA_LOADING`.
- **`vid_restart`** (`0x46a180`): `FS_ConditionalRestart` if `fs_game` changed, `R_Init` (reloads the §7.1 list; `ui_mp` is retained because its `free` is 0), then re-loads the current map's `_load` + map zone if in-game (`0x46a49c`).

## 8. What is loaded vs. unloaded at each step (summary)

| Event | Loaded | Unloaded |
|---|---|---|
| Boot, client | `code_post_gfx_mp`, `localized_code_post_gfx_mp`, [`mod`], `ui_mp`, `common_mp`, `localized_common_mp` | none |
| Boot, dedicated | same, **without `ui_mp`** | none |
| Map load, listen server / client | `<map>_load` (tag 0x20), then `<map>` (tag 8) | previous `_load`/tag 0x40/0x20; then tag 8 = previous map **and `ui_mp`** |
| Map load, dedicated | `<map>` (no `_load`, no `ui_mp`) | previous `<map>` (tag 8) |
| Return to menu / disconnect (client) | `ui_mp` | tags `0x68` = map, `_load`, old `ui_mp` |
| `vid_restart` | renderer list (§7.1) | none |

The tags `1,2,4,0x10` (the common/code/mod zones) are **never freed** by map loads: their `free` masks are 0 and no map's mask includes them. **[V]**

## 9. Implications for the Rust engine (decisions the facts support)

1. **Model the FS as an ordered node list**: dirs and IWD nodes, localized flag + language per node, first-hit-wins. Re-implement the ordering exactly (§3). It is small and the stock content relies on it (`iw_13` vs `iw_00`/`iw_01`).
2. **Rescan on every map load** (`FS_Restart`); IWDs added between maps are seen.
3. **Keep `iw_*`-only enforcement in basepath `main`** only if the project wants stock behavior; it affects `zz_` patches placed in `main/` (they are ignored). Mods go in `mods/<name>/` instead. Decision for the map.
4. **Zone loader**: honor `{name, allocTag, freeMask}` semantics (§7), including `mod.ff` tag 0x10 as the override layer and the "equal tag → later wins" rule. `ui_mp` memory reuse (free at map load) is an optimization, not a requirement for correctness.
5. **Resolve `zone/<lang>/` from `localization.txt` line 1 and the install root**, not from `fs_basepath`.
6. **Treat images, sounds, weapon defs as IWD-backed names** referenced from zones (§6.2); route loose IWD overrides through the same FS list.
7. `servercache.dat` and `hunkusage.dat` are skippable. `players/` needs only `active.txt`, `config_mp.cfg`, and some stats store.

## 10. Open unknowns (for tickets/fog)

1. **Asset-type enumeration and override-error exemptions** (§6.1): which types are `0x1f`/`0xf`, and exactly which other types may legitimately be overridden across zones. Belongs with the fastfile/asset ticket (#2/#10).
2. **Do GSC scripts from IWDs ever take effect?** Mods ship both `mod.ff` and IWD `maps/**/*.gsc` (`z_svr_rozo_maps.iwd` has 302 entries under `maps/`, PeZBOT has `maps/`). Whether the script/rawfile loader (`DB_FindXAssetHeader`/`Scr_LoadScript`) falls back to the FS for rawfiles is unverified. Needs a trace of the rawfile lookup (`0x489570` callers).
3. **Which weapon/`.accu` loader runs at runtime in fastfile mode** (`0x41d270` callers), and whether weapon assets in `common_mp` are authoritative over the loose `weapons/mp/*`. Same for `mp/*.csv` and `playeranim.script`.
4. **Byte differences** between rawfile cfgs in `localized_code_post_gfx_mp` and the IWD copies for `default_mp_gamesettings.cfg` and `server_map.cfg` (§6.3).
5. **Exact meaning of tag `0x40`** (zone at `0xe62a40`, dev builds), and the `mp_patch`/`update:` path (not shipped).
6. **`sv_pure` / `sv_iwds` / `sv_iwdNames`** (referenced strings `0x6d2814`) and the `fs_restrict` demo path: not traced. Network compatibility is out of scope, but a pure-server check may constrain which IWDs a server loads.
7. **Loading screen**: how `loadscreen_<map>` and the `<map>_load` zone are drawn (`0x5412b5` builds `loadscreen_%s`); this belongs with the renderer/UI work.
8. **Platform quirks** for re-implementation on macOS/Linux: case-insensitive FS lookups for `main/` IWD names and `\` vs `/` in zone paths. The original is Windows-only and builds `\`-separated paths for zones.
9. **Does a headless server load `ui_mp`?** `0x46cd60` omits it at boot, but `0x500200` (disconnect/restart/killed paths) has no `dedicated` check. Needs a run or a trace of its callers under `dedicated=2`.
10. **CWD vs exe directory**: confirm with a run that `fs_basepath` really defaults to CWD (inferred from `0x55e45b`–`0x55e46a`, a `_getcwd`-style call with a 255-byte buffer).
