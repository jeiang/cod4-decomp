// SPDX-License-Identifier: GPL-3.0-or-later
//! Instance, adapter and device creation, and the facts about them the harness reports.

use std::fmt;

/// The wgpu device and what it supports. Cheap to share by reference; wgpu handles are reference counted.
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// BC1-3 textures are uploaded as they are; otherwise they are decoded to RGBA8 on the CPU.
    pub bc: bool,
    /// GPU pass timing via timestamp queries.
    pub timestamps: bool,
}

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
    pub fn new(surface: Option<&wgpu::Surface<'_>>) -> Result<Gpu, GpuError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        Self::with_instance(instance, surface)
    }

    pub fn with_instance(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Gpu, GpuError> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: surface,
            ..Default::default()
        }))
        .map_err(|e| GpuError::NoAdapter(e.to_string()))?;
        let have = adapter.features();
        let want = (wgpu::Features::TEXTURE_COMPRESSION_BC | wgpu::Features::TIMESTAMP_QUERY) & have;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("cod4e"),
            required_features: want,
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(|e| GpuError::Device(e.to_string()))?;
        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
            bc: want.contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
            timestamps: want.contains(wgpu::Features::TIMESTAMP_QUERY),
        })
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
