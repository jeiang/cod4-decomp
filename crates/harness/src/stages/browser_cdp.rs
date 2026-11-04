// SPDX-License-Identifier: GPL-3.0-only
//! A trusted click in the headless page, through Chromium's DevTools pipe. It grants the transient activation a real
//! player's click grants; mouse deltas themselves still go through the dev control channel.

use serde_json::Value;
#[cfg(unix)]
use serde_json::json;
use std::io;
use std::process::Command;

/// The flags every stage's headless Chromium starts with: WebGPU on SwiftShader's Vulkan (`--use-vulkan=swiftshader`,
/// `--use-webgpu-adapter=swiftshader`, Chromium's switches for a GPU-less Linux host: `gpu/config/gpu_switches.cc`,
/// `docs/gpu/swiftshader.md`) with ANGLE on SwiftShader too. Probed on the slot host (Chromium 153), every other
/// combination loses WebGPU:
/// - without `--enable-features=Vulkan` the GPU process has no shared-image backing that both Dawn and the compositor
///   can use (`gpu/command_buffer/service/shared_image/shared_image_factory.cc`: "Could not find
///   SharedImageBackingFactory ... usage: ...WebgpuSwapChainTexture") and the page's device is destroyed at its first
///   `getCurrentTexture`;
/// - `--disable-vulkan-surface` leaves Dawn no Vulkan instance ("A valid external Instance reference no longer
///   exists"), so `requestAdapter` resolves to null;
/// - `--use-angle=vulkan` fails `eglInitialize` (`Internal Vulkan error (-3)`) and the GPU process exits.
///
/// The canvas is read back in the task that drew it (`web/main.js` `captureCanvas`, called right after the frame's
/// submit): once the frame is handed to the compositor the WebGPU canvas's shared image reads as uninitialized here
/// (`shared_image_representation.cc`: "Attempt to read from an uninitialized SharedImage"), as the WebGPU spec's
/// "get a copy of the image contents of a context" allows only for the current texture.
pub const CHROMIUM_FLAGS: &[&str] = &[
    "--headless=new",
    "--no-first-run",
    "--enable-unsafe-webgpu",
    "--enable-features=Vulkan",
    "--use-angle=swiftshader",
    "--use-vulkan=swiftshader",
    "--use-webgpu-adapter=swiftshader",
    "--ignore-gpu-blocklist",
    "--enable-unsafe-swiftshader",
    "--window-size=1280,720",
];

/// The first line of a Chromium log that shows the page's WebGPU device was lost, its canvas could not be backed, or a
/// frame's command buffer was rejected by validation: the page may keep presenting a stale or black frame afterwards,
/// so a run that logged one is not WebGPU evidence.
pub fn gpu_failure(log: &std::path::Path) -> Option<String> {
    const MARKS: &[&str] = &[
        "device lost",
        "Unable to create shared image",
        "Could not find SharedImageBackingFactory",
        "Invalid CommandBuffer",
    ];
    let text = std::fs::read_to_string(log).ok()?;
    text.lines()
        .find(|l| MARKS.iter().any(|m| l.contains(m)))
        .map(|l| l.trim().to_owned())
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;
    use std::time::Duration;

    /// The host ends of Chromium's `--remote-debugging-pipe`.
    pub struct BrowserPipe {
        reader: UnixStream,
        writer: UnixStream,
        next: u64,
        session: Option<String>,
        // Kept until the pipe is dropped so the descriptors remain valid when `Command::spawn` forks.
        _child_read: OwnedFd,
        _child_write: OwnedFd,
    }

    pub fn pipe(command: &mut Command) -> io::Result<BrowserPipe> {
        let (reader, child_write) = UnixStream::pair()?;
        let (child_read, writer) = UnixStream::pair()?;
        reader.set_read_timeout(Some(Duration::from_secs(30)))?;
        // The child ends live on as plain descriptors until the browser has been spawned (the parent's copies close
        // when the pipe is dropped; the child keeps its duplicates).
        let child_read: OwnedFd = child_read.into();
        let child_write: OwnedFd = child_write.into();
        let read_fd = child_read.as_raw_fd();
        let write_fd = child_write.as_raw_fd();
        // Chromium reads commands on fd 3 and writes replies on fd 4. `dup2` clears close-on-exec on the duplicates.
        // The write end is moved out of the way first in case it already sits on fd 3.
        unsafe {
            command.pre_exec(move || {
                let write_fd = if write_fd == 3 {
                    let moved = libc_dup(write_fd);
                    if moved == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    moved
                } else {
                    write_fd
                };
                for (from, to) in [(read_fd, 3), (write_fd, 4)] {
                    if libc_dup2(from, to) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        command.arg("--remote-debugging-pipe");
        Ok(BrowserPipe {
            reader,
            writer,
            next: 0,
            session: None,
            _child_read: child_read,
            _child_write: child_write,
        })
    }

    unsafe extern "C" {
        #[link_name = "dup2"]
        fn libc_dup2(oldfd: i32, newfd: i32) -> i32;
        #[link_name = "dup"]
        fn libc_dup(oldfd: i32) -> i32;
    }

    impl BrowserPipe {
        fn call(&mut self, method: &str, params: Value, session: bool) -> Result<Value, String> {
            self.next += 1;
            let id = self.next;
            let mut req = json!({"id": id, "method": method, "params": params});
            if session {
                req["sessionId"] = json!(self.session.as_ref().ok_or("no page session")?);
            }
            let mut line = req.to_string().into_bytes();
            line.push(0);
            self.writer.write_all(&line).map_err(|e| e.to_string())?;
            let mut buf = Vec::new();
            loop {
                let mut byte = [0];
                self.reader
                    .read_exact(&mut byte)
                    .map_err(|e| e.to_string())?;
                if byte[0] == 0 {
                    let reply: Value = serde_json::from_slice(&buf).map_err(|e| e.to_string())?;
                    if reply["id"] == id {
                        if reply["error"].is_object() {
                            return Err(format!("{method}: {}", reply["error"]));
                        }
                        return Ok(reply["result"].clone());
                    }
                    buf.clear();
                } else {
                    buf.push(byte[0]);
                }
            }
        }

        fn attach(&mut self, port: u16) -> Result<(), String> {
            if self.session.is_none() {
                let targets = self.call("Target.getTargets", json!({}), false)?;
                let target = targets["targetInfos"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|t| {
                        t["type"] == "page"
                            && t["url"]
                                .as_str()
                                .is_some_and(|u| u.contains(&format!(":{port}/")))
                    })
                    .and_then(|t| t["targetId"].as_str())
                    .ok_or("Chromium has no matching page target")?;
                let attached = self.call(
                    "Target.attachToTarget",
                    json!({"targetId": target, "flatten": true}),
                    false,
                )?;
                self.session = Some(
                    attached["sessionId"]
                        .as_str()
                        .ok_or("the page attachment has no session id")?
                        .to_owned(),
                );
            }
            Ok(())
        }

        /// Probe the actual adapter, not merely API presence. Properties are explicit because GPUAdapterInfo's
        /// WebIDL getters do not appear when JSON.stringify serializes the object itself.
        pub fn adapter(&mut self, port: u16) -> Result<Value, String> {
            self.attach(port)?;
            let result = self.call(
                "Runtime.evaluate",
                json!({
                    "expression": "(async () => { const gpu = !!navigator.gpu; try { const a = await navigator.gpu?.requestAdapter(); const i = a?.info; return {gpu, adapter: !!a, info: i ? {vendor: i.vendor, architecture: i.architecture, device: i.device, description: i.description, isFallbackAdapter: i.isFallbackAdapter} : null}; } catch (e) { return {gpu, adapter: false, error: String(e)}; } })()",
                    "awaitPromise": true,
                    "returnByValue": true
                }),
                true,
            )?;
            if result["exceptionDetails"].is_object() {
                return Err(format!(
                    "adapter probe threw: {}",
                    result["exceptionDetails"]
                ));
            }
            result["result"]["value"]
                .as_object()
                .ok_or("adapter probe returned no object")?;
            Ok(result["result"]["value"].clone())
        }

        /// Independent page/runtime evidence; never sends a dev-control request or mutates gameplay.
        pub fn page_readiness(&mut self, port: u16) -> Result<Value, String> {
            self.attach(port)?;
            let result = self.call(
                "Runtime.evaluate",
                json!({
                    "expression": "({url: location.href, document: document.readyState, frames: window.__cod4?.overlay?.frames ?? 0, error: window.__cod4?.error ?? null})",
                    "returnByValue": true
                }),
                true,
            )?;
            if result["exceptionDetails"].is_object() {
                return Err(format!(
                    "page readiness threw: {}",
                    result["exceptionDetails"]
                ));
            }
            result["result"]["value"]
                .as_object()
                .ok_or("page readiness returned no object")?;
            Ok(result["result"]["value"].clone())
        }

        pub fn click(&mut self) -> Result<(), String> {
            self.attach(18092)?;
            for (kind, count) in [("mousePressed", 1), ("mouseReleased", 1)] {
                self.call(
                    "Input.dispatchMouseEvent",
                    json!({"type": kind, "x": 640, "y": 316, "button": "left", "clickCount": count}),
                    true,
                )?;
            }
            Ok(())
        }
    }
}

#[cfg(unix)]
pub use imp::*;

#[cfg(not(unix))]
pub struct BrowserPipe;

#[cfg(not(unix))]
pub fn pipe(_: &mut Command) -> io::Result<BrowserPipe> {
    Ok(BrowserPipe)
}

#[cfg(not(unix))]
impl BrowserPipe {
    pub fn page_readiness(&mut self, _: u16) -> Result<Value, String> {
        Err("page readiness needs Chromium's DevTools pipe on Unix".into())
    }
    pub fn click(&mut self) -> Result<(), String> {
        Err("a trusted browser click needs Chromium's DevTools pipe on Unix".into())
    }
    pub fn adapter(&mut self, _: u16) -> Result<Value, String> {
        Err("a browser adapter probe needs Chromium's DevTools pipe on Unix".into())
    }
}
