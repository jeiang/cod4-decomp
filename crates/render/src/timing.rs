// SPDX-License-Identifier: GPL-3.0-only
//! GPU pass timing with timestamp queries.
//!
//! Every render pass of a frame writes a begin and an end timestamp. The frame resolves them into a buffer, copies that
//! to one of a few staging buffers and maps it; a few frames later [`GpuTimer::begin_frame`] finds the mapping done and
//! turns the ticks into milliseconds. The CPU never waits for the GPU.

use crate::gpu::Gpu;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Passes per frame the timer can record.
const MAX_PASSES: usize = 16;
const SLOTS: usize = 4;

struct Slot {
    staging: wgpu::Buffer,
    ready: Arc<AtomicBool>,
    in_flight: bool,
    /// The frame (count of [`GpuTimer::begin_frame`] calls) whose passes the slot holds.
    frame: u64,
    names: Vec<&'static str>,
}

/// A slot of [`GpuTimer::span`].
#[derive(Clone, Copy)]
pub struct Span(u32);

pub struct GpuTimer {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    slots: Vec<Slot>,
    /// Nanoseconds per tick.
    period: f64,
    names: Vec<&'static str>,
    frame: u64,
    /// Completed frames not yet taken: `(frame, total ms)`.
    finished: Vec<(u64, f64)>,
    /// The slot this frame's results go to.
    current: Option<usize>,
    /// Pass durations of the most recent completed frame, in milliseconds.
    pub last: Vec<(&'static str, f64)>,
    /// First begin to last end of that frame, in milliseconds.
    pub last_total_ms: Option<f64>,
}

impl GpuTimer {
    /// `None` when the device has no timestamp queries.
    pub fn new(gpu: &Gpu) -> Option<GpuTimer> {
        if !gpu.timestamps {
            return None;
        }
        let n = (MAX_PASSES * 2) as u32;
        let size = u64::from(n) * 8;
        let slots = (0..SLOTS)
            .map(|_| Slot {
                staging: gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("timestamps staging"),
                    size,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                ready: Arc::new(AtomicBool::new(false)),
                in_flight: false,
                frame: 0,
                names: Vec::new(),
            })
            .collect();
        Some(GpuTimer {
            set: gpu.device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("frame timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: n,
            }),
            resolve: gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("timestamps resolve"),
                size,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            slots,
            period: f64::from(gpu.queue.get_timestamp_period()),
            names: Vec::new(),
            frame: 0,
            finished: Vec::new(),
            current: None,
            last: Vec::new(),
            last_total_ms: None,
        })
    }

    /// Collect finished frames and pick the staging slot for this one.
    pub fn begin_frame(&mut self, gpu: &Gpu) {
        let _ = gpu.device.poll(wgpu::PollType::Poll);
        self.collect();
        self.frame += 1;
        self.names.clear();
        self.current = self.slots.iter().position(|s| !s.in_flight);
    }

    /// Read every finished slot, oldest frame first.
    fn collect(&mut self) {
        let mut done: Vec<usize> = (0..self.slots.len())
            .filter(|&i| self.slots[i].in_flight && self.slots[i].ready.load(Ordering::Acquire))
            .collect();
        done.sort_by_key(|&i| self.slots[i].frame);
        for i in done {
            self.read(i);
        }
    }

    fn read(&mut self, i: usize) {
        let slot = &mut self.slots[i];
        let ticks: Vec<u64> = {
            let Ok(view) = slot.staging.slice(..).get_mapped_range() else {
                slot.in_flight = false;
                return;
            };
            view.as_chunks::<8>()
                .0
                .iter()
                .map(|b| u64::from_le_bytes(*b))
                .collect()
        };
        slot.staging.unmap();
        slot.in_flight = false;
        slot.ready.store(false, Ordering::Release);
        let period = self.period;
        let ms = |a: u64, b: u64| b.saturating_sub(a) as f64 * period * 1e-6;
        self.last = slot
            .names
            .iter()
            .enumerate()
            .map(|(k, n)| (*n, ms(ticks[2 * k], ticks[2 * k + 1])))
            .collect();
        let first = (0..slot.names.len()).map(|k| ticks[2 * k]).min();
        let end = (0..slot.names.len()).map(|k| ticks[2 * k + 1]).max();
        self.last_total_ms = first.zip(end).map(|(a, b)| ms(a, b));
        if let Some(total) = self.last_total_ms {
            self.finished.push((slot.frame, total));
        }
    }

    /// The frame number of the frame being recorded: 1 for the first [`GpuTimer::begin_frame`].
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// Frames whose timings arrived since the last call: `(frame number, GPU milliseconds first begin to last end)`.
    pub fn take_finished(&mut self) -> Vec<(u64, f64)> {
        std::mem::take(&mut self.finished)
    }

    /// Timestamp writes for a pass named `name`; `None` when this frame cannot be timed.
    pub fn pass(&mut self, name: &'static str) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        self.current?;
        let k = self.names.len();
        if k >= MAX_PASSES {
            return None;
        }
        self.names.push(name);
        Some(wgpu::RenderPassTimestampWrites {
            query_set: &self.set,
            beginning_of_pass_write_index: Some(2 * k as u32),
            end_of_pass_write_index: Some(2 * k as u32 + 1),
        })
    }

    /// A timed stretch of consecutive passes named `name`, which takes one of the frame's slots however many passes it
    /// covers; `None` when this frame cannot be timed.
    pub fn span(&mut self, name: &'static str) -> Option<Span> {
        self.current?;
        let k = self.names.len();
        if k >= MAX_PASSES {
            return None;
        }
        self.names.push(name);
        Some(Span(k as u32))
    }

    /// Timestamp writes of one pass of `span`: the first writes the begin, the last the end, the ones between none.
    pub fn writes(
        &self,
        span: Span,
        first: bool,
        last: bool,
    ) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        (first || last).then(|| wgpu::RenderPassTimestampWrites {
            query_set: &self.set,
            beginning_of_pass_write_index: first.then_some(2 * span.0),
            end_of_pass_write_index: last.then_some(2 * span.0 + 1),
        })
    }

    /// Resolve this frame's timestamps; call before finishing the encoder.
    pub fn resolve(&mut self, enc: &mut wgpu::CommandEncoder) {
        let (Some(i), n) = (self.current, self.names.len() as u32) else {
            return;
        };
        if n == 0 {
            return;
        }
        enc.resolve_query_set(&self.set, 0..2 * n, &self.resolve, 0);
        enc.copy_buffer_to_buffer(
            &self.resolve,
            0,
            &self.slots[i].staging,
            0,
            u64::from(2 * n) * 8,
        );
    }

    /// Start mapping this frame's results; call after the queue submit.
    pub fn submitted(&mut self) {
        let Some(i) = self.current else { return };
        if self.names.is_empty() {
            return;
        }
        let slot = &mut self.slots[i];
        slot.frame = self.frame;
        slot.names = std::mem::take(&mut self.names);
        slot.in_flight = true;
        let ready = slot.ready.clone();
        slot.staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| {
                if r.is_ok() {
                    ready.store(true, Ordering::Release);
                }
            });
    }

    /// Block until every frame in flight has been read, for tools that need the last numbers.
    pub fn flush(&mut self, gpu: &Gpu) {
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        self.collect();
    }
}
