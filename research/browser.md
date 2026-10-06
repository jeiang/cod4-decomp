# Browser client: hard limits in 2026 (ticket #9)

Researched 2026-10-06. Scope: a wgpu/WASM **client** reading a user-owned **original install**. Single player and original-network compatibility are out of scope (map #1).
"Verified" = read from a primary source or measured on the real install in `COD4/`. "[INFERENCE]" = my reasoning, not observed.

## 1. Decisions this forces on the native design (read this first)

1. **Memory budget: design for a 32-bit-address, ~1-2 GiB resident client heap; never assume memory64.** wasm32 caps at 4 GiB, memory64 has no Safari release, and `wasm-bindgen` cannot combine memory64 with threads. Chrome/Firefox can do >4 GiB; Safari cannot be relied on.
2. **Fastfile loading must be per-zone streaming with a bounded resident set.** The 48 MP zone files total ~1.55 GiB decompressed; the largest single map zone is 104 MiB. Load `common_mp` + `ui_mp` + localized + one map zone at a time (~225 MiB), never the whole set. Decompression must be incremental (no "inflate whole file into a Vec<u8>" API assumption beyond one zone).
3. **Asset access goes through a random-access byte-source trait** (`read_at(offset,len)` over a file, an OPFS handle, or a browser `File`). IWD (zip) reading must use the central directory plus ranged reads, not mmap or whole-file reads. mmap does not exist in the browser.
4. **No blocking I/O or `std::thread` assumptions on the render/main path.** Threads in the browser need cross-origin isolation (COOP+COEP) and a nightly-only build; the main thread cannot block on a lock or sync I/O. Make threading an optional accelerator (worker-side decompression), with a single-thread-correct path.
5. **Renderer feature floor decision (needed by #12):** WebGPU is not universal (Firefox Linux/Intel Mac, Chrome Linux non-Intel-Gen12, no Firefox Android). The wgpu WebGL2 fallback has *no compute, no storage buffers/textures*, 2048 max texture, 16 KiB uniform bindings, 256 MiB max buffer. If a WebGL2 fallback is wanted, the renderer's baseline path must not use compute or storage buffers. If the spike targets WebGPU only, say so and drop those constraints.
6. **Transport abstraction in the client netcode:** a `Datagram + reliable stream` interface (WebTransport maps directly; native UDP maps to it; WebSocket is a degraded, ordered-only fallback). The server must be able to expose WebTransport (HTTP/3/QUIC + TLS) for browser clients; that is a server-side constraint the headless design must leave room for.
7. **Audio is an AudioWorklet pulling 128-frame quanta**; the mixer must be callable as `fill(&mut [f32])` with no allocation, no locks, and no I/O, fed by lock-free ring buffers or messages. Audio contexts start only after a user gesture.
8. **Platform layer: winit (not SDL3) on web.** winit has a Web backend; SDL3 on web means Emscripten, which does not pair with wgpu's `wasm32-unknown-unknown` web-sys path [INFERENCE]. The window/input abstraction must be thin enough that native can use SDL3 or winit and web uses winit.
9. **User-provided files:** only Chromium has a persistent directory-picker handle. Firefox and Safari need `<input webkitdirectory>` / drag-drop (no persistent handle; re-select every session) or a one-time copy into OPFS. The loader must work from either, which is constraint 3.

## 2. GPU

| Fact | Value | Source |
|---|---|---|
| Chrome WebGPU | 113 on ChromeOS/macOS/Windows; **144 added Linux, Intel Gen12+ only**; Android 121 | MDN BCD `api/GPU.json` |
| Edge | mirrors Chrome (144) | web-features explorer |
| Safari | **26** (macOS and iOS, released 2025-09-15) | BCD; web-features explorer |
| Firefox | **141 Windows** (not service workers); **145 macOS Tahoe Apple Silicon, 147 older macOS on Apple Silicon**; **not** Intel Mac, **not** Linux, **not** Android (Nightly only) | BCD `api/GPU.json`; MDN Firefox experimental-features page; Firefox 141/147 release notes on MDN |
| Baseline status | "Limited availability"; blocked by Firefox | web-features explorer (2026-10) |
| Usage | ~11.7% of Chrome page loads | Chrome Platform Status via web-features explorer |
| Secure context | HTTPS (or localhost) required; also available in workers | MDN WebGPU API |
| WebGPU default limits (what wgpu `Limits::default()` assumes) | tex2D 8192; uniform binding 64 KiB; storage binding 128 MiB; max buffer **256 MiB**; 4 bind groups | `wgpu-types/src/limits.rs` @ wgpu 30.0.0 |
| wgpu WebGL2 fallback (`Limits::downlevel_webgl2_defaults`) | tex2D **2048**, tex3D 256; **storage buffers 0, storage textures 0, compute 0**; uniform binding **16 KiB**; 4 color attachments; vertex buffers 8; max buffer 256 MiB | same file |

Notes:
- Higher-than-default limits must be requested from the adapter at device creation; a device cannot exceed what it requested (wgpu docs, same file). Browsers do not expose VRAM size, so there is no VRAM budget API: the native renderer needs its own texture-memory budget and eviction rather than querying the GPU.
- wgpu's `webgpu` / `webgl` Cargo features choose the web backend; objects are not `Send`/`Sync` on web unless the `fragile-send-sync-non-atomic-wasm` feature is used on a no-atomics build (wgpu `Cargo.toml`). So a multi-threaded renderer design cannot assume wgpu handles cross threads on web.
- Texture compression: WebGPU exposes BC / ETC2 / ASTC as optional features (web-features `webgpu.yml` feature list). CoD4 images are DXT-family (BC1/2/3) in the original [INFERENCE, owned by the fastfile/renderer tickets]; BC is available on desktop adapters but not on most mobile ones, so mobile browsers would need a CPU transcode.

## 3. WebAssembly memory, features, threads

| Fact | Value | Source |
|---|---|---|
| wasm32 ceiling | 65,536 pages = **4 GiB**; V8 supports the full spec limit on 64-bit hosts | V8 `src/wasm/wasm-limits.h` |
| memory64 spec ceiling in the JS API | 262,144 pages = **16 GiB** (V8 constant `kSpecMaxMemory64Pages`) | V8 `wasm-limits.h` |
| memory64 shipped | Chrome **133**, Firefox **134**, Safari "preview" only (not released) | BCD `webassembly/memory64.json` |
| Wasm 3.0 (includes memory64, multi-memory, exnref exceptions, GC, tail calls) completed Sept 2025 | phase 5 on the roadmap | webassembly.org features data; `webassembly.org/news/2025-09-17-wasm-3.0` (not re-read here) |
| Threads/atomics/shared memory | Chrome 74, Firefox 79, Safari 15.2 | BCD `threads-and-atomics.json` |
| JSPI (sync-looking async calls) | Chrome 137, Firefox 153, Safari 27 | BCD `jspi.json` |
| COEP `require-corp` | Chrome 83, Firefox 79, Safari 15.2 | BCD |
| COEP `credentialless` | Chrome 96, Firefox 119, **Safari no** | BCD |
| Rust `wasm32-unknown-unknown` defaults | multivalue, mutable-globals, reference-types, sign-ext, nontrapping-fptoint, bulk-memory (Rust 1.87+); **atomics/SIMD not default**; tier 2 | rustc book |
| Rust `wasm64-unknown-unknown` | tier 3 (no CI/builds), needs `-Zbuild-std` nightly | rustc book platform-support |
| `wasm-bindgen` memory64 | `wasm64-unknown-unknown` support merged 2026-04-27 (PR #5004). **Threads + memory64 explicitly rejected** by the threads transform (open issue #5330, 2026-09). Latest release 0.2.129 | wasm-bindgen GitHub |
| Rust threads on web | nightly + `-Zbuild-std` + `+atomics,+bulk-memory` (wasm-bindgen-rayon README, pinned nightly-2025-11-15); needs cross-origin isolation for `SharedArrayBuffer` | wasm-bindgen-rayon |

Practical consequences:
- **Max heap:** treat 4 GiB as the address-space ceiling, not an allocation guarantee. iOS/Safari kills the content process under memory pressure well below it, and Apple publishes no per-tab number [INFERENCE from general WebKit behavior; not measured; design for a few hundred MiB to ~1 GiB on iOS and verify on a device]. This is why section 1.1-1.2 plans for a ~225 MiB working set per match, not 1.55 GiB.
- **memory64 vs threads is a hard fork:** with today's tooling you get (a) wasm32 + threads, or (b) memory64 without threads. Choose (a) for the spike; the working set fits.
- **COOP/COEP cost:** the page must be served `Cross-Origin-Opener-Policy: same-origin` and `Cross-Origin-Embedder-Policy: require-corp` (Safari lacks `credentialless`). Every subresource (CDN assets, any iframes, analytics) must be same-origin or send CORP. Cannot be embedded in third-party iframes cleanly. Hosting is therefore on our own origin with custom headers (GitHub Pages cannot set headers; a service-worker shim is the usual workaround [INFERENCE]).
- **WebAssembly feature gating:** enabling SIMD/atomics produces a binary that fails to load on engines without them; ship feature-detected variants (`wasm-feature-detect`, referenced by webassembly.org).

## 4. Getting ~2.5 GB of original files into the browser

Measured on the real install (`COD4/zone/english`, 72 fastfiles; zlib stream at offset 12 inflates cleanly to EOF with no trailing data for all 72):

| Set | Compressed | Decompressed |
|---|---|---|
| All 72 fastfiles | 2.38 GiB | **4.34 GiB** |
| MP-related files (48; `mp_*`, `common_mp`, `localized_common_mp`, `ui_mp`, `*_post_gfx_mp`, plus `simplecredits` caught by my name filter) | ~0.85 GiB | ~1.58 GiB |
| Largest single zone (`jeepride`, SP) | 92 MiB | 173 MiB |
| Largest MP map zone (`mp_carentan`) | 52 MiB | **104 MiB** |
| `common_mp` / `localized_common_mp` / `ui_mp` | 13 / 54 / 0.4 MiB | 40 / 67 / 13.5 MiB |
| `main/*.iwd` | 23 files, 5.6 MiB to 168 MiB each; `main/` total 4.1 GB on disk | n/a |

Whole-install resident size (4.34 GiB inflated) exceeds the wasm32 ceiling, so loading all fastfiles into memory is impossible even on Chrome without memory64. Which IWDs and fastfiles MP actually needs is owned by the fastfile/VFS tickets and is not determined here. The `*_load.ff` zones are empty (0.0 MiB).

APIs (BCD for support; MDN storage article for quotas):

| API | Chrome | Firefox | Safari |
|---|---|---|---|
| `showDirectoryPicker` / `showOpenFilePicker` (persistent handles, stored in IndexedDB) | 86 (Android 132) | **no** | **no** |
| `DataTransferItem.getAsFileSystemHandle` (drop gives a handle) | 86 | no | no |
| `<input webkitdirectory>` / `webkitGetAsEntry` (drag-drop) | yes | 50 | 11.1 (iOS 18.4 for the input) |
| OPFS `navigator.storage.getDirectory()` | 86 | 111 | 15.2 |
| OPFS `createSyncAccessHandle` (sync read/write, **workers only**) | 102 | 111 | 15.2 |
| `navigator.storage.persist()` | 55 | 57 (prompts user) | 15.2 (silent heuristic) |
| `storage.estimate()` | 61 | 57 | 17 |

Quotas per origin (MDN "Storage quotas and eviction criteria"): Chromium 60% of total disk; Safari ~60% of disk (macOS 14/iOS 17+; ~15% in embedded WKWebView; cross-origin frames 1/10); **Firefox best-effort min(10% of disk, 10 GiB group limit)**, persistent up to 50% of disk. Safari proactively evicts script-created data after 7 days without user interaction (when tracking prevention is on) unless persisted. Eviction is whole-origin, all-or-nothing, LRU. Older Safari prompts above 1 GiB.

Consequences:
- A browser **can** hold a copy: ~0.85 GiB (MP subset, compressed fastfiles) to ~2.4 GiB (all fastfiles) fits within every quota above on any realistic disk, but only if `persist()` is granted should we rely on it surviving. Safari's 7-day eviction means "import once, forget" is unsafe there; the client must detect missing data and re-prompt.
- The least-copy path is **no copy at all**: read directly from the user's chosen files with `File.slice().arrayBuffer()` (works everywhere) or Chromium directory handles; this needs the random-access byte-source trait (section 1.3) and works with the per-zone streaming plan. Copy to OPFS is an optimization for Firefox/Safari repeat sessions; sync access handles give fast reads in a worker.
- Fastfile zlib streams can be inflated natively with `DecompressionStream("deflate")` (zlib framing), or a wasm inflater [INFERENCE: not tested against these files in a browser].
- Uploading original content to any server is excluded by policy (never redistribute), so all of this stays client-local. No proxying through our own origin.

## 5. Transport to a native server

| Fact | Value | Source |
|---|---|---|
| WebTransport | Chrome 97, Firefox 114, **Safari 26.4** (all three now); datagrams, bidi/uni streams in all three | BCD `api/WebTransport.json` |
| `serverCertificateHashes` (connect to a self-signed server) | Chrome 100, Firefox 125, Safari 26.4; cert validity must be **at most two weeks**, allowed key algorithms are restricted | BCD; W3C WebTransport spec |
| `WebTransport` `protocols` option | Chrome 143, Firefox 155, Safari 26.4 | BCD |
| `congestionControl`, `requireUnreliable`, `sendOrder` | Firefox/Safari yes, **Chrome no** | BCD |
| WebSocketStream | Chrome 124 only | BCD |
| WebSocket | universal; ordered, reliable (TCP, head-of-line blocking) | standard |

- WebTransport runs over HTTP/3 (QUIC, UDP) and needs TLS. For a server without a CA certificate, use `serverCertificateHashes` with a rotating short-lived cert, or a real cert. That is a server operational requirement.
- Latency: I did not measure. [INFERENCE] WebSocket adds no fixed latency over the TCP baseline but stalls all later packets behind one lost packet, which hurts a 30 Hz snapshot stream (each stale snapshot is useless); WebTransport datagrams avoid that. Design snapshot delivery as unreliable-latest-wins and commands/reliable messages as a stream.
- A browser cannot open raw UDP/TCP sockets, so the original UDP protocol is unreachable; this fits the out-of-scope compatibility decision. WebRTC data channels are another unreliable transport but require ICE/signaling; not researched.
- HTTPS pages cannot use `ws://` (mixed content); the server needs `wss://` or WebTransport anyway.
- The client transport is bound by the 4 GiB/threads constraints only indirectly. WebTransport works from workers (not verified here), so networking could live off the main thread.

## 6. Audio

- AudioWorklet: Chrome 66, Firefox 76, Safari 14.1 (BCD).
- Render quantum is **128 frames** by default; `renderSizeHint: "hardware"` lets the UA choose (Web Audio spec; field is new and its browser support was not checked). Real-time constraints: the worklet cannot block or allocate in the hot loop.
- Wasm inside the worklet needs the module and, for shared memory, SAB, hence cross-origin isolation (section 3).
- Autoplay policy requires a user gesture before the context runs [standard; not re-read for this sheet], which the engine's startup flow must tolerate.

## 7. Sources and licenses (all read; none copied)

| Source | License | Reuse |
|---|---|---|
| MDN browser-compat-data (`mdn/browser-compat-data`) | CC0-1.0 | data, free to use |
| MDN content (storage quotas, Firefox experimental features/release notes) | prose CC-BY-SA-2.5, code samples CC0 | facts only, cited |
| web-platform-dx/web-features | Apache-2.0 | data usable |
| WebAssembly/website feature data | Apache-2.0 | data usable |
| W3C specs (WebTransport, Web Audio, WebGPU) and WebAssembly spec repos | W3C Document License / W3C-WG terms (GitHub reports NOASSERTION; not individually verified) | facts only |
| gfx-rs/wgpu (`wgpu-types`, Cargo.toml) | MIT OR Apache-2.0 | **GPL-3-compatible**; reusable |
| winit (incl. winit-web) | Apache-2.0 | **GPL-3-compatible**; reusable |
| wasm-bindgen, wasm-bindgen-rayon | Apache-2.0 (wasm-bindgen is MIT OR Apache-2.0 per its crate metadata; GitHub reports Apache-2.0) | **GPL-3-compatible**; reusable |
| SDL3 | zlib | **GPL-3-compatible**; reusable |
| rust-lang/rust (rustc book) | Apache-2.0 / MIT | docs read only |
| V8 `wasm-limits.h`, WebKit `WasmLimits.h` | BSD-3 / BSD+LGPL mix | numbers read only |
| CoD4X | AGPL | **not used** in this sheet |

All measurements of the original install are aggregate sizes only. No content was copied or committed.

## 8. Open unknowns

- Real iOS/Safari WebAssembly allocation ceiling (not published; needs a device test). Android Chrome WebGPU adapter requirements not verified.
- Whether `DecompressionStream("deflate")` handles the fastfile stream at ~170 MiB without buffering all of it; whether WebTransport works from a Worker in all three browsers; actual WebTransport/WebSocket latency to a native server (needs a measurement spike).
- Exact minimal IWD + fastfile set MP needs (fastfile/VFS tickets).
- Whether the original engine's texture formats are BC-compressed in zones (renderer ticket) and therefore which GPUs/browsers need CPU transcode.
- wgpu-on-wasm64 (memory64) status; I only verified `wasm-bindgen`.
- Pointer lock / raw mouse, fullscreen, keyboard lock, gamepad, and WebGPU timing/jank constraints (input and frame pacing) were not researched.
