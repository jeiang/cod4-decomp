// SPDX-License-Identifier: GPL-3.0-only
//! WAV and MP3 decoding with Symphonia (MPL-2.0), whole into memory or packet by packet into a ring.
//!
//! The stock install holds only 16-bit PCM WAV and MPEG layer 3 (see the audio research); loaded sounds in
//! fastfiles are raw 16-bit PCM and need no decoder.

use crate::mixer::Pcm;
use crate::ring::Producer;
use std::io::Cursor;
use std::sync::Arc;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

/// A file being decoded one packet at a time, as interleaved `f32` of at most two channels.
pub struct Decoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track: u32,
    pub rate: u32,
    pub channels: u8,
    /// The first packet, decoded to learn the format, not yet handed out.
    first: Option<Vec<f32>>,
    /// The file's length in frames and its sample rate, when the container says.
    length: Option<(u64, u32)>,
}

impl Decoder {
    pub fn open(bytes: Arc<[u8]>, ext: &str) -> Result<Self, String> {
        let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
        let mut hint = Hint::new();
        hint.with_extension(ext);
        let format = symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|e| format!("not a sound file: {e}"))?;
        let track = format
            .default_track(TrackType::Audio)
            .ok_or("no audio track")?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or("no audio codec parameters")?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|e| format!("unsupported codec: {e}"))?;
        let length = track
            .num_frames
            .zip(params.sample_rate)
            .filter(|(f, r)| *f > 0 && *r > 0);
        let track = track.id;
        let mut d = Self {
            format,
            decoder,
            track,
            rate: 0,
            channels: 0,
            first: None,
            length,
        };
        let mut buf = Vec::new();
        if !d.decode_packet(&mut buf)? {
            return Err("empty sound file".into());
        }
        d.first = Some(buf);
        Ok(d)
    }

    /// Skips to `fraction` (0 to 1) of the file's length, when the container tells its length and can seek; the
    /// file plays from the start otherwise.
    pub fn seek_fraction(&mut self, fraction: f32) {
        let Some((frames, rate)) = self.length else {
            return;
        };
        let secs = frames as f64 * f64::from(fraction.clamp(0.0, 0.99)) / f64::from(rate);
        let Some(time) = Time::try_new(secs as i64, (secs.fract() * 1e9) as u32) else {
            return;
        };
        let to = SeekTo::Time {
            time,
            track_id: Some(self.track),
        };
        if self.format.seek(SeekMode::Coarse, to).is_ok() {
            self.decoder.reset();
            self.first = None;
        }
    }

    /// Appends the next packet's samples to `out`; `false` at the end of the file.
    pub fn next(&mut self, out: &mut Vec<f32>) -> Result<bool, String> {
        if let Some(f) = self.first.take() {
            out.extend(f);
            return Ok(true);
        }
        self.decode_packet(out)
    }

    fn decode_packet(&mut self, out: &mut Vec<f32>) -> Result<bool, String> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(false),
                Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(false);
                }
                Err(e) => return Err(e.to_string()),
            };
            if packet.track_id != self.track {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(buf) => {
                    let ch = buf.spec().channels().count();
                    self.rate = buf.spec().rate();
                    self.channels = ch.min(2) as u8;
                    let mut all = vec![0.0f32; buf.samples_interleaved()];
                    buf.copy_to_slice_interleaved(&mut all);
                    if ch <= 2 {
                        out.extend(all);
                    } else {
                        for f in all.chunks_exact(ch) {
                            out.extend_from_slice(&f[..2]);
                        }
                    }
                    return Ok(true);
                }
                // A corrupt packet is skipped.
                Err(Error::DecodeError(_) | Error::IoError(_)) => continue,
                Err(e) => return Err(e.to_string()),
            }
        }
    }
}

/// The extension a file name selects the format by.
pub fn extension(name: &str) -> &str {
    name.rsplit_once('.').map_or("", |(_, e)| e)
}

/// Decodes a whole file to 16-bit samples.
pub fn decode_all(bytes: Arc<[u8]>, ext: &str) -> Result<Pcm, String> {
    let mut d = Decoder::open(bytes, ext)?;
    let mut f = Vec::new();
    while d.next(&mut f)? {}
    Ok(Pcm {
        rate: d.rate,
        channels: d.channels,
        samples: f.iter().map(|s| (s * 32767.0).round() as i16).collect(),
    })
}

/// A file decoding into a ring as the mixer drains it.
pub struct StreamJob {
    bytes: Arc<[u8]>,
    ext: String,
    dec: Decoder,
    looping: bool,
    tx: Producer,
    /// Decoded samples the ring had no room for yet.
    pending: Vec<f32>,
    at: usize,
}

/// Decode at most this far ahead of the mixer (samples).
const RING: usize = 1 << 17;

impl StreamJob {
    /// Opens `bytes`; returns the job and the ring's consumer end with the format.
    pub fn open(
        bytes: Arc<[u8]>,
        ext: &str,
        looping: bool,
        start: f32,
    ) -> Result<(Self, crate::mixer::Stream), String> {
        let mut dec = Decoder::open(bytes.clone(), ext)?;
        if start > 0.0 {
            dec.seek_fraction(start);
        }
        let (tx, rx) = crate::ring::ring(RING);
        let stream = crate::mixer::Stream {
            ring: rx,
            rate: dec.rate,
            channels: dec.channels,
        };
        Ok((
            Self {
                bytes,
                ext: ext.to_owned(),
                dec,
                looping,
                tx,
                pending: Vec::new(),
                at: 0,
            },
            stream,
        ))
    }

    /// Decodes while the ring has room. Returns `false` when the job is over (finished, failed or the mixer
    /// dropped the voice).
    pub fn pump(&mut self) -> bool {
        self.pump_for(usize::MAX)
    }

    /// [`StreamJob::pump`] that stops after decoding about `budget` samples, for owners that share a thread
    /// with the game (the browser has none to spare). A packet already begun is finished.
    pub fn pump_for(&mut self, mut budget: usize) -> bool {
        loop {
            if self.tx.is_closed() {
                return false;
            }
            self.at += self.tx.push(&self.pending[self.at..]);
            if self.at < self.pending.len() {
                return true;
            }
            self.pending.clear();
            self.at = 0;
            // A packet is at most 1152 frames of two channels.
            if self.tx.space() < 4096 || budget == 0 {
                return true;
            }
            match self.dec.next(&mut self.pending) {
                Ok(true) => budget = budget.saturating_sub(self.pending.len()),
                Ok(false) if self.looping => match Decoder::open(self.bytes.clone(), &self.ext) {
                    Ok(d) => self.dec = d,
                    Err(_) => {
                        self.tx.finish();
                        return false;
                    }
                },
                Ok(false) | Err(_) => {
                    self.tx.finish();
                    return false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16-bit PCM WAV of a ramp, `frames` long.
    fn wav(rate: u32, channels: u16, frames: usize) -> Arc<[u8]> {
        let data: Vec<u8> = (0..frames * usize::from(channels))
            .flat_map(|i| ((i as i16).wrapping_mul(7)).to_le_bytes())
            .collect();
        let mut v = Vec::new();
        v.extend(b"RIFF");
        v.extend((36 + data.len() as u32).to_le_bytes());
        v.extend(b"WAVEfmt ");
        v.extend(16u32.to_le_bytes());
        v.extend(1u16.to_le_bytes());
        v.extend(channels.to_le_bytes());
        v.extend(rate.to_le_bytes());
        v.extend((rate * u32::from(channels) * 2).to_le_bytes());
        v.extend((channels * 2).to_le_bytes());
        v.extend(16u16.to_le_bytes());
        v.extend(b"data");
        v.extend((data.len() as u32).to_le_bytes());
        v.extend(data);
        v.into()
    }

    #[test]
    fn a_wav_decodes_to_its_samples() {
        let p = decode_all(wav(22_050, 1, 3000), "wav").unwrap();
        assert_eq!((p.rate, p.channels, p.samples.len()), (22_050, 1, 3000));
        assert_eq!(p.samples[100], 700);
        let p = decode_all(wav(44_100, 2, 500), "wav").unwrap();
        assert_eq!((p.rate, p.channels, p.frames()), (44_100, 2, 500));
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(decode_all(Arc::from(&b"definitely not audio"[..]), "wav").is_err());
    }

    #[test]
    fn a_stream_job_fills_the_ring_and_finishes_it() {
        let (mut job, mut s) = StreamJob::open(wav(22_050, 1, 5000), "wav", false, 0.0).unwrap();
        assert!(!job.pump());
        let mut all = vec![0.0; 5000];
        assert!(s.ring.pop_exact(&mut all));
        assert!(s.ring.is_finished());
    }

    #[test]
    fn a_stream_can_start_part_way_through() {
        let (mut job, s) = StreamJob::open(wav(22_050, 1, 20_000), "wav", false, 0.5).unwrap();
        assert!(!job.pump());
        let n = s.ring.len();
        assert!(
            (9_000..=11_000).contains(&n),
            "started half way but {n} of 20000 samples remain"
        );
    }

    #[test]
    fn a_looping_stream_job_never_finishes() {
        let (mut job, mut s) = StreamJob::open(wav(22_050, 1, 5000), "wav", true, 0.0).unwrap();
        for _ in 0..4 {
            assert!(job.pump());
            let mut drain = vec![0.0; s.ring.len()];
            assert!(s.ring.pop_exact(&mut drain));
            assert!(
                drain.len() > 5000,
                "the loop refilled the ring past one pass"
            );
        }
        assert!(!s.ring.is_finished());
    }

    #[test]
    fn a_budgeted_pump_decodes_in_slices_and_ends_like_a_full_one() {
        let (mut job, mut s) = StreamJob::open(wav(22_050, 1, 50_000), "wav", false, 0.0).unwrap();
        let mut got = 0;
        let mut calls = 0;
        let mut buf = vec![0.0; 50_000];
        while job.pump_for(4096) {
            calls += 1;
            let n = s.ring.len();
            assert!(n <= 8192, "a slice decoded {n} samples");
            assert!(s.ring.pop_exact(&mut buf[..n]));
            got += n;
        }
        let n = s.ring.len();
        assert!(s.ring.pop_exact(&mut buf[..n]));
        got += n;
        assert!(calls > 5 && s.ring.is_finished());
        assert_eq!(got, 50_000);
    }
}
