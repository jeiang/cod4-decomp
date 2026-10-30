// SPDX-License-Identifier: GPL-3.0-only
//! A trusted click in the headless page, through Chromium's DevTools pipe. It grants the transient activation a real
//! player's click grants; mouse deltas themselves still go through the dev control channel.

use serde_json::{Value, json};
use std::io;
use std::process::Command;

#[cfg(unix)]
mod imp {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
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
        let read_fd = child_read.as_raw_fd();
        let write_fd = child_write.as_raw_fd();
        // Chromium reads commands on fd 3 and writes replies on fd 4. `dup2` clears close-on-exec on the duplicates.
        unsafe {
            command.pre_exec(move || {
                for (from, to) in [(read_fd, 3), (write_fd, 4)] {
                    if libc_dup2(from, to) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        // The command owns the duplicates after spawning; these close the parent's original child ends.
        let child_read = unsafe { OwnedFd::from_raw_fd(read_fd) };
        let child_write = unsafe { OwnedFd::from_raw_fd(write_fd) };
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

        pub fn click(&mut self) -> Result<(), String> {
            if self.session.is_none() {
                let targets = self.call("Target.getTargets", json!({}), false)?;
                let target = targets["targetInfos"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|t| {
                        t["type"] == "page"
                            && t["url"].as_str().is_some_and(|u| u.contains(":18092/"))
                    })
                    .and_then(|t| t["targetId"].as_str())
                    .ok_or("Chromium has no web-match page target")?;
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
    pub fn click(&mut self) -> Result<(), String> {
        Err("a trusted browser click needs Chromium's DevTools pipe on Unix".into())
    }
}
