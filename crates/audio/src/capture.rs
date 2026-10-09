// SPDX-License-Identifier: GPL-3.0-only
//! The microphone, for voice chat: the default input device, folded to mono and reduced to [`RATE`] samples a second
//! (a box filter over each output sample's span of input samples: coarse, and enough for speech) into a ring the game
//! thread reads a frame at a time. Opening a device may ask the person for permission, so [`Capture::open`] is called
//! when they first press the talk key, not at startup.

#[cfg(target_arch = "wasm32")]
use crate::ring::Consumer;

/// Samples a second of what [`Capture::read`] returns.
pub const RATE: u32 = 8000;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::RATE;
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
        let rate = config.sample_rate;
        // Box filter state: input samples summed toward the next output sample.
        let (mut sum, mut count, mut phase) = (0.0f32, 0u32, 0u32);
        device
            .build_input_stream(
                config,
                move |data: &[T], _| {
                    for frame in data.chunks_exact(channels) {
                        let mono = frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>()
                            / channels as f32;
                        sum += mono;
                        count += 1;
                        phase += RATE;
                        if phase >= rate {
                            phase -= rate;
                            // A full ring drops the newest: the game thread is not reading.
                            let _ = tx.push(&[sum / count as f32]);
                            sum = 0.0;
                            count = 0;
                        }
                    }
                },
                |e| eprintln!("audio capture: {e}"),
                None,
            )
            .map_err(|e| format!("cannot open the microphone: {e}"))
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::Capture;

/// The browser build has no microphone access: voice chat there is listen-only.
#[cfg(target_arch = "wasm32")]
pub struct Capture(Consumer);

#[cfg(target_arch = "wasm32")]
impl Capture {
    pub fn open() -> Result<Self, String> {
        Err("the browser build cannot use a microphone".into())
    }

    pub fn read(&mut self, out: &mut [f32]) -> bool {
        self.0.pop_exact(out)
    }

    pub fn waiting(&self) -> usize {
        self.0.len()
    }
}
