# Renderer: what stock MP content needs, and how it maps to wgpu

Ticket: jeiang/cod4-decomp#6. Scope: stock MP only (IW3 1.7). Everything below is tagged **[V]** verified against the real files in `COD4/` (or by running a tool), **[S]** taken from a published source (named), or **[I]** inference, not verified.

No original-install content is reproduced here: only counts, format ids, identifier names, and register/constant names.

## 0. Method and sources

| Source | Used for | License | Reuse |
|---|---|---|---|
| OpenAssetTools (Laupetin/OpenAssetTools, `src/Common/Game/IW3/IW3_Assets.h`, `ZoneCode/Game/IW3`, `ObjImage/Image/IwiTypes.h`) | Struct layouts, enums, IWI header, zone loader, Unlinker used to list and dump assets | GPL-3.0 (GitHub SPDX `GPL-3.0`; `LICENSE` is the GPLv3 text; only one file, `DxgiFormat.h`, carries "or later" wording) | **Code reusable under GPL-3.** Because it is GPL-3.0 and not clearly "or later", anything copied ties that part to GPL-3.0-only. ADR owner should decide whether to reuse code or treat it as a fact source. |
| wgpu / naga (gfx-rs/wgpu, `naga/README.md`, `wgpu-types/src/features.rs`) | Frontends, backends, texture features | Apache-2.0 and MIT dual (`LICENSE.APACHE`, `LICENSE.MIT`) | Dependency |
| MojoShader (icculus/mojoshader) | D3D9 bytecode to GLSL and SPIR-V; built and run locally | zlib | Reusable in GPL-3 |
| DXVK (doitsujin/dxvk, `src/d3d9`) | D3D9 SM3 handling in a Vulkan translation layer | zlib | Reusable |
| dxbc-spirv (doitsujin/dxbc-spirv, `sm3/`) | SSA compiler for SM1-5.1 to SPIR-V, used by current DXVK | MIT | Reusable |
| vkd3d-shader (Wine vkd3d, repo.or.cz/vkd3d.git) | SM1-3 bytecode to SPIR-V | LGPL-2.1-or-later (from the project README via search; LICENSE not read) | Linkable from GPL-3; a C library |
| CoD4X | **Not used** | AGPL | Fact source only, never consulted for this sheet |
| Microsoft D3D9 token format | Opcode and register encodings | Proprietary docs | Not copied; encodings were validated by parsing all 1,377 shaders (below) |

Tooling used, all temporary under `/tmp/cod4-renderer`: OAT `Unlinker` built from source (needed a few macOS build fixes only), a throwaway patch to its RawFile dumper to print `GfxWorld`/`XModel`/`FxEffectDef` fields, Python scripts for the counts, MojoShader built with `clang`, and `naga-cli` 30.0.1 via `cargo install`. None of it is committed.

Zones analysed: `mp_crash.ff`, `common_mp.ff` (requested), plus `code_post_gfx_mp.ff` (holds the post-process techsets) and a count-only pass over all 21 stock MP map zones, `common_mp`, `code_post_gfx_mp`, `ui_mp`.

## 1. Fastfile container (only what the renderer chain needs)

- **[V]** `IWffu100`, version 5, zlib stream from offset 12. Decompressed stream starts with 11 uint32: `size`, `externalSize`, 9 block sizes, then the `XAssetList` (`stringCount`, ptr, `assetCount`, ptr). mp_crash: 63,106,889 B decompressed, 372 script strings, **823 assets**; common_mp: 41,520,538 B, 591 strings, 2,595 assets. Asset counts match the Unlinker listing exactly.
- **[S]** Asset type ids: `TECHNIQUE_SET=5`, `GFXWORLD=0x10`, `MATERIAL=4`, `IMAGE=6`, `XMODEL=3`, `XANIMPARTS=2`, `FX=0x19`, `LIGHT_DEF=0x11` (OAT `IW3_Assets.h`).

mp_crash assets by type **[V]** (Unlinker `--list`): image 823, material 362, xmodel 300, techniqueset 226, sound 149, fx 135, loadedsound 71, physpreset 21, rawfile 7, soundcurve 6, lightdef 2, xanim 1, mapents 1, impactfx 1, gfxworld 1, gameworldmp 1, comworld 1, clipmap 1.
common_mp: xanim 1125, xmodel 599, material 502, image 404, fx 281, rawfile 277, menu 277, techniqueset 154, weapon 116, menulist 34, physpreset 10, lightdef 1, impactfx 1.

## 2. The asset chain (what draws a surface)

```
GfxWorld ──surface──► Material ──► MaterialTechniqueSet ──[34 slots]──► MaterialTechnique
   │                    │ textureTable (GfxImage*)                         └─ pass: VertexDecl routing,
   │                    │ constantTable (literals)                            VertexShader bytecode,
   │                    └ stateBitsTable (blend/depth/stencil/cull)           PixelShader bytecode,
   │                                                                          args[] (register bindings)
   ├ lightmaps[] (primary + secondary GfxImage), light grid, reflection probe cubemaps
   └ cells / portals / aabb trees / static model instances → XModel → XSurface → Material
```

**[S]** (OAT `IW3_Assets.h`, field names exact):

- `Material` = `MaterialInfo` (name, `sortKey`, `gameFlags`, `drawSurf`, `surfaceTypeBits`), `stateBitsEntry[34]` (one index per technique slot), `textureCount`, `constantCount`, `stateBitsCount`, `cameraRegion`, then pointers to `MaterialTechniqueSet`, `MaterialTextureDef[]`, `MaterialConstantDef[]`, `GfxStateBits[]`.
- `MaterialTechniqueSet` = `name`, `worldVertFormat`, `remappedTechniqueSet`, `techniques[34]`. The 34 slots are the `MaterialTechniqueType` enum: depth prepass, build floatZ, build shadowmap depth/color, unlit, emissive, emissive shadow, then **lit** (7..0x14): lit, lit sun, lit sun shadow, lit spot, lit spot shadow, lit omni, lit omni shadow, and the same seven again as *instanced*; then light spot/omni/spot-shadow, fakelight normal/view, sunlight preview, case texture, wireframe solid/shaded, shadowcookie caster/receiver, debug bumpmap (+instanced).
- `MaterialTechnique` = `name`, `flags`, `passCount`, `passArray[]`. `MaterialPass` = `vertexDecl`, `vertexShader`, `pixelShader`, `perPrimArgCount`, `perObjArgCount`, `stableArgCount`, `customSamplerFlags`, `args`.
- `MaterialShaderArgument` = `type`, `dest` (register), union. Types: material vertex const, literal vertex const, material pixel sampler, code vertex const, code pixel sampler, code pixel const, material pixel const, literal pixel const. "Code" args are engine-supplied (matrices, light params, fog, samplers like the shadow map); "material" args come from the material's textures/constants. This table is what binds the bytecode's `c#`/`s#` registers to engine data.
- Shader program = raw D3D9 token stream: `GfxPixelShaderLoadDef { unsigned* program; uint16 programSize; }`. The `IDirect3D*Shader9*` is a runtime pointer, not in the zone.
- `GfxStateBits.loadBits` is 8 bytes: src/dst blend RGB and alpha (11 `GfxBlend` values), blend ops (add, sub, revsub, min, max), alpha test (2 bits), cull face, color write masks, polymode line, depth write, depth test, polygon offset, and front/back stencil (op pass/fail/zfail, func). All map directly onto `wgpu::RenderPipeline` blend/depth/stencil/cull state. Alpha test has no hardware equivalent in wgpu: it becomes `discard` in the pixel shader or a specialization constant.
- `MaterialTextureDef` = `nameHash`, `samplerState` (filter 3 bits, mipMap 2, clampU/V/W), `semantic` (2D, function, color map, normal map, specular map, water map), and a union (`GfxImage*` or `water_t*`).
- Vertex routing: `MaterialVertexStreamRouting.data[16]` = (source, dest) pairs.

**[V]** Technique files dumped from the zones show every technique is **one pass** (994 of 995 in the two zones, one has two) and every one carries the stateMap `passthrough` (the dumper prints a "TODO"; the per-material state lives in `GfxStateBits`). Vertex inputs in the 995 techniques come from only these code streams: position, color, texcoord[0] (+texcoord[1] for lightmap UV), normal, tangent. So there are very few vertex layouts.

### 2.1 Where the colors come from (lighting model, from shader and sampler names)

**[V]** Sampler names in the shader constant tables (CTAB) across all stock MP shaders, with D3DX class: `colorMapSampler` (2D, 916 shaders), `normalMapSampler`, `specularMapSampler`, `detailMapSampler`, `lightmapSamplerPrimary`, `lightmapSamplerSecondary`, `reflectionProbeSampler` (**cube**), `modelLightingSampler` (**3D / volume**), `attenuationSampler`, `shadowmapSamplerSun`, `shadowmapSamplerSpot`, `floatZSampler`, `outdoorMapSampler`, `skyMapSampler` (cube), `shadowCookieSampler`, `colorMapPostSunSampler`.

**[I]** Reading those with the shader name prefixes:

- `lm_*` shaders (210 PS, 52 VS) are **lightmapped world** shading (primary + secondary lightmap samplers).
- `lp_*` shaders (448 PS, 188 VS) are **light-probe/grid shading for models** (sample the 3D `modelLightingSampler`, a volume texture; the world light grid in `GfxWorld.lightGrid` is the data source).
- `l_*` (60 PS, 24 VS) are per-light additive passes (the `light spot/omni` technique slots).
- `sun`, `spot`, `omni` pick the dynamic light type; `sm` = shadow-map variant; `hsm` = a second shadow-map variant, **most likely** the hardware depth-texture one [I] (the zone has separate `build_shadowmap_depth` and `build_shadowmap_color` techniques).

Techset-name tokens **[V by listing; meaning [I]]**: `{mc|wc}_l_{sm|hsm|<none>}_{b|r|t}0c0[d0][n0][s0]` plus `skin` and `flag` for model-only variants. `mc`/`wc` = model/world with vertex color (`MaterialType` enum in OAT: `m_`, `mc_`, `w_`, `wc_`). `c0` is the color map; `d0` detail map, `n0` normal map, `s0` specular map. The leading `b`/`r`/`t` letter is the base blend mode of the color layer (names suggest blend/replace/add; **not confirmed**).

## 3. Techset families in stock MP

Each techset exists twice: `X` (SM3 path) and `sm2/X` (SM2 fallback path). **[V]** Across all 402 techset names in stock MP zones, exactly 201 are `sm2/`-prefixed and every `sm2/X` has an `X`. **[S]** `remappedTechniqueSet` in the struct is the link between them; the pairing semantics is **[I]**.

### 3.1 The two requested zones

| Zone | techset assets | SM3 names | `sm2/` twins |
|---|---|---|---|
| mp_crash | 226 | 113 | 113 |
| common_mp | 154 | 77 | 77 |
| shared by both | 114 | 57 | 57 |
| union of the two | 266 | **133** | 133 |
| code_post_gfx_mp (post/UI, loaded in all MP) | 94 | 47 | 47 |
| union of all three zones | 360 | **173** SM3 families | 173 |

mp_crash only uses **15** distinct techsets on its 5,558 world surfaces **[V]** (see 4.1).

### 3.2 All stock MP zones (21 map zones + common_mp + code_post_gfx_mp + ui_mp)

**[V]** 402 techset names, **201 SM3 families** (155 across the 21 map zones alone; each map holds 78-119). Breakdown of the 201:

- Lit family **98**: `mc_l_*` 56, `wc_l_*` 42 (in the two requested zones the lit permutations are 92 = 56 mc + 36 wc, spanning sm/hsm/none x b/r/t x d0/n0/s0 + skin/flag).
- Unlit and effects: `effect*` 20 plus `mc_effect*` 7, `particle_cloud*` 4, `distortion*` 2 (+2 wc), `wc_unlit*` 9, `mc_unlit*` 6, `mc_ambient`, `mc_reflexsight`, `mc_shadowcaster`, `wc_sky`.
- Post/utility (mostly code_post_gfx_mp): `filter_symmetric_1..8` 8, `glow_*` 4, `postfx*` 4, `passthru_*` 3, `dof_*` 2, `shell_shock*` 2, `pixel_cost_*` 2, plus `cinematic`, `color_channel_mixer`, `clear_alpha_stencil`, `depthprepass`, `floatz`, `floatzdisplay`, `processed_floatz`, `shadowcaster`, `shadowclear`, `shadowcookieblur`, `shadowcookieoverlay`, `shadowoverlay`, `stencilshadow`, `stencildisplay`, `small_blur`, `default`, `2d`, `tools`.

The names beginning with `,` (for example `,wc_l_sm_b0c0` on some mp_crash surfaces and in the listing) are unexplained **[V that they exist; meaning unknown]**.

### 3.3 Techniques and shaders behind them

**[V]** Across all stock MP zones: 1,123 unique technique names, **1,377 unique shader names** (1,029 PS + 348 VS, 1.95 MB bytecode). 91 shader names have different bytes in different zones (same name, different content), so the corpus is not strictly name-keyed.

Shader models by name **[V]** (version token of each blob): PS 3.0 714, PS 2.0 307, PS 1.1 8; VS 3.0 212, VS 2.0 111, VS 1.1 25.

Which path needs what **[V]** (shaders referenced by the techniques of each techset family): **SM3 path 299 VS + 811 PS** (855 distinct VS/PS pairs); **SM2 path 180 VS + 371 PS**. The SM3 path is the only one that needs 3.0 shaders; it also pulls in a few 1.1/2.0 utility shaders.

Bytecode characteristics **[V]** (opcode scan of all shaders; tokens validated as MojoShader parsed all 1,377 with zero errors in both its GLSL and SPIR-V profiles):

- ps_3_0 (649 in common_mp + mp_crash): median 70 instructions, max 163. Opcode set: `mov add mul mad dp3 dp4 dp2add rcp rsq nrm pow exp abs max cmp lrp texld texldl texkill` plus **static `if/else/endif`** in 116 shaders and **`dsx/dsy`** in 2. No `loop`/`rep`, no `vPos`/`vFace` (checked via `dcl` register types), no subroutines.
- vs_3_0: median 54 instructions, max 102; `mad dp3 dp4 rcp rsq exp frc sincos` only. No texture fetch in any VS.
- ps_2_0 median 47, max 91; vs_2_0 median 46, max 97; the 8 PS 1.1 and 25 VS 1.1 shaders are tiny debug/utility variants (VS 1.1 length is not encoded in tokens, so a naive scan misparses them).
- **No bone-matrix constants appear in any VS constant table** (30 distinct VS constant names: world/viewProjection/worldViewProjection matrices, fog, shadow lookup, base lighting coords, time, falloff, feather, etc.). **[V]** So GPU skinning is not in the shaders **[I: skinning is done on the CPU into pre-skinned vertices]**; the `XSurface` data (`vertInfo.vertsBlend`, `XRigidVertList`, `partBits`) is consistent with that.
- Sampler dimensions in ps_3_0 declarations: 2D 2,419, cube 289, volume 424 (`dcl_2d/_cube/_volume`).

Pixel constants seen (CTAB): `fogColor lightingLookupScale lightDiffuse lightPosition lightSpotDir lightSpotFactors envMapParms sunDiffuse spotShadowmapPixelAdjust sunPosition detailScale lightSpecular lightFalloffPlacement shadowmapSwitchPartition shadowmapScale sunSpecular filterTap featherParms colorBias colorTintBase colorTintDelta shadowmapPolygonOffset particleCloudColor dofEquationScene dofEquationViewModelAndFarBlur dofLerpBias dofLerpScale glowSetup glowApply renderTargetSize shadowParms`. These correspond to the `ShaderCodeConstants` enum in OAT (light, nearplane, shadow, DoF, ...). The full engine-side constant list is the work item for the renderer design.

## 4. Stock MP content the renderer must draw

### 4.1 GfxWorld (mp_crash, **[V]** from the loaded struct via the patched Unlinker)

| Field | mp_crash |
|---|---|
| planeCount / nodeCount | 11,400 / 5,416 |
| indexCount / vertexCount | 347,493 / 166,418 (`GfxWorldVertex`: xyz, binormalSign, packed color, texCoord[2], lmapCoord[2], packed normal and tangent) |
| surfaceCount (static) | 5,558; no-decal 4,650; lit range 0-5,516, decals 5,516-5,558, emissive none |
| skySurfCount | 5 |
| static models (`smodelCount`) | 4,038 instances of 165 distinct models |
| cells (`dpvsPlanes.cellCount`) | 23 (cellBitsCount 16); cull groups 0 |
| lightmaps | **1 array** (primary + secondary image) |
| reflection probes | 19 (64x64 cubemaps), per-surface probe index used, usage skewed (probe 13 on 1,789 surfaces) |
| primary lights / sun | 5 primary lights, sun valid, `sunPrimaryLightIndex` 1; `sunflare_t` present |
| light grid | `hasLightRegions` true, 30,087 entries, 12,406 colors, 14,616 B of row data |
| brush models | 34 (`modelCount`) |
| materials on world surfaces | 134 distinct, with 15 distinct techsets: `wc_l_sm_r0c0n0s0` 4,083 surfaces, `wc_unlit_multiply` 378, `wc_l_sm_b0c0n0s0` 347, `wc_l_sm_r0c0` 243, `wc_l_sm_r0c0n0` 149, `wc_l_sm_b0c0n0` 103, `wc_l_sm_r0c0d0n0s0` 75, `wc_l_sm_t0c0n0s0` 73, `wc_l_sm_b0c0d0n0` 30, `wc_unlit_distfalloff` 30, `wc_l_sm_b0c0` 18, `wc_unlit_falloff_add` 12, `wc_sky` 5, plus two minor `s0` ones |
| surfaces without a lightmap | 0 |
| `GfxSurface` size | 56 bytes (OAT struct, x86) |

Structure **[S]** (OAT): `GfxCell` has mins/maxs, `GfxAabbTree[]` (children, surface ranges incl. a no-decal split, static-model index lists), `GfxPortal[]` (plane, target cell, vertices, hull axes) and per-cell reflection probe ids. `GfxWorldDpvsStatic` carries the visibility byte arrays (`smodelVisData`, `surfaceVisData` x3), LOD data, sorted surface index, `surfaceCastsSunShadow`. `GfxWorldVertexLayerData` holds extra layer data (4 bytes here).

Visibility **[I]**: PVS-style cell/portal culling plus per-surface and per-model vis bits, precomputed in the map and not needing a runtime BSP/PVS build.

Only one lightmap array in mp_crash means a world draws with at most two large samplers; larger maps may have several (not measured).

### 4.2 Images

**[V]** Image source split for mp_crash's 823 `GfxImage` assets: **801 are IWI files in the IWDs** (`images/<name>.iwi`, 6,561 IWIs exist across the install's IWDs); **22 live in the zone** with an inline `GfxImageLoadDef`: 19 reflection probes, 2 lightmap images, and `$outdoor`. common_mp: 404 of 404 from IWDs. Total IWI payload referenced by mp_crash is 244 MB and common_mp 105 MB versus a 63 MB / 41 MB decompressed zone, so IWD-backed pixel data is not inline.

IWI header **[V]** (matches OAT `iwi6`): tag `IWi`, version byte **6**, then `format` (1 B), `flags` (1 B), `dimensions[3]` (u16), `fileSizeForPicmip[4]` (u32). Example: a 512x512 DXT1 colour map reads 174,804 / 43,732 / 10,964 / 2,772 for the four picmip sizes.

IWI `format` values **[S]** (OAT): 1 BGRA8, 2 BGR8, 3 L8A8, 4 L8, 5 A8, 6-10 wavelet RGBA/RGB/LA/L/A, 11 **DXT1**, 12 **DXT3**, 13 **DXT5**, 14 DXN (BC5). Flags: nopicmip, nomipmaps, cubemap, volmap, streaming, legacy normals, clampU, clampV (+dynamic/rendertarget/systemmem).

Format counts of IWD-backed images **[V]**:

| Format | mp_crash (801) | common_mp (404) | All 6,561 IWIs in the install |
|---|---|---|---|
| DXT5 (13) | 528 | 255 | 3,967 |
| DXT1 (11) | 249 | 115 | 2,061 |
| DXT3 (12) | 18 | 30 | 457 |
| BGR8 (2) | 2 | 0 | 13 |
| L8 (4) | 2 | 1 | 5 |
| L8A8 (3) | 0 | 2 | 20 |
| BGRA8 (1) | 0 | 0 | 13 |
| **Wavelet (6-9)** | **2** (RGBA, UI icons) | **1** (L, a beam) | 25 (6:20, 7:2, 8:2, 9:1) |
| DXN (14) | 0 | 0 | 0 |

DXT1/3/5 make up 98.8% of the install's IWIs. **Wavelet is the one non-trivial codec and does occur in stock MP content**, though rarely (UI/effect images); OAT has a decoder (`IwiWaveletDecoder.cpp`, GPL-3).

In-zone images **[V]** (bytes read from the loaded zone): `*lightmap0_primary` **D3DFMT_L8 (0x32)**, 2 MiB; `*lightmap0_secondary` **D3DFMT_A8R8G8B8 (0x15)**, 4 MiB, no mip chain; `*reflection_probe0..18` **A8R8G8B8**, 64x64, cube, 7 mips (131,064 B each). Cubemaps in IWD form also occur (1 in mp_crash).

wgpu mapping for these:

- DXT1/3/5 → `Bc1/Bc2/Bc3RgbaUnorm[Srgb]`. Requires `Features::TEXTURE_COMPRESSION_BC`. **[S]** `features.rs` lists support for "desktops", "Mobile (All Apple9 and some Apple7 and Apple8 devices)" and WebGPU as an optional feature. Desktop macOS/Linux/Windows targets are covered; in the browser it depends on the adapter, so a software or RGBA8 fallback is needed there **[I]**.
- A8R8G8B8 → `Bgra8Unorm`. L8 and L8A8 have no direct swizzle in wgpu (no texture-view swizzle): `R8Unorm`/`Rg8Unorm` plus a per-sampler swizzle in the shader, or expand to RGBA8 at load **[I]**. BGR8 has no 24-bit format, so expand to RGBA8. 3D light-grid texture → `D3`; cube → `Cube`; wgpu supports both.
- Sampler state (filter, mip, clamp) maps to `SamplerDescriptor`; anisotropy is a renderer option.
- sRGB handling per sampler is not recorded anywhere I checked **[unknown]**: D3D9 sRGB read is a sampler state set by the engine.

### 4.3 XModel, LOD, skinning, XAnim

**[V]** mp_crash: 300 XModels. 251 have 1 or 0 bones (static props), **49 are skinned** (max 71 bones); LOD counts per model: 4 LODs 50, 3 LODs 107, 2 LODs 38, 1 LOD 102, 0 LODs 3. common_mp: 599 models, **1,125 XAnimParts** (weapons, viewmodels and player animation), mp_crash has 1 xanim.

**[S]** `XModel`: bones, root bones, `parentList`, `quats`/`trans` (local pose), `baseMat` (DObjAnimMat), `surfs`, `materialHandles`, `lodInfo[4]` (dist, surfIndex, partBits), collision, `boneInfo`. `XSurface`: `vertCount`, `triCount`, `triIndices` (u16), `verts0` (`GfxPackedVertex`, 16-byte aligned), `vertInfo` (`vertCount[4]` and blend list `vertsBlend`), `vertList` (`XRigidVertList` with boneOffset, vertCount, triOffset), `partBits`.

**[I]** Rendering skinned models: sample the animation into bone matrices on the CPU, blend into a dynamic vertex buffer per frame (no bone constants in VS).

XAnim **[S]**: `XAnimParts` stores quantised bone data (`dataByte/Short/Int`, `indices`, `randomData*`, `names`, notifies, `framerate`, `numframes`). XAnim format specifics are owned by the animation work, not the renderer.

### 4.4 FX and particles

**[V]** FX element types in common_mp (281 effects, 1,346 elements): billboard sprite 699, oriented sprite 23, tail 199, trail 5, cloud 13, model 112, omni light 80, decal 64, runner (sub-effect spawn) 151. mp_crash (135 effect entries, 52 resolved elements) has billboard 34, tail 5, cloud 3, model 4, omni light 2, runner 4. Neither zone has spot-light or sound elements.

**[S]** Struct (OAT): `FxEffectDef` (flags, looping/oneshot/emission counts, `elemDefs`) → `FxElemDef` (spawn, ranges, atlas, `elemType`, vel/vis state samples, visuals = Material / XModel / effect / sound, collision bounds, `effectOnImpact/Death/Emitted`, `trailDef`, `sortOrder`, `lightingFrac`). Particle shading uses the `effect*`, `particle_cloud*` (with outdoor variants) and `zfeather*` (soft particles that read the float-Z target) techsets listed in 3.2. The FX file format and simulation are not renderer work except for drawing these element types.

### 4.5 Shadows, lights, sky, post effects (evidence from zone data)

- **Sun shadows**: techniques `build shadowmap depth`/`color`, `lit sun shadow` plus `sm`/`hsm` shading variants, constants `shadowmapSwitchPartition` and `shadowmapScale` (**[I]** 2-partition cascades), samplers `shadowmapSamplerSun/Spot`. The `GfxWorld` also stores `shadowGeom`, `primaryLightEntityShadowVis`, `surfaceCastsSunShadow`.
- **Dynamic lights**: spot/omni lit techniques, per-light additive `l_*` passes, `attenuationSampler` from `GfxLightDef` (2 lightdefs present).
- **Float-Z / soft particles / depth prepass**: `floatz`, `build_floatz*`, `processed_floatz`, `zfeather*`.
- **Sky**: `wc_sky` techset, `ps_sky`/`vs_sky`, cube `skyMapSampler`, `GfxWorld.skyImage`.
- **Post effects** (techsets in `code_post_gfx_mp`): bloom/glow (`glow_consistent_setup`, `glow_apply_bloom`, `glow_setup`, `glow_apply_sky_bleed`, `filter_symmetric_1..8`), depth of field (`dof_downsample`, `dof_near_coc`, `postfx_dof`, `postfx_dof_color`), `postfx`, `postfx_color`, film (`passthru_film`), `color_channel_mixer`, `shell_shock`/`shell_shock_flashed`, `cinematic`, distortion (`distortion_scale*`), `small_blur`, debug `pixel_cost_*`. Exact pass graph is **[unknown]**; it would come from the `R_*` renderer code in the binary (ticket #10's Ghidra baseline), or by replaying the technique list.
- **Decals**: 42 of mp_crash's world surfaces are decals; FX also has a decal element type.

## 5. Mapping the model to wgpu (what carries over directly)

- **Z range and orientation**: D3D9 clip z is [0,1], NDC y up, texture origin top-left, same as wgpu. Matrices can be reused as-is **[I]**. No shader uses `vPos`/`vFace`, so the D3D9 half-pixel convention does not matter in stock MP shaders **[V]**.
- **Pipelines**: one pipeline per (technique pass, state bits, vertex layout, render target format). Counts above (1,123 techniques, small vertex-layout set, single pass) give a bounded pipeline set that can be created lazily; zone-level caching is possible.
- **Bindings**: material textures + code samplers; 2D, cube and 3D only (no arrays, no storage textures). D3D9 allows at most 16 PS samplers; the maximum actually used per stock shader was not measured, so check it against wgpu's default per-stage texture/sampler limits.
- **State**: blend/depth/stencil/cull/color mask map 1:1; alpha test is a shader/spec-constant feature; polygon offset maps to depth bias; line polygon mode maps to `PolygonMode::Line` (native only feature) or a wireframe technique.
- **Depth shadow maps**: hardware compare needs `textureSampleCompare` with a comparison sampler. D3D9 depth-texture compare is implicit in the sampler format, so a translator cannot know it from bytecode alone; the engine must tag those samplers (the `hsm` shaders) **[I]**.
- **HDR**: the original uses LDR 8-bit targets plus float-Z **[I]**; no HDR target format requirement was observed.

## 6. Shader options for WGSL

### Option A: hand-written WGSL per technique family

- **Scope [V]**: the SM3 path needs 299 VS + 811 PS programs; collapsed by name stems that is about 175 VS stems and 163 PS stems (stem = name with option tokens `b/r/t`, `d0`, `n0`, `s0`, `dtex`, `sm2/sm3` removed). The lit set is a combinatorial product (shadow variant x light type x base-layer x detail/normal/specular x instanced/skin/flag), so it is feasible only with a macro/override-driven template, not 1,000+ separate files.
- **Pros**: native WGSL (no translation step at runtime, readable and debuggable, tunable for wgpu quirks such as shadow comparison); shipping the shaders in the repo with no per-user processing; no dependency on bytecode translators.
- **Cons**: the original HLSL is not available. Reproducing behavior means reading disassembly of ~1,100 programs and rewriting them; any numeric mismatch (lighting model constants, `lightingLookupScale`, fog, specular, shadow cascade selection, `texldl` LOD use in 348 PS) changes how the game looks. Needs a reference image harness. Also a licensing exposure: WGSL written "from" the proprietary bytecode is arguably a translation of it, so a clean-room note is advisable (**flag for ADR 0001**).
- **Effort**: high up-front for the lit families, plus fixed work for ~40 effect/post families. All 21 stock maps are already inside the 201-family figure, so no later stock map adds shaders.

### Option B: translate the D3D9 bytecode

Because the bytecode is in the zone, translation can run on the user's machine at load or first run (and be cached), so no derived shader is shipped in the repo or binary.

Tools:

| Tool | Input | Output | License | Notes |
|---|---|---|---|---|
| naga (wgpu) | SPIR-V, WGSL, GLSL 440+ | WGSL, MSL, HLSL, GLSL, SPIR-V | MIT/Apache | **No D3D9 bytecode frontend.** [S] README |
| MojoShader | SM1-3 bytecode (+effects) | GLSL, HLSL, Metal, ARB, SPIR-V (profile `spirv`/`glspirv`) | zlib | Production use through FNA3D. [S] |
| dxbc-spirv (DXVK uses it for D3D9) | SM1-3 and SM4/5 | SPIR-V 1.6 | MIT | Current DXVK D3D9 path (`d3d9_shader.cpp` includes `sm3/sm3_parser.h`, `sm3_converter.h`) |
| vkd3d-shader | SM1-3 bytecode | SPIR-V (other targets per its README) | LGPL-2.1+ | C library |
| Rust crates | None found: crates.io searches for "d3d9 shader", "dxbc", "mojoshader", "vkd3d-shader", "dxso" return only `fna3d-sys` (MojoShader via FNA3D FFI), a DXBC SM4/5 disassembler and a `dxil-spirv` binding |

**Experiments I ran (all on the real shaders):**

1. MojoShader (built locally with `clang`) **parsed all 1,377 shaders with zero errors** in its `glsl` profile and in its `spirv` profile. Shader models seen include 1.1, 2.0 and 3.0, including 2D/cube/volume samplers and static branching.
2. Linking each of the **855 SM3 (VS, PS) pairs** with `MOJOSHADER_linkSPIRVShaders` and giving the SPIR-V to `naga` 30.0.1: **all 855 vertex shaders pass; only 12 of 855 pixel shaders pass.** The remaining 843 are rejected with `invalid id %N`. Checking `ps_sky`: MojoShader's SPIR-V declares a combined `OpTypeSampledImage` variable in UniformConstant storage and `OpLoad`s it, which is valid Vulkan SPIR-V but not accepted by naga's SPIR-V frontend (**[I]** root cause: combined image-sampler variables; the 12 that pass probably sample no textures, not checked).
3. So the stock bytecode converts, but MojoShader → SPIR-V → naga does not work out of the box for pixel shaders. Open paths: split combined samplers into separate texture+sampler in a SPIR-V post-pass; use dxbc-spirv/vkd3d-shader (which emit separate texture/sampler bindings in the DXVK/vkd3d style) as the front end; or emit WGSL directly from a custom SM3 parser (the instruction set in section 3.3 is small: ~25 opcodes in use, no loops, static branches only).

Pros: **exactly reproduces the original math** (identical to what the game shipped, no re-derivation); handles all current and future stock maps for free; the SM3 corpus uses a small, regular opcode subset; the bytecode already contains constant-table (CTAB) names for every constant and sampler, which gives automatic reflection (name → engine data) that hand-written WGSL would have to hand-maintain; the `MaterialShaderArgument` tables already tell which register gets which engine value.

Cons: a translator is a build or runtime dependency (C library via FFI, or a custom parser in Rust); naga rejects the common output as shown; shadow-compare samplers, `texldl` and alpha-test need hints that bytecode does not carry; translation results are harder to read and tune; the browser spike would need the translator (or pre-translated) output compiled to WASM, or pre-translation of user files at import time; no existing Rust implementation. Translated WGSL could not be shipped in the repo (it derives from the proprietary bytecode), so it must be generated from the user's install and cached, which is the model the project already uses for assets.

### Observations bearing on the decision

- SM2 vs SM3: the SM3 path covers the whole lit set; supporting SM2 additionally would be for old GPUs only and is not needed.
- Both options share the same host-side work: techset/technique/pass/args binding, state bits, vertex layouts, constant upload, shadow/probe/lightgrid resources.
- A hybrid is possible: translate bytecode for the lit/world/model families (the 98 `*_l_*` + unlit/effect families dominate shader count), hand-write the ~40 post-process/utility shaders that are fixed and tiny (filter taps, DoF, glow). The hybrid decision belongs to the owner of ticket #15.

## 7. Open unknowns

1. Exact pass graph and ordering of the frame (depth prepass, shadow cascades, lit opaque, decals, sky, particles/zfeather, post chain, distortion). Comes from the `R_*`/`RB_*` code in `iw3mp.exe` (ticket #10) or by replaying technique order.
2. Meaning of the `,`-prefixed techset names and of the `b/r/t` token in lit techset names.
3. Per-sampler sRGB handling and which samplers are depth-compare (`hsm`).
4. Whether naga can consume SPIR-V from dxbc-spirv or vkd3d-shader for these shaders (not tried; the MojoShader result is the only naga data).
5. `GfxWorld` for the other 20 maps (lightmap array counts, cell counts); only mp_crash was loaded.
6. How the 3D `modelLightingSampler` volume is built from `GfxLightGrid` (runtime-generated, not in the zone), and the `$outdoor` image semantics.
7. Wavelet IWI decode needs a decoder (OAT's is GPL-3, reusable); how common in other map zones was not counted per map (25 wavelet IWIs in the whole install).
8. Water (`TS_WATER_MAP`, `water_t`) and the one `ps_water`/`vs_water` shader: not located in the zones inspected.

## 8. Reproduction notes

- `Unlinker --list` per zone for asset lists; `--include-assets techniqueset,material,image` for techniques, shader blobs and images; IWI/DDS output formats.
- Zone loader needed the install's `main/` IWDs on its search path (it logs them), which is how IWD-backed images were resolved.
- Shader version = high bits of the first dword of each blob (`0xFFFE....` VS, `0xFFFF....` PS); constant tables from the `CTAB` comment token.
- No original-install bytes are part of this document or the branch.
