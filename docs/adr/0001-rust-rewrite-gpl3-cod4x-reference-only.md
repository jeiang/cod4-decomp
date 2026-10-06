# Rust rewrite under GPL-3.0-or-later; CoD4X is reference-only

The engine is a new Rust codebase (wgpu for rendering) and does not fork the C lineage (ioquake3, CoD4X). Reasons: one renderer covers Vulkan, Metal, DX12, and WebGPU; the browser path goes through WASM; and the server gets memory safety. The project is licensed GPL-3.0-or-later. Code under GPL-2.0-or-later or GPL-3.0-compatible licenses (for example ioquake3) may be translated. CoD4X is AGPL-3.0, so it may be read only to learn facts about the original engine; its code is not translated or copied.

## Consequences

- Every research note records the license of each source it uses, so provenance can be checked later.
- Reverse engineering of `iw3mp.exe` fills gaps that compatible sources do not cover.
