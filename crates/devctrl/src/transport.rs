// SPDX-License-Identifier: GPL-3.0-only
//! Where request and reply lines travel. The channel's logic never knows: it asks its [`Transport`] for lines
//! and hands reply lines back.
//!
//! - [`TcpTransport`]: one listener on loopback (the native client and server). Each connection carries whole
//!   lines both ways, so any TCP client (`nc`, a script, an MCP wrapper) can drive it.
//! - [`StdioTransport`]: stdin and stdout, for a process someone is already talking to.
//! - [`QueueTransport`]: two queues, no socket: the browser page polls the requests and posts the replies.

use crate::MAX_LINE;
use std::collections::{HashMap, VecDeque};

/// One route for the protocol's lines.
pub trait Transport {
    /// The request lines that arrived since the last call, each with the number of the connection it came from.
    /// An empty vector when nothing has arrived; never blocks.
    fn poll(&mut self) -> Vec<(u64, String)>;

    /// Writes one reply line back to the connection that asked, newline included.
    fn reply(&mut self, conn: u64, line: &str);
}

/// A transport whose requests are pushed by hand: the browser's, where the page polls a `/control/next` endpoint
/// and posts the reply to `/control/reply`.
#[derive(Default)]
pub struct QueueTransport {
    inbox: VecDeque<(u64, String)>,
    out: HashMap<u64, String>,
    seq: u64,
}

impl QueueTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues one request line as if a connection had sent it; returns its connection number.
    pub fn push(&mut self, line: impl Into<String>) -> u64 {
        self.seq += 1;
        self.inbox.push_back((self.seq, line.into()));
        self.seq
    }

    /// Takes the reply to connection `conn`, once one was written.
    pub fn take(&mut self, conn: u64) -> Option<String> {
        self.out.remove(&conn)
    }

    /// Whether a reply for `conn` is waiting.
    pub fn ready(&self, conn: u64) -> bool {
        self.out.contains_key(&conn)
    }
}

impl Transport for QueueTransport {
    fn poll(&mut self) -> Vec<(u64, String)> {
        self.inbox.drain(..).collect()
    }

    fn reply(&mut self, conn: u64, line: &str) {
        self.out.insert(conn, line.to_owned());
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::{StdioTransport, TcpTransport};

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    /// A listener on loopback TCP. The accept loop and one reader thread per connection run on their own threads and
    /// only ever push lines into a channel; replies are written from the thread that owns the channel, so a request
    /// that waits costs the game nothing.
    pub struct TcpTransport {
        inbox: mpsc::Receiver<(u64, String)>,
        writers: Arc<Mutex<HashMap<u64, TcpStream>>>,
        addr: SocketAddr,
    }

    impl TcpTransport {
        /// Listens on `port` of loopback (0 picks a free one). When `addr_file` is given, the bound address is
        /// written there as `127.0.0.1:<port>` before this returns, so a script can find the port it asked for.
        pub fn bind(port: u16, addr_file: Option<&Path>) -> Result<Self, String> {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
                .map_err(|e| format!("cannot listen on 127.0.0.1:{port}: {e}"))?;
            let addr = listener
                .local_addr()
                .map_err(|e| format!("cannot read the control address: {e}"))?;
            if let Some(path) = addr_file {
                std::fs::write(path, format!("{addr}\n")).map_err(|e| {
                    format!(
                        "cannot write the control address to {}: {e}",
                        path.display()
                    )
                })?;
            }
            tracing::info!(%addr, "control channel listening");
            let (tx, inbox) = mpsc::channel();
            let writers: Arc<Mutex<HashMap<u64, TcpStream>>> = Arc::default();
            let next = Arc::new(AtomicU64::new(0));
            {
                let writers = writers.clone();
                std::thread::Builder::new()
                    .name("control-accept".into())
                    .spawn(move || {
                        for stream in listener.incoming() {
                            let Ok(stream) = stream else { continue };
                            let conn = next.fetch_add(1, Ordering::Relaxed) + 1;
                            if let Err(e) = stream.set_write_timeout(Some(Duration::from_secs(5))) {
                                tracing::warn!(error = %e, "control: cannot time the writes");
                            }
                            match stream.try_clone() {
                                Ok(w) => {
                                    if let Ok(mut map) = writers.lock() {
                                        map.insert(conn, w);
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "control: cannot keep the connection");
                                    continue;
                                }
                            }
                            let tx = tx.clone();
                            let writers = writers.clone();
                            std::thread::Builder::new()
                                .name(format!("control-conn-{conn}"))
                                .spawn(move || {
                                    read_lines(stream, conn, &tx);
                                    if let Ok(mut map) = writers.lock() {
                                        map.remove(&conn);
                                    }
                                })
                                .ok();
                        }
                    })
                    .map_err(|e| format!("cannot start the control listener: {e}"))?;
            }
            Ok(Self {
                inbox,
                writers,
                addr,
            })
        }

        /// The address the listener took.
        pub fn addr(&self) -> SocketAddr {
            self.addr
        }
    }

    /// Reads whole lines off one connection until it closes, ignoring any beyond [`MAX_LINE`].
    fn read_lines(stream: TcpStream, conn: u64, tx: &mpsc::Sender<(u64, String)>) {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            match (&mut reader).take(MAX_LINE as u64).read_line(&mut line) {
                Ok(0) => return,
                Ok(_) => {
                    let text = line.trim_end_matches(['\r', '\n']);
                    if !text.trim().is_empty() && tx.send((conn, text.to_owned())).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    tracing::debug!(conn, error = %e, "control: a connection ended");
                    return;
                }
            }
        }
    }

    impl Transport for TcpTransport {
        fn poll(&mut self) -> Vec<(u64, String)> {
            self.inbox.try_iter().collect()
        }

        fn reply(&mut self, conn: u64, line: &str) {
            let Ok(mut map) = self.writers.lock() else {
                return;
            };
            let Some(stream) = map.get_mut(&conn) else {
                return;
            };
            if let Err(e) = stream
                .write_all(line.as_bytes())
                .and_then(|()| stream.write_all(b"\n"))
                .and_then(|()| stream.flush())
            {
                tracing::debug!(conn, error = %e, "control: cannot answer a connection");
                map.remove(&conn);
            }
        }
    }

    /// stdin and stdout: one request per line on the way in, one reply per line on the way out.
    pub struct StdioTransport {
        inbox: mpsc::Receiver<(u64, String)>,
    }

    impl StdioTransport {
        pub fn new() -> Self {
            let (tx, inbox) = mpsc::channel();
            std::thread::Builder::new()
                .name("control-stdin".into())
                .spawn(move || {
                    let stdin = std::io::stdin();
                    let mut line = String::new();
                    loop {
                        line.clear();
                        match (&mut stdin.lock())
                            .take(MAX_LINE as u64)
                            .read_line(&mut line)
                        {
                            Ok(0) => return,
                            Ok(_) => {
                                let text = line.trim_end_matches(['\r', '\n']);
                                if !text.trim().is_empty() && tx.send((1, text.to_owned())).is_err()
                                {
                                    return;
                                }
                            }
                            Err(_) => return,
                        }
                    }
                })
                .ok();
            Self { inbox }
        }
    }

    impl Default for StdioTransport {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Transport for StdioTransport {
        fn poll(&mut self) -> Vec<(u64, String)> {
            self.inbox.try_iter().collect()
        }

        fn reply(&mut self, _: u64, line: &str) {
            let out = std::io::stdout();
            let mut out = out.lock();
            if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
                tracing::debug!("control: stdout is gone");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_carries_requests_in_and_replies_out() {
        let mut t = QueueTransport::new();
        assert!(t.poll().is_empty());
        let a = t.push(r#"{"cmd":"ping"}"#);
        let b = t.push(r#"{"cmd":"state"}"#);
        assert_ne!(a, b);
        let got = t.poll();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, a);
        assert!(got[0].1.contains("ping"));
        assert!(!t.ready(a));
        t.reply(a, r#"{"ok":true}"#);
        assert!(t.ready(a));
        assert_eq!(t.take(a).as_deref(), Some(r#"{"ok":true}"#));
        assert!(t.take(a).is_none());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn tcp_answers_on_the_connection_that_asked() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpStream;
        use std::time::Duration;
        let mut t = TcpTransport::bind(0, None).unwrap();
        let mut c = TcpStream::connect(t.addr()).unwrap();
        c.write_all(b"{\"cmd\":\"ping\"}\n").unwrap();
        c.flush().unwrap();
        let (conn, line) = loop {
            if let Some(x) = t.poll().into_iter().next() {
                break x;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(line, r#"{"cmd":"ping"}"#);
        t.reply(conn, r#"{"ok":true}"#);
        let mut back = String::new();
        BufReader::new(c).read_line(&mut back).unwrap();
        assert_eq!(back, "{\"ok\":true}\n");
    }
}
