# Relicense to GPL-3.0-only; GPL-3.0 sources may be translated

Supersedes the licensing part of ADR 0001. The project moves from GPL-3.0-only to **GPL-3.0-only**, so that code from GPL-3.0-only projects may be translated into it: OpenAssetTools, gsc-tool, and KisakCOD. Play testing found many behaviour gaps, and those projects already encode the original engine's behaviour, so translating from them is faster and more faithful than rediscovering each detail. The owner accepted the loss of the "or later" option.

Rules:

- Code may be translated from sources whose license allows GPL-3.0-only distribution: GPL-3.0(-only or -or-later), GPL-2.0-or-later, LGPL-2.1-or-later or later, MIT, BSD, zlib, Apache-2.0.
- A file that translates code from another project says so in a comment under its SPDX line, naming the project, the source file(s), and its copyright holders, as GPL-3.0 section 5 requires for modified works.
- CoD4X stays reference-only: it is AGPL-3.0, and its terms (network-use source offer) are not adopted.
- Code from `iw3mp.exe` itself is still never copied; functional format constants remain allowed (ADR 0001).

## Consequences

- Every SPDX header and the crate metadata say `GPL-3.0-only`.
- KisakCOD is apparently derived from decompiling `iw3mp.exe`. Translating it carries that provenance into this project. The owner accepted this risk; it is recorded here so it can be revisited.
