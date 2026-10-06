# GSC: what a VM must support to run stock MP gametypes

Ticket: jeiang/cod4-decomp#5. Scope: stock multiplayer scripts in the original install (CoD4 1.7). All counts below were produced by a throwaway tokenizer run over scripts extracted to `/tmp`; no script source is stored in this repo.

Legend: **[V]** verified against the real files or `iw3mp.exe`; **[I]** inference or prior knowledge, not verified here.

## 1. Storage

- **[V] GSC ships as plain source text**, not bytecode. Each script is a `rawfile` asset inside a fastfile (`zone/english/*.ff`). There are no `.gsc` files in any IWD (`main/*.iwd`: 0 of 21 contain one).
- **[V] Rawfile layout** in the decompressed zone stream (zlib from offset 12 of the `IWffu100` file): `u32 0xFFFFFFFF` (name pointer placeholder), `u32 length`, `u32 0xFFFFFFFF` (data pointer placeholder), then the NUL-terminated name (for example `maps/mp/gametypes/_callbacksetup.gsc`), then `length` bytes of text, then one `0x00`. Text uses CRLF line endings. Tab-indented, comments intact.
- **[V]** Across all MP zones (`common_mp.ff` plus `mp_*.ff`) the pattern yields 210 distinct `.gsc` rawfiles, with 0 length/terminator mismatches and no name whose content differs between zones. 208 appear in exactly one zone and 2 appear in 9 zones. (Over all 100+ zones, 823 `.gsc` names exist, but most are single-player scripts from `common.ff` and SP mission zones; they are out of scope.)
- **[V] Split of the 210 MP scripts**: `common_mp.ff` holds 124 (gametypes, `maps/mp/_*.gsc`, `common_scripts/*`, `codescripts/*`); the rest are per-map zones: `maps/mp/mp_<map>.gsc` and `maps/mp/mp_<map>_fx.gsc` (21 stock maps = 42 files), `maps/createart/mp_<map>_art.gsc`, `maps/createfx/mp_<map>_fx.gsc`, plus `mptype/*` and `character/*` (data-like scripts that define character/model assignments).
- **[V] Other rawfile-type assets next to scripts**: `animtrees/multiplayer.atr`, `animtrees/vehicles.atr` (referenced by `#using_animtree`), `mp/playeranimtypes.txt`, rumble `*.rmb`, `.cfg`. Stringtables are read via `tablelookup` from `mp/*.csv` (see section 4).
- **[V] Stock layout** (102 files under `maps/mp/`): 6 stock gametype files `dm dom koth sab sd war` plus 34 `_*.gsc` support modules in `maps/mp/gametypes/` (`_globallogic`, `_callbacksetup`, `_gameobjects`, `_spawnlogic`, `_weapons`, `_class`, `_rank`, `_persistence`, `_hardpoints`, `_killcam`, `_hud*`, `_teams`, ...); 20 system modules in `maps/mp/` (`_load`, `_utility`, `_fx`, `_createfx`, `_destructible*`, `_helicopter`, `_ac130`, `_compass`, `_art`, `_flashgrenades`, ...); `common_scripts/utility.gsc`, `struct.gsc`, `character*.gsc`, `delete.gsc`; `codescripts/*`.
- **[V] Engine-side path strings** in `iw3mp.exe`: `maps/mp/gametypes/%s`, `maps/mp/%s`, `maps/mp/%s_fx.gsc`, `maps/mp/gametypes/_callbacksetup`, `maps/mp/gametypes/_gametypes.txt`, `animtrees/%s.atr`. So the engine compiles source at load time and looks up scripts by path; the VM implementation needs a lexer/parser/compiler (or tree-walking interpreter) over text, which suits a Rust project.
- **[V]** Engine entry points: `CodeCallback_StartGameType`, `CodeCallback_PlayerConnect`, `CodeCallback_PlayerDisconnect`, `CodeCallback_PlayerDamage`, `CodeCallback_PlayerKilled`, `CodeCallback_PlayerLastStand` (all six names exist in `iw3mp.exe` and are defined once in `_callbacksetup.gsc`, which assigns them to `level.callbackStartGameType`, `level.callbackPlayerConnect`, and so on). Map main is `main()` in `maps/mp/<map>.gsc`, which calls `maps\mp\_load::main()`. Gametype `main()` is called through `maps/mp/gametypes/<gt>`. [I] The engine calls `main` of the map script and the gametype script early in level start (the call order was not decompiled).

## 2. Language (as used by the stock MP scripts)

Token-level counts over the 210 MP scripts (comments and strings stripped by the tokenizer):

| feature | count | notes |
|---|---:|---|
| functions defined (top-level `name(params) {…}`) | 1196 distinct names (lowercased) | no `function` keyword, no return types, no default params; names case-insensitive [I] (scripts mix cases for one function) |
| calls to script-defined functions | 5548 (2655 path-qualified) | `maps\mp\_utility::name(...)` file-qualified calls are the main cross-file mechanism |
| `self` | 4988 | the entity/object the function runs on |
| `level` / `game` | 4245 / 879 | `level` = per-map global struct; `game` = global array that persists across `map_restart` [I] |
| `if/else` | 3324 / 813 | |
| `for` / `while` | 618 / 166 | C style `for(;;)`; `while(1)` loops rely on `wait` |
| `switch/case/default` | 44 / 252 / 31 | string and int cases |
| `break` / `continue` / `return` | 284 / 240 / 878 | |
| `thread` / `childthread` | 667 tokens (of which 191 bare `thread f()`, 432 `obj thread f()`) / 0 | |
| `wait` | 399 (168 with `wait(x)`, 181 `wait 0.05`, 50 `wait var`) | |
| `waittill` | 178 | 62 distinct event names; arg counts: 1 arg ×116, 2 ×43, 3 ×11, 4 ×1, 6 ×5, 10 ×2 (extra args receive notify payload) |
| `waittillmatch` | 3 | |
| `waittillframeend` | 18 | |
| `notify` | 205 | 135 distinct names; payload counts: 1 arg ×192, 2 ×6, 3 ×2, 8 ×5 |
| `endon` | 436 | top: `disconnect` 132, `death` 98, `game_ended` 46 |
| `undefined` / `true` / `false` | 659 / 503 / 452 | |
| vector literal `(x,y,z)` | ~3196 | |
| array index `[ "str" ]` / `[ int ]` / `[]` append | 9681 / 667 / 326 | arrays are associative: string keys are the norm |
| `.size` | 847 | |
| `[[ ptr ]]( … )` call through pointer | 138 | |
| `::name` function pointer / reference | 124 bare refs (plus 2847 `::` tokens, most path-qualified calls) | |
| `&"LOC_REF"` localized string literal | 331 | |
| `%animname` | 6 uses (1 distinct) | |
| `#include` | 71 | targets: `maps\mp\_utility` 28, `common_scripts\utility` 20, `maps\mp\gametypes\_hud_util` 19, `maps\mp\_createfx` 2, `maps\mp\_createFxMenu` 1, `maps\mp\_destructible` 1 |
| `#using_animtree` / `#animtree` | 3 / 6 | engine error strings show it is required before `%anim` refs and must not sit inside a `/# #/` block |
| `/# … #/` developer blocks | 95 | [I] stripped when `developer` is 0; the error string "cannot put #using_animtree inside /# ... #/ comment" in the exe confirms the construct exists |
| `++` / `--` / `&&` / `||` | 624 / 13 / 595 / 259 | |
| `%` / `&` / `|` / `<<` | 10 / 7 / 6 / 4 | bit ops are rare but present |
| `foreach`, `?:`, `in`, `goto`, `do…while`, `@` | 0 | not used by stock MP scripts [V]; a minimal VM need not implement them |

Conclusions for the grammar [V from corpus]: C-like statements; `thread`/`childthread` prefixes on calls (`thread f()`, `obj thread f()`, `obj thread path::f()`); object-call syntax `<expr> func(args)` for methods, with no dot; `::` for pointers/qualified calls; `[[ expr ]]` for dynamic calls (also `obj [[ ptr ]](args)` and `thread [[ ptr ]](args)`); `wait n;` without parentheses is idiomatic. Whitespace between a `(`-less `wait`/`waittill` and its operand varies. Paths use backslashes: `maps\mp\_utility::fn`.

Types observed [V]: int, float, string, localized string (`&"…"`), vector `(x,y,z)`, undefined, entity, array (associative, dynamic, string/int keys, `.size`, `[]=` append and `getarraykeys`), struct (`spawnstruct()` then free `.field` assignment, 70 calls), function pointer (`::fn`, `path::fn`), animation reference (`%name`), hud elements and other engine objects. Booleans are ints (`true`/`false` are keywords); [I] truthiness follows int/float != 0.

## 3. VM semantics (what the scripts rely on)

Verified from script corpus and engine strings:

- **[V corpus, I semantics] Threads and events**: `thread f()` starts a coroutine that runs immediately until its first `wait`/`waittill*` and then yields; the caller continues. `obj waittill("name", a, b, …)` blocks on an event on `obj`, assigning notify payload to the listed variables (up to 10 seen). `obj notify("name", args…)` wakes all waiters. `self endon("name")` registers: if that event is later notified on `self`, the thread terminates (436 uses: nearly every long-lived thread starts with `self endon("disconnect")`). The exe errors "first parameter of waittill/notify/endon must evaluate to a string" show names are strings, validated at runtime. The exe prints "count: %d, var usage: %d, endon usage: %d", indicating endon registrations are tracked per thread.
- **[V] Time**: `wait <seconds>`; `wait 0.05` ≈ one 20 Hz server frame in the original engine ([I] frame length `sv_fps` default 20; minimum wait one frame). `waittillframeend` resumes at the end of the frame (18 uses). `gettime()` returns ms (98 uses).
- **[V] Runaway protection**: exe strings "potential infinite loop in script" (runtime warning, then "killing thread") and "script stack overflow (too many embedded function calls)", "Internal script stack overflow". [I] Loop detection is wall-time per frame budget, not an instruction count; exact limits not extracted (need decompilation).
- **[V] Limits (names only)**: "exceeded maximum number of script variables" and "exceeded maximum number of script strings (increase STRINGLIST_SIZE)". [I] The variable pool is a fixed array; sizes not extracted; not needed to decide architecture.
- **[V usage, I semantics] Scopes**: locals and parameters are per function invocation (including threads); `self` is the object a call was made on (`obj func()` → `self == obj` inside); `level` is a global struct-like, `game` a global array, `anim` a third global used by animation scripts (3 uses in MP). A function pointer called with `obj [[ptr]](...)` sets `self` the same way.
- **[I, except the builtin cross-check which is V] Function resolution**: unqualified call resolves first to the file's own functions, then to `#include`d files, then to builtins (all 296 distinct builtin names used were found in the engine tables: see section 5). Qualified `path::fn` call can name any loaded script; the script file `maps\mp\foo` is loaded on first reference and its `main` is not run implicitly [I].
- **[I]** Entities are referenced by handle: assignments to an entity variable keep a reference; after `delete()` or a death, the handle becomes invalid (`isdefined` false, `isalive`). Entity ownership of waiting threads is released on delete (threads waiting on a deleted entity are killed). This is the classic IW behavior and was not decompiled here.
- **[I]** Threads are scheduled single-threaded by the server frame: pending `wait`s with elapsed time are resumed in creation/wake order within the frame, before game frame ends, then `waittillframeend` waiters run. Exact order was not verified; ordering ties are a likely source of subtle divergences; flag for a follow-up on decompilation (ticket #10 baseline).
- **[I] Persistence across map load**: `game["..."]` stores state across rounds/restarts, `level` is reset. Persisted player stats go through `getstat`/`setstat` (25/19 calls) with the stats table `mp/statsTable.csv` etc.

## 4. Data tables the scripts read

`tablelookup`/`tablelookupistring` first arg (MP scripts): `mp/statstable.csv` (33+14, case variants), `mp/ranktable.csv` (15+1), `mp/attachmentTable.csv` (8+3), `mp/playerStatsTable.csv` (4), `mp/rankIconTable.csv` (3), `mp/mapsTable.csv` (2), `mp/challengeTable.csv` (1), `mp/classtable.csv` (1). Table-name case varies, so the VM's stringtable lookup must be case-insensitive. These are stringtable assets in fastfiles ([I] also overlays in IWDs; see ticket for fastfiles).

## 5. Builtin inventory (counted)

Method: tokenize all 210 MP `.gsc` files (strings/comments removed); a call is `ident(`; names that are defined by the scripts themselves were excluded; remaining names are builtins. A call preceded by an expression (object, `thread`, `)`) is a **method**; otherwise a **function**. The engine's own builtin name tables were read from `iw3mp.exe` to cross-check: the function table at RVA `0x325110` (205 entries, 12 bytes each: name pointer, function pointer, developer flag) and three method tables at `0x2bc340` (43), `0x2bc778` (105), `0x325ab8` (82). Every counted name was found in these tables except two (below). `maps/mp/**` counts the 102 core files; `other` is the createart/createfx/mptype/character/common_scripts/codescripts files.

- **Function builtins used: 142 distinct, 4754 calls. Method builtins used: 159 distinct, 1210 calls.** Five names are both a global function and an entity method: `spawn`, `radiusdamage`, `iprintln`, `iprintlnbold`, `logstring`. Distinct total across the two lists: 296. Engine tables contain 204 distinct function names and 230 method names, so 64 function and 71 method builtins are unused by stock MP scripts (they can be stubbed or errored without breaking stock play; list in 5.4).
- **[V] `prof_begin` / `prof_end`** (25 calls each) are called in `_globallogic.gsc` and other scripts but the string does not exist anywhere in `iw3mp.exe`, and the scripts do not define them. [I] Either the compiler drops calls to unknown/dev functions or they are resolved elsewhere; **open unknown**: the VM must treat them as no-ops (safe regardless).
- Language-level statements (`wait`, `waittill`, `waittillmatch`, `waittillframeend`, `notify`, `endon`, `thread`) are not in the builtin tables: they are VM opcodes/keywords. They are counted in section 2.

### 5.1 By subsystem (functions)

| subsystem | distinct | calls |
|---|---:|---:|
| core/type | 9 | 1701 |
| dvar | 5 | 654 |
| precache (asset) | 11 | 615 |
| diagnostics/debug | 13 | 586 |
| math | 25 | 375 |
| entities/world | 9 | 174 |
| stringtable/file | 8 | 137 |
| string | 4 | 108 |
| time | 1 | 98 |
| hud/UI | 12 | 75 |
| fx | 6 | 56 |
| vision/render | 3 | 50 |
| collision/physics | 7 | 45 |
| game state/session | 16 | 30 |
| audio | 1 | 22 |
| weapon data | 8 | 19 |
| rumble | 1 | 5 |
| animation | 3 | 4 |

### 5.2 By subsystem (methods)

| subsystem | distinct | calls |
|---|---:|---:|
| dvar/UI to client | 26 | 347 |
| entity/transform | 41 | 327 |
| player weapons/inventory | 24 | 173 |
| player input/state | 25 | 84 |
| sound | 6 | 68 |
| misc | 4 | 67 |
| player stats/identity | 4 | 52 |
| vehicle/turret/AI | 19 | 32 |
| game state/session | 1 | 26 |
| collision/physics | 3 | 19 |
| perks | 4 | 13 |
| chat | 2 | 2 |

### 5.3a Function builtins (call syntax `name(args)`), sorted by calls

| builtin | calls | maps/mp/** | other | files | subsystem |
|---|---:|---:|---:|---:|---|
| `isdefined` | 1400 | 1294 | 106 | 52 | core/type |
| `setdvar` | 294 | 254 | 40 | 64 | dvar |
| `loadfx` | 233 | 233 | 0 | 38 | precache (asset) |
| `getdvar` | 204 | 204 | 0 | 50 | dvar |
| `assert` | 197 | 178 | 19 | 32 | diagnostics/debug |
| `println` | 127 | 99 | 28 | 17 | diagnostics/debug |
| `precacheshader` | 110 | 110 | 0 | 19 | precache (asset) |
| `precachestring` | 109 | 109 | 0 | 15 | precache (asset) |
| `int` | 108 | 108 | 0 | 19 | core/type |
| `precachemodel` | 107 | 18 | 89 | 42 | precache (asset) |
| `gettime` | 98 | 97 | 1 | 17 | time |
| `getdvarint` | 93 | 92 | 1 | 19 | dvar |
| `getentarray` | 90 | 88 | 2 | 27 | entities/world |
| `tablelookup` | 85 | 85 | 0 | 8 | stringtable/file |
| `assertex` | 83 | 73 | 10 | 15 | diagnostics/debug |
| `issubstr` | 70 | 70 | 0 | 11 | string |
| `spawnstruct` | 70 | 65 | 5 | 20 | core/type |
| `isalive` | 54 | 54 | 0 | 14 | core/type |
| `randomint` | 54 | 47 | 7 | 14 | math |
| `randomfloat` | 46 | 43 | 3 | 15 | math |
| `distance` | 44 | 44 | 0 | 14 | math |
| `getdvarfloat` | 41 | 41 | 0 | 16 | dvar |
| `anglestoforward` | 38 | 38 | 0 | 10 | math |
| `isplayer` | 38 | 38 | 0 | 11 | core/type |
| `spawn` | 37 | 37 | 0 | 20 | entities/world |
| `line` | 34 | 33 | 1 | 10 | diagnostics/debug |
| `vectornormalize` | 34 | 34 | 0 | 13 | math |
| `getent` | 30 | 30 | 0 | 10 | entities/world |
| `vectordot` | 30 | 30 | 0 | 11 | math |
| `getarraykeys` | 28 | 24 | 4 | 13 | core/type |
| `print3d` | 26 | 26 | 0 | 7 | diagnostics/debug |
| `visionsetnaked` | 26 | 6 | 20 | 23 | vision/render |
| `prof_begin` | 25 | 25 | 0 | 2 | diagnostics/debug |
| `prof_end` | 25 | 25 | 0 | 2 | diagnostics/debug |
| `logstring` | 24 | 24 | 0 | 9 | diagnostics/debug |
| `precachemenu` | 24 | 24 | 0 | 2 | precache (asset) |
| `strtok` | 24 | 22 | 2 | 12 | string |
| `setexpfog` | 23 | 5 | 18 | 22 | vision/render |
| `ambientplay` | 22 | 22 | 0 | 22 | audio |
| `bullettrace` | 22 | 22 | 0 | 9 | collision/physics |
| `makedvarserverinfo` | 22 | 22 | 0 | 4 | dvar |
| `iprintln` | 21 | 21 | 0 | 10 | diagnostics/debug |
| `cos` | 20 | 20 | 0 | 9 | math |
| `fgetarg` | 18 | 18 | 0 | 1 | stringtable/file |
| `playfx` | 18 | 18 | 0 | 8 | fx |
| `sin` | 18 | 18 | 0 | 9 | math |
| `vectortoangles` | 16 | 16 | 0 | 11 | math |
| `precacheitem` | 15 | 15 | 0 | 6 | precache (asset) |
| `distancesquared` | 12 | 12 | 0 | 7 | math |
| `newclienthudelem` | 12 | 12 | 0 | 7 | hud/UI |
| `newhudelem` | 12 | 12 | 0 | 3 | hud/UI |
| `playfxontag` | 12 | 12 | 0 | 5 | fx |
| `setgameendtime` | 12 | 12 | 0 | 3 | hud/UI |
| `anglestoup` | 11 | 11 | 0 | 3 | math |
| `bullettracepassed` | 10 | 10 | 0 | 3 | collision/physics |
| `randomfloatrange` | 10 | 10 | 0 | 7 | math |
| `assertmsg` | 9 | 9 | 0 | 7 | diagnostics/debug |
| `spawnfx` | 9 | 9 | 0 | 6 | fx |
| `triggerfx` | 9 | 9 | 0 | 6 | fx |
| `anglestoright` | 8 | 8 | 0 | 5 | math |
| `closefile` | 8 | 7 | 1 | 5 | stringtable/file |
| `newteamhudelem` | 8 | 8 | 0 | 3 | hud/UI |
| `openfile` | 8 | 7 | 1 | 5 | stringtable/file |
| `earthquake` | 7 | 7 | 0 | 4 | fx |
| `getnorthyaw` | 7 | 7 | 0 | 2 | entities/world |
| `getsubstr` | 7 | 7 | 0 | 4 | string |
| `iprintlnbold` | 7 | 7 | 0 | 8 | diagnostics/debug |
| `tolower` | 7 | 7 | 0 | 3 | string |
| `getteamscore` | 6 | 6 | 0 | 2 | game state/session |
| `getweaponmodel` | 6 | 6 | 0 | 1 | weapon data |
| `objective_position` | 6 | 6 | 0 | 2 | hud/UI |
| `precacheshellshock` | 6 | 6 | 0 | 4 | precache (asset) |
| `setclientnamemode` | 6 | 6 | 0 | 6 | hud/UI |
| `setmapcenter` | 6 | 6 | 0 | 6 | entities/world |
| `fprintln` | 5 | 4 | 1 | 4 | stringtable/file |
| `min` | 5 | 5 | 0 | 4 | math |
| `objective_add` | 5 | 5 | 0 | 2 | hud/UI |
| `playrumbleonposition` | 5 | 5 | 0 | 2 | rumble |
| `precacherumble` | 5 | 5 | 0 | 3 | precache (asset) |
| `tablelookupistring` | 5 | 5 | 0 | 2 | stringtable/file |
| `atan` | 4 | 4 | 0 | 1 | math |
| `fprintfields` | 4 | 4 | 0 | 1 | stringtable/file |
| `freadln` | 4 | 4 | 0 | 1 | stringtable/file |
| `length` | 4 | 4 | 0 | 4 | math |
| `lengthsquared` | 4 | 4 | 0 | 3 | math |
| `logprint` | 4 | 4 | 0 | 1 | diagnostics/debug |
| `max` | 4 | 4 | 0 | 4 | math |
| `objective_state` | 4 | 4 | 0 | 2 | hud/UI |
| `objective_team` | 4 | 4 | 0 | 1 | hud/UI |
| `positionwouldtelefrag` | 4 | 4 | 0 | 1 | collision/physics |
| `setprintchannel` | 4 | 0 | 4 | 1 | diagnostics/debug |
| `acos` | 3 | 3 | 0 | 3 | math |
| `announcement` | 3 | 3 | 0 | 2 | hud/UI |
| `combineangles` | 3 | 3 | 0 | 1 | math |
| `exitlevel` | 3 | 3 | 0 | 2 | game state/session |
| `floor` | 3 | 3 | 0 | 2 | math |
| `kick` | 3 | 3 | 0 | 2 | game state/session |
| `physicstrace` | 3 | 3 | 0 | 1 | collision/physics |
| `precacheheadicon` | 3 | 3 | 0 | 2 | precache (asset) |
| `weaponaltweaponname` | 3 | 3 | 0 | 2 | weapon data |
| `weaponclass` | 3 | 3 | 0 | 2 | weapon data |
| `animhasnotetrack` | 2 | 2 | 0 | 1 | animation |
| `endparty` | 2 | 2 | 0 | 2 | game state/session |
| `obituary` | 2 | 2 | 0 | 1 | game state/session |
| `objective_icon` | 2 | 2 | 0 | 2 | hud/UI |
| `physicsexplosionsphere` | 2 | 2 | 0 | 2 | collision/physics |
| `playerphysicstrace` | 2 | 2 | 0 | 1 | collision/physics |
| `precachestatusicon` | 2 | 2 | 0 | 1 | precache (asset) |
| `radiusdamage` | 2 | 2 | 0 | 6 | collision/physics |
| `resettimeout` | 2 | 2 | 0 | 1 | game state/session |
| `setplayerteamrank` | 2 | 2 | 0 | 1 | game state/session |
| `setteamscore` | 2 | 2 | 0 | 1 | game state/session |
| `weaponfiretime` | 2 | 2 | 0 | 1 | weapon data |
| `weaponinventorytype` | 2 | 2 | 0 | 2 | weapon data |
| `addtestclient` | 1 | 1 | 0 | 1 | game state/session |
| `ceil` | 1 | 1 | 0 | 1 | math |
| `endlobby` | 1 | 1 | 0 | 1 | game state/session |
| `getanimlength` | 1 | 1 | 0 | 1 | animation |
| `getassignedteam` | 1 | 1 | 0 | 1 | game state/session |
| `getnotetracktimes` | 1 | 1 | 0 | 1 | animation |
| `getteamradar` | 1 | 1 | 0 | 1 | game state/session |
| `issplitscreen` | 1 | 1 | 0 | 1 | core/type |
| `isstring` | 1 | 1 | 0 | 1 | core/type |
| `isweaponcliponly` | 1 | 1 | 0 | 1 | core/type |
| `map_restart` | 1 | 1 | 0 | 1 | game state/session |
| `missile_createattractorent` | 1 | 1 | 0 | 1 | entities/world |
| `objective_onentity` | 1 | 1 | 0 | 1 | hud/UI |
| `playloopedfx` | 1 | 1 | 0 | 1 | fx |
| `pointonsegmentnearesttopoint` | 1 | 1 | 0 | 1 | math |
| `precachelocationselector` | 1 | 1 | 0 | 1 | precache (asset) |
| `randomintrange` | 1 | 1 | 0 | 1 | math |
| `sendranks` | 1 | 1 | 0 | 1 | game state/session |
| `setarchive` | 1 | 1 | 0 | 1 | game state/session |
| `setminimap` | 1 | 1 | 0 | 1 | entities/world |
| `setteamradar` | 1 | 1 | 0 | 1 | game state/session |
| `spawnhelicopter` | 1 | 1 | 0 | 1 | entities/world |
| `spawnplane` | 1 | 1 | 0 | 1 | entities/world |
| `sqrt` | 1 | 1 | 0 | 1 | math |
| `visionsetnight` | 1 | 1 | 0 | 1 | vision/render |
| `weaponclipsize` | 1 | 1 | 0 | 1 | weapon data |
| `weaponmaxammo` | 1 | 1 | 0 | 1 | weapon data |
| `weaponstartammo` | 1 | 1 | 0 | 1 | weapon data |

### 5.3b Method builtins (call syntax `<entity> name(args)`), sorted by calls

| builtin | calls | maps/mp/** | other | files | subsystem |
|---|---:|---:|---:|---:|---|
| `setclientdvar` | 111 | 111 | 0 | 9 | dvar/UI to client |
| `delete` | 84 | 83 | 1 | 18 | entity/transform |
| `setmodel` | 49 | 16 | 33 | 41 | entity/transform |
| `settext` | 37 | 37 | 0 | 7 | dvar/UI to client |
| `attach` | 32 | 4 | 28 | 29 | entity/transform |
| `setviewmodel` | 30 | 0 | 30 | 30 | player weapons/inventory |
| `playsound` | 27 | 27 | 0 | 12 | sound |
| `allowspectateteam` | 26 | 26 | 0 | 2 | game state/session |
| `fadeovertime` | 26 | 26 | 0 | 10 | dvar/UI to client |
| `setshader` | 26 | 26 | 0 | 10 | dvar/UI to client |
| `getstat` | 25 | 25 | 0 | 5 | player stats/identity |
| `giveweapon` | 25 | 25 | 0 | 7 | player weapons/inventory |
| `playlocalsound` | 25 | 25 | 0 | 8 | sound |
| `getentitynumber` | 23 | 23 | 0 | 3 | entity/transform |
| `iprintln` | 21 | 21 | 0 | 10 | misc |
| `logstring` | 20 | 20 | 0 | 9 | misc |
| `setactionslot` | 20 | 20 | 0 | 3 | dvar/UI to client |
| `setstat` | 19 | 19 | 0 | 4 | player stats/identity |
| `iprintlnbold` | 16 | 16 | 0 | 8 | misc |
| `openmenu` | 16 | 16 | 0 | 3 | dvar/UI to client |
| `radiusdamage` | 16 | 16 | 0 | 6 | collision/physics |
| `usebuttonpressed` | 16 | 16 | 0 | 7 | player input/state |
| `setweaponammoclip` | 15 | 15 | 0 | 4 | player weapons/inventory |
| `closeingamemenu` | 14 | 14 | 0 | 2 | dvar/UI to client |
| `closemenu` | 14 | 14 | 0 | 2 | dvar/UI to client |
| `destroy` | 14 | 14 | 0 | 6 | dvar/UI to client |
| `getcurrentweapon` | 14 | 14 | 0 | 5 | player weapons/inventory |
| `setclientdvars` | 14 | 14 | 0 | 3 | dvar/UI to client |
| `hide` | 13 | 13 | 0 | 6 | entity/transform |
| `linkto` | 11 | 11 | 0 | 9 | entity/transform |
| `setpulsefx` | 11 | 11 | 0 | 1 | dvar/UI to client |
| `gettagorigin` | 10 | 10 | 0 | 3 | entity/transform |
| `playloopsound` | 10 | 10 | 0 | 7 | sound |
| `show` | 10 | 10 | 0 | 3 | entity/transform |
| `spawn` | 10 | 10 | 0 | 20 | misc |
| `givemaxammo` | 9 | 9 | 0 | 3 | player weapons/inventory |
| `isonground` | 8 | 8 | 0 | 2 | player input/state |
| `istouching` | 8 | 8 | 0 | 4 | entity/transform |
| `shellshock` | 8 | 8 | 0 | 7 | player input/state |
| `suicide` | 8 | 8 | 0 | 2 | player input/state |
| `switchtoweapon` | 8 | 8 | 0 | 4 | player weapons/inventory |
| `getammocount` | 7 | 7 | 0 | 3 | player weapons/inventory |
| `rotateto` | 7 | 7 | 0 | 2 | entity/transform |
| `setoffhandsecondaryclass` | 7 | 7 | 0 | 2 | player weapons/inventory |
| `settimer` | 7 | 7 | 0 | 3 | dvar/UI to client |
| `setvalue` | 7 | 7 | 0 | 2 | dvar/UI to client |
| `setwaypoint` | 7 | 7 | 0 | 6 | dvar/UI to client |
| `getguid` | 6 | 6 | 0 | 1 | player stats/identity |
| `getplayerangles` | 6 | 6 | 0 | 2 | player input/state |
| `getweaponammoclip` | 6 | 6 | 0 | 3 | player weapons/inventory |
| `setcandamage` | 6 | 6 | 0 | 4 | entity/transform |
| `setmovespeedscale` | 6 | 6 | 0 | 1 | player input/state |
| `setplayernamestring` | 6 | 6 | 0 | 1 | dvar/UI to client |
| `setteamfortrigger` | 6 | 6 | 0 | 1 | entity/transform |
| `takeallweapons` | 6 | 6 | 0 | 4 | player weapons/inventory |
| `freezecontrols` | 5 | 5 | 0 | 1 | player input/state |
| `getweaponslist` | 5 | 5 | 0 | 4 | player weapons/inventory |
| `hasperk` | 5 | 5 | 0 | 4 | perks |
| `hasweapon` | 5 | 5 | 0 | 2 | player weapons/inventory |
| `notsolid` | 5 | 5 | 0 | 3 | entity/transform |
| `switchtooffhand` | 5 | 5 | 0 | 3 | player weapons/inventory |
| `takeweapon` | 5 | 5 | 0 | 3 | player weapons/inventory |
| `detach` | 4 | 4 | 0 | 2 | entity/transform |
| `getweaponammostock` | 4 | 4 | 0 | 2 | player weapons/inventory |
| `moveto` | 4 | 4 | 0 | 2 | entity/transform |
| `rotateroll` | 4 | 4 | 0 | 2 | entity/transform |
| `rotateyaw` | 4 | 4 | 0 | 3 | entity/transform |
| `setdamagestage` | 4 | 4 | 0 | 1 | entity/transform |
| `setperk` | 4 | 4 | 0 | 3 | perks |
| `setspeed` | 4 | 4 | 0 | 1 | vehicle/turret/AI |
| `setweaponammostock` | 4 | 4 | 0 | 2 | player weapons/inventory |
| `buttonpressed` | 3 | 3 | 0 | 1 | player input/state |
| `clearperks` | 3 | 3 | 0 | 2 | perks |
| `detachall` | 3 | 1 | 2 | 2 | entity/transform |
| `detonate` | 3 | 3 | 0 | 1 | entity/transform |
| `enableweapons` | 3 | 3 | 0 | 2 | player weapons/inventory |
| `fireweapon` | 3 | 3 | 0 | 1 | player weapons/inventory |
| `geteye` | 3 | 3 | 0 | 2 | entity/transform |
| `gettagangles` | 3 | 3 | 0 | 2 | entity/transform |
| `itemweaponsetammo` | 3 | 3 | 0 | 2 | player weapons/inventory |
| `meleebuttonpressed` | 3 | 3 | 0 | 1 | player input/state |
| `rotatepitch` | 3 | 3 | 0 | 2 | entity/transform |
| `setspawnweapon` | 3 | 3 | 0 | 2 | player weapons/inventory |
| `setvehgoalpos` | 3 | 3 | 0 | 1 | vehicle/turret/AI |
| `setvehweapon` | 3 | 3 | 0 | 1 | vehicle/turret/AI |
| `setyawspeed` | 3 | 3 | 0 | 1 | vehicle/turret/AI |
| `solid` | 3 | 3 | 0 | 2 | entity/transform |
| `startragdoll` | 3 | 3 | 0 | 1 | entity/transform |
| `stoploopsound` | 3 | 3 | 0 | 3 | sound |
| `unlink` | 3 | 3 | 0 | 2 | entity/transform |
| `attackbuttonpressed` | 2 | 2 | 0 | 1 | player input/state |
| `cleartargetent` | 2 | 2 | 0 | 1 | vehicle/turret/AI |
| `dropitem` | 2 | 2 | 0 | 1 | player weapons/inventory |
| `fragbuttonpressed` | 2 | 2 | 0 | 1 | player input/state |
| `getcorpseanim` | 2 | 2 | 0 | 1 | entity/transform |
| `getorigin` | 2 | 2 | 0 | 2 | entity/transform |
| `getstance` | 2 | 2 | 0 | 1 | player input/state |
| `getxuid` | 2 | 2 | 0 | 1 | player stats/identity |
| `givestartammo` | 2 | 2 | 0 | 1 | player weapons/inventory |
| `movegravity` | 2 | 2 | 0 | 2 | entity/transform |
| `movez` | 2 | 2 | 0 | 1 | entity/transform |
| `playrumbleonentity` | 2 | 2 | 0 | 2 | player input/state |
| `playsoundasmaster` | 2 | 2 | 0 | 2 | sound |
| `scaleovertime` | 2 | 2 | 0 | 1 | dvar/UI to client |
| `secondaryoffhandbuttonpressed` | 2 | 2 | 0 | 1 | player input/state |
| `setdepthoffield` | 2 | 2 | 0 | 1 | dvar/UI to client |
| `setgoalyaw` | 2 | 2 | 0 | 1 | vehicle/turret/AI |
| `sethintstring` | 2 | 2 | 0 | 2 | dvar/UI to client |
| `setrank` | 2 | 2 | 0 | 1 | dvar/UI to client |
| `setturrettargetent` | 2 | 2 | 0 | 1 | vehicle/turret/AI |
| `setturrettargetvec` | 2 | 2 | 0 | 1 | vehicle/turret/AI |
| `sightconetrace` | 2 | 2 | 0 | 1 | collision/physics |
| `updatedmscores` | 2 | 2 | 0 | 1 | dvar/UI to client |
| `updatescores` | 2 | 2 | 0 | 1 | dvar/UI to client |
| `anyammoforweaponmodes` | 1 | 1 | 0 | 1 | player weapons/inventory |
| `beginlocationselection` | 1 | 1 | 0 | 1 | dvar/UI to client |
| `clearalltextafterhudelem` | 1 | 1 | 0 | 1 | dvar/UI to client |
| `cleargoalyaw` | 1 | 1 | 0 | 1 | vehicle/turret/AI |
| `cleartargetyaw` | 1 | 1 | 0 | 1 | vehicle/turret/AI |
| `clearturrettarget` | 1 | 1 | 0 | 1 | vehicle/turret/AI |
| `clientclaimtrigger` | 1 | 1 | 0 | 1 | player input/state |
| `clientreleasetrigger` | 1 | 1 | 0 | 1 | player input/state |
| `cloneplayer` | 1 | 1 | 0 | 1 | player input/state |
| `damageconetrace` | 1 | 1 | 0 | 1 | collision/physics |
| `devaddpitch` | 1 | 1 | 0 | 1 | vehicle/turret/AI |
| `devaddroll` | 1 | 1 | 0 | 1 | vehicle/turret/AI |
| `devaddyaw` | 1 | 1 | 0 | 1 | vehicle/turret/AI |
| `disableweapons` | 1 | 1 | 0 | 1 | player weapons/inventory |
| `endlocationselection` | 1 | 1 | 0 | 1 | dvar/UI to client |
| `finishplayerdamage` | 1 | 1 | 0 | 1 | player input/state |
| `getattachmodelname` | 1 | 0 | 1 | 1 | entity/transform |
| `getattachsize` | 1 | 0 | 1 | 1 | entity/transform |
| `getattachtagname` | 1 | 0 | 1 | 1 | entity/transform |
| `getvelocity` | 1 | 1 | 0 | 1 | entity/transform |
| `hidepart` | 1 | 1 | 0 | 1 | entity/transform |
| `ismantling` | 1 | 1 | 0 | 1 | player input/state |
| `isonladder` | 1 | 1 | 0 | 1 | player input/state |
| `isragdoll` | 1 | 1 | 0 | 1 | entity/transform |
| `moveovertime` | 1 | 1 | 0 | 1 | dvar/UI to client |
| `physicslaunch` | 1 | 1 | 0 | 1 | entity/transform |
| `pingplayer` | 1 | 1 | 0 | 1 | player input/state |
| `placespawnpoint` | 1 | 1 | 0 | 1 | entity/transform |
| `playsoundtoteam` | 1 | 1 | 0 | 1 | sound |
| `releaseclaimedtrigger` | 1 | 1 | 0 | 1 | player input/state |
| `rotatevelocity` | 1 | 1 | 0 | 1 | entity/transform |
| `sayall` | 1 | 1 | 0 | 1 | chat |
| `sayteam` | 1 | 1 | 0 | 1 | chat |
| `setmaxpitchroll` | 1 | 1 | 0 | 1 | vehicle/turret/AI |
| `setneargoalnotifydist` | 1 | 1 | 0 | 1 | vehicle/turret/AI |
| `setnormalhealth` | 1 | 1 | 0 | 1 | entity/transform |
| `setplayerangles` | 1 | 1 | 0 | 1 | player input/state |
| `settargetent` | 1 | 1 | 0 | 1 | vehicle/turret/AI |
| `settargetyaw` | 1 | 1 | 0 | 1 | vehicle/turret/AI |
| `settenthstimer` | 1 | 1 | 0 | 1 | dvar/UI to client |
| `setturningability` | 1 | 1 | 0 | 1 | vehicle/turret/AI |
| `showpart` | 1 | 1 | 0 | 1 | entity/transform |
| `stoprumble` | 1 | 1 | 0 | 1 | player input/state |
| `stopshellshock` | 1 | 1 | 0 | 1 | player input/state |
| `unsetperk` | 1 | 1 | 0 | 1 | perks |

### 5.4 Engine builtins not called by stock MP scripts

Functions (64): abs, allclientsprint, ambientstop, asin, ban, clientannouncement, clientprint, closer, createprintchannel, distance2d, getangledelta, getbrushmodelcenter, getentbynum, getmovedelta, getnumparts, getpartname, getstarttime, getteamplayersalive, grenadeexplosioneffect, isplayernumber, isvalidgametype, isweapondetonationtimed, map, mapexists, matchend, missile_createattractororigin, missile_createrepulsorent, missile_createrepulsororigin, missile_deleteattractor, musicplay, musicstop, objective_current, objective_delete, physicsexplosioncylinder, physicsjitter, physicsjolt, playrumblelooponposition, precacheturret, print, quitlobby, quitparty, searchforonlinegames, setplayerignoreradiusdamage, setvotenocount, setvotestring, setvotetime, setvoteyescount, setwinningplayer, setwinningteam, sighttracepassed, soundexists, soundfade, spawnturret, startparty, startprivatematch, stopallrumbles, tan, updateclientnames, vectorfromlinetopoint, vectorlerp, weaponisboltaction, weaponissemiauto, weapontype, worldentnumber

Methods (71): adsbuttonpressed, allowads, allowjump, allowsprint, clearlookatent, deactivatechannelvolumes, deactivatereverb, disableaimassist, disablegrenadetouchdamage, enableaimassist, enablegrenadetouchdamage, enablelinkto, freehelicopter, getattachignorecollision, getclanid, getclanname, getcurrentoffhand, getfractionmaxammo, getfractionstartammo, getnormalhealth, getoffhandsecondaryclass, getspeed, getspeedmph, getviewmodel, getweaponslistprimaries, istalking, laseroff, laseron, localtoworldcoords, missile_settarget, movex, movey, openmenunomouse, playerads, playrumblelooponentity, playsoundtoplayer, reset, resetspreadoverride, resumespeed, sendleaderboards, setairresistance, setbottomarc, setchannelvolumes, setclock, setclockup, setcontents, setcursorhint, setentertime, setgametypestring, sethoverparams, setleftarc, setlookatent, setmapnamestring, setorigin, setreverb, setrightarc, setspreadoverride, setstablemissile, settenthstimerup, settimerup, settoparc, setvehicleteam, setviewmodeldepthoffield, showallparts, showscoreboard, showtoplayer, stoplocalsound, useby, usetriggerrequirelookat, vibrate, viewkick

(Notes: the 12-entry table at `0x3248b8` — `userinfo`, `disconnect`, `cp`, `vdr`, `download`, `nextdl`, `stopdl`, `donedl`, `retransdl`, `wwwdl`, `muteplayer`, `unmuteplayer` — is the client-command table, not script builtins. Name-based subsystem classification above is mine [I]; the call counts are measured.)

### 5.5 What this implies for the VM surface

- A tiny set (`isdefined`, `setdvar`/`getdvar*`, `assert*`, `println`, `gettime`, `int`, `randomint`, `spawnstruct`, `getent/getentarray`, `tablelookup`, `issubstr`, `isalive`, `isplayer`) accounts for most calls; the long tail has 1 to 5 calls each.
- Precache calls (`loadfx`, `precache*`: ~615 calls) are load-time asset registration; in a headless server they can map to asset-presence checks.
- Renderer/audio-only builtins (`loadfx`, `playfx*`, `triggerfx`, `spawnfx`, `ambientplay`, `visionset*`, `setexpfog`, `earthquake`, `playloopedfx`, `playsound*`) must still exist on a headless server as no-ops that do not break script flow ([I]: in the original they are server calls that notify clients).
- Client-visible state (hud elems, `setclientdvar`, `openmenu`, `setstat`, objective_*) needs a server→client replication path.

## 6. GPL-3-compatible references (parsers and VMs)

All checked with `gh api repos/<repo>` (license field) on 2026-10-06.

| project | license | language | useful for | notes |
|---|---|---|---|---|
| `xensik/gsc-tool` https://github.com/xensik/gsc-tool | **GPL-3.0** | C++ | lexer/parser/preprocessor/compiler/decompiler design (`src/gsc/{lexer,parser,preprocessor,compiler,assembler}.cpp`) | README lists IW5+ (and later) games, **not IW3**. Grammar is largely shared, but IW3's source is compiled in-engine so this is a grammar/AST reference, not a drop-in. Code may be reused (GPL-3 into GPL-3-or-later requires attributing; confirm "or later" headers when reusing) |
| `Blakintosh/gscode` https://github.com/Blakintosh/gscode | **GPL-3.0** | C# | grammar/semantic checks; a language server; per a web search summary it handles CoD4 (not verified here) | reference; not a runtime |
| `eyza-cod2/vscode-cod-gsc` https://github.com/eyza-cod2/vscode-cod-gsc | **GPL-3.0** | TypeScript | CoD2/CoD4 GSC editor tooling (per its name; contents not inspected here) | [I] may carry builtin signatures; inspect before relying on it |
| `SE2Dev/gsc_parser` https://github.com/SE2Dev/gsc_parser | **no SPDX license detected** by GitHub | C++ Flex/Bison | standalone lexer/parser | treat as fact source only until a license is confirmed |
| `Kyasuta/bo2_gsc_compiler` https://github.com/Kyasuta/bo2_gsc_compiler | **GPL-2.0** (not "or later" per GitHub field) | C++ Flex/Yacc | small grammar | likely GPL-2.0-only, so **not compatible** with GPL-3 reuse; fact source only |
| CoD4X server (AGPL) | AGPL-3.0 | C | builtin semantics and the original VM behavior | **fact source only** per ADR 0001; nothing was read or copied for this sheet |

No mature Rust GSC parser or VM was found [V: `gh search repos` and a web search]. A parser written in Rust (for example with a hand-written lexer plus Pratt or recursive-descent parser) is small: the stock corpus uses no `foreach`/ternary, so the grammar is statement-level C plus thread/event keywords.

## 7. Sizes (for planning the VM)

- 210 MP scripts, 1.6 MB of source text [V] (all 823 extracted `.gsc` files, mostly SP, total 10.6 MB).
- 5,548 user-function calls and 5,964 builtin calls in MP scripts.
- Dev-only: 95 `/# … #/` blocks; `_dev.gsc`, `_createfx*.gsc`, `maps/createfx/*` are developer tools and can be skipped for stock play [I].

## 8. Open unknowns

1. Exact scheduler order and tie-breaking (same-frame `wait` wake order, `waittillframeend`, notify delivery order when multiple threads wait on one event): not decompiled. Needs the Ghidra baseline (#10) or black-box tests.
2. Exact limits (script variable pool, string table size `STRINGLIST_SIZE`, stack depth, infinite-loop budget).
3. How `prof_begin`/`prof_end` are accepted when absent from the engine tables.
4. Entity-handle lifetime rules on delete/disconnect (waiters killed? `self` becoming undefined?).
5. Whether case-insensitivity applies to all identifiers and event names (event strings compared case-sensitively or not).
6. Per-entity-type method tables: the 230 method names are three tables whose entity-type bindings were not decoded.
7. MP script size share and how gametype `main` is dispatched through `_gametypes.txt`.
