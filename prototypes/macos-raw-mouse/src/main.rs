// THROWAWAY PROTOTYPE (ticket #19): raw mouse deltas on macOS. Not engine code.
use std::collections::{HashSet, VecDeque};
use std::num::NonZeroU32;
use std::process::Command;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use block2::{DynBlock, RcBlock};
use crossbeam_queue::ArrayQueue;
use dispatch2::{DispatchQueue, DispatchRetained};
use font8x8::{UnicodeFonts, BASIC_FONTS};
use objc2_game_controller::{GCController, GCDevice, GCMouse, GCMouseInput};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorGrabMode, Window, WindowId};

const WINDOW: Duration = Duration::from_secs(1);
const SWIPE_GAP: Duration = Duration::from_millis(150);

#[derive(Clone, Copy)]
struct Ev {
    t: Instant,
    dx: f64,
    dy: f64,
}

#[derive(Clone, Copy)]
struct Swipe {
    nx: f64,
    ny: f64,
    dur: f64,
}

struct Source {
    name: &'static str,
    note: String,
    win: VecDeque<Ev>,
    total: u64,
    zeros: u64,
    last: (f64, f64),
    sum: (f64, f64),
    cur: Option<(Instant, Instant, f64, f64, f64)>, // start,last,sx,sy,path
    swipes: VecDeque<Swipe>,
}

impl Source {
    fn new(name: &'static str) -> Self {
        Self { name, note: String::new(), win: VecDeque::new(), total: 0, zeros: 0, last: (0., 0.), sum: (0., 0.), cur: None, swipes: VecDeque::new() }
    }
    fn push(&mut self, e: Ev) {
        self.total += 1;
        if e.dx == 0. && e.dy == 0. {
            self.zeros += 1;
        }
        self.last = (e.dx, e.dy);
        self.sum.0 += e.dx;
        self.sum.1 += e.dy;
        self.win.push_back(e);
        self.finalize_swipe(e.t);
        let c = self.cur.get_or_insert((e.t, e.t, 0., 0., 0.));
        c.1 = e.t;
        c.2 += e.dx;
        c.3 += e.dy;
        c.4 += e.dx.hypot(e.dy);
    }
    fn finalize_swipe(&mut self, now: Instant) {
        if let Some((s, l, sx, sy, path)) = self.cur {
            if now.duration_since(l) > SWIPE_GAP {
                self.cur = None;
                if path >= 20.0 {
                    self.swipes.push_back(Swipe { nx: sx, ny: sy, dur: l.duration_since(s).as_secs_f64().max(0.001) });
                    if self.swipes.len() > 4 {
                        self.swipes.pop_front();
                    }
                }
            }
        }
    }
    fn tick(&mut self, now: Instant) {
        while self.win.front().map_or(false, |e| now.duration_since(e.t) > WINDOW) {
            self.win.pop_front();
        }
        self.finalize_swipe(now);
    }
    fn reset(&mut self) {
        self.win.clear();
        self.total = 0;
        self.zeros = 0;
        self.last = (0., 0.);
        self.sum = (0., 0.);
        self.cur = None;
        self.swipes.clear();
    }
    fn gaps_ms(&self) -> (f64, f64) {
        let mut g: Vec<f64> = self.win.iter().zip(self.win.iter().skip(1)).map(|(a, b)| b.t.duration_since(a.t).as_secs_f64() * 1000.).collect();
        if g.is_empty() {
            return (0., 0.);
        }
        g.sort_by(|a, b| a.partial_cmp(b).unwrap());
        (g[g.len() / 2], *g.last().unwrap())
    }
}

struct Gc {
    q: Arc<ArrayQueue<(Instant, f32, f32)>>,
    attached: HashSet<usize>,
    keep: Vec<objc2::rc::Retained<GCMouse>>,
    queue: Option<DispatchRetained<DispatchQueue>>,
    names: Vec<String>,
}

impl Gc {
    fn new() -> Self {
        if std::env::var("GC_BG").map_or(false, |v| !v.is_empty()) {
            unsafe { GCController::setShouldMonitorBackgroundEvents(true) };
        }
        let queue = if std::env::var("GC_QUEUE").as_deref() == Ok("main") { None } else { Some(DispatchQueue::new("gcmouse.proto", None)) };
        Self { q: Arc::new(ArrayQueue::new(65536)), attached: HashSet::new(), keep: vec![], queue, names: vec![] }
    }
    /// Attach a handler to every GCMouse not yet seen. Called each frame (cheap); also catches hotplug.
    fn poll_attach(&mut self) {
        let mice = unsafe { GCMouse::mice() };
        for m in mice.iter() {
            let addr = objc2::rc::Retained::as_ptr(&m) as usize;
            if !self.attached.insert(addr) {
                continue;
            }
            let Some(input) = (unsafe { m.mouseInput() }) else { continue };
            let q = self.q.clone();
            let block: Rc<RcBlock<dyn Fn(NonNull<GCMouseInput>, f32, f32)>> = Rc::new(RcBlock::new(move |_i: NonNull<GCMouseInput>, dx: f32, dy: f32| {
                let _ = q.push((Instant::now(), dx, dy));
            }));
            unsafe {
                if let Some(dq) = &self.queue {
                    m.setHandlerQueue(dq);
                }
                input.setMouseMovedHandler(&**block as *const DynBlock<_> as *mut _);
            }
            let name = unsafe { m.vendorName() }.map(|s| s.to_string()).unwrap_or_else(|| "?".into());
            self.names.push(name);
            self.keep.push(m);
        }
    }
}

fn read_defaults(key: &str) -> String {
    Command::new("defaults").args(["read", "-g", key]).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_else(|| "(unset)".into())
}

struct App {
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    src: [Source; 3],
    gc: Gc,
    locked: bool,
    prev_pos: Option<(f64, f64)>,
    mouse_scaling: String,
    trackpad_scaling: String,
    last_print: Instant,
    start: Instant,
    quit_after: Option<f64>,
}

impl App {
    fn lock(&mut self, on: bool) {
        let Some(w) = &self.window else { return };
        if on {
            let r = w.set_cursor_grab(CursorGrabMode::Locked).or_else(|_| w.set_cursor_grab(CursorGrabMode::Confined));
            self.locked = r.is_ok();
            w.set_cursor_visible(false);
        } else {
            let _ = w.set_cursor_grab(CursorGrabMode::None);
            w.set_cursor_visible(true);
            self.locked = false;
        }
        self.prev_pos = None;
    }

    fn drain_gc(&mut self) {
        self.gc.poll_attach();
        while let Some((t, dx, dy)) = self.gc.q.pop() {
            // GCMouse y is positive-up; flip to screen-down to match winit.
            self.src[2].push(Ev { t, dx: dx as f64, dy: -(dy as f64) });
        }
        self.src[2].note = format!("{} mouse(s): {}", self.gc.names.len(), self.gc.names.join(","));
    }

    fn draw(&mut self) {
        let (Some(w), Some(surface)) = (&self.window, &mut self.surface) else { return };
        let size = w.inner_size();
        let (Some(nw), Some(nh)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };
        surface.resize(nw, nh).unwrap();
        let mut buf = surface.buffer_mut().unwrap();
        let (bw, bh) = (size.width as usize, size.height as usize);
        let sf = w.scale_factor();
        let sc = ((2.0 * sf).round() as usize).max(1);
        let mut cv = Canvas { px: &mut buf, w: bw, h: bh };
        cv.fill(0, 0, bw, bh, 0x101418);
        let now = Instant::now();
        let colw = bw / 3;
        for (i, s) in self.src.iter_mut().enumerate() {
            s.tick(now);
            let x0 = i * colw + 8 * sc;
            let mut y = 8 * sc;
            let col = [0x66ccff, 0xffcc66, 0x88ee88][i];
            cv.text(x0, y, sc, s.name, col);
            y += 10 * sc;
            for l in s.note.clone().split('\n') {
                cv.text(x0, y, sc, l, 0x888888);
                y += 9 * sc;
            }
            let (med, max) = s.gaps_ms();
            let lines = [
                format!("events/s: {}", s.win.len()),
                format!("total: {} (zero-delta {})", s.total, s.zeros),
                format!("last dx,dy: {:+.3} {:+.3}", s.last.0, s.last.1),
                format!("sum X,Y: {:+.1} {:+.1}", s.sum.0, s.sum.1),
                format!("gap ms med/max: {:.2}/{:.2}", med, max),
            ];
            for l in lines {
                cv.text(x0, y, sc, &l, 0xffffff);
                y += 9 * sc;
            }
            y += 4 * sc;
            // sparkline: 100 bins x 10 ms, magnitude per bin
            let sw = colw - 16 * sc;
            let sh = 40 * sc;
            cv.fill(x0, y, sw, sh, 0x1c2229);
            let mut bins = [0f64; 100];
            for e in &s.win {
                let age = now.duration_since(e.t).as_secs_f64();
                let b = 99usize.saturating_sub(((age * 100.0) as usize).min(99));
                bins[b] += e.dx.hypot(e.dy);
            }
            let mx = bins.iter().cloned().fold(1.0, f64::max);
            for (b, v) in bins.iter().enumerate() {
                let bh_ = (v / mx * sh as f64) as usize;
                let bx = x0 + b * sw / 100;
                cv.fill(bx, y + sh - bh_, (sw / 100).max(1), bh_, col);
            }
            cv.text(x0, y + sh + sc, sc, &format!("1 s of |delta| per 10ms bin, peak {:.1}", mx), 0x888888);
            y += sh + 12 * sc;
            // crosshair: cumulative position (1 unit = 1 px), wrapped; trail = last 1 s
            let cs = sw.min(bh.saturating_sub(y + 50 * sc));
            cv.fill(x0, y, cs, cs, 0x1c2229);
            cv.fill(x0 + cs / 2, y, 1, cs, 0x2c343d);
            cv.fill(x0, y + cs / 2, cs, 1, 0x2c343d);
            let wrap = |v: f64| -> usize { (v.rem_euclid(cs as f64)) as usize };
            let (mut px, mut py) = (s.sum.0 + cs as f64 / 2.0, s.sum.1 + cs as f64 / 2.0);
            cv.fill(x0 + wrap(px).saturating_sub(3), y + wrap(py).saturating_sub(3), 7, 7, col);
            for e in s.win.iter().rev() {
                px -= e.dx;
                py -= e.dy;
                cv.fill(x0 + wrap(px), y + wrap(py), 2, 2, 0x556677);
            }
            y += cs + 4 * sc;
            // swipe readout
            cv.text(x0, y, sc, "swipes (net dist / dur / speed):", 0xaaaaaa);
            y += 9 * sc;
            let n = s.swipes.len();
            for (k, sw_) in s.swipes.iter().enumerate().skip(n.saturating_sub(2)) {
                let d = sw_.nx.hypot(sw_.ny);
                cv.text(x0, y, sc, &format!("#{} {:.0} / {:.2}s / {:.0}/s", k, d, sw_.dur, d / sw_.dur), 0xffffff);
                y += 9 * sc;
            }
            if n >= 2 {
                let (a, b) = (s.swipes[n - 2], s.swipes[n - 1]);
                let (slow, fast) = if a.dur >= b.dur { (a, b) } else { (b, a) };
                let r = fast.nx.hypot(fast.ny) / slow.nx.hypot(slow.ny).max(1e-9);
                let verdict = if r > 1.25 { "ACCELERATED" } else if r < 1.12 && r > 0.89 { "1:1 (no accel)" } else { "inconclusive" };
                cv.text(x0, y, sc, &format!("fast/slow dist ratio {:.2} {}", r, verdict), if r > 1.25 { 0xff7777 } else { 0x88ee88 });
            } else {
                cv.text(x0, y, sc, "do a slow then a fast swipe", 0x666666);
            }
        }
        let hdr = format!(
            "{}  | mouse.scaling={}  trackpad.scaling={}  (-1/unset-> accel off/default; S re-reads)  | Esc release, click lock, R reset, Q quit",
            if self.locked { "LOCKED" } else { "UNLOCKED" },
            self.mouse_scaling,
            self.trackpad_scaling
        );
        cv.text(8 * sc, bh.saturating_sub(10 * sc), sc.max(1), &hdr, 0xffee88);
        buf.present().unwrap();
    }
}

struct Canvas<'a> {
    px: &'a mut [u32],
    w: usize,
    h: usize,
}

impl Canvas<'_> {
    fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, c: u32) {
        for yy in y..(y + h).min(self.h) {
            let row = yy * self.w;
            for xx in x..(x + w).min(self.w) {
                self.px[row + xx] = c;
            }
        }
    }
    fn text(&mut self, x: usize, y: usize, sc: usize, s: &str, c: u32) {
        for (i, ch) in s.chars().enumerate() {
            if let Some(g) = BASIC_FONTS.get(ch) {
                for (ry, row) in g.iter().enumerate() {
                    for rx in 0..8 {
                        if row >> rx & 1 == 1 {
                            self.fill(x + (i * 8 + rx) * sc, y + ry * sc, sc, sc, c);
                        }
                    }
                }
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let w = Rc::new(el.create_window(Window::default_attributes().with_title("macos-raw-mouse PROTOTYPE").with_inner_size(winit::dpi::LogicalSize::new(1200.0, 760.0))).unwrap());
        let ctx = softbuffer::Context::new(w.clone()).unwrap();
        self.surface = Some(softbuffer::Surface::new(&ctx, w.clone()).unwrap());
        self.window = Some(w);
        self.lock(true);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::RedrawRequested => {
                self.drain_gc();
                self.draw();
            }
            WindowEvent::Focused(f) => self.lock(f),
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if !self.locked {
                    self.lock(true)
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = (position.x, position.y);
                let (dx, dy) = self.prev_pos.map_or((0., 0.), |q| (p.0 - q.0, p.1 - q.1));
                self.prev_pos = Some(p);
                self.src[1].push(Ev { t: Instant::now(), dx, dy });
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => match event.logical_key {
                Key::Named(NamedKey::Escape) => self.lock(false),
                Key::Character(c) => match c.as_str() {
                    "r" | "R" => self.src.iter_mut().for_each(Source::reset),
                    "s" | "S" => {
                        self.mouse_scaling = read_defaults("com.apple.mouse.scaling");
                        self.trackpad_scaling = read_defaults("com.apple.trackpad.scaling");
                    }
                    "q" | "Q" => el.exit(),
                    _ => {}
                },
                _ => {}
            },
            _ => {}
        }
    }

    fn device_event(&mut self, _el: &ActiveEventLoop, _id: DeviceId, ev: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = ev {
            self.src[0].push(Ev { t: Instant::now(), dx: delta.0, dy: delta.1 });
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let now = Instant::now();
        el.set_control_flow(ControlFlow::WaitUntil(now + Duration::from_millis(4)));
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        if now.duration_since(self.last_print) >= Duration::from_secs(1) {
            self.last_print = now;
            println!(
                "t={:.0}s locked={} | a(DeviceEvent) {}/s sum=({:+.1},{:+.1}) | b(CursorMoved) {}/s sum=({:+.1},{:+.1}) | c(GCMouse) {}/s sum=({:+.1},{:+.1}) [{}]",
                self.start.elapsed().as_secs_f64(), self.locked,
                self.src[0].win.len(), self.src[0].sum.0, self.src[0].sum.1,
                self.src[1].win.len(), self.src[1].sum.0, self.src[1].sum.1,
                self.src[2].win.len(), self.src[2].sum.0, self.src[2].sum.1, self.src[2].note,
            );
        }
        if let Some(q) = self.quit_after {
            if self.start.elapsed().as_secs_f64() > q {
                el.exit();
            }
        }
    }
}

fn main() {
    let el = EventLoop::new().unwrap();
    let mut app = App {
        window: None,
        surface: None,
        src: [Source::new("(a) winit DeviceEvent::MouseMotion"), Source::new("(b) winit CursorMoved deltas"), Source::new("(c) GCMouse mouseMovedHandler")],
        gc: Gc::new(),
        locked: false,
        prev_pos: None,
        mouse_scaling: read_defaults("com.apple.mouse.scaling"),
        trackpad_scaling: read_defaults("com.apple.trackpad.scaling"),
        last_print: Instant::now(),
        start: Instant::now(),
        quit_after: std::env::var("QUIT_AFTER").ok().and_then(|s| s.parse().ok()).filter(|&v: &f64| v > 0.0),
    };
    el.run_app(&mut app).unwrap();
}
