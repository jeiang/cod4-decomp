// SPDX-License-Identifier: GPL-3.0-only
//! The browser output: an `AudioContext` with an `AudioWorkletNode` whose processor is embedded in this
//! crate (`worklet.js`, loaded through a Blob URL, so nothing extra is served).
//!
//! The mixer runs on the page's main thread (the wasm build has no threads): a 10 ms `setInterval` keeps
//! [`TARGET_MS`] of audio rendered ahead of the worklet, as [`crate::transport`] describes.
//!
//! `AudioWorklet.addModule` is async but [`Config::probe`] is not: `probe` creates the context and starts
//! loading; [`Config::start`] returns at once and the node attaches when the module is ready. Until then the
//! mixer is not run, so queued commands and sounds wait instead of elapsing unheard.
//!
//! The context starts suspended until a user gesture; `probe` tries `resume()` and also resumes on every
//! `pointerdown`, `keydown` and `touchend` on the document while the context is not running, so sound begins
//! at the first click or key and recovers if the browser suspends it later.

use crate::mixer::Mixer;
use crate::transport::{CAPACITY, OutputStats, Pacer, TARGET_MS};
use js_sys::{
    Array, Atomics, Float32Array, Int32Array, Object, Promise, Reflect, SharedArrayBuffer,
};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use wasm_bindgen::prelude::*;
use web_sys::{
    AudioContext, AudioContextState, AudioWorkletNode, AudioWorkletNodeOptions, Blob,
    BlobPropertyBag, MessageEvent, MessagePort, Url,
};

const WORKLET: &str = include_str!("worklet.js");
const PROCESSOR: &str = "cod4e-mixer";
const TICK_MS: i32 = 10;
const GESTURES: [&str; 3] = ["pointerdown", "keydown", "touchend"];
const WRITE: u32 = 0;
const READ: u32 = 1;
const UNDERRUNS: u32 = 2;

fn js(e: JsValue) -> String {
    e.as_string().unwrap_or_else(|| format!("{e:?}"))
}

/// The context and what must be undone when it goes away.
struct Ctx {
    ctx: AudioContext,
    url: String,
    resume: Closure<dyn FnMut()>,
}

impl Drop for Ctx {
    fn drop(&mut self) {
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            for e in GESTURES {
                let _ = doc
                    .remove_event_listener_with_callback(e, self.resume.as_ref().unchecked_ref());
            }
        }
        let _ = Url::revoke_object_url(&self.url);
        let _ = self.ctx.close();
    }
}

/// An audio context created and its worklet module loading; the mixer is built for its rate.
pub struct Config {
    ctx: Ctx,
    module: Promise,
}

impl Config {
    /// Creates the context, starts loading the worklet module and arms the resume-on-gesture listeners.
    pub fn probe() -> Result<Self, String> {
        let window = web_sys::window().ok_or("no window")?;
        let doc = window.document().ok_or("no document")?;
        let ctx = AudioContext::new().map_err(|e| format!("no audio context: {}", js(e)))?;
        let worklet = ctx
            .audio_worklet()
            .map_err(|_| "no AudioWorklet (needs a secure context: https or localhost)")?;
        let parts = Array::of1(&JsValue::from_str(WORKLET));
        let kind = BlobPropertyBag::new();
        kind.set_type("text/javascript");
        let blob = Blob::new_with_str_sequence_and_options(&parts, &kind).map_err(js)?;
        let url = Url::create_object_url_with_blob(&blob).map_err(js)?;
        let module = worklet.add_module(&url).map_err(js)?;
        let again = ctx.clone();
        let resume = Closure::<dyn FnMut()>::new(move || {
            if again.state() != AudioContextState::Running {
                let _ = again.resume();
            }
        });
        for e in GESTURES {
            doc.add_event_listener_with_callback(e, resume.as_ref().unchecked_ref())
                .map_err(js)?;
        }
        let _ = ctx.resume();
        Ok(Self {
            ctx: Ctx { ctx, url, resume },
            module,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.ctx.ctx.sample_rate() as u32
    }

    /// Starts playing `mixer`, which must have been built for [`Config::sample_rate`]. The sound begins when
    /// the worklet module has loaded (and the browser lets the context run); see [`Output::stats`].
    pub fn start(self, mixer: Mixer) -> Result<Output, String> {
        let window = web_sys::window().ok_or("no window")?;
        let rate = self.sample_rate();
        let target = rate * TARGET_MS / 1000;
        let state = Rc::new(RefCell::new(State {
            mixer,
            scratch: vec![0.0; target as usize * 2],
            pacer: Pacer::new(CAPACITY, target),
            link: None,
            error: None,
        }));
        let attach = Rc::downgrade(&state);
        let (ctx, module) = (self.ctx.ctx.clone(), self.module);
        wasm_bindgen_futures::spawn_local(async move {
            let loaded = wasm_bindgen_futures::JsFuture::from(module).await;
            let Some(state) = Weak::upgrade(&attach) else {
                return;
            };
            let linked = loaded
                .map_err(|e| format!("worklet module failed: {}", js(e)))
                .and_then(|_| Link::attach(&ctx));
            let mut s = state.borrow_mut();
            match linked {
                Ok(l) => s.link = Some(l),
                Err(e) => s.error = Some(e),
            }
        });
        let tick = {
            let state = state.clone();
            Closure::<dyn FnMut()>::new(move || state.borrow_mut().tick())
        };
        let interval = window
            .set_interval_with_callback_and_timeout_and_arguments_0(
                tick.as_ref().unchecked_ref(),
                TICK_MS,
            )
            .map_err(js)?;
        Ok(Output {
            state,
            interval,
            _tick: tick,
            _ctx: self.ctx,
        })
    }
}

/// A running output; the sound stops when it is dropped.
pub struct Output {
    state: Rc<RefCell<State>>,
    interval: i32,
    _tick: Closure<dyn FnMut()>,
    _ctx: Ctx,
}

impl Output {
    /// Counters for the debug overlay; cheap enough to call every frame.
    pub fn stats(&self) -> OutputStats {
        self.state.borrow().stats()
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        if let Some(w) = web_sys::window() {
            w.clear_interval_with_handle(self.interval);
        }
        if let Some(l) = &self.state.borrow().link {
            let _ = l.node.disconnect();
            l.port.close();
        }
    }
}

struct State {
    mixer: Mixer,
    /// Interleaved stereo for one tick's worth of frames (at most the target).
    scratch: Vec<f32>,
    pacer: Pacer,
    link: Option<Link>,
    error: Option<String>,
}

impl State {
    fn tick(&mut self) {
        let Some(link) = &self.link else {
            return;
        };
        self.pacer.observe(link.read());
        let n = self.pacer.wanted();
        if n == 0 {
            return;
        }
        let out = &mut self.scratch[..n as usize * 2];
        self.mixer.fill(out);
        link.write(&mut self.pacer, out);
    }

    fn stats(&self) -> OutputStats {
        let mut s = OutputStats {
            frames_played: self.pacer.played(),
            buffered_frames: self.pacer.buffered(),
            frames_rendered: self.pacer.rendered(),
            error: self.error.clone(),
            ..OutputStats::default()
        };
        if let Some(l) = &self.link {
            let r = l.report.get();
            s.underruns = l.underruns();
            s.peak = r[2] as f32;
            s.crossings = r[3] as u32;
            s.shared = l.shared.is_some();
            s.running = true;
        }
        s
    }
}

/// The ring and control words when they are shared with the worklet.
struct Shared {
    ring: Float32Array,
    ctrl: Int32Array,
}

/// The attached worklet node.
struct Link {
    node: AudioWorkletNode,
    port: MessagePort,
    shared: Option<Shared>,
    /// The worklet's last status message: `[read, underruns, peak, crossings]`.
    report: Rc<Cell<[f64; 4]>>,
    _on_report: Closure<dyn FnMut(MessageEvent)>,
}

impl Link {
    fn attach(ctx: &AudioContext) -> Result<Self, String> {
        let isolated = Reflect::get(&js_sys::global(), &JsValue::from_str("crossOriginIsolated"))
            .is_ok_and(|v| v.is_truthy());
        let options = Object::new();
        let set =
            |k: &str, v: &JsValue| Reflect::set(&options, &JsValue::from_str(k), v).map(|_| ());
        set("capacity", &JsValue::from(CAPACITY)).map_err(js)?;
        let shared = isolated.then(|| {
            let ring = SharedArrayBuffer::new(CAPACITY * 8);
            let ctrl = SharedArrayBuffer::new(16);
            (ring, ctrl)
        });
        let shared = match shared {
            Some((ring, ctrl)) => {
                set("ring", &ring).map_err(js)?;
                set("ctrl", &ctrl).map_err(js)?;
                Some(Shared {
                    ring: Float32Array::new(&ring),
                    ctrl: Int32Array::new(&ctrl),
                })
            }
            None => None,
        };
        let node_options = AudioWorkletNodeOptions::new();
        node_options.set_number_of_inputs(0);
        node_options.set_number_of_outputs(1);
        node_options.set_output_channel_count(&Array::of1(&JsValue::from(2)));
        node_options.set_processor_options(Some(&options));
        let node = AudioWorkletNode::new_with_options(ctx, PROCESSOR, &node_options).map_err(js)?;
        let port = node.port().map_err(js)?;
        let report = Rc::new(Cell::new([0.0; 4]));
        let latest = report.clone();
        let on_report = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let a = Array::from(&e.data());
            latest.set(std::array::from_fn(|i| {
                a.get(i as u32).as_f64().unwrap_or(0.0)
            }));
        });
        port.set_onmessage(Some(on_report.as_ref().unchecked_ref()));
        node.connect_with_audio_node(&ctx.destination())
            .map_err(js)?;
        Ok(Self {
            node,
            port,
            shared,
            report,
            _on_report: on_report,
        })
    }

    /// Frames consumed by the worklet (a `u32` word that wraps).
    fn read(&self) -> u32 {
        match &self.shared {
            Some(s) => Atomics::load(&s.ctrl, READ).unwrap_or(0) as u32,
            None => self.report.get()[0] as u32,
        }
    }

    fn underruns(&self) -> u32 {
        match &self.shared {
            Some(s) => Atomics::load(&s.ctrl, UNDERRUNS).unwrap_or(0) as u32,
            None => self.report.get()[1] as u32,
        }
    }

    /// Hands `frames` (interleaved) to the worklet.
    fn write(&self, pacer: &mut Pacer, frames: &[f32]) {
        let n = (frames.len() / 2) as u32;
        match &self.shared {
            Some(s) => {
                let (a, b) = pacer.spans(n);
                let first = a.len() * 2;
                s.ring
                    .subarray(a.start as u32 * 2, a.end as u32 * 2)
                    .copy_from(&frames[..first]);
                if !b.is_empty() {
                    s.ring
                        .subarray(0, b.end as u32 * 2)
                        .copy_from(&frames[first..]);
                }
                let _ = Atomics::store(&s.ctrl, WRITE, pacer.commit(n) as i32);
            }
            None => {
                let chunk = Float32Array::from(frames);
                let _ = self
                    .port
                    .post_message_with_transferable(&chunk, &Array::of1(&chunk.buffer()));
                pacer.commit(n);
            }
        }
    }
}
