// SPDX-License-Identifier: GPL-3.0-or-later
//! Harness video recorder: wgpu texture -> async readback -> background AV1 (rav1e) encode -> MP4.
//!
//! The render thread only records a texture->buffer copy and polls finished maps; scaling,
//! RGB->YUV420 (BT.709, limited range) and encoding all run on the encoder thread. When the
//! readback ring or the encoder queue is full the frame is dropped and counted, never queued.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::Duration;

use rav1e::prelude::*;

/// Staging buffers in flight on the GPU/readback side.
const RING: usize = 4;
/// Frames queued for the encoder thread beyond the one being encoded.
const QUEUE: usize = 3;

/// Result of a finished recording.
#[derive(Debug, Clone)]
pub struct VideoStats {
    pub path: PathBuf,
    pub frames: u64,
    pub dropped: u64,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

struct Frame8 {
    bgra: bool,
    slot: u64,
    /// Padded rows as read back (`stride` bytes per row).
    data: Vec<u8>,
}

struct Staging {
    buf: wgpu::Buffer,
    ready: Arc<AtomicBool>,
}

pub struct Recorder {
    path: PathBuf,
    src: (u32, u32),
    out: (u32, u32),
    fps: u32,
    stride: u32,
    format: Option<wgpu::TextureFormat>,
    bgra: bool,
    ring: Vec<Staging>,
    free: Vec<usize>,
    pending: VecDeque<(usize, u64)>,
    last_slot: Option<u64>,
    dropped: u64,
    tx: SyncSender<Frame8>,
    thread: JoinHandle<io::Result<(u64, u64)>>,
}

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

impl Recorder {
    /// `src` = size of the textures passed to `capture`; output height is `<= max_height`,
    /// aspect is kept, both dimensions are even.
    pub fn start(path: &Path, src: (u32, u32), max_height: u32, fps: u32) -> io::Result<Recorder> {
        if src.0 == 0 || src.1 == 0 || fps == 0 || max_height < 2 {
            return Err(other("invalid recorder size/fps"));
        }
        let oh = (src.1.min(max_height)) & !1;
        let ow =
            (((src.0 as u64 * oh as u64 + src.1 as u64 / 2) / src.1 as u64) as u32).max(2) & !1;
        let path = path.with_extension("mp4");
        let stride = (src.0 * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);

        let mut enc = EncoderConfig::with_speed_preset(10);
        enc.width = ow as usize;
        enc.height = oh as usize;
        enc.bit_depth = 8;
        enc.chroma_sampling = ChromaSampling::Cs420;
        enc.pixel_range = PixelRange::Limited;
        enc.color_description = Some(ColorDescription {
            color_primaries: ColorPrimaries::BT709,
            transfer_characteristics: TransferCharacteristics::BT709,
            matrix_coefficients: MatrixCoefficients::BT709,
        });
        enc.time_base = Rational::new(1, fps as u64);
        enc.low_latency = true;
        enc.speed_settings.rdo_lookahead_frames = 1;
        enc.speed_settings.cdef = false; // ~25% faster; rav1e has no asm here
        enc.quantizer = 110;
        enc.min_quantizer = 110;
        enc.tile_cols = 4;
        enc.tile_rows = 4;
        enc.max_key_frame_interval = fps as u64 * 4;
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        let cfg = Config::new().with_encoder_config(enc).with_threads(threads);
        let ctx: Context<u8> = cfg.new_context().map_err(other)?;
        let seq_header = ctx.container_sequence_header();

        let mut file = BufWriter::new(File::create(&path)?);
        let mux = Mux::begin(&mut file, ow, oh, fps, &seq_header)?;

        let (tx, rx) = mpsc::sync_channel::<Frame8>(QUEUE);
        let job = EncodeJob {
            ctx,
            file,
            mux,
            rx,
            src,
            out: (ow, oh),
            stride: stride as usize,
        };
        let thread = std::thread::Builder::new()
            .name("video-encoder".into())
            .spawn(move || job.run())?;
        Ok(Recorder {
            path,
            src,
            out: (ow, oh),
            fps,
            stride,
            format: None,
            bgra: false,
            ring: Vec::new(),
            free: Vec::new(),
            pending: VecDeque::new(),
            last_slot: None,
            dropped: 0,
            tx,
            thread,
        })
    }

    /// Record at most one frame per `1/fps`; returns false when skipped (too soon, or the
    /// readback ring / encoder is saturated, in which case it is counted in `dropped`).
    pub fn capture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        elapsed: Duration,
    ) -> bool {
        assert_eq!(
            (texture.width(), texture.height()),
            self.src,
            "texture size != recorder source size"
        );
        let slot = (elapsed.as_secs_f64() * self.fps as f64) as u64;
        if self.last_slot.is_some_and(|l| slot <= l) {
            return false;
        }
        if self.format.is_none() {
            let f = texture.format();
            assert!(
                matches!(
                    f,
                    wgpu::TextureFormat::Rgba8Unorm
                        | wgpu::TextureFormat::Rgba8UnormSrgb
                        | wgpu::TextureFormat::Bgra8Unorm
                        | wgpu::TextureFormat::Bgra8UnormSrgb
                ),
                "unsupported recorder texture format {f:?}"
            );
            self.format = Some(f);
            self.bgra = matches!(
                f,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
            );
            let size = self.stride as u64 * self.src.1 as u64;
            for i in 0..RING {
                self.ring.push(Staging {
                    buf: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("video staging"),
                        size,
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    }),
                    ready: Arc::new(AtomicBool::new(false)),
                });
                self.free.push(i);
            }
        }
        let _ = device.poll(wgpu::PollType::Poll);
        self.drain(false);

        let Some(idx) = self.free.pop() else {
            self.dropped += 1;
            return false;
        };
        let st = &self.ring[idx];
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("video copy"),
        });
        enc.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &st.buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.stride),
                    rows_per_image: Some(self.src.1),
                },
            },
            wgpu::Extent3d {
                width: self.src.0,
                height: self.src.1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([enc.finish()]);
        let ready = st.ready.clone();
        st.buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            if r.is_ok() {
                ready.store(true, Ordering::Release);
            }
        });
        self.pending.push_back((idx, slot));
        self.last_slot = Some(slot);
        true
    }

    /// Move finished readbacks to the encoder. `block` = wait for all (finish) and use a
    /// blocking send; otherwise a full queue drops the frame.
    fn drain(&mut self, block: bool) {
        while let Some(&(idx, slot)) = self.pending.front() {
            let st = &self.ring[idx];
            if !st.ready.load(Ordering::Acquire) {
                break;
            }
            self.pending.pop_front();
            let data = st.buf.slice(..).get_mapped_range().unwrap().to_vec();
            st.buf.unmap();
            st.ready.store(false, Ordering::Release);
            self.free.push(idx);
            let frame = Frame8 {
                bgra: self.bgra,
                slot,
                data,
            };
            if block {
                let _ = self.tx.send(frame);
            } else if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) =
                self.tx.try_send(frame)
            {
                self.dropped += 1;
            }
        }
    }

    /// Drain pending readbacks, flush the encoder and finalize the file.
    pub fn finish(mut self, device: &wgpu::Device) -> io::Result<VideoStats> {
        while !self.pending.is_empty() {
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .map_err(other)?;
            self.drain(true);
        }
        let Recorder {
            path,
            out,
            fps,
            tx,
            thread,
            dropped,
            ..
        } = self;
        drop(tx);
        let (frames, bytes) = thread
            .join()
            .map_err(|_| other("encoder thread panicked"))??;
        Ok(VideoStats {
            path,
            frames,
            dropped,
            bytes,
            width: out.0,
            height: out.1,
            fps,
        })
    }
}

struct EncodeJob {
    ctx: Context<u8>,
    file: BufWriter<File>,
    mux: Mux,
    rx: mpsc::Receiver<Frame8>,
    src: (u32, u32),
    out: (u32, u32),
    stride: usize,
}

impl EncodeJob {
    fn run(mut self) -> io::Result<(u64, u64)> {
        let (ow, oh) = (self.out.0 as usize, self.out.1 as usize);
        let rgb_order = |bgra| Scaler::new(self.src, self.out, bgra);
        let (rgba_scaler, bgra_scaler) = (rgb_order(false), rgb_order(true));
        let mut rgb = vec![0u8; ow * oh * 3];
        let mut slots: Vec<u64> = Vec::new();
        while let Ok(f) = self.rx.recv() {
            (if f.bgra { &bgra_scaler } else { &rgba_scaler }).scale(
                &f.data,
                self.stride,
                &mut rgb,
            );
            let mut frame = self.ctx.new_frame();
            let [y, u, v] = yuv420(&rgb, ow, oh);
            frame.planes[0].copy_from_raw_u8(&y, ow, 1);
            frame.planes[1].copy_from_raw_u8(&u, ow / 2, 1);
            frame.planes[2].copy_from_raw_u8(&v, ow / 2, 1);
            slots.push(f.slot);
            self.ctx.send_frame(frame).map_err(other)?;
            self.pump(&slots)?;
        }
        self.ctx.flush();
        self.pump(&slots)?;
        let frames = self.mux.finish(&mut self.file)?;
        self.file.flush()?;
        let bytes = self.file.get_ref().metadata()?.len();
        Ok((frames, bytes))
    }

    fn pump(&mut self, slots: &[u64]) -> io::Result<()> {
        loop {
            match self.ctx.receive_packet() {
                Ok(p) => {
                    let slot = slots[p.input_frameno as usize];
                    self.mux.push(
                        &mut self.file,
                        &p.data,
                        p.frame_type == FrameType::KEY,
                        slot,
                    )?;
                }
                Err(EncoderStatus::Encoded) => {}
                Err(EncoderStatus::NeedMoreData | EncoderStatus::LimitReached) => return Ok(()),
                Err(e) => return Err(other(e)),
            }
        }
    }
}

/// Bilinear RGBA/BGRA -> packed RGB scaler.
struct Scaler {
    sw: usize,
    ow: usize,
    oh: usize,
    /// per output column: (x0*4, x1*4, weight of x1 in 0..=256)
    xs: Vec<(usize, usize, u32)>,
    /// per output row: (y0, y1, weight of y1)
    ys: Vec<(usize, usize, u32)>,
    /// byte offsets of R, G, B within a source pixel
    rgb: [usize; 3],
}

impl Scaler {
    fn new(src: (u32, u32), out: (u32, u32), bgra: bool) -> Scaler {
        let axis = |s: u32, o: u32| -> Vec<(usize, usize, u32)> {
            let k = s as f64 / o as f64;
            (0..o)
                .map(|i| {
                    let p = ((i as f64 + 0.5) * k - 0.5).clamp(0.0, (s - 1) as f64);
                    let i0 = p.floor() as usize;
                    let i1 = (i0 + 1).min(s as usize - 1);
                    (i0, i1, ((p - i0 as f64) * 256.0) as u32)
                })
                .collect()
        };
        let xs = axis(src.0, out.0)
            .into_iter()
            .map(|(a, b, w)| (a * 4, b * 4, w))
            .collect();
        Scaler {
            sw: src.0 as usize,
            ow: out.0 as usize,
            oh: out.1 as usize,
            xs,
            ys: axis(src.1, out.1),
            rgb: if bgra { [2, 1, 0] } else { [0, 1, 2] },
        }
    }

    fn scale(&self, data: &[u8], stride: usize, rgb: &mut [u8]) {
        for (oy, &(y0, y1, wy)) in self.ys.iter().enumerate().take(self.oh) {
            let r0 = &data[y0 * stride..y0 * stride + self.sw * 4];
            let r1 = &data[y1 * stride..y1 * stride + self.sw * 4];
            let dst = &mut rgb[oy * self.ow * 3..(oy + 1) * self.ow * 3];
            for (d, &(x0, x1, wx)) in dst.as_chunks_mut::<3>().0.iter_mut().zip(&self.xs) {
                for (c, &o) in self.rgb.iter().enumerate() {
                    let top = r0[x0 + o] as u32 * (256 - wx) + r0[x1 + o] as u32 * wx;
                    let bot = r1[x0 + o] as u32 * (256 - wx) + r1[x1 + o] as u32 * wx;
                    d[c] = ((top * (256 - wy) + bot * wy + 32768) >> 16) as u8;
                }
            }
        }
    }
}

/// Packed RGB -> planar Y, U, V (4:2:0, BT.709 limited range). `w`,`h` even.
fn yuv420(rgb: &[u8], w: usize, h: usize) -> [Vec<u8>; 3] {
    let mut y = vec![0u8; w * h];
    let mut u = vec![0u8; w * h / 4];
    let mut v = vec![0u8; w * h / 4];
    for (row, yrow) in y.chunks_exact_mut(w).enumerate() {
        for (yo, p) in yrow
            .iter_mut()
            .zip(rgb[row * w * 3..].as_chunks::<3>().0.iter())
        {
            let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
            *yo = (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16) as u8;
        }
    }
    for cy in 0..h / 2 {
        let r0 = &rgb[(cy * 2) * w * 3..(cy * 2 + 1) * w * 3];
        let r1 = &rgb[(cy * 2 + 1) * w * 3..(cy * 2 + 2) * w * 3];
        let (ur, vr) = (
            &mut u[cy * w / 2..(cy + 1) * w / 2],
            &mut v[cy * w / 2..(cy + 1) * w / 2],
        );
        for (cx, (uo, vo)) in ur.iter_mut().zip(vr.iter_mut()).enumerate() {
            let a = cx * 6;
            let avg = |c: usize| {
                (r0[a + c] as i32
                    + r0[a + 3 + c] as i32
                    + r1[a + c] as i32
                    + r1[a + 3 + c] as i32
                    + 2)
                    >> 2
            };
            let (r, g, b) = (avg(0), avg(1), avg(2));
            *uo = (((-26 * r - 86 * g + 112 * b + 128) >> 8) + 128) as u8;
            *vo = (((112 * r - 102 * g - 10 * b + 128) >> 8) + 128) as u8;
        }
    }
    [y, u, v]
}

// ---------------------------------------------------------------------------------------
// Minimal ISO-BMFF (MP4) muxer: mdat written first, moov appended at the end.

struct Mux {
    w: u32,
    h: u32,
    timescale: u32,
    av1c: Vec<u8>,
    mdat_pos: u64,
    offset: u64,
    sizes: Vec<u32>,
    offsets: Vec<u64>,
    slots: Vec<u64>,
    sync: Vec<u32>,
}

fn bx(kind: &[u8; 4], body: &[&[u8]]) -> Vec<u8> {
    let len: usize = 8 + body.iter().map(|b| b.len()).sum::<usize>();
    let mut v = Vec::with_capacity(len);
    v.extend_from_slice(&(len as u32).to_be_bytes());
    v.extend_from_slice(kind);
    for b in body {
        v.extend_from_slice(b);
    }
    v
}

fn u32s(vals: &[u32]) -> Vec<u8> {
    vals.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// Build the `av1C` payload from the encoder's sequence header OBU (4:2:0, 8-bit).
fn av1c(seq_obu: &[u8]) -> Vec<u8> {
    // OBU header byte, then a one-byte leb128 size for headers this small.
    let payload = seq_obu.get(2..).unwrap_or(&[]);
    let mut bits = payload
        .iter()
        .flat_map(|b| (0..8).rev().map(move |i| (b >> i) & 1));
    let mut read = |n: u32| (0..n).fold(0u32, |a, _| (a << 1) | bits.next().unwrap_or(0) as u32);
    let profile = read(3);
    read(1); // still_picture
    let reduced = read(1);
    let timing = read(1);
    let (mut level, mut tier) = (8, 0);
    if reduced == 0 && timing == 0 {
        read(1); // initial_display_delay_present_flag
        read(5); // operating_points_cnt_minus_1
        read(12); // operating_point_idc[0]
        level = read(5);
        if level > 7 {
            tier = read(1);
        }
    }
    vec![
        0x81,
        (profile << 5 | level) as u8,
        (tier << 7 | 0b0000_1100) as u8,
        0,
    ]
}

impl Mux {
    fn begin(
        f: &mut (impl Write + Seek),
        w: u32,
        h: u32,
        fps: u32,
        seq_obu: &[u8],
    ) -> io::Result<Mux> {
        f.write_all(&bx(
            b"ftyp",
            &[b"isom", &0u32.to_be_bytes(), b"isomiso2av01mp41"],
        ))?;
        let mdat_pos = f.stream_position()?;
        f.write_all(&[0, 0, 0, 0, b'm', b'd', b'a', b't'])?;
        Ok(Mux {
            w,
            h,
            timescale: fps,
            av1c: av1c(seq_obu),
            mdat_pos,
            offset: mdat_pos + 8,
            sizes: Vec::new(),
            offsets: Vec::new(),
            slots: Vec::new(),
            sync: Vec::new(),
        })
    }

    fn push(&mut self, f: &mut impl Write, data: &[u8], key: bool, slot: u64) -> io::Result<()> {
        // Samples must not carry the temporal delimiter OBU (type 2, size 0).
        let data = data.strip_prefix(&[0x12, 0x00]).unwrap_or(data);
        f.write_all(data)?;
        self.sizes.push(data.len() as u32);
        self.offsets.push(self.offset);
        self.offset += data.len() as u64;
        self.slots.push(slot);
        if key {
            self.sync.push(self.sizes.len() as u32);
        }
        Ok(())
    }

    /// Patch the mdat size, append moov; returns the sample count.
    fn finish(self, f: &mut (impl Write + Seek)) -> io::Result<u64> {
        let end = self.offset;
        let n = self.sizes.len();
        let ver0 = [0u8; 4];
        let dur: Vec<u32> = (0..n)
            .map(|i| {
                self.slots
                    .get(i + 1)
                    .map_or(1, |&s| (s - self.slots[i]) as u32)
            })
            .collect();
        let total: u32 = dur.iter().sum();

        // stts: run-length encode durations.
        let mut runs: Vec<(u32, u32)> = Vec::new();
        for &d in &dur {
            match runs.last_mut() {
                Some((c, rd)) if *rd == d => *c += 1,
                _ => runs.push((1, d)),
            }
        }
        let stts_body: Vec<u8> = runs
            .iter()
            .flat_map(|&(c, d)| [c, d])
            .flat_map(|v| v.to_be_bytes())
            .collect();
        let stts = bx(
            b"stts",
            &[&ver0, &(runs.len() as u32).to_be_bytes(), &stts_body],
        );
        let stss = bx(
            b"stss",
            &[
                &ver0,
                &(self.sync.len() as u32).to_be_bytes(),
                &u32s(&self.sync),
            ],
        );
        let stsc = bx(b"stsc", &[&ver0, &u32s(&[1, 1, 1, 1])]);
        let stsz = bx(b"stsz", &[&ver0, &u32s(&[0, n as u32]), &u32s(&self.sizes)]);
        let co: Vec<u32> = self.offsets.iter().map(|&o| o as u32).collect();
        let stco = bx(b"stco", &[&ver0, &(n as u32).to_be_bytes(), &u32s(&co)]);

        let mut entry = vec![0u8; 6];
        entry.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        entry.extend_from_slice(&[0; 16]); // pre_defined / reserved
        entry.extend_from_slice(&(self.w as u16).to_be_bytes());
        entry.extend_from_slice(&(self.h as u16).to_be_bytes());
        entry.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // 72 dpi
        entry.extend_from_slice(&0x0048_0000u32.to_be_bytes());
        entry.extend_from_slice(&[0; 4]);
        entry.extend_from_slice(&1u16.to_be_bytes()); // frame_count
        entry.extend_from_slice(&[0; 32]); // compressorname
        entry.extend_from_slice(&0x0018u16.to_be_bytes()); // depth
        entry.extend_from_slice(&0xFFFFu16.to_be_bytes()); // pre_defined = -1
        let colr = bx(
            b"colr",
            &[
                b"nclx",
                &1u16.to_be_bytes(),
                &1u16.to_be_bytes(),
                &1u16.to_be_bytes(),
                &[0],
            ],
        );
        let av01 = bx(b"av01", &[&entry, &bx(b"av1C", &[&self.av1c]), &colr]);
        let stsd = bx(b"stsd", &[&ver0, &1u32.to_be_bytes(), &av01]);
        let stbl = bx(b"stbl", &[&stsd, &stts, &stss, &stsc, &stsz, &stco]);

        let url = bx(b"url ", &[&[0, 0, 0, 1]]);
        let dref = bx(b"dref", &[&ver0, &1u32.to_be_bytes(), &url]);
        let dinf = bx(b"dinf", &[&dref]);
        let vmhd = bx(b"vmhd", &[&[0, 0, 0, 1], &[0; 8]]);
        let minf = bx(b"minf", &[&vmhd, &dinf, &stbl]);
        let hdlr = bx(b"hdlr", &[&ver0, &[0; 4], b"vide", &[0; 12], b"video\0"]);
        let mdhd = bx(
            b"mdhd",
            &[
                &ver0,
                &[0; 8],
                &self.timescale.to_be_bytes(),
                &total.to_be_bytes(),
                &[0x55, 0xC4, 0, 0],
            ],
        );
        let mdia = bx(b"mdia", &[&mdhd, &hdlr, &minf]);

        let matrix = u32s(&[0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000]);
        let movie_dur = total; // movie timescale == media timescale
        let tkhd = bx(
            b"tkhd",
            &[
                &[0, 0, 0, 3],
                &[0; 8],
                &1u32.to_be_bytes(),
                &[0; 4],
                &movie_dur.to_be_bytes(),
                &[0; 16],
                &matrix,
                &(self.w << 16).to_be_bytes(),
                &(self.h << 16).to_be_bytes(),
            ],
        );
        let trak = bx(b"trak", &[&tkhd, &mdia]);
        let mvhd = bx(
            b"mvhd",
            &[
                &ver0,
                &[0; 8],
                &self.timescale.to_be_bytes(),
                &movie_dur.to_be_bytes(),
                &0x0001_0000u32.to_be_bytes(), // rate
                &0x0100u16.to_be_bytes(),      // volume
                &[0; 10],
                &matrix,
                &[0; 24],
                &2u32.to_be_bytes(), // next_track_ID
            ],
        );
        f.write_all(&bx(b"moov", &[&mvhd, &trak]))?;
        f.seek(SeekFrom::Start(self.mdat_pos))?;
        f.write_all(&((end - self.mdat_pos) as u32).to_be_bytes())?;
        Ok(n as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    }

    /// Count `stsz` sample_count in the moov of an MP4 (our own muxer's layout).
    fn mp4_samples(b: &[u8]) -> Option<u32> {
        let i = b.windows(4).rposition(|w| w == b"stsz")?;
        Some(u32::from_be_bytes(b[i + 12..i + 16].try_into().ok()?))
    }

    #[test]
    fn records_three_seconds_of_moving_pattern() {
        let Some((device, queue)) = device() else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        let (w, h) = (1920u32, 1080u32);
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let dir = std::env::temp_dir().join(format!("cod4e-video-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut rec = Recorder::start(&dir.join("t.out"), (w, h), 960, 30).unwrap();
        // A handful of distinct frames, cycled, so the render loop outruns the 30 fps cap.
        let patterns: Vec<Vec<u8>> = (0..8u32)
            .map(|i| {
                let mut px = vec![0u8; (w * h * 4) as usize];
                for (y, row) in px.chunks_exact_mut(w as usize * 4).enumerate() {
                    for (x, p) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                        // Smooth gradient plus a moving box: game-like, not worst-case noise.
                        let (xx, yy) = (x as u32, y as u32);
                        let bx = i * 200;
                        let inside = xx >= bx && xx < bx + 200 && (400..600).contains(&yy);
                        let c = if inside {
                            [250, 40, 40]
                        } else {
                            [(xx / 8) as u8, (yy / 5) as u8, 90 + i as u8]
                        };
                        p.copy_from_slice(&[c[0], c[1], c[2], 255]);
                    }
                }
                px
            })
            .collect();
        let t0 = Instant::now();
        let mut captured = 0u64;
        let mut ticks = 0usize;
        // Simulated 120 Hz render clock: capture cadence must not depend on runner speed (WARP on CI is slow).
        while ticks < 360 {
            let now = Duration::from_secs_f64(ticks as f64 / 120.0);
            queue.write_texture(
                tex.as_image_copy(),
                &patterns[ticks % 8],
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 4),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            if rec.capture(&device, &queue, &tex, now) {
                captured += 1;
            }
            ticks += 1;
        }
        let t_cap = t0.elapsed();
        let stats = rec.finish(&device).unwrap();
        let t_all = t0.elapsed();
        eprintln!(
            "captured {captured}, encoded {} dropped {} -> {}x{} {} bytes; capture loop {:?}, total {:?} ({:.1} encoded fps)",
            stats.frames,
            stats.dropped,
            stats.width,
            stats.height,
            stats.bytes,
            t_cap,
            t_all,
            stats.frames as f64 / t_all.as_secs_f64()
        );
        assert_eq!((stats.width, stats.height), (1706, 960));
        let bytes = std::fs::read(&stats.path).unwrap();
        assert_eq!(bytes.len() as u64, stats.bytes);
        assert_eq!(stats.path.extension().unwrap(), "mp4");
        assert_eq!(mp4_samples(&bytes), Some(stats.frames as u32));
        // Every captured frame was either encoded or lost to a full queue; none vanished.
        assert!(
            stats.frames > 0 && stats.frames + stats.dropped >= 80,
            "{stats:?}"
        );
        assert!(captured >= 80, "capture cadence: {captured}");
        let i = bytes.windows(4).rposition(|w| w == b"mdhd").unwrap();
        let dur = u32::from_be_bytes(bytes[i + 20..i + 24].try_into().unwrap());
        assert!(
            (84..=91).contains(&dur),
            "media duration {dur} ticks (30/s)"
        );
        if std::process::Command::new("ffprobe")
            .arg("-version")
            .output()
            .is_ok()
        {
            let o = std::process::Command::new("ffprobe")
                .args([
                    "-v",
                    "error",
                    "-count_frames",
                    "-select_streams",
                    "v:0",
                    "-show_entries",
                    "stream=codec_name,nb_read_frames,width,height",
                    "-of",
                    "csv=p=0",
                ])
                .arg(&stats.path)
                .output()
                .unwrap();
            let s = String::from_utf8_lossy(&o.stdout);
            eprintln!("ffprobe: {s}{}", String::from_utf8_lossy(&o.stderr));
            assert!(
                s.contains("av1") && s.contains(&format!(",{}", stats.frames)),
                "ffprobe: {s}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
