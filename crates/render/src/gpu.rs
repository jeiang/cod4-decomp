// SPDX-License-Identifier: GPL-3.0-only
//! Instance, adapter and device creation, and the facts about them the harness reports.

use std::fmt;
use std::sync::{Arc, Mutex};

/// The wgpu device and what it supports. Cheap to share by reference; wgpu handles are reference counted.
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// BC1-3 textures are uploaded as they are; otherwise they are decoded to RGBA8 on the CPU.
    pub bc: bool,
    /// Indexed draws may start at a base vertex; WebGL2 has no such draw.
    pub base_vertex: bool,
    /// GPU pass timing via timestamp queries.
    pub timestamps: bool,
    health: Arc<Health>,
}

/// What the device reported through its callbacks: wgpu would panic on these without a handler, and a lost device
/// reports nothing else (calls on it only fail later, far from the cause).
#[derive(Default)]
struct Health {
    lost: Mutex<Option<String>>,
    errors: Mutex<Vec<String>>,
}

/// The size a buffer holding `len` bytes is created with: a whole number of copy units, at least one.
fn padded_size(len: u64) -> u64 {
    len.next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
        .max(wgpu::COPY_BUFFER_ALIGNMENT)
}

/// Errors kept for the message of the failure they led to.
const KEPT_ERRORS: usize = 8;

#[derive(Debug)]
pub enum GpuError {
    NoAdapter(String),
    Device(String),
}

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GpuError::NoAdapter(e) => write!(f, "no suitable GPU adapter: {e}"),
            GpuError::Device(e) => write!(f, "cannot create the GPU device: {e}"),
        }
    }
}

impl std::error::Error for GpuError {}

impl Gpu {
    /// `surface` makes the adapter choice compatible with a window; `None` is headless.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(surface: Option<&wgpu::Surface<'_>>) -> Result<Gpu, GpuError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        Self::with_instance(instance, surface)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_instance(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Gpu, GpuError> {
        pollster::block_on(Self::with_instance_async(instance, surface))
    }

    /// The browser has no blocking wait for the adapter and device, so this is the only constructor on the web.
    pub async fn with_instance_async(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Gpu, GpuError> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: surface,
                ..Default::default()
            })
            .await
            .map_err(|e| GpuError::NoAdapter(e.to_string()))?;
        let have = adapter.features();
        let want =
            (wgpu::Features::TEXTURE_COMPRESSION_BC | wgpu::Features::TIMESTAMP_QUERY) & have;
        // The WebGL2 fallback runs on the downlevel limits; the adapter's own are already that tier.
        let adapter_backend = adapter.get_info().backend;
        let base = if cfg!(target_arch = "wasm32") && adapter_backend == wgpu::Backend::Gl {
            wgpu::Limits::downlevel_webgl2_defaults()
        } else {
            wgpu::Limits::default()
        };
        let have_limits = adapter.limits();
        // A world's vertex buffer can pass the 256 MiB default; take what the adapter offers.
        let mut limits = base.using_resolution(have_limits.clone());
        limits.max_buffer_size = have_limits.max_buffer_size;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("cod4e"),
                required_features: want,
                required_limits: limits,
                ..Default::default()
            })
            .await
            .map_err(|e| GpuError::Device(e.to_string()))?;
        let health = Arc::new(Health::default());
        let h = health.clone();
        device.set_device_lost_callback(move |reason, message| {
            let text = format!("{reason:?}: {message}");
            eprintln!("graphics device lost ({text})");
            *h.lost.lock().unwrap_or_else(|e| e.into_inner()) = Some(text);
        });
        let h = health.clone();
        device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
            eprintln!("graphics error: {e}");
            let mut errors = h.errors.lock().unwrap_or_else(|e| e.into_inner());
            if errors.len() < KEPT_ERRORS {
                errors.push(e.to_string());
            }
        }));
        Ok(Gpu {
            health,
            instance,
            adapter,
            device,
            queue,
            base_vertex: !(cfg!(target_arch = "wasm32") && adapter_backend == wgpu::Backend::Gl),
            bc: want.contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
            timestamps: want.contains(wgpu::Features::TIMESTAMP_QUERY),
        })
    }

    /// Why the device was lost, once it is: every call on it fails from then on and nothing can be recovered
    /// without a new device and new resources. wgpu reports the loss from a device poll, so this polls.
    pub fn lost(&self) -> Option<String> {
        let _ = self.device.poll(wgpu::PollType::Poll);
        self.health
            .lost
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The errors wgpu reported outside any error scope since the last call, oldest first.
    pub fn take_errors(&self) -> Vec<String> {
        std::mem::take(&mut *self.health.errors.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// A buffer holding `contents`, or why it could not be made: larger than the device's `max_buffer_size`, the
    /// device lost, or the creation rejected (the reported errors are named).
    pub fn buffer_init(
        &self,
        label: Option<&str>,
        contents: &[u8],
        usage: wgpu::BufferUsages,
    ) -> Result<wgpu::Buffer, String> {
        let name = label.unwrap_or("unnamed");
        let size = padded_size(contents.len() as u64);
        let max = self.device.limits().max_buffer_size;
        if size > max {
            return Err(format!(
                "the buffer '{name}' needs {size} bytes, over the device's limit of {max}"
            ));
        }
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label,
            size,
            usage,
            mapped_at_creation: true,
        });
        let mapped = buffer.get_mapped_range_mut(..);
        match mapped {
            Ok(mut view) => {
                view.slice(..contents.len()).copy_from_slice(contents);
                drop(view);
                buffer.unmap();
                Ok(buffer)
            }
            Err(e) => Err(self.explain(&format!(
                "cannot create the buffer '{name}' ({size} bytes): {e}"
            ))),
        }
    }

    /// `what` with the device's loss and the errors it reported appended.
    pub fn explain(&self, what: &str) -> String {
        let mut out = what.to_owned();
        if let Some(l) = self.lost() {
            out += &format!("; the graphics device was lost ({l})");
        }
        for e in self.take_errors() {
            out += &format!("; {}", e.replace('\n', " ").trim());
        }
        out
    }

    /// Adapter facts for the run manifest.
    pub fn describe(&self) -> GpuInfo {
        let i = self.adapter.get_info();
        let l = self.device.limits();
        GpuInfo {
            name: i.name,
            backend: format!("{:?}", i.backend).to_lowercase(),
            driver: format!("{} {}", i.driver, i.driver_info).trim().to_owned(),
            device_type: format!("{:?}", i.device_type).to_lowercase(),
            features: self
                .device
                .features()
                .iter_names()
                .map(|(n, _)| n.to_owned())
                .collect(),
            max_texture_dimension_2d: l.max_texture_dimension_2d,
            max_bind_groups: l.max_bind_groups,
        }
    }
}

/// Adapter facts for the run manifest (plain data; the client serializes it).
#[derive(Clone, Debug, PartialEq)]
pub struct GpuInfo {
    pub name: String,
    pub backend: String,
    pub driver: String,
    pub device_type: String,
    pub features: Vec<String>,
    pub max_texture_dimension_2d: u32,
    pub max_bind_groups: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffers_are_padded_to_the_copy_unit() {
        assert_eq!(padded_size(0), wgpu::COPY_BUFFER_ALIGNMENT);
        assert_eq!(padded_size(1), wgpu::COPY_BUFFER_ALIGNMENT);
        assert_eq!(padded_size(5), 8);
        assert_eq!(padded_size(8), 8);
    }
}
