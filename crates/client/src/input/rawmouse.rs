// SPDX-License-Identifier: GPL-3.0-only
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

/// Frames in which winit saw motion but GCMouse reported none, in a row, before winit takes over. A mouse the
/// GameController framework does not list (a trackpad next to a USB mouse, a device that reconnected, a handler the
/// framework dropped) would otherwise leave mouse look dead for as long as any other mouse stays attached.
const GC_SILENT_FRAMES: u32 = 6;
/// While silent, re-install the handlers this often (in frames with winit motion).
const REATTACH_EVERY: u32 = 120;

/// Raw mouse delta source. Events are always collected; the caller decides, per frame, whether to use them.
pub struct RawMouse {
    winit: (f64, f64),
    winit_events: u64,
    #[cfg(target_os = "macos")]
    gc: gc::GcMouse,
    /// Motion events drained from the source in use since creation.
    events: u64,
    /// Every event each source delivered since creation, used or not.
    gc_total: u64,
    winit_total: u64,
    /// Frames with winit motion and no GCMouse event since GCMouse last reported.
    quiet: u32,
}

/// Cumulative per-source counters, for the input debug log.
#[derive(Clone, Copy, Default)]
pub struct Totals {
    pub gc: u64,
    pub winit: u64,
    pub used: u64,
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
            gc_total: 0,
            winit_total: 0,
            quiet: 0,
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
        let wn = std::mem::take(&mut self.winit_events);
        #[cfg(target_os = "macos")]
        let gc = self.gc.drain();
        #[cfg(not(target_os = "macos"))]
        let gc = None;
        let (d, reattach) = self.select(gc, w, wn);
        #[cfg(target_os = "macos")]
        if reattach {
            eprintln!("input: GCMouse silent while winit sees motion; re-installing handlers");
            self.gc.reattach();
        }
        #[cfg(not(target_os = "macos"))]
        let _ = reattach;
        d
    }

    /// One frame's delta and whether the GCMouse handlers should be re-installed. `gc` is `None` while no GCMouse is
    /// attached. GCMouse wins whenever it reports (winit's copy of the same motion is discarded, it is accelerated and
    /// lumped); winit is used only after GCMouse has stayed silent through [`GC_SILENT_FRAMES`] frames of motion.
    pub(super) fn select(
        &mut self,
        gc: Option<((f64, f64), u64)>,
        w: (f64, f64),
        wn: u64,
    ) -> ((f64, f64), bool) {
        self.winit_total += wn;
        let Some((d, n)) = gc else {
            self.quiet = 0;
            self.events += wn;
            return (w, false);
        };
        self.gc_total += n;
        if n > 0 {
            self.quiet = 0;
            self.events += n;
            return (d, false);
        }
        if wn > 0 {
            self.quiet = self.quiet.saturating_add(1);
        }
        if !self.gc_silent() {
            return ((0.0, 0.0), false);
        }
        self.events += wn;
        let over = self.quiet - GC_SILENT_FRAMES;
        (w, wn > 0 && over.is_multiple_of(REATTACH_EVERY))
    }

    pub(super) fn gc_silent(&self) -> bool {
        self.quiet >= GC_SILENT_FRAMES
    }

    /// Which source [`Self::drain`] reads right now.
    pub fn source(&self) -> &'static str {
        #[cfg(target_os = "macos")]
        if self.gc.active() {
            return if self.gc_silent() {
                "winit (GCMouse silent)"
            } else {
                "GCMouse"
            };
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

    /// The GCMouse devices the framework lists and whether each has our handler, for the debug log.
    pub fn describe_devices(&self) -> String {
        #[cfg(target_os = "macos")]
        {
            self.gc.describe()
        }
        #[cfg(not(target_os = "macos"))]
        {
            "n/a".into()
        }
    }

    pub fn totals(&self) -> Totals {
        Totals {
            gc: self.gc_total,
            winit: self.winit_total,
            used: self.events,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: (f64, f64) = (0.0, 0.0);

    #[test]
    fn gc_wins_and_its_winit_twin_is_discarded() {
        let mut m = RawMouse::detached();
        assert_eq!(
            m.select(Some(((5.0, 1.0), 3)), (9.0, 9.0), 1),
            ((5.0, 1.0), false)
        );
        // The twin of that motion arrives a frame late: GCMouse has been quiet for one frame only.
        assert_eq!(m.select(Some((NONE, 0)), (9.0, 9.0), 1), (NONE, false));
        assert!(!m.gc_silent());
    }

    #[test]
    fn winit_alone_when_no_gc_mouse() {
        let mut m = RawMouse::detached();
        assert_eq!(m.select(None, (2.0, 3.0), 2), ((2.0, 3.0), false));
        assert_eq!(m.totals().used, 2);
    }

    /// A mouse the framework does not list (trackpad beside a USB mouse) or a handler it dropped: GCMouse stays
    /// attached and silent while winit sees every motion. Mouse look must not stay dead.
    #[test]
    fn winit_takes_over_when_gc_goes_silent() {
        let mut m = RawMouse::detached();
        m.select(Some(((1.0, 0.0), 1)), NONE, 0);
        for _ in 0..GC_SILENT_FRAMES - 1 {
            assert_eq!(m.select(Some((NONE, 0)), (4.0, 0.0), 1), (NONE, false));
        }
        assert_eq!(
            m.select(Some((NONE, 0)), (4.0, 0.0), 1),
            ((4.0, 0.0), true),
            "winit used and handlers re-installed on the frame GCMouse is declared silent"
        );
        assert_eq!(
            m.select(Some((NONE, 0)), (4.0, 0.0), 1),
            ((4.0, 0.0), false)
        );
        // An idle frame neither counts nor re-arms anything.
        assert_eq!(m.select(Some((NONE, 0)), NONE, 0), (NONE, false));
        // GCMouse reports again: it takes back, winit is discarded.
        assert_eq!(
            m.select(Some(((2.0, 0.0), 2)), (4.0, 0.0), 1),
            ((2.0, 0.0), false)
        );
        assert!(!m.gc_silent());
    }

    #[test]
    fn reinstall_is_rate_limited_while_silent() {
        let mut m = RawMouse::detached();
        let mut reinstalls = 0;
        for _ in 0..GC_SILENT_FRAMES + 2 * REATTACH_EVERY {
            reinstalls += u32::from(m.select(Some((NONE, 0)), (1.0, 0.0), 1).1);
        }
        // On the frame it is declared silent, then every REATTACH_EVERY frames.
        assert_eq!(reinstalls, 3);
    }

    #[test]
    fn totals_count_every_event_per_source() {
        let mut m = RawMouse::detached();
        m.select(Some(((1.0, 0.0), 4)), (1.0, 0.0), 2);
        let t = m.totals();
        assert_eq!((t.gc, t.winit, t.used), (4, 2, 4));
    }
}

#[cfg(target_os = "macos")]
mod gc {
    use block2::{DynBlock, RcBlock};
    use crossbeam_queue::ArrayQueue;
    use dispatch2::{DispatchQueue, DispatchRetained};
    use objc2::rc::Retained;
    use objc2_game_controller::{GCDevice, GCMouse, GCMouseInput};
    use std::ptr::NonNull;
    use std::sync::Arc;

    /// A second of 1 kHz events x 8 headroom; a full queue drops the newest.
    const QUEUE: usize = 8192;
    /// Hot-plug scan interval, in drains.
    const SCAN_EVERY: u32 = 30;

    pub struct GcMouse {
        q: Arc<ArrayQueue<(f32, f32)>>,
        /// Mice that have our handler. The framework lists every connected mouse and trackpad in `GCMouse::mice()`
        /// (`GCMouse::current()` is only the last one used, so it is not enough).
        attached: Vec<Retained<GCMouse>>,
        queue: Option<DispatchRetained<DispatchQueue>>,
        drains: u32,
    }

    fn name(m: &GCMouse) -> String {
        let vendor = unsafe { m.vendorName() }.map_or_else(|| "?".into(), |v| v.to_string());
        format!("{vendor}/{}@{:p}", unsafe { m.productCategory() }, m)
    }

    impl GcMouse {
        pub fn disabled() -> Self {
            Self {
                q: Arc::new(ArrayQueue::new(1)),
                attached: Vec::new(),
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

        /// Every mouse the framework lists, whether it is the current one and whether our handler is still on it.
        pub fn describe(&self) -> String {
            let cur = unsafe { GCMouse::current() };
            let list: Vec<String> = unsafe { GCMouse::mice() }
                .iter()
                .map(|m| {
                    let handler = unsafe { m.mouseInput() }
                        .is_some_and(|i| !unsafe { i.mouseMovedHandler() }.is_null());
                    let ours = self.attached.iter().any(|a| std::ptr::eq(&**a, &*m));
                    format!(
                        "{}{}{}",
                        name(&m),
                        if cur.as_ref().is_some_and(|c| std::ptr::eq(&**c, &*m)) {
                            " current"
                        } else {
                            ""
                        },
                        match (ours, handler) {
                            (true, true) => " handler=ok",
                            (true, false) => " handler=CLEARED",
                            (false, _) => " handler=none",
                        }
                    )
                })
                .collect();
            format!("[{}]", list.join(", "))
        }

        /// Attach a handler to every listed mouse not yet attached; forget the ones that went away.
        fn scan(&mut self) {
            let Some(dq) = &self.queue else { return };
            let mice = unsafe { GCMouse::mice() };
            self.attached.retain(|a| {
                let listed = mice.iter().any(|m| std::ptr::eq(&*m, &**a));
                if !listed {
                    eprintln!("input: GCMouse detached: {}", name(a));
                }
                listed
            });
            for m in mice.iter() {
                if self.attached.iter().any(|a| std::ptr::eq(&**a, &*m)) {
                    continue;
                }
                // No input profile yet: not remembered, so the next scan tries again.
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
                eprintln!("input: GCMouse attached: {}", name(&m));
                self.attached.push(m);
            }
        }

        /// Forget every attachment and install the handlers again (a handler the framework dropped is not noticed
        /// otherwise: the device is still listed).
        pub fn reattach(&mut self) {
            self.attached.clear();
            self.scan();
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
