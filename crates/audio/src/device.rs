// SPDX-License-Identifier: GPL-3.0-or-later
//! The sound card: a cpal output stream that runs [`Mixer::fill`] in its callback.
//!
//! cpal gives CoreAudio on macOS, ALSA on Linux, WASAPI on Windows and Web Audio in a browser. The mixer is
//! stereo; a device with more channels gets the pair on its first two and silence on the rest.

use crate::mixer::Mixer;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

/// A device chosen and its format known; the mixer is built for its rate.
pub struct Config {
    device: cpal::Device,
    config: StreamConfig,
    format: SampleFormat,
}

impl Config {
    /// The default output device, or why there is none.
    pub fn probe() -> Result<Self, String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no audio output device")?;
        let c = device
            .default_output_config()
            .map_err(|e| format!("no output format: {e}"))?;
        Ok(Self {
            device,
            format: c.sample_format(),
            config: c.into(),
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    /// Starts playing `mixer`, which must have been built for [`Config::sample_rate`].
    pub fn start(self, mixer: Mixer) -> Result<Output, String> {
        let Self {
            device,
            config,
            format,
        } = self;
        let stream = match format {
            SampleFormat::F32 => run::<f32>(&device, config, mixer),
            SampleFormat::I16 => run::<i16>(&device, config, mixer),
            SampleFormat::I32 => run::<i32>(&device, config, mixer),
            SampleFormat::U16 => run::<u16>(&device, config, mixer),
            f => Err(format!("unsupported sample format {f}")),
        }?;
        stream.play().map_err(|e| format!("cannot start the stream: {e}"))?;
        Ok(Output { _stream: stream })
    }
}

/// A running output; the sound stops when it is dropped.
pub struct Output {
    _stream: cpal::Stream,
}

/// Frames mixed per pass, so the callback needs no buffer of the device's size.
const BLOCK: usize = 1024;

fn run<T>(device: &cpal::Device, config: StreamConfig, mut mixer: Mixer) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels).max(1);
    let mut scratch = [0.0f32; BLOCK * 2];
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                for chunk in data.chunks_mut(BLOCK * channels) {
                    let frames = chunk.len() / channels;
                    mixer.fill(&mut scratch[..frames * 2]);
                    for (f, out) in chunk.chunks_exact_mut(channels).enumerate() {
                        for (c, s) in out.iter_mut().enumerate() {
                            *s = T::from_sample(if c < 2 { scratch[2 * f + c] } else { 0.0 });
                        }
                    }
                }
            },
            |e| eprintln!("audio: {e}"),
            None,
        )
        .map_err(|e| format!("cannot open the output stream: {e}"))
}
