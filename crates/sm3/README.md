# sm3

D3D9 `vs_3_0` / `ps_3_0` bytecode to WGSL. Token parser, CTAB reflection, WGSL emitter. No runtime dependencies; builds for
`wasm32-unknown-unknown`. See the crate docs (`src/lib.rs`) for the generated bind layout.

```rust
let t = sm3::translate(&blob, &sm3::Options::default())?;
// t.wgsl, t.reflection (CTAB names, used constant/sampler registers, semantics)
```

WebGL2-compatible floor: separate texture and sampler bindings, no storage buffers, one `array<vec4<f32>, 256>` uniform
for float constants. Caller hooks (`Options`): `alpha_test` appends a discard on the final alpha of color output 0;
`comparison_samplers` makes the listed sampler registers depth textures with a comparison sampler.

## Tests

`cargo test -p sm3` runs hand-assembled shader tests. With `COD4_PATH` set, `tests/corpus.rs` byte-scans every MP zone
(`mp_*`, `*_mp`) for SM3 version tokens that end in an END token and carry a CTAB, translates each unique blob, and
validates it with naga (no optional capabilities). It expects 662 unique blobs (114 VS + 548 PS) and prints per-zone
translation time with `-- --nocapture`. Nothing from the install is stored.

## D3D9-free cross-check (manual run, skips by default)

`tests/crosscheck` compares our translation against two independent ones that never touch D3D9: MojoShader (zlib) and
vkd3d-shader 2.1 (LGPL), each run as a separate program, so nothing from them is linked into or vendored in this repo.
Both emit SPIR-V, which goes through naga (`spv-in`, after a pass that splits combined image-samplers) to WGSL.

For every unique stock MP blob the test pairs each PS with the first VS that writes every semantic it reads (and each VS
with the first PS that fits), then renders one fixed frame per pair offscreen in wgpu with our WGSL and with each
reference's: a quad, fixed constants (matrices from the CTAB are identity, everything else a deterministic pattern in
[0.2, 0.8], booleans zero), fixed gradient 2D/3D/cube textures with a linear clamp sampler, and per-semantic vertex data. It
asserts that every pixel of every color target agrees within `TOLERANCE` (2/255) per channel; on an Apple M3 Pro (Metal) and an AMD RX 9070 XT (Vulkan/RADV) the measured maximum is 0 for MojoShader and 1 for vkd3d-shader over 658 pairs. It prints the number of
blobs, pairs and comparisons, the max difference per reference, and every pair over tolerance.

```sh
scripts/build-crosscheck-tools.sh            # fetches + builds the two drivers into target/crosscheck-tools (needs nix, git, curl)
B=$PWD/target/crosscheck-tools/bin
COD4_PATH=/path/to/COD4 COD4E_MOJOSHADER_DRV=$B/mojo_drv COD4E_VKD3D_DRV=$B/vkd3d_drv \
  cargo test -p sm3 --test crosscheck --release -- --nocapture
```

The test skips (prints the reason, passes) without either driver variable, `COD4_PATH` or a GPU adapter. CI never runs
it for real: it needs the original install, which CI does not have. It is a manual check, run when the emitter changes.
The driver sources are in `tools/crosscheck/`; MojoShader (`ad5dff8`) and vkd3d 2.1 are downloaded by the script. They are
not in the dev shell: they are only needed for this check and the build is a few seconds of compile once cached.
This proves the translators agree with each other, not that they equal the original D3D9 output.
