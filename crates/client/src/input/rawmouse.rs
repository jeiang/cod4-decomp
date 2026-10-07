// SPDX-License-Identifier: GPL-3.0-or-later
//! Raw (unaccelerated) relative mouse motion.
//!
//! On Windows and Linux winit's `DeviceEvent::MouseMotion` is raw already. On macOS it comes from `NSEvent` deltas:
//! OS-accelerated and lumped once per display refresh (rust-windowing/winit#4581). There we read `GCMouse` from the
//! GameController framework instead (ticket #19): its `mouseMovedHandler` fires on its own dispatch queue at the
//! device rate (~1000/s for a 1000 Hz mouse), unaccelerated, and works from an unbundled binary with no entitlements.
//! The handler pushes into a lock-free queue that [`RawMouse::drain`] empties once per frame. When no `GCMouse`
//! exists (headless, SSH, no mouse attached) it falls back to winit's events.
//!
//! This whole file's macOS half is removed once winit#4581 lands or SDL 3.6 (`SDL_HINT_MAC_USE_GCMOUSE`) is in use.
//!
//! Sign convention of [`RawMouse::drain`]: x right-positive, y down-positive (screen space, like winit).

/// Raw mouse delta source. Events are always collected; the caller decides, per frame, whether to use them.
pub struct RawMouse {
    winit: (f64, f64),
    winit_events: u64,
    #[cfg(target_os = "macos")]
    gc: gc::GcMouse,
    /// Motion events drained from the active source since creation.
    events: u64,
}

impl RawMouse {
    /// winit events only.
    pub fn detached() -> Self {
        Self {
            winit: (0.0, 0.0),
            winit_events: 0,
            #[cfg(target_os = "macos")]
            gc: gc::GcMouse::disabled(),
            events: 0,
        }
    }

    pub fn new() -> Self {
        #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
        let mut m = Self::detached();
        #[cfg(target_os = "macos")]
        {
            m.gc = gc::GcMouse::new();
        }
        m
    }

    /// Feed a winit `DeviceEvent::MouseMotion` delta.
    pub fn winit_motion(&mut self, delta: (f64, f64)) {
        self.winit.0 += delta.0;
        self.winit.1 += delta.1;
        self.winit_events += 1;
    }

    /// Everything since the last call, summed. Never blocks.
    pub fn drain(&mut self) -> (f64, f64) {
        let w = std::mem::take(&mut self.winit);
        #[cfg(target_os = "macos")]
        if let Some((d, n)) = self.gc.drain() {
            self.events += n;
            self.winit_events = 0;
            return d;
        }
        self.events += std::mem::take(&mut self.winit_events);
        w
    }

    /// Which source [`Self::drain`] reads right now.
    pub fn source(&self) -> &'static str {
        #[cfg(target_os = "macos")]
        if self.gc.active() {
            return "GCMouse";
        }
        "winit"
    }

    /// Mice attached to the GCMouse source (always 0 elsewhere).
    pub fn devices(&self) -> usize {
        #[cfg(target_os = "macos")]
        {
            self.gc.devices()
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    }

    /// Motion events drained from the active source so far.
    pub fn events(&self) -> u64 {
        self.events
    }
}

#[cfg(target_os = "macos")]
mod gc {
    use block2::{DynBlock, RcBlock};
    use crossbeam_queue::ArrayQueue;
    use dispatch2::{DispatchQueue, DispatchRetained};
    use objc2::rc::Retained;
    use objc2_game_controller::{GCDevice, GCMouse, GCMouseInput};
    use std::collections::HashSet;
    use std::ptr::NonNull;
    use std::sync::Arc;

    /// A second of 1 kHz events x 8 headroom; a full queue drops the newest.
    const QUEUE: usize = 8192;
    /// Hot-plug scan interval, in drains.
    const SCAN_EVERY: u32 = 30;

    pub struct GcMouse {
        q: Arc<ArrayQueue<(f32, f32)>>,
        attached: HashSet<usize>,
        keep: Vec<Retained<GCMouse>>,
        queue: Option<DispatchRetained<DispatchQueue>>,
        drains: u32,
    }

    impl GcMouse {
        pub fn disabled() -> Self {
            Self {
                q: Arc::new(ArrayQueue::new(1)),
                attached: HashSet::new(),
                keep: Vec::new(),
                queue: None,
                drains: 1,
            }
        }

        pub fn new() -> Self {
            Self {
                q: Arc::new(ArrayQueue::new(QUEUE)),
                queue: Some(DispatchQueue::new("cod4e.gcmouse", None)),
                drains: 0,
                ..Self::disabled()
            }
        }

        pub fn active(&self) -> bool {
            !self.attached.is_empty()
        }

        pub fn devices(&self) -> usize {
            self.attached.len()
        }

        /// Attach a handler to every mouse not yet seen; picks up hot-plugged mice.
        fn scan(&mut self) {
            let Some(dq) = &self.queue else { return };
            for m in unsafe { GCMouse::mice() }.iter() {
                if !self.attached.insert(Retained::as_ptr(&m) as usize) {
                    continue;
                }
                let Some(input) = (unsafe { m.mouseInput() }) else {
                    continue;
                };
                let q = self.q.clone();
                let block: RcBlock<dyn Fn(NonNull<GCMouseInput>, f32, f32)> =
                    RcBlock::new(move |_: NonNull<GCMouseInput>, dx: f32, dy: f32| {
                        // GCMouse y is up-positive; flip to screen space. Full queue: drop.
                        let _ = q.push((dx, -dy));
                    });
                // SAFETY: the framework copies the block; the pointer need only live for the call.
                unsafe {
                    m.setHandlerQueue(dq);
                    input.setMouseMovedHandler(&*block as *const DynBlock<_> as *mut _);
                }
                self.keep.push(m);
            }
        }

        /// `None` while no `GCMouse` is attached (caller falls back to winit).
        pub fn drain(&mut self) -> Option<((f64, f64), u64)> {
            self.queue.as_ref()?;
            if self.drains.is_multiple_of(SCAN_EVERY) {
                self.scan();
            }
            self.drains = self.drains.wrapping_add(1);
            if self.attached.is_empty() {
                return None;
            }
            let (mut dx, mut dy, mut n) = (0.0, 0.0, 0);
            while let Some((x, y)) = self.q.pop() {
                dx += f64::from(x);
                dy += f64::from(y);
                n += 1;
            }
            Some(((dx, dy), n))
        }
    }
}
