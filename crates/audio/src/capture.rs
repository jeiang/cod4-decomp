// SPDX-License-Identifier: GPL-3.0-only
//! The microphone, for voice chat: the default input device, folded to mono and reduced to [`RATE`] samples a second
//! (a box filter over each output sample's span of input samples: coarse, and enough for speech) into a ring the game
//! thread reads a frame at a time. Opening a device may ask the person for permission, so [`Capture::open`] is called
//! when they first press the talk key, not at startup.

/// Samples a second of what [`Capture::read`] returns.
pub const RATE: u32 = 8000;

/// Folds a mono input stream at `rate` Hz down to [`RATE`]: a box filter over each output sample's span of input
/// samples.
pub struct BoxFilter {
    rate: u32,
    sum: f32,
    count: u32,
    phase: u32,
}

impl BoxFilter {
    pub fn new(rate: u32) -> Self {
        Self {
            rate: rate.max(RATE),
            sum: 0.0,
            count: 0,
            phase: 0,
        }
    }

    /// Takes one input sample; returns an output sample when its span is complete.
    pub fn push(&mut self, mono: f32) -> Option<f32> {
        self.sum += mono;
        self.count += 1;
        self.phase += RATE;
        if self.phase < self.rate {
            return None;
        }
        self.phase -= self.rate;
        let out = self.sum / self.count as f32;
        self.sum = 0.0;
        self.count = 0;
        Some(out)
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::{BoxFilter, RATE};
    use crate::ring::{Consumer, Producer, ring};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

    /// A running microphone; capture stops when it is dropped.
    pub struct Capture {
        _stream: cpal::Stream,
        ring: Consumer,
    }

    impl Capture {
        pub fn open() -> Result<Self, String> {
            let device = cpal::default_host()
                .default_input_device()
                .ok_or("no microphone")?;
            let c = device
                .default_input_config()
                .map_err(|e| format!("no input format: {e}"))?;
            let format = c.sample_format();
            let config: StreamConfig = c.into();
            let (tx, rx) = ring(RATE as usize * 2);
            let stream = match format {
                SampleFormat::F32 => run::<f32>(&device, config, tx),
                SampleFormat::I16 => run::<i16>(&device, config, tx),
                SampleFormat::I32 => run::<i32>(&device, config, tx),
                SampleFormat::U16 => run::<u16>(&device, config, tx),
                f => Err(format!("unsupported sample format {f}")),
            }?;
            stream
                .play()
                .map_err(|e| format!("cannot start the microphone: {e}"))?;
            Ok(Self {
                _stream: stream,
                ring: rx,
            })
        }

        /// Fills `out` with the next samples (`-1..1`) and returns `true`, or returns `false` and takes nothing when
        /// fewer are waiting.
        pub fn read(&mut self, out: &mut [f32]) -> bool {
            self.ring.pop_exact(out)
        }

        /// Samples waiting.
        pub fn waiting(&self) -> usize {
            self.ring.len()
        }

        /// A failure after the device opened; the native device reports it at [`Capture::open`].
        pub fn failure(&mut self) -> Option<String> {
            None
        }
    }

    fn run<T>(
        device: &cpal::Device,
        config: StreamConfig,
        mut tx: Producer,
    ) -> Result<cpal::Stream, String>
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        let channels = usize::from(config.channels).max(1);
        let mut filter = BoxFilter::new(config.sample_rate);
        device
            .build_input_stream(
                config,
                move |data: &[T], _| {
                    for frame in data.chunks_exact(channels) {
                        let mono = frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>()
                            / channels as f32;
                        if let Some(out) = filter.push(mono) {
                            // A full ring drops the newest: the game thread is not reading.
                            let _ = tx.push(&[out]);
                        }
                    }
                },
                |e| tracing::warn!("audio capture stream: {e}"),
                None,
            )
            .map_err(|e| format!("cannot open the microphone: {e}"))
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::Capture;

#[cfg(target_arch = "wasm32")]
mod web {
    use super::{BoxFilter, RATE};
    use crate::ring::{Consumer, Producer, ring};
    use js_sys::{Array, Float32Array, Reflect};
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{
        AudioContext, AudioNode, AudioWorkletNode, AudioWorkletNodeOptions, Blob, BlobPropertyBag,
        MediaStream, MediaStreamConstraints, MessageEvent, Url,
    };

    const WORKLET: &str = include_str!("capture_worklet.js");
    const PROCESSOR: &str = "cod4e-mic";

    /// `name: message` of a rejected promise's `DOMException`.
    fn reason(e: JsValue) -> String {
        let get = |k: &str| {
            Reflect::get(&e, &JsValue::from_str(k))
                .ok()
                .and_then(|v| v.as_string())
        };
        match (get("name"), get("message")) {
            (Some(n), Some(m)) if !m.is_empty() => format!("{n}: {m}"),
            (Some(n), _) => n,
            _ => e.as_string().unwrap_or_else(|| format!("{e:?}")),
        }
    }

    fn stop(stream: &MediaStream) {
        for t in stream.get_tracks().iter() {
            if let Ok(t) = t.dyn_into::<web_sys::MediaStreamTrack>() {
                t.stop();
            }
        }
    }

    /// The graph `microphone -> worklet -> muted gain -> destination` (the muted gain keeps the browser pulling
    /// the worklet).
    struct Live {
        stream: MediaStream,
        source: AudioNode,
        node: AudioWorkletNode,
        mute: AudioNode,
        _on_chunk: Closure<dyn FnMut(MessageEvent)>,
    }

    struct Mic {
        ctx: AudioContext,
        url: String,
        live: Option<Live>,
        error: Option<String>,
    }

    impl Drop for Mic {
        fn drop(&mut self) {
            if let Some(l) = self.live.take() {
                stop(&l.stream);
                let _ = l.source.disconnect();
                let _ = l.node.disconnect();
                let _ = l.mute.disconnect();
                l.node.port().ok().inspect(|p| p.close());
            }
            let _ = Url::revoke_object_url(&self.url);
            let _ = self.ctx.close();
        }
    }

    /// A microphone opened through `getUserMedia`: the browser asks the person for permission, so audio begins
    /// (or [`Capture::failure`] reports a refusal) some frames after [`Capture::open`] returns. Capture stops when
    /// it is dropped.
    pub struct Capture {
        ring: Consumer,
        mic: Rc<RefCell<Mic>>,
    }

    impl Capture {
        pub fn open() -> Result<Self, String> {
            let window = web_sys::window().ok_or("no window")?;
            let media = window
                .navigator()
                .media_devices()
                .map_err(|_| "no microphone access (needs a secure context: https or localhost)")?;
            let ctx =
                AudioContext::new().map_err(|e| format!("no audio context: {}", reason(e)))?;
            let worklet = ctx.audio_worklet().map_err(|_| "no AudioWorklet")?;
            let parts = Array::of1(&JsValue::from_str(WORKLET));
            let kind = BlobPropertyBag::new();
            kind.set_type("text/javascript");
            let blob = Blob::new_with_str_sequence_and_options(&parts, &kind).map_err(reason)?;
            let url = Url::create_object_url_with_blob(&blob).map_err(reason)?;
            let module = worklet.add_module(&url).map_err(reason)?;
            let constraints = MediaStreamConstraints::new();
            constraints.set_audio(&JsValue::TRUE);
            constraints.set_video(&JsValue::FALSE);
            let stream = media
                .get_user_media_with_constraints(&constraints)
                .map_err(reason)?;
            let _ = ctx.resume();
            let (tx, rx) = ring(RATE as usize * 2);
            let mic = Rc::new(RefCell::new(Mic {
                ctx: ctx.clone(),
                url,
                live: None,
                error: None,
            }));
            let weak = Rc::downgrade(&mic);
            wasm_bindgen_futures::spawn_local(async move {
                let stream = JsFuture::from(stream).await.map(MediaStream::from);
                let module = JsFuture::from(module).await;
                let Some(mic) = weak.upgrade() else {
                    if let Ok(s) = &stream {
                        stop(s);
                    }
                    return;
                };
                let live = match (stream, module) {
                    (Ok(s), Ok(_)) => Live::attach(&ctx, s, tx),
                    (Ok(s), Err(e)) => {
                        stop(&s);
                        Err(format!("worklet module failed: {}", reason(e)))
                    }
                    (Err(e), _) => Err(format!("cannot open the microphone: {}", reason(e))),
                };
                let mut m = mic.borrow_mut();
                match live {
                    Ok(l) => m.live = Some(l),
                    Err(e) => m.error = Some(e),
                }
            });
            Ok(Self { ring: rx, mic })
        }

        /// Fills `out` with the next samples (`-1..1`) and returns `true`, or returns `false` and takes nothing when
        /// fewer are waiting.
        pub fn read(&mut self, out: &mut [f32]) -> bool {
            self.ring.pop_exact(out)
        }

        /// Samples waiting.
        pub fn waiting(&self) -> usize {
            self.ring.len()
        }

        /// Why the microphone could not be used (a refused permission, no device), once.
        pub fn failure(&mut self) -> Option<String> {
            self.mic.borrow_mut().error.take()
        }
    }

    impl Live {
        fn attach(
            ctx: &AudioContext,
            stream: MediaStream,
            mut tx: Producer,
        ) -> Result<Self, String> {
            let attach = || -> Result<(AudioNode, AudioWorkletNode, AudioNode, Closure<dyn FnMut(MessageEvent)>), String> {
                let source: AudioNode = ctx.create_media_stream_source(&stream).map_err(reason)?.into();
                let options = AudioWorkletNodeOptions::new();
                options.set_number_of_inputs(1);
                options.set_number_of_outputs(1);
                options.set_output_channel_count(&Array::of1(&JsValue::from(1)));
                let node = AudioWorkletNode::new_with_options(ctx, PROCESSOR, &options).map_err(reason)?;
                let gain = ctx.create_gain().map_err(reason)?;
                gain.gain().set_value(0.0);
                let mute: AudioNode = gain.into();
                let mut filter = BoxFilter::new(ctx.sample_rate() as u32);
                let on_chunk = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
                    let chunk = Float32Array::new(&e.data()).to_vec();
                    for v in chunk {
                        if let Some(out) = filter.push(v) {
                            // A full ring drops the newest: the game is not reading.
                            let _ = tx.push(&[out]);
                        }
                    }
                });
                node.port().map_err(reason)?.set_onmessage(Some(on_chunk.as_ref().unchecked_ref()));
                source.connect_with_audio_node(&node).map_err(reason)?;
                node.connect_with_audio_node(&mute).map_err(reason)?;
                mute.connect_with_audio_node(&ctx.destination()).map_err(reason)?;
                Ok((source, node, mute, on_chunk))
            };
            match attach() {
                Ok((source, node, mute, _on_chunk)) => Ok(Self {
                    stream,
                    source,
                    node,
                    mute,
                    _on_chunk,
                }),
                Err(e) => {
                    stop(&stream);
                    Err(e)
                }
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use web::Capture;

#[cfg(test)]
mod tests {
    use super::{BoxFilter, RATE};

    #[test]
    fn the_filter_makes_one_output_sample_per_span_and_averages_it() {
        let mut f = BoxFilter::new(RATE * 6);
        let out: Vec<f32> = (0..12)
            .filter_map(|i| f.push(if i < 6 { 0.5 } else { -0.25 }))
            .collect();
        assert_eq!(out, [0.5, -0.25]);
    }

    #[test]
    fn the_filter_keeps_the_rate_ratio_for_a_rate_that_does_not_divide() {
        let mut f = BoxFilter::new(44100);
        let n = (0..44100).filter_map(|_| f.push(0.1)).count();
        assert_eq!(n, RATE as usize);
    }
}
