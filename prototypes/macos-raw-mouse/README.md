# THROWAWAY PROTOTYPE: macOS raw mouse deltas (ticket #19)

Not engine code. Delete when #19 is decided.

## Run (one command each; needs Nix for cargo, builds with winit 0.30 + softbuffer + objc2-game-controller)

    ./run.sh bin     # unbundled binary
    ./run.sh app     # builds RawMouseProto.app (hand-written Info.plist, ad-hoc signed), launches it via `open`

Window opens pointer-locked while focused. Esc releases, left click re-locks, R resets sums,
S re-reads the macOS speed setting, Q quits. A once-per-second summary also goes to stdout
(app: /tmp/rawmouse-app.log). Env: `QUIT_AFTER=secs` auto-exits, `GC_BG=1` sets
`GCController.shouldMonitorBackgroundEvents`, `GC_QUEUE=main` runs the GCMouse handler on the main queue
(default: its own dispatch queue, feeding a lock-free `ArrayQueue`).

## Columns
(a) winit `DeviceEvent::MouseMotion`  (b) winit `CursorMoved` position differences
(c) `GCMouse.mouseInput.mouseMovedHandler` (y flipped to screen-down).
Each shows events/s over the last 1 s, last delta, cumulative X/Y, median/max gap between events (ms),
a 1 s sparkline (|delta| per 10 ms bin), a crosshair (1 unit = 1 px, wrapped; trail = last 1 s),
and a swipe readout.

## Testing acceleration by hand
Put a ruler under the mouse. Do a SLOW swipe, pause >150 ms, then a FAST swipe of the same physical
distance. Each source reports net distance / duration / speed of the last two swipes and the
fast/slow distance ratio. ~1.0 = 1:1 (raw); well above 1.25 = pointer acceleration applies.
Header shows `com.apple.mouse.scaling` (unset = default accel curve; -1 = acceleration disabled).
Compare a USB mouse and the trackpad; check events/s of a 1000 Hz mouse in (a) vs (c).
