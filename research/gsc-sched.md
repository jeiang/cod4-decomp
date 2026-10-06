# GSC VM scheduling and event-ordering rules (CoD4 1.7 MP)

Ticket: jeiang/cod4-decomp#20 (feeds #13, builds on #5 and baseline #10).

Evidence tags: **[V]** verified in the `iw3mp.exe` 1.7 retail binary (SHA-256 `e41fcd59…a1fa893`, Ghidra 12.1.2 decompile and disassembly). **[K]** read from the KisakCOD clone only (facts only; prose, no code copied). **[I]** inference. **[C]** observed in the stock MP scripts (`common_mp.ff`, 60 `.gsc` rawfiles, extracted to `/tmp` only).

Function names below are labels. `FUN_` addresses are in `iw3mp.exe` (image base 0x400000); the label is the name of the corresponding KisakCOD function. Only `VM_ExecuteInternal` carries an applied name in `symbols.csv`.

| Label | Address | Role |
| --- | --- | --- |
| `G_RunFrame` | 0x4c01b0 | per-server-frame game tick |
| `G_XAnimUpdateEnt` | 0x4bfc90 | entity anim update loop that calls a thread drain |
| `Scr_FreeEntityList` | 0x51bd20 | deferred entity-handle teardown |
| `Scr_FreeEntityNum` | 0x51bc80 | marks an entity handle freed |
| `GetNewVariableIndexInternal2` | 0x519630 | child-list insert at head |
| `GetNewVariableIndexReverseInternal2` | 0x5196e0 | child-list insert at tail |
| variable alloc | 0x519cf0 | variable pool allocation |
| `VM_Resume` | 0x5226c0 | drain one time bucket |
| `VM_SetTime` | 0x522ce0 | find and drain the bucket for the current tick |
| `VM_Execute` | 0x522860 | engine-initiated thread call |
| `VM_ExecuteInternal` | 0x51d350 | opcode loop |
| `VM_Notify` | 0x521c60 | notify delivery |
| `Scr_CancelNotifyList` | 0x5223e0 | drops all waiters on a freed object |
| `VM_TrimStack` | 0x521660 | discards a waiter's stack |
| `VM_TerminateStack` | 0x5214b0 | endon termination |
| `Scr_TerminateThread` | 0x521c10 | endon termination dispatcher |
| `Scr_TerminateWaitThread` | 0x521890 | endon termination of a thread parked in a time bucket |
| `Scr_GetMethod` | 0x4d8570 | method-table resolution |

## 1. Core model

- **[V]** Thread scheduling is cooperative, single threaded, and driven by an integer *script tick* `scrVarPub.time` (24-bit, wraps at `& 0xFFFFFF`). It is **not** `level.time`. It advances exactly once per `G_RunFrame`, at `Scr_IncTime` (G_RunFrame 0x4c01b0: `time = (time + 1) & 0xFFFFFF`).
- **[V]** A parked-on-time thread lives in a *time bucket*: a variable-pool array keyed by the tick number, whose children are archived thread stacks keyed by thread id. `VM_SetTime` (0x522ce0) looks up the bucket for the **current** tick, calls `VM_Resume` (0x5226c0) on it, then removes the bucket.
- **[V]** `VM_Resume` repeatedly takes the bucket's **first sibling** (the list head), removes it, unarchives its stack, runs `VM_ExecuteInternal`, and loops until the bucket has no children. Anything inserted into the same bucket while it drains is therefore run in the same drain.
- **[V]** Each drain calls `Scr_ResetTimeout` (wall-clock timer for the loop guard) first.
- **[V]** `thread f()` / method-thread calls run the callee **immediately and synchronously** inside the caller's `VM_ExecuteInternal` until its first `wait`/`waittill*`/end, then control returns to the caller (nested thread frame, `thread_count` in the loop). Engine-initiated calls (`VM_Execute` 0x522860, i.e. `Scr_ExecThread`/`Scr_ExecEntThread`) behave the same: the thread body runs to its first yield before the engine call returns. [K] for the call-side opcode source; [V] for the 0x5226c0/0x522860 structure.

## 2. Same-frame `wait` wake order

- **[V]** `wait n` stores the thread in bucket `time + ticks`. Insertion into a bucket uses the generic child insert 0x519630, which **prepends** (new entry becomes the list head; the previous head's `prev` is set to it).
- **[V]** `VM_Resume` always consumes the head. So within a bucket the run order is **last inserted, first run (LIFO)**. Two threads that both do `wait 0.05` in a frame run in the *opposite* order next frame, and a thread that loops `wait 0.05` alternates its position relative to its siblings every frame.

  Worked example: bucket for tick T runs a, b, c in that order. During that, a, b, c each `wait 0.05`, inserting into the T+1 bucket in order a, b, c, so the T+1 bucket head→tail is c, b, a and runs c, b, a. [I] derived from the two [V] facts above; not separately executed.
- **[V]** Tick conversion: float operand = `nearest_int(x * 20.0 + 2^-30)` (constants at 0x70b378 = double 20.0, 0x70b088 = double 2^-30); if that rounds to 0 but `x != 0`, result is 1 tick; integer operand = `n * 20`. The factor 20 is **hard-coded**, not `sv_fps`. Negative waits error "negative wait is not allowed". Waits above `0xFFFFFE` ticks error "wait is too long".
- **[K]** `sv_fps` defaults to 20 (min 10, max 1000; registered in `SV_Init`), frame length is `1000 / sv_fps` ms integer division, so `wait 1` is 20 *frames*: at `sv_fps 30` it is ~667 ms, not 1 s. [V] only for the hard-coded 20 in the VM and for `0x4c01b0` running once per server frame.
- **[V]** `wait 0` (or any `wait` that rounds to 0 with a zero operand) inserts into the **current** tick bucket at the head. [I] During an active drain it becomes the next thread taken, so it runs before the other threads still pending in that bucket (derived from head insertion plus the consume-head loop; not separately executed).
- **[V]** A thread that waits a non-zero time calls `Scr_ResetTimeout` at the `wait` (so one long computation per tick is measured per `wait`).

## 3. `waittillframeend`

- **[V]** The opcode (jump-table case 0x4d, 0x51d350) archives the thread into the **current tick's bucket** using the **tail-insert** variant 0x5196e0 (all other waits use head insert). There is no separate end-of-frame queue.
- **[V]** Consequence: "frame end" means "when the currently draining bucket has no other thread left". It is **not** the end of `G_RunFrame`. A `waittillframeend` thread parked while bucket T is draining runs after every head-inserted thread (including `wait 0` and notify-woken ones inserted later), and multiple frameend threads run in FIFO call order.
- **[I]** A `waittillframeend` called from outside a drain (engine code calling a thread, between frames) joins bucket T and is run at the end of the next drain of that bucket.
- **[C]** 17 uses in `common_mp.ff` scripts, e.g. `_globallogic.gsc` (comment: "so we don't endon the end_respawn from spawning as a spectator").

## 4. `notify` delivery order and re-entrancy

Data structure **[V]** (`VM_Notify` 0x521c60, `waittill` case 0x78/0x77 of 0x51d350): each object has a *notify list* (hidden key 0x18000) = name → list of waiters. `waittill` and `endon` both insert at the **head** of the per-name list (head insert 0x519630). The waiter list therefore has the **oldest** registration at the tail.

Delivery **[V]**:
1. `notify` never runs a waiter inline. For each registered entry it removes the entry, and for a real waiter re-files the thread in the **current tick bucket (head insert)**, appending the notify payload values to the waiter's saved stack (reference-counted copies).
2. Entries are visited from the **tail (oldest) to the head**, so waiters wake oldest-first. Because each wake prepends to the bucket, the woken threads **run newest-registered-first** (reverse of registration), after the notifier yields, in the same drain if the notifier is itself running inside a drain, otherwise in the next drain of bucket `time`.
3. `endon` entries live in the same list and are processed in the same oldest-to-newest pass. An `endon` hit terminates its thread **during the notify call** (before any woken thread has run) and nothing is queued for it.
4. `waittillmatch` entries are compared to the payload; a non-match leaves the waiter registered (the entry is skipped, the payload is not consumed).
5. A thread woken by a notify is already removed from the list, so a second `notify` of the same name before it runs does not wake it twice.
6. Re-entrancy: a `notify` issued by a thread that has an `endon` for the same event on the same object terminates the notifier itself when that entry is reached (it is the running thread, whose resume position is overwritten with the end marker); `notify` saves and restores the current resume position around the call. Since no code runs during delivery, notifications inside notifications cannot nest.
- **[V]** Payload: values after the event name are delivered to `waittill` variable lists; the engine's own `Scr_Notify` (`Scr_NotifyNum`) pushes args then calls the same delivery. It is a no-op if the entity has no script object yet (no waiters possible). [K] for the engine wrapper, [V] for the delivery loop.
- **[K]** Event names are `SL` string handles; matching is by handle identity (hash over raw bytes with no case folding), so **event names are case-sensitive**. [C]: all 163 distinct event names in the 60 scripts are written in one casing each (no variants), so the corpus neither confirms nor contradicts this.

## 5. `endon` cleanup

- **[V]** `endon("x")` on object O registers a hidden helper entry in O's notify list for `x`, and records in the thread's own pause-array slot (key = the thread/function-frame id) which object/name it is registered on.
- **[V]** `endon` applies to the **function frame** that executed it, not the whole coroutine. On a hit (`Scr_TerminateThread` 0x521c10 → `VM_TerminateStack` 0x5214b0): the frame that ran `endon` and every frame called synchronously below it are killed (their locals released). If that frame was the thread's start frame the whole thread is freed. If it was an inner frame (called as a plain function from a caller that is part of the same coroutine), the **caller is resumed** instead: the stack is re-filed in the **current tick bucket (head insert)** with the dead callee's return value set to **undefined**. So `f()` containing `endon` that fires makes the caller continue with `undefined` as the result.
- **[V]** A thread parked in a time bucket that is hit by `endon` is first removed from that bucket (`Scr_TerminateWaitThread` 0x521890; the bucket is deleted if it becomes empty and is not the current tick).
- **[V]** When a thread ends or is killed, all of its `endon` and `waittill` registrations on other objects are removed with it (loop in `Scr_KillThread`: cancel notify entry, drop helper object).
- **[K]** A thread blocked in `waittill` on a still-live object that hits `endon` on another object: the wait entry is cancelled (`VM_CancelNotify`) and the thread terminated; it never resumes.

## 6. Entity-handle lifetime (delete / disconnect)

- **[V]** `G_FreeEntity` → `Scr_FreeEntity` → `Scr_FreeEntityNum` (0x51bc80): the script object for the entity is retagged as *dead entity* (type 0x13), gets an extra reference and is pushed on a pending list (`freeEntList`); the entity-number→object map entry is removed, so a **new entity in the same slot gets a fresh script object**. The old handle stays valid to script code that still holds it (e.g. `self`, a stored variable) but is no longer an entity.
- **[V]** `Scr_FreeEntityList` (0x51bd20) drains the pending list. It is called from **`Scr_IncTime`, after `Scr_RunCurrentThreads`, before the tick increments** (also at game init and shutdown). For each freed object it calls `Scr_CancelNotifyList` (0x5223e0) and clears its fields.
- **[V]** `Scr_CancelNotifyList` takes every registered entry on the freed object: waiting `waittill` stacks are discarded via `VM_TrimStack` (0x521660): the thread **never resumes and is not woken or notified**; for `endon` helper entries the entry is dropped. **No event is delivered by freeing an entity** (no implicit "death" or "disconnect"). A thread that had an `endon("disconnect")` registered on a freed player keeps running unless something notified it.
- **[V]** A waiter stack whose caller frames carry other live registrations is parked (marked with no resume position, kept under a hidden key) rather than freed until those registrations are removed; it never runs again either way. (`VM_TrimStack` 0x521660: pause-array check, `pos = 0` park.)
- **[K]** `isdefined(ent)` is false for dead entities and dead threads (object types 0x13 and ≥ 0x16), true for live objects. Method calls on a dead entity fail with "`<type>` is not an entity" because only `VAR_ENTITY` objects are accepted (`OP_CallBuiltinMethod` source, [K]). Field reads on a dead entity read plain stored fields only (no entity-field getters). [I] fields set on the handle stay readable until the handle's last reference goes.
- **[K]** Disconnect order in MP (`ClientDisconnect`, `g_client_mp.cpp`): engine notifies `menuresponse "disconnect"`, calls `Scr_PlayerDisconnect` (runs the script callback as a thread), then `G_FreeEntity`. [C] `maps/mp/gametypes/_callbacksetup.gsc` `CodeCallback_PlayerDisconnect` does `self notify("disconnect")` itself before the game-type callback, so the `disconnect` event is script-generated, not engine-generated.
- **[I]** Because freeing is deferred to `Scr_IncTime` of the next drain-capable tick, a disconnect that happens between frames lets the threads woken by `disconnect` run (first drain of the next frame) before the handle's waiters are cancelled.

## 7. Identifier and event-name case sensitivity

- **[K]** The parser lowercases identifiers (function names, local/field variable names, builtin names, call targets and file paths via a `LowerCase` grammar action and `Scr_CreateCanonicalFilename`), but **not** string literals. Function-handle lookup also lowercases the name. So `isDefined`, `isdefined` and `ISDEFINED` are the same identifier; `"Disconnect"` and `"disconnect"` are different events.
- **[C]** 95 callable identifiers appear with mixed casing in the stock scripts (e.g. `isDefined` ×497 and `isdefined` ×888, `getDvar`/`getdvar`, `randomInt`/`randomint`): confirms identifiers are case-insensitive. Not separately confirmed in the binary (the lexer actions use a table-driven parser).
- **[K]** The compiler also lowercases map-time field names and `#include`/path components (canonical filename = lowercase, `/`→`\`).

## 8. VM limits

| Limit | Value | Tag |
| --- | --- | --- |
| Call depth (script frames, includes `thread` calls) | a call/thread is refused when `function_count >= 31` ("script stack overflow (too many embedded function calls)"); `function_frame` array has 32 slots | [V] 0x51d350 (compare with 0x1f), init 0x51d0e0 |
| Engine-initiated call depth | `VM_Execute` 0x522860 refuses when `function_count > 29` | [V] |
| Value stack | 2048 `VariableValue` entries (`maxstack = stack + 2047`); overflow → "Internal script stack overflow" | [V] init 0x51d0e0 (0x1798370 − 0x1794378 = 0x3ff8 = 2047×8) |
| Local-variable index stack | 2048 entries | [K] |
| Variable pool: parent (object) slots | 0x8000 | [V] pool layout (child array starts 0x80010 bytes after parent array, 16-byte slots) |
| Variable pool: child (value) slots | 0xFFFE (hash modulus `0xFFFD`) | [V] hash `% 0xfffd + 1` in 0x519630/0x519cf0 callers; [K] 0xFFFE |
| Pool exhaustion | thread dump, then runtime error "exceeded maximum number of script variables" (allocator 0x519cf0) | [V] |
| Jump-back variable-count guard (`> 0xF37E` values or `> 0x7380` objects → `Sys_Error`) | **absent from the 1.7 MP binary**: case 99 (jump back, 0x51d350) only has the timer check | [V]; the KisakCOD source has this check |
| String table | 20 000 hash entries (`STRINGLIST_SIZE`), handles < 0x10000, 16-bit refcounts | [K]; [V] error strings "exceeded maximum number of script strings (increase STRINGLIST_SIZE)" |
| String literal max | 8192-byte parse buffer, "max string length exceeded" | [K] |
| Script memory tree | 0xC0000 bytes, 0x10000 nodes | [K] |
| Parameters per call | 256 ("parameter count exceeds 256", compile time) | [V] string |
| Wait length | ≤ 0xFFFFFE ticks | [V] |
| Script tick | 24 bits, wraps (`& 0xFFFFFF`), 20 ticks/s nominal, ≈ 233 h | [V] |
| **Infinite-loop guard** | checked only on **backward jumps** (opcode 0x63): if wall-clock elapsed since the last `Scr_ResetTimeout` ≥ **2500 ms** (0x9c4) the VM acts. While loading (`scrVmGlob.loading`): only a printed warning, then it resets and continues. Otherwise: if `showError` (developer): dump threads, error "potential infinite loop in script". If not: print "script runtime error: potential infinite loop in script - killing thread." and kill the thread chain. If errors abort: terminal error. KisakCOD says 5000 ms; the 1.7 MP binary uses 2500 ms | [V] 0x51d350 case 99 |
| Timeout resets | at each `VM_Resume` drain, at each non-zero `wait`, when a thread stack is unarchived at a different tick than it was archived, and at each engine `Scr_ExecThread` entry | [V] drain/wait; [K] others |
| Recursion without loops | guarded only by depth 31, not by the timer | [V] |

## 9. How the method tables bind to entity types

- **[V]** Binding is **by name at compile time, not by the receiver's entity type.** `Scr_GetMethod` (0x4d8570) searches five tables in this fixed order and returns the first match: **Player** (0x4b1fc0, 83 entries, `PlayerCmd_*`) → **ScriptEnt** (0x4dad80, 18 entries, movers/models) → **HudElem** (0x4bac80, 22 entries) → **Helicopter** (0x4c9e90, 25 entries) → **BuiltIn entity methods** (0x4d84f0, 82 entries). Counts are from the loop bounds in each lookup (0x3e4/12, 0xd8/12, 0x108/12, 300/12, 0x3d8/12). Names are therefore one flat namespace; a name that exists in two tables resolves to the earlier table.
- **[V]** The ticket says "three"; the MP binary has **five**. [I] The three the ticket meant are Player, ScriptEnt, HudElem.
- **[K]** Runtime dispatch: `OP_CallBuiltinMethod*` requires the receiver to be a `VAR_POINTER` whose object type is `VAR_ENTITY` ("`<type>` is not an entity" otherwise). It builds an entity reference (entity number + class: entity / hudelem / pathnode / vehicle node) and calls the table function with it. **The method body validates the class**: e.g. Player methods error "not an entity" when the class is not the entity class and fail if the entity has no client; hudelem methods require class hudelem.
- **[K]** Plain builtin *functions* are resolved by the same compile-time mechanism from a separate 205-entry table (`Scr_GetFunction`), with a developer-only flag per entry; results are cached per name.

## 10. `prof_begin` / `prof_end`

- **[K]** These are **language statements**, not builtins: the grammar has `prof_begin ( "name" ) ;` and `prof_end ( "name" ) ;`. This explains why the strings are absent from the executable (they are lexer tokens) and why scripts do not define them.
- **[K]** The compiler emits opcodes 0x85/0x86 only when `developer_script` is on; otherwise the statement compiles to nothing. Each name is registered through `Profile_AddScriptName` (max 32 names; "max profile names exceeded").
- **[V]** The VM opcode loop (0x51d350, cases 0x85 and 0x86) treats both opcodes as a no-op that skips one operand byte. A new VM can parse the statement and drop it.
- **[C]** 26 `prof_begin` uses in the 60 scripts, e.g. `_spawnlogic.gsc` (note the leading space in some names such as `" spawn_final"`; names are opaque strings).

## 11. Per-server-frame order (MP)

`Com_Frame` (dedicated, KisakCOD [K]): poll `Com_EventLoop` → `Cbuf_Execute` → `SV_Frame(msec)`.

Outside the frame, **between frames**, in `Com_EventLoop`/`SV_PacketEvent`: client messages execute (`SV_ExecuteClientMessage` → `SV_UserMove` → `SV_ClientThink` → `ClientThink`/`ClientThink_real`: usercmd processing, pmove, weapon/use events). Script notifications raised here, and engine callback threads started here (`Scr_ExecEntThread`), run synchronously or file wake-ups into the **current tick bucket** (the tick that `G_RunFrame` will drain next). [K] call chain.

`SV_FrameInternal` [K]: add `msec` to `timeResidual`; while ≥ `1000/sv_fps` run frames: `SV_CalcPings`, `SV_PreFrame` (bots, dvar configstrings), then loop { `timeResidual -= frameMsec; svs.time += frameMsec; SV_RunFrame()` (= `G_RunFrame(svs.time)`)`; Scr_SetLoading(0)`; `SV_PostFrame` between catch-up frames (timeouts, `SV_SendClientMessages`) }.

`G_RunFrame` (0x4c01b0 [V] structure; names and details [K]):
1. `level.time = svs.time`, frame counters, `level.frametime`.
2. `G_TouchTriggers` for each in-use client (touch notifies filed, not run).
3. Anim timers `SV_DObjInitServerTime` for all entities.
4. Trigger check: copy pending trigger list; loop { for each entry whose entity still matches its use count: `Scr_AddEntity(other); Scr_Notify(trigger_ent, "trigger", 1)`; then **`Scr_RunCurrentThreads`** } until no entry was deferred (an entity may fire only once per pass). **This is the first drain of the tick's bucket** in a normal frame: the wait-woken, between-frame-notified and touch-notified threads run here.
5. `G_ClientDoPerFrameNotifies` for each in-use client (weapon_change, begin/end_firing, night vision, sprint begin/end).
6. For every entity: `G_XAnimUpdateEnt` → loop { while the entity's anim step produced a notetrack notify: **`Scr_RunCurrentThreads`** } (0x4bfc90, a drain per fired notetrack).
7. **`Scr_IncTime`**: `Scr_RunCurrentThreads` → `Scr_FreeEntityList` (deferred entity teardown, section 6) → `time++` (24-bit) → reset the error flag. [V] order (0x4c01b0 inlines the drain then calls 0x51bd20 then increments).
8. For every in-use entity `G_RunFrameForEntity` (think handlers, missiles, items, movers; script threads started by think code run synchronously and wake-ups from here fill bucket T+1).
9. `G_UpdateObjectiveToClients`, `G_UpdateHudElemsToClients`, `ClientEndFrame` per client, team status, vote check, scoreboard, registered weapons/items.

Net behavior for an exact port:
- The **only time-based wake-up point** is bucket T at the first `Scr_RunCurrentThreads` of tick T (step 4). Later drains in the same tick (steps 6 and 7) only see entries added after step 4 (`wait 0`, notify wake-ups, frameend threads, endon-resumed callers, notetrack notifies).
- Notifies and thread starts that happen **after** step 7 (entity think, `ClientThink`, disconnects, `SV_CheckTimeouts`) land in bucket T+1 **ahead (head insert) of earlier `wait`ers of that bucket**, so they run first at the next tick's first drain.
- A tick drains **bucket T only once per `Scr_RunCurrentThreads` call**; a later call at the same T re-creates and drains a bucket only if something was inserted meanwhile.
- `level.time`/`gettime()` is ms, unrelated to ticks. A wait of N seconds is N×20 ticks, one tick per `G_RunFrame` call, even when several `G_RunFrame`s run back to back during catch-up (no client input is processed between them).

## 12. Open items

1. Names/cases: lexer lowercasing and the 8192 string buffer rest on KisakCOD; not checked in the binary (table-driven parser).
2. `sv_fps` default 20 and the `Com_Frame`/`SV_FrameInternal` ordering are [K]; the VM-side constant 20 is [V].
3. Behavior of `Scr_ExecThread` re-entrancy limits (depth 29 for engine calls) is verified structurally (0x522860) but the error text/path was not traced.
4. Entity-field behavior on dead entities ([K]) and the `isdefined` type test ([K]) were not re-derived from the binary.
5. The 5-table method order and counts are [V]; the per-method class checks are [K] and were checked for one method only.
