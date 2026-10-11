// SPDX-License-Identifier: GPL-3.0-only
//! Adapter-gated: a buffer made on a device that is gone is an error with the reason, not a panic, and a device
//! that works hands out buffers with their contents.

use render::Gpu;

#[test]
fn buffer_on_a_lost_device_is_an_error_with_the_reason() {
    let Ok(gpu) = Gpu::new(None) else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let ok = gpu
        .buffer_init(Some("fine"), &[1, 2, 3], wgpu::BufferUsages::VERTEX)
        .expect("a small buffer");
    assert_eq!(ok.size(), 4);
    assert!(gpu.lost().is_none());

    gpu.device.destroy();
    let err = gpu
        .buffer_init(Some("world vertices"), &[0; 64], wgpu::BufferUsages::VERTEX)
        .expect_err("a buffer on a destroyed device");
    assert!(err.contains("world vertices"), "{err}");
    assert!(gpu.lost().is_some(), "the loss is recorded: {err}");
}
