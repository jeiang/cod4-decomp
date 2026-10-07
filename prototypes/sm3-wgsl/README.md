# sm3-wgsl: THROWAWAY prototype for ticket #18 (can stock SM3 shaders become WGSL?)

**Run it:** `./run.sh` (full pipeline, ~10 min cold, needs Nix for cargo/meson/flex/bison) or, with the intermediate files already in
`$WORK` (default `/tmp/sm3wgsl-work`): `cargo run --release --bin viewer -- --work $WORK --cod4 /path/to/COD4`.
Nothing derived from the original install is stored in this repo; everything goes to `$WORK`.

Viewer keys: **Tab / T** next material (Shift+Tab previous) · **P** cycle translation path (c → a → b) · **F** only draw the focused material ·
**WASD/QE** move (Shift = fast) · **mouse drag / arrows** look. Window title = material, techset, path, number of bound constants (or the failure);
the full binding table (register ← code constant / material constant / sampler texture, with CTAB names) is printed to stdout on each change.
Headless flags: `--path a|b|c --focus N|name --only-focus --cam x,y,z,yawdeg,pitchdeg --size WxH --shot out.png`, `--survey` (pipeline creation
for every world material and path), `--diff` (same frame through all 3 paths, pixel diff).

What is drawn: the **real mp_crash world** (166,418 vertices, 5,558 surfaces, 134 materials decoded from `mp_crash.ff`), real DXT1/3/5 IWI textures from the IWDs,
shaders = the stock SM3 `lit_sun` (fallback lit/unlit) technique of each material's techset, translated at startup. Not done: cull state (disabled), alpha test, shadows,
real lightmaps/probes/light grid (flat placeholder textures), static models, sky, fog tuning. Code constants are fixed values (sun from the BSP), not the engine's.

Layout: `tools/extract.py` (+`ffparse.py`) shader/techset extractor; `tools/world_extract.py` world decoder; `tools/*_drv.c` MojoShader / vkd3d drivers;
`src/spvsplit.rs` SPIR-V post-passes; `src/sm3.rs` + `src/emit.rs` custom path; `src/paths.rs` per-path reflection; `src/bin/{report,viewer}.rs`.
`RESULTS.txt` holds the measured output.

## Results (662 unique SM3 blobs = 114 VS + 548 PS; 794 unique VS/PS pairs; from the 21 stock MP map zones + common_mp, code_post_gfx_mp, ui_mp and localized MP zones)
Validation = naga SPIR-V/WGSL parse + naga validation (no capabilities) + WGSL writer + re-parse/validate; then real wgpu 30 (Metal, Apple M3 Pro) `create_shader_module`.

| path | tool / license | VS | PS | note |
|---|---|---|---|---|
| a. MojoShader → SPIR-V → naga, no post-pass | MojoShader zlib | 114/114 | 7/548 | `invalid id`: combined image-sampler variables |
| a. + sampler-split post-pass | | 114/114 | 548/548 | 794/794 pairs; wgpu module + Metal pipeline ok |
| b1. vkd3d-shader 1.18 d3dbc → SPIR-V | LGPL-2.1-or-later | 0/114 (107 with PointSize fixup) | 67/548 | 481 PS / 7 VS rejected by vkd3d itself: `nrm`, `lrp`, `pow`, `dp2add` unhandled |
| b1. vkd3d-shader **2.1** + PointSize→Location fixup | LGPL-2.1-or-later | 114/114 | 548/548 | VS raw: 0/114 (naga cannot write `PointSize`); no sampler split needed |
| b2. dxbc-spirv (c7f0697) SM3 | MIT | 0/114 | 0/548 | converts all 662 to SPIR-V 1.6 but naga rejects: PS `DemoteToHelperInvocation`; VS DXVK-style uniform layout (alignment) |
| c. custom SM3 token stream → WGSL (this repo) | own (GPL-3.0-or-later) | 114/114 | 548/548 | needs only 15 VS / ~30 PS opcodes; see histogram in RESULTS.txt |

Extra findings: vkd3d output also needs (1) semantic-based re-location of varyings (it numbers by D3D register), (2) VS/PS bind groups kept apart, (3) interpolant widening to vec4
(it narrows PS inputs to the used components; Metal rejects the mismatch at pipeline creation although naga/WGSL allow it). With those, **all three working paths build a Metal render
pipeline for 134/134 mp_crash world materials and render the same frame within 1–2/255 per channel** (`--diff`), i.e. three independent translators agree; this proves
mutual consistency, not equality with the original D3D9 output (no reference render exists).
