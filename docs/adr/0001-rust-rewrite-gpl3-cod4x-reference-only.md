# Rust rewrite; non-compatible sources are reference-only

**Status:** licensing superseded by ADR 0003 (the project is now GPL-3.0-only, and GPL-3.0 sources may be translated). The rest stands.

The engine is a new Rust codebase (wgpu for rendering) and does not fork the C lineage (ioquake3, CoD4X). Reasons: one renderer covers Vulkan, Metal, DX12, and WebGPU; the browser path goes through WASM; and the server gets memory safety. The project is licensed GPL-3.0-or-later, and every file stays under that license.

Code from a source may be translated only if its license allows GPL-3.0-or-later distribution, for example GPL-2.0-or-later (ioquake3), LGPL-2.1-or-later, MIT, BSD, or zlib. All other sources may be read only to learn facts about the original engine (layouts, semantics, names, addresses). Their code is never copied or translated. These reference-only sources include:

- CoD4X (AGPL-3.0).
- OpenAssetTools and gsc-tool (GPL-3.0-only: their README and file headers grant "GPLv3" with no "or later" option). We chose to keep "or later" over translating their struct tables, zone loader, IWI wavelet decoder, and GSC compiler.
- KisakCOD (GPL-3.0-only by its LICENSE file, and apparently derived from decompiling `iw3mp.exe`). We treat it like our own decompilation: a guide to names, semantics, and order of operations, checked against `iw3mp.exe` where it matters. Its code is not translated, which also keeps any provenance claim against it out of this project.

## Consequences

- Every research note records the license of each source it uses, so provenance can be checked later.
- The IW3 asset structs, the zone loader, the wavelet IWI decoder, and the GSC parser are written from our own fact sheets and from reverse engineering of `iw3mp.exe`.
- Functional format constants that are needed to read the original files (codec tables such as the wavelet IWI Huffman codes, enum values, struct layouts) may be committed, including values read from `iw3mp.exe`. Code from the binary, or from any reference-only source, is never copied.
