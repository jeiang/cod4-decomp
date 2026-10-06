# IW3 v5 fastfiles: layout and asset decoding (ticket #2)

Scope: PC `IWffu100` version 5 fastfiles from the CoD4 1.7 install (`COD4/zone/english/*.ff`). Everything under "Verified" was checked against real files (chiefly `common_mp.ff` and `mp_crash.ff`). Everything under "Inference" was not.
No original-install content is reproduced here; only counts, sizes, field names and short identifiers.

## 1. Sources and licenses

| Source | What it gives | License (checked via GitHub API / repo) | Use |
|---|---|---|---|
| **OpenAssetTools (OAT)**, `Laupetin/OpenAssetTools`, commit `59904a3e` (2026-10-03) | Complete IW3 zone loader: header, blocks, pointer rules, every asset struct (`src/Common/Game/IW3/IW3_Assets.h`), load commands (`src/ZoneCode/Game/IW3/XAssets/*.txt`), generator template (`src/ZoneCodeGeneratorLib/Generating/Templates/ZoneLoadTemplate.cpp`), constants (`src/ZoneCommon/Game/IW3/ZoneConstantsIW3.h`) | **GPL-3.0** (`LICENSE` is GPLv3 text; README says "GPLv3"; per-file headers carry no "or later" grant, so treat it as **GPL-3.0-only**) | Fact source. Code reuse is **not clean**: our project is GPL-3.0-**or-later**, and GPL-3.0-only code cannot carry that grant. Translating structs/algorithms is a derivative; get a decision (ADR) before copying anything. Facts (field layouts, enum values) are free to re-derive. |
| **ZoneTool**, `ZoneTool/zonetool` | IW3 runtime zone dump/link (structs, serializers) | GPL-3.0 (GitHub license API) | Fact source / cross-check; not read in depth. Same only/or-later caveat unverified. |
| **CoD-FF-Tools**, `primetime43/CoD-FF-Tools` (C#) | FastFile container I/O, rawfile/localize editing for CoD4 PC | GPL-3.0 (GitHub license API) | Cross-check only; CoD4 PC support listed as partial. Not read in depth. |
| **Zeroy wiki "Call of Duty 4: FastFile Format"** (wiki.zeroy.com) | Early header notes, block-type table | Community wiki, license not stated | Fact source only. Describes an older/pre-1.7 type numbering (see section 4). Its header notes match what I measured: field 0 = inflated size − 44, field 1 = "external" size (confirmed on `mp_backlot_load.ff`: 0x12AAB8). |
| **CoD4X server**, `callofduty4x/CoD4x_Server` | Not used for this ticket | AGPL-3.0 | Reference only per ADR 0001. Not consulted. |

OAT was also **built locally on macOS arm64** (scratch copy in `/tmp`, needed 5 trivial portability patches for Apple clang; nothing committed) and its `Unlinker --list` run on both validation zones. This is the strongest end-to-end proof below.

## 2. Container layout (verified)

All integers little-endian.

```
offset 0   char[8]  magic "IWffu100"   (unsigned; "IWff0100" = signed/Xbox-style, with an auth header; none of the 72 PC zones here is signed)
offset 8   u32      version = 5
offset 12  zlib stream (RFC1950, header bytes 78 DA), a single stream to EOF
```

Verified on all 72 files in `zone/english`: magic and version identical; `zlib.decompress` consumes the whole file with no trailing bytes (`common_mp.ff`: 13,517,381 B -> 41,520,582 B; `mp_crash.ff`: 35,287,381 B -> 63,106,933 B).
OAT inflates in 0x2000-byte chunks but that is only its buffering; there is no chunk framing on PC.

Inflated stream (the "XFile"):

```
u32 size           = inflated_length - 44   (verified exactly on all 72 zones)
u32 externalSize   = bytes of external data (IWI image files); see below
u32 blockSize[9]   one per XFileBlock
XAssetList (16 bytes, read with no block pushed):
    u32 stringCount;  ptr strings;       // ptr = 0xFFFFFFFF ("follows")
    u32 assetCount;   ptr assets;        // ptr = 0xFFFFFFFF
then the zone body (all further reads go through the block allocator, section 3)
```

Measured:

| zone | size | externalSize | blockSize TEMP / RUNTIME / VIRTUAL / VERTEX / INDEX | stringCount | assetCount |
|---|---|---|---|---|---|
| common_mp | 41,520,538 | 104,788,937 | 2,404 / 0 / 30,188,532 / 9,572,992 / 1,128,448 | 591 | 2,595 |
| mp_crash | 63,106,889 | 243,254,095 | 4,195,104 / 193,984 / 21,034,581 / 13,404,672 / 2,100,116 | 372 | 823 |

- `LARGE`, `LARGE_RUNTIME`, `PHYSICAL`, `PHYSICAL_RUNTIME` are 0 in both zones (they matter on consoles).
- Sum of non-TEMP block sizes for mp_crash = 36.7 MB: this is the in-memory footprint to load a map's zone (plus ~243 MB of IWI image data that is read separately; see 5.3).
- `externalSize` ~= sum of the sizes of the IWI files the zone's images reference. On `mp_backlot_load.ff` it is exactly the two IWI sizes minus 2 x 28 (the IWI header). On `mp_crash.ff` the 801 images that exist as IWI files sum to 243,863,405 B vs 243,254,095 reported (0.25% apart; the exact accounting rule is **unknown**, harmless for loading).
- Body order after the 16-byte header: `stringCount` pointers (4 B each), then the strings (NUL-terminated, each `0xFFFFFFFF`-flagged one in sequence), then `assetCount` x `XAsset {u32 type; ptr header}` (8 B), then each asset's data in array order. Verified: in both zones every `header` is `0xFFFFFFFF` (the assets are always stored inline in the list; none is an offset reference) and 590/591 resp. 372/372 string pointers are `0xFFFFFFFF` (the one other is a null at index 0).
- The string list is the **script string table** (bone names, notetrack names, etc.): XModel/XAnim/weapon fields typed `ScriptString` (u16) index into it.

## 3. Block and pointer model (OAT source, spot-validated)

Blocks (enum `XFileBlock`, order is the order of the 9 sizes): `TEMP, RUNTIME, LARGE_RUNTIME, PHYSICAL_RUNTIME, VIRTUAL, LARGE, PHYSICAL, VERTEX, INDEX`.

- Types per OAT: TEMP and the four normal ones consume bytes from the stream; the three `*RUNTIME` blocks only reserve zeroed space (no bytes in the stream; the engine fills them at runtime).
- A stack of blocks is kept. Each read allocates in the **top** block: the offset is first aligned, then bytes are read, then the offset advances.
- TEMP is reset to its push-time offset when popped (it is a scratch area for asset *headers*; its declared size is the maximum).
- Default load procedure for an asset header (`ZoneLoadTemplate.cpp`, `PrintLoadPtrMethod` + `PrintLoadMethod`): push TEMP; allocate+read the fixed-size header struct; push VIRTUAL; load members in declaration order (or the command file's `reorder`); pop both. Strings, arrays and nested non-asset structs therefore land in VIRTUAL unless a command sets another block (VERTEX/INDEX for XModel surface vertices/indices, RUNTIME for world/dynamic-entity runtime arrays, TEMP for e.g. `GfxImageLoadDef`).

Pointer values in the stored structs (32-bit):

| value | meaning |
|---|---|
| `0` | null |
| `0xFFFFFFFF` (-1) "FOLLOWING" | the pointee's data follows inline in the stream at this point |
| `0xFFFFFFFE` (-2) "INSERT" | as FOLLOWING, and additionally reserve a 4-byte pointer slot (aligned 4) in the **VIRTUAL** block; after loading, that slot receives the pointee's address. Used for TEMP-resident assets (headers) so later references can alias it |
| anything else | "offset": `((block << 28) \| offset) + 1`, i.e. 4 block bits (OAT `OFFSET_BLOCK_BIT_COUNT = 4`) and 28 offset bits, with +1 so block 0 offset 0 is not null. For ordinary data this points at the data. For INSERT'ed assets it points at the **slot** (an alias), whose contents are the asset pointer |

Notes:
- "reusable" members in the command files are the ones that may legitimately be an offset (shared data: techniques, shaders, vertex decls, texture tables, `boneNames`, physics geoms, etc.).
- Alignment is per type (natural alignment plus explicit `set allocalign`, e.g. `Material` 4, `MaterialConstantDef` 16 from `type_align(16)`).

Evidence for the rules above (own parser, section 8): all 21 `mp_*_load.ff` zones (each = 2 techsets + 3 materials + 1 rawfile, with nested images, shaders, vertex decls) decode **to the last byte**, and VIRTUAL block usage equals the declared VIRTUAL size exactly in all 21. This exercises -1, -2, offset (reused techsets/shaders/decls), TEMP resets, alignment and the stream/size accounting. Discrepancy: my TEMP maximum is 148 B vs 164 B declared (writer-side reservation; not needed for decoding, treat the declared size as authoritative).

## 4. XAsset type enum (IW3 1.7, verified against real files)

`XAssetType` (OAT `src/Common/Game/IW3/IW3.h`); numbers confirmed by parsing real asset lists (e.g. first entries of `mp_*_load.ff` are type 5 x2, 4 x3, 31; map zones carry 11/12/14/16).

```
 0 xmodelpieces*   1 physpreset      2 xanimparts     3 xmodel        4 material
 5 technique_set   6 image           7 sound (alias list)             8 sound_curve
 9 loaded_sound   10 clipmap        11 clipmap_pvs   12 comworld     13 gameworld_sp
14 gameworld_mp   15 map_ents       16 gfxworld      17 light_def    18 ui_map*
19 font           20 menulist       21 menu          22 localize     23 weapon
24 snddriver_globals               25 fx            26 impact_fx    27 aitype*
28 mptype*        29 character*     30 xmodelalias*  31 rawfile      32 stringtable
```
`*` = defined in the enum, no loader in OAT (`LoadXAsset` throws `UnsupportedAssetTypeException` for them and treats `snddriver_globals` as a no-op skip). **None of these appear in the asset lists of any of the 72 zones** except `snddriver_globals` (in `code_post_gfx*.ff`).
The Zeroy wiki table has `pixelshader=5, techset=6, image=7, ...` which is shifted by one from 1.7: that page documents an earlier version. In 1.7 there are **no top-level pixel/vertex shader assets**: shaders are nested inside technique passes (section 6.5). `clipmap` (10, SP) and `clipmap_pvs` (11, MP) are loaded by the same code (`clipMap_t`).

### Asset-list contents by type (verified, own parser on the asset list)

Top-level entries only. Nested assets (materials, images, ...) are referenced from within and do not show up here.

| type | common_mp | mp_crash |
|---|---|---|
| technique_set | 154 | 226 |
| material | 9 | 1 |
| image | 0 | 0 |
| xanimparts | 1,125 | 1 |
| xmodel | 597 | 297 |
| sound (alias lists) | 0 | 149 |
| clipmap_pvs | 0 | 1 |
| comworld | 0 | 1 |
| gameworld_mp | 0 | 1 |
| gfxworld | 0 | 1 |
| light_def | 1 | 2 |
| menulist | 34 | 0 |
| weapon | 116 | 0 |
| fx | 281 | 135 |
| impact_fx | 1 | 1 |
| rawfile | 277 | 7 |
| **total** | **2,595** | **823** |

Across all 72 zones the top-level counts are: sound 81,402; xmodel 14,281; localize 10,920; xanimparts 10,464; technique_set 9,918; fx 6,728; rawfile 1,987; material 1,051; weapon 615; image 58; light_def 50; menulist 45; comworld/gfxworld 43 each; stringtable 32; impact_fx 29; gameworld_sp/clipmap 22 each; gameworld_mp/clipmap_pvs 21 each; font 18; physpreset 11; plus 1-2 each of sound_curve and snddriver_globals. Never at top level in any zone: map_ents, menu, loaded_sound, pixel/vertex shaders.

### Full per-type counts including nested assets (OAT `Unlinker --list`, 0 warnings / 0 errors on both zones)

| type | common_mp | mp_crash |
|---|---|---|
| xanim | 1,125 | 1 |
| xmodel | 599 | 300 |
| material | 502 | 362 |
| image | 404 | 823 |
| technique set | 154 | 226 |
| fx | 281 | 135 |
| rawfile | 277 | 7 |
| menu | 277 | 0 |
| weapon | 116 | 0 |
| menulist | 34 | 0 |
| physpreset | 10 | 21 |
| sound (alias list) | 0 | 149 |
| loaded sound | 0 | 71 |
| sound curve | 0 | 6 |
| light def | 1 | 2 |
| impact fx | 1 | 1 |
| map ents | 0 | 1 (nested in clipMap) |
| clipmap / comworld / gameworld_mp / gfxworld | 0 | 1 / 1 / 1 / 1 |

OAT listed no localize/font/stringtable in these two zones, consistent with the top-level survey (localize/font/stringtable live in `common.ff`, `code_post_gfx*.ff`, `localized_*.ff`).
Pixel/vertex shaders and vertex declarations are not counted by OAT's asset list (sub-assets).

## 5. What is external to the fastfile

### 5.1 Images
`GfxImage` in the zone is a 36-byte header (name, dimensions, `mapType`, semantic, category flags) plus a `GfxImageLoadDef` (16-byte header + `resourceSize` bytes of data) in the TEMP block. For normal images `resourceSize` is 0: **pixel data is not in the fastfile**. It is read from `images/<name>.iwi` inside the `*.iwd` archives (6,565 `.iwi` in `main/*.iwd`; the sample I opened is `IWi` magic + version byte 6). Confirmed: OAT dumped 781 of mp_crash's images as DDS from IWD IWIs in one run (239 MB, same order as `externalSize`), and 801 of the 823 names exist as IWI files in the IWDs.
The remaining 22 are engine-generated or inline: 21 `*lightmapN_primary/_secondary` and `*reflection_probeN` images (19 probes + 1 lightmap pair; 8.8 MB) were dumped by OAT from data **in the zone**, plus `$outdoor`; `$white`, `$identitynormalmap`, `falloff_linear` etc. are special/auto-generated names. **Unknown (needs decode of GfxWorld/GfxImage runtime paths or decompile):** exactly which block holds the inline lightmap/probe texel data and its format fields. IWI format itself is a separate ticket's matter (header per file: format, flags, w/h/depth, 4 mip-offset sizes).

### 5.2 Sounds
- `snd_alias_list_t` / `snd_alias_t` are in the zone (names, volume/pitch/distance, curve and speaker map refs, a `SoundFile` selecting loaded vs streamed).
- **Loaded sounds** (`SAT_LOADED`) have their data inline in the zone: `MssSound` = `AILSOUNDINFO` (format, rate, bits, channels, samples, block_size, data_len) + data bytes. OAT dumps them as RIFF WAV; sample checked: PCM, 1 channel, 48 kHz, 16-bit.
- **Streamed sounds** (`SAT_STREAMED`) are not in the zone: they are named by `StreamedSound {dir, name}` and read from `sound/**.mp3` etc. in IWDs. The IWDs hold 16,993 `.wav` and 119 `.mp3` under `sound/` (wavs are standalone loose-file versions; mapping alias->file is by `dir/name`, details **not verified**).
- 15 `accuracy/*.accu` files live in the IWDs (weapon accuracy graphs referenced by name from WeaponDef).

### 5.3 Script and config data
`rawfile` payloads are **plain text**, not bytecode: `maps/mp/*.gsc`, `maps/createfx/*.gsc`, `mptype/*.gsc` (with CRLF line endings and `//` comments), plus `*.vision` and the mapents text. `common_mp.ff` has 277 rawfiles (124 `.gsc`, 59 under `maps/mp`, the rest `vision/*.vision` etc.). So the engine must compile GSC source at load time (a separate ticket; this answers "where does GSC live": the fastfile, as text).
`MapEnts.entityString` is the standard `{ "key" "val" }` entity text (mp_crash: ~43.7 KB), embedded in `clipMap_t` (not a top-level asset).

## 6. Struct layouts: where they are and what is known

The authoritative layouts are OAT's `IW3_Assets.h` (GPL-3.0; read for facts, do not copy). Struct start lines in `src/Common/Game/IW3/IW3_Assets.h` at commit `59904a3e`:

PhysPreset 154, XAnimParts 288 (XAnimDeltaPart 266), XModel 528 (XSurface 409, XModelCollSurf_s 445, BrushWrapper 480), Material 988, MaterialTechniqueSet 1440 (MaterialTechnique 1369, MaterialPass 1345, MaterialVertexShader 1320, MaterialPixelShader 1339), GfxImage 1520, SndCurve 1537, snd_alias_t 1591, snd_alias_list_t 1618, MssSound 1638, LoadedSound 1644, clipMap_t 1824, ComWorld 1896, PathData 2020, GameWorldSp 2034, GameWorldMp 2040, MapEnts 2045, GfxLightDef 2094, GfxWorld 2418, Font_s 2489, MenuList 2499, itemDef_s 2805, menuDef_t 2851, LocalizeEntry 2879, WeaponDef 3132, FxElemDef 3646, FxEffectDef 3686, FxImpactTable 3704, RawFile 3710, StringTable 3717.

OAT status: the loader decodes **every** type above (`LOAD_ASSET` list) - this is proven for the two zones by 0 errors with all nested content. OAT's *dumping* is partial (no clipMap/GfxWorld/ComWorld/GameWorld/FX/SoundAlias dumpers in its support table), but dumping is irrelevant to decoding.
Each type's decode rules (counts, strings, blocks, reusable members, reorderings) are in `src/ZoneCode/Game/IW3/XAssets/<Type>.txt` (short, readable). Key points per requested type:

| asset | header struct highlights | variable-size data (count expression) | blocks / notes |
|---|---|---|---|
| **RawFile** (12 B) | `name, len, buffer` | `buffer`: `len + 1` bytes | header TEMP. **Verified** (own parser + OAT dump). |
| **StringTable** | `name, columnCount, rowCount, values**` | `values`: `columnCount*rowCount` strings | header is **not** TEMP ("not in the temp block for some reason" per OAT comment) |
| **LocalizeEntry** | `value, name` (both strings) | - | TEMP |
| **MapEnts** | `name, entityString, numEntityChars` | `entityString`: `numEntityChars` | nested in clipMap |
| **Material** (80 B, align 4) | `MaterialInfo` (name, gameFlags, sortKey, atlas rows/cols, 8-byte `GfxDrawSurf`, `surfaceTypeBits`, `hashIndex`), `stateBitsEntry[34]`, `textureCount/constantCount/stateBitsCount/stateFlags/cameraRegion`, ptrs `techniqueSet, textureTable, constantTable, stateBitsTable` | textureTable 12 B x textureCount (`MaterialTextureDef`: nameHash, sampler state bitfield, semantic, `image*` or `water_t*` when semantic = 0xB); constantTable 32 B x constantCount (align 16); stateBitsTable 8 B x stateBitsCount | **Verified**: 80 B size and member order via byte-exact decode of 63 materials in the 21 load zones |
| **MaterialTechniqueSet** (148 B) | `name, worldVertFormat, hasBeenUploaded, remappedTechniqueSet (never serialized), techniques[34]` | per technique: 8 B header (`name, flags, passCount`), `passCount x 20 B MaterialPass` (vertexDecl*, vertexShader*, pixelShader*, perPrim/perObj/stable arg counts, customSamplerFlags, args*), then per pass: decl (100 B), VS/PS (16 B header + `programSize*4` bytes of code), args (8 B each; literal-const args carry a 16-byte vec4) | **Verified** byte-exact on 42 techsets in load zones (they include shaders and decls where used) |
| **Pixel/vertex shaders** | `{name, prog{ptr, loadDef{program*, programSize(u16 dwords), loadForRenderer}}}` | `programSize` u32 words | **D3D9 shader model 2/3 bytecode** (see 6.1) |
| **GfxImage** (36 B) + `GfxImageLoadDef` (16 B + data) | `mapType (1,2=invalid; 3=2D, 4=3D, 5=cube), picmip, semantic, cardMemory, width/height/depth, category, delayLoadPixels, name` | loadDef: levelCount, flags, 3 dims, `format` (D3D format code), `resourceSize` | **Verified** (36 B header; data inline only for lightmaps/probes/generated, else 0) |
| **XModel**, **XAnimParts**, **PhysPreset** | see command files: bone arrays by `numBones`/`numRootBones`, `surfs` x `numsurfs`, collision tree, phys geoms; anim data arrays by `dataByteCount/dataShortCount/dataIntCount`, delta parts, notifies | verts/indices live in **VERTEX/INDEX** blocks (`XSurface.verts0`, `triIndices`); `XSurface` loads `zoneHandle, vertInfo, verts0, vertList, triIndices` in that order | Decoded by OAT on 597 xmodels + 1,125 anims (no errors). Layout not independently decoded by me. |
| **clipMap_t** (MP uses type 11) | planes, brushes, nodes, leafs, leafbrush nodes, collision partitions/aabb trees/verts/tris, cmodels, visibility `numClusters*clusterBytes`, `MapEnts*`, dynent defs | many `set count` rules; **RUNTIME** block for dynEnt pose/client/coll lists | One-time `reorder` for leafs/leafbrushes/leafbrushNodes |
| **ComWorld** | `name, isInUse, primaryLightCount, primaryLights*` | `primaryLights`: `primaryLightCount`, each with `defName` string | - |
| **GameWorldMp** | **only `name`** | none | MP "game world" has no path data; SP has `PathData` (nodes, chains, visibility, node tree) |
| **GfxWorld** | name/baseName, plane/node/index counts, sky surfs, sun light, reflection probes, lightmaps, cells with AABB trees/portals/cullgroups, light grid, vertex data (`vd`) and layer data (`vld`), static models, DPVS static/dynamic, shadow geometry, light regions, primary-light shadow-visibility arrays | very many; vertex/layer buffers in zone, many runtime arrays in **RUNTIME** | Decoded by OAT on mp_crash (no errors) |
| **GfxLightDef**, **Font_s** | light def name+attenuation image; font glyph array (`glyphCount`) + material refs | - | TEMP |
| **snd_alias_list_t** | `aliasName, head*, count` | `head`: `count` x `snd_alias_t` (strings + `soundFile`, `speakerMap`, curve) | `SoundFile.type`: 1 loaded, 2 streamed |
| **LoadedSound / MssSound** | name, `AILSOUNDINFO`, data | `data`: `data_len` | in TEMP; `data_ptr/initial_ptr` skipped |
| **WeaponDef** | huge fixed struct (~hundreds of fields), many strings, `xanim` names (as `assetref`), notetrack sound maps by script string, 29-entry bounce sound table, accuracy-graph knot arrays | counts in `...KnotCount` | OAT comment: original-knots arrays reuse the non-original counts |
| **MenuList / menuDef_t / itemDef_s** | `menuCount` menus; each menu: window, many strings, `visibleExp`/`rectXExp`... expressions, `items x itemCount`; item type switch selects `listBox/editField/multi/enumDvarName` union member | expression entries: `numEntries`, operand type selects int/float/string | Type enums in header; menus are **not** top-level in IW3 (menulist -> menus nested): common_mp has 34 menulists, 277 menus |
| **FxEffectDef** | `elemDefCountLooping/OneShot/Emission` -> `elemDefs`; per elem: velocity/vis samples by interval counts, visuals union selected by `elemType` (material / model / sound name / effect ref / decal mark array) | trail verts/indices | handles by name (`assetref`) |
| **FxImpactTable** | `name, table[12]` | 12 | - |

### 6.1 Shader bytecode format (matters for the renderer)
Searching the inflated `mp_crash.ff` for D3D9 version tokens gives 230 `vs_3_0` (`0xFFFE0300`), 659 `ps_3_0` and 341 `0xFFFF0200` (ps_2_0) hits, and 702 `CTAB` (D3D constant-table) markers (heuristic byte scan, so possible small false positives). Conclusion: the zone ships **Direct3D 9 SM2/SM3 shader bytecode** with embedded constant tables, **not** HLSL/GLSL source. A wgpu renderer needs a DXBC(SM3 token stream) -> SPIR-V/WGSL translator or a hand-reimplementation of the stock techniques; the shader argument table (`MaterialShaderArgument.type` 0..7 = material/literal/code const or sampler, `dest` register, union index/hash) is what binds engine "code constants" (OAT enum `ShaderCodeConstants`) to registers. This is the main downstream risk for #12-style renderer tickets.

## 7. Open unknowns and whether decompiling `iw3mp.exe` answers them

| Unknown | Answered by decompile? |
|---|---|
| Whether any field in the layouts above is mis-sized for an asset kind OAT loaded without erroring (OAT does not assert "stream consumed" at the end) | Partly: my parser proves it for load zones only. A strict full-zone consumption check (extend the parser or add that assert to a scratch OAT build) is cheap and avoids decompiling. |
| Semantics (not layout) of flags: `gameFlags`, `stateFlags`, `GfxDrawSurf` sort-key bit fields, `TechniqueFlags`, `MaterialStateFlags`, weapon flag bits | OAT names many in headers; remaining semantics need decompile of the renderer/`DB_*`/`Material_*` paths (or the renderer ticket) |
| Meaning/ordering of `ShaderCodeConstants` and `CustomSamplers`, and which techniques MP actually selects | Decompile or render-capture; belongs with the renderer ticket |
| Exact accounting of `externalSize`, TEMP size 164 vs 148 | Not needed to load; `DB_LoadXFile` decompile would answer |
| Inline lightmap/probe image data block and format | Small decode of GfxWorld lightmap path (my parser can be extended) or decompile |
| Streamed-sound lookup rules (`dir/name` -> IWD entry, mp3 vs wav) | Decompile of the sound loader (`SND_*`) or inspection; unverified |
| IWD (zip) / IWI details and `localized_*` zones overlay order | Separate tickets (not fastfile layout) |
| License path for reusing OAT code (GPL-3.0-only vs our or-later) | Policy/ADR decision, not decompile |
| Xbox/"IWff0100" signed variant | Out of scope (PC only); OAT documents an RSA-PSS/SHA-256 auth header |

## 8. Method and reproduction (throwaway, not committed)

1. Python: parse header, inflate with `zlib`, read XFile sizes, XAssetList, string table and asset array for all 72 zones (counts above). Own byte-exact decoder for rawfile/image/material/technique-set/vertex-decl/shader and the pointer/block rules, run over all 21 `*_load.ff`: 21/21 consumed 100% of the stream with VIRTUAL/VERTEX/INDEX/RUNTIME block sizes matching, TEMP off by a constant 16 B.
2. OAT built from a shallow clone with `premake5 gmake` + `make config=release_x64 UnlinkerCli` using `nix shell nixpkgs#premake5 nixpkgs#gnumake`. Local-only patches: `-mmacosx-version-min=14.0`, `-include algorithm -include sstream -Wno-nonportable-include-path`, `-include unistd.h` for bundled zlib, and a `template` keyword in `GlobalAssetPoolsLoader.h`. Then `Unlinker --list` on `common_mp.ff` and `mp_crash.ff`, and targeted dumps of rawfile/mapents/loadedsound/image from `mp_crash.ff` (output discarded).
3. All outputs stayed in `/tmp`; no original content is in this repo.
