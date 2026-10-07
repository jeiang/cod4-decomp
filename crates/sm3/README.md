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

## Dev-only cross-check (not a dependency)

The prototype (branch `prototype/sm3-wgsl`, `tools/mojo_drv.c`, `tools/vkd3d_drv.c`, `tools/build_c_tools.sh`) built
MojoShader (zlib) and vkd3d-shader 2.1 (LGPL) drivers that translate the same blobs to SPIR-V; after a sampler-split pass
naga accepted all 662 from both, and the three translators rendered the same frame within 1-2/255 per channel. To redo
it, build those drivers outside this repo, feed them the blobs the corpus test finds, and diff renders. These tools stay
out of `Cargo.toml` and out of CI.
