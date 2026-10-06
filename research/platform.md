# Research: windowing and present stack (ticket #8)

Question: SDL3 (Rust bindings) vs winit, each with wgpu, on macOS, Linux (Wayland/Hyprland, X11), Windows, web. Display and refresh enumeration, fullscreen, high refresh/VRR/ProMotion/`CAMetalDisplayLink`, present modes and frame pacing, raw mouse/pointer lock/gamepad, HiDPI.

Research date: 2026-10-06. Source checkouts were taken from each project's default branch on that date; crate versions come from crates.io.
Legend: **[V]** verified in source/docs read for this sheet. **[I]** inference or unverified, with the reason.

## 1. Versions current as of 2026-10-06 (crates.io)

| Crate | Latest stable | Notes | License |
|---|---|---|---|
| `wgpu` | 30.0.1 (2026-08-22) | 30.0.0 shipped 2026-07-01; major version roughly every quarter | MIT OR Apache-2.0 |
| `winit` | 0.30.13 (2026-03-02) | **0.31.0-beta.3 (2026-09-04)** is the only newer line; still pre-release. Workspace split into `winit-appkit/-wayland/-x11/-win32/-web` crates, edition 2024, MSRV 1.86 | Apache-2.0 |
| `sdl3` | 0.20.0 (2026-09-07) | 0.19.0 was the day before; 0.18.x in spring. Fast-moving, pre-1.0 | MIT |
| `sdl3-sys` | 0.7.2+SDL-3.4.18 (2026-10-03) | bundles/targets SDL 3.4.18 | Zlib |
| SDL (C) | 3.4.18 (2026-10-02) | `main` branch reports 3.5.0 and its hint docs say 3.6.0 for new features | Zlib |
| `gilrs` | 0.11.2 (2026-05-30) | pure-Rust gamepad | Apache-2.0 / MIT |
| `raw-window-handle` | 0.6.2 | both winit lines and `sdl3` use rwh 0.6 | MIT OR Apache-2.0 |
| `objc2-quartz-core` | 0.3.2 | has `CAMetalDisplayLink` bindings behind a feature | Zlib OR Apache-2.0 OR MIT |
| `objc2-game-controller` | 0.3.2 | GameController framework bindings (GCMouse) | Zlib OR Apache-2.0 OR MIT |

Everything above is GPL-3.0-or-later compatible (permissive). Sources read: winit (Apache-2.0), wgpu (MIT/Apache-2.0), SDL (Zlib), sdl3 crate (MIT), gilrs (Apache/MIT). **None is AGPL or proprietary.** CoD4X was not consulted. Permissive code from these projects may be reused or vendored with attribution if needed.

## 2. wgpu present layer (shared by both candidates)

All **[V]** from `wgpu-types/src/surface.rs`, `wgpu-hal/src/{metal,dx12}`:

- Present modes: `AutoVsync`, `AutoNoVsync`, `Fifo` (universal, default), `FifoRelaxed` (AMD on Vulkan), `Immediate` ("most platforms except older DX12 and Wayland"), `Mailbox` (DX12 on Win10, NVIDIA Vulkan, Wayland on Vulkan).
- `SurfaceConfiguration::desired_maximum_frame_latency` maps to: Vulkan swapchain image count = N+1; DX12 `SetMaximumFrameLatency(N)` with a frame-latency waitable object (range 1..=16); Metal `CAMetalLayer.maximumDrawableCount = N+1` with supported range **1..=2**; OpenGL ignored. Docs recommend 1 for lowest latency (CPU and GPU can't overlap), 2 as default.
- Mailbox interaction: DX12 caps fps at `N * Hz`; Vulkan/Metal with N=2 caps at `2 * Hz`, N>=3 unlimited.
- **Metal backend supports only `Fifo` and `Immediate`.** Fifo = `displaySyncEnabled = true`, Immediate = `displaySyncEnabled = false`. No Mailbox on macOS.
- **Metal drawable acquisition is `CAMetalLayer.nextDrawable()`** (`metal/surface.rs`). No `CAMetalDisplayLink` use anywhere in wgpu-hal (grep for `DisplayLink` in `wgpu-hal/src/metal` matched nothing).
- DX12 uses `DXGI_SWAP_EFFECT_FLIP_DISCARD`; `Immediate` only offered if `DXGI_FEATURE_PRESENT_ALLOW_TEARING` is supported (so VRR+tearing on Windows works when the driver allows).
- wgpu 30 moved presentation to `Queue::present(surface_texture)` (seen in issue #9937 repro). Open perf regression reports in 30: "Higher present wall time after upgrading from wgpu 29 to 30" gfx-rs/wgpu#9937; "fence wait blows up acquire latency on AMD + Immediate" #9559. **Pin and benchmark.**
- No frame-pacing/present-timing API: "Extended Presentation API Investigation" gfx-rs/wgpu#2869 is open (covers present wait/display timing/scheduled present). So pacing is: choose Fifo/Immediate, set latency, throttle yourself.
- **No VRR API.** VRR is controlled by OS/compositor/driver (Hyprland `vrr` option, Windows "Optimized for windowed games"/G-Sync, macOS ProMotion). **[I]** The app only influences it via present mode and how frames are submitted.
- `CAMetalDisplayLink` (**[V]** Apple docs: macOS 14.0+, iOS 17+): created from a `CAMetalLayer`, delivers the `drawable` inside a delegate callback (`CAMetalDisplayLinkUpdate.drawable`, `targetPresentationTimestamp` per the `objc2-quartz-core` 0.3.2 bindings), with `preferredFrameRateRange` and `preferredFrameLatency`. Because wgpu takes drawables via `nextDrawable`, **using the display link requires bypassing wgpu's Metal surface** (a wgpu-hal change or fork). Treat as an optional later optimization, not a v1 requirement. **[I]** Plain `CAMetalLayer` with display sync already follows the display's refresh on macOS; Apple's own docs say `CADisplayLink`/`CAMetalDisplayLink` matter for variable-rate control. I did not measure ProMotion behavior (no 120 Hz display exercised).

## 3. Comparison matrix

| Capability | winit 0.30.13 / 0.31-beta.3 + wgpu 30 | SDL3 (`sdl3` 0.20 / SDL 3.4.18) + wgpu 30 |
|---|---|---|
| **Surface handoff to wgpu** | First-class: `Arc<Window>` implements rwh 0.6 **[V]** | `sdl3` feature `raw-window-handle`; upstream example `raw-window-handle-with-wgpu` pinned to wgpu 30.0.0 **[V]** (`sdl3-0.20.0/Cargo.toml`) |
| **Monitor enumeration** | `available_monitors()`, `MonitorHandle` with name, position, scale, `video_modes()`, `current_video_mode()` **[V]** | `SDL_GetDisplays`, `SDL_GetFullscreenDisplayModes`, `SDL_GetDesktopDisplayMode`; mode has rational refresh (`refresh_rate_numerator/denominator`) **[V]** |
| **Refresh rate** | `VideoMode::refresh_rate_millihertz()` (`Option`); macOS falls back to `CVDisplayLink` nominal period when CG reports 0 **[V]**; Wayland modes from `wl_output`, no bit depth **[V]** | Same macOS CVDisplayLink fallback **[V]**; Wayland native modes via `internal->refresh` in mHz (`SDL_waylandvideo.c`) **[V]** |
| **Borderless fullscreen** | `Fullscreen::Borderless(Option<Monitor>)` on all desktop OSes; macOS puts it on a separate Space (`set_simple_fullscreen` avoids) **[V]** | Default fullscreen (desktop mode) **[V]** |
| **Exclusive fullscreen** | `Fullscreen::Exclusive(VideoMode)`: macOS (true mode change, but no task switching, dock/menu disabled), Windows, X11. **Wayland: no-op**; Web needs permission **[V]** | `SDL_SetWindowFullscreenMode` with a real mode; Wayland uses mode emulation via viewporter (hint `SDL_VIDEO_WAYLAND_MODE_EMULATION`, default on) **[V]**. [I] exact per-OS behavior not tested |
| **Wayland / Hyprland** | Native `winit-wayland` backend, `wp_fractional_scale`, relative pointer, pointer constraints **[V]** | Native Wayland driver with fractional-scale-v1 **[V]**; X11 fallback via `SDL_VIDEO_DRIVER` |
| **Windows raw mouse** | `RegisterRawInputDevices` / `WM_INPUT` -> `DeviceEvent::PointerMotion` **[V]** | Raw input supported (hint `SDL_HINT_MOUSE_RELATIVE_*`) **[I]** not source-checked |
| **Linux raw mouse** | X11: XInput2 raw motion; Wayland: `zwp_relative_pointer` unaccelerated delta **[V]** | Same protocols **[I]** |
| **macOS raw mouse** | **Weak.** `DeviceEvent::PointerMotion` comes from `NSEvent.deltaX/Y` (OS-accelerated, delivered in one lump per display refresh). Upstream issue rust-windowing/winit#4581 (open) measured, on a 120 Hz ProMotion panel with a 1000 Hz mouse: CGEventTap 13,452 events (median gap 1.02 ms) vs NSEvent 2,269 (median gap 8.08 ms). Proposed fix: GCMouse. GameController needs an Info.plist/bundle consideration noted in the thread **[V]** | **SDL 3.4.18 has no GCMouse path** (grep of `release-3.4.18` finds GCMouse only in `SDL_cocoavideo.m`, not `SDL_cocoamouse.m`). `main` has GCMouse relative mode; hint `SDL_HINT_MAC_USE_GCMOUSE` says "available since SDL 3.6.0" (unreleased as of today; latest is 3.4.18) **[V]**. So both are accelerated/lumpy on macOS today with released versions |
| **Pointer lock / confine** | `CursorGrabMode::Confined` (not macOS) and `Locked` (not X11); apps use `Confined` then fall back to `Locked` as documented **[V]**. Hide cursor manually | `SDL_SetWindowRelativeMouseMode` unifies this across OSes **[I]** (API not read) |
| **Gamepad** | **None in winit** (no gamepad/joystick API; FEATURES.md has no entry). Use `gilrs` 0.11.2 (evdev, Windows, macOS, wasm; no macOS rumble) **[V]** | Built-in gamepad/joystick/haptic with mapping DB (`SDL_gamepad_db.h`) and rumble; `sdl3` has `gamepad.rs` example **[V]** |
| **HiDPI** | `scale_factor()`, `ScaleFactorChanged`, logical/physical sizes in `dpi` crate; fractional scale on Wayland **[V]** | `SDL_WINDOW_HIGH_PIXEL_DENSITY`, `SDL_GetWindowPixelDensity`, display scale **[V]** |
| **Frame callback / redraw driving** | `request_redraw`, `pre_present_notify` (needed on Wayland for frame callbacks); `ControlFlow::Poll` for uncapped **[V]** | You own the loop; `SDL_PollEvent` + render when you want **[I]** |
| **Web (wasm32-unknown-unknown)** | `winit-web` backend; wgpu WebGPU/WebGL2 canvas surface **[V]** | No wasm32-unknown-unknown support documented in `sdl3` Cargo.toml/README (grep for wasm/emscripten empty); SDL web path is Emscripten C. Browser spike would need a cfg split **[I]** |
| **Native deps** | Pure Rust (X11 via protocol crates, Wayland via smithay-client-toolkit) **[I]** from workspace structure | C library: dynamic link, `build-from-source` (CMake), or `link-framework`. Extra packaging on three OSes **[V]** features list |
| **Maturity / churn** | Stable 0.30.13 is 7 months old; 0.31 beta has breaking trait redesign (`ApplicationHandler`, `dyn Window`). Beta since 2025-11 | Binding 0.x, minor versions weekly; SDL itself very stable C API |
| **License** | Apache-2.0 | MIT (bindings), Zlib (SDL) |

## 4. Can either deliver "smooth, uncapped rendering with a fixed simulation tick"?

Yes, with the same loop on both, since the limits are in wgpu not the windowing crate:

1. Fixed tick runs from an accumulator on a monotonic clock; render interpolates. Independent of present mode. Standard design, no stack-specific blocker found.
2. Present mode: expose `Fifo` (default), `Immediate` (uncapped, may tear), `Mailbox` where reported by `get_capabilities` (not on macOS). Always select from `SurfaceCapabilities::present_modes`.
3. Set `desired_maximum_frame_latency` to 1 for latency-first, 2 for throughput. On Metal the range is 1..=2 only.
4. Wayland: no `Immediate` on many setups; `Mailbox` available via Vulkan **[V]** per wgpu docs. Frame callbacks (via `pre_present_notify` in winit) drive pacing there.
5. macOS: only Fifo/Immediate. Uncapped = Immediate (display sync off). High refresh follows the display in Fifo **[I]**.
6. VRR/ProMotion: not controllable from wgpu or either windowing layer. `CAMetalDisplayLink` would need wgpu-hal changes.

## 5. Recommendation

**Primary: winit + wgpu 30.0.x, plus gilrs for gamepads.** Pin `winit =0.30.13` first (stable); revisit 0.31 once it exits beta, since the engine's own platform trait should hide the winit version.

Rationale:
- Only stack with a documented, in-tree **web** backend, which the browser feasibility spike needs, and pure-Rust build on all three desktop OSes.
- Equal on monitor/mode enumeration and borderless/exclusive fullscreen (except Wayland exclusive is a no-op, which is a platform reality).
- Gaps are small and fillable: gamepad (`gilrs`, MIT/Apache), and macOS raw mouse.

Known gaps and the plan for each:
- **macOS raw mouse** (the one real weakness, and it affects aiming feel): neither released winit nor released SDL 3.4.18 solves it. Options: (a) own small GCMouse shim via `objc2-game-controller` 0.3.2 feeding raw deltas into the engine input layer, bypassing winit's `DeviceEvent`; (b) wait for winit#4581 / SDL 3.6. The `objc2-game-controller` shim is the pragmatic route, with bundle/Info.plist constraint to be tested. Needs a ticket.
- **Exclusive fullscreen on Wayland**: not possible in winit; treat borderless as the Linux/Hyprland mode.
- **Frame pacing/ProMotion/`CAMetalDisplayLink`**: not available through wgpu; defer, hold a perf-test ticket.

**Fallback: SDL3 + wgpu** if the winit path blocks on input or fullscreen behavior. SDL3 wins on gamepad (built-in), Wayland mode emulation, unified relative-mouse mode, and GCMouse once 3.6 ships. It loses on web (spike would need a separate path), C dependency packaging, and binding churn (0.19 to 0.20 in one day).

Hybrid worth knowing: winit for window/surface/web and SDL3 only for gamepad (`SDL_INIT_GAMEPAD`) was not evaluated; it adds the C dependency for one feature, so `gilrs` is preferred unless its mapping coverage proves inadequate.

## 6. Open unknowns (not verified)

1. ProMotion 120 Hz behavior of wgpu-Metal Fifo and Immediate was not measured (no hardware run).
2. SDL3's per-OS behavior for exclusive fullscreen and raw input on Windows/Linux was not source-checked in depth. SDL relative-mouse API surface in the Rust crate not read.
3. Whether winit 0.31 changes Wayland/Windows fullscreen or monitor APIs vs 0.30 was not diffed; the API read here is from the beta.3 workspace head.
4. Whether `objc2-game-controller` GCMouse works for an unbundled (cargo-run) binary and for a bundled `.app` without extra entitlements.
5. wgpu 30 present regressions (#9937, #9559) need a local benchmark on target GPUs.
6. gilrs mapping coverage vs SDL's DB for the controllers we care about.

## 7. Source index

- winit: https://github.com/rust-windowing/winit (Apache-2.0), files `winit-core/src/{window,monitor,event}.rs`, `winit-appkit/src/monitor.rs`, `winit-wayland/src/seat/pointer/relative_pointer.rs`, `winit-win32/src/raw_input.rs`; issue #4581.
- wgpu: https://github.com/gfx-rs/wgpu (MIT/Apache-2.0), `wgpu-types/src/surface.rs`, `wgpu-hal/src/metal/{adapter,surface}.rs`, `wgpu-hal/src/dx12/*`; issues #2869, #9937, #9559.
- SDL: https://github.com/libsdl-org/SDL (Zlib), `include/SDL3/SDL_hints.h`, `SDL_video.h`, `src/video/{cocoa,wayland}`; tag `release-3.4.18`.
- sdl3 crate 0.20.0 (MIT) from crates.io: `Cargo.toml`, `examples/`.
- gilrs: https://gitlab.com/gilrs-project/gilrs README (Apache/MIT).
- Apple: https://developer.apple.com/documentation/quartzcore/cametaldisplaylink
- crates.io metadata via https://crates.io/api/v1/crates/<name>.
