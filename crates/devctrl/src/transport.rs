// SPDX-License-Identifier: GPL-3.0-only
//! Where request and reply lines travel. The channel's logic never knows: it asks its [`Transport`] for lines
//! and hands reply lines back.
//!
//! - [`TcpTransport`]: one listener on loopback (the native client), with an accept thread and a reader thread per
//!   connection. Each connection carries whole lines both ways, so any TCP client (`nc`, a script, an MCP wrapper)
//!   can drive it.
//! - [`PolledTcpTransport`]: the same wire without a thread (the headless server): the owner's poll accepts,
//!   reads and writes nonblocking, and drops a peer that stops taking its replies.
//! - [`StdioTransport`]: stdin and stdout, for a process someone is already talking to.
//! - [`QueueTransport`]: two queues, no socket: the browser page polls the requests and posts the replies.

#[cfg(not(target_arch = "wasm32"))]
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
pub use native::{PolledTcpTransport, StdioTransport, TcpTransport};

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use crate::MAX_PENDING;
    use crate::protocol::Reply;
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
    use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    /// Listens on `port` of loopback (0 picks a free one) and, when `addr_file` is given, writes the bound address
    /// there as `127.0.0.1:<port>` before returning, so a script can find the port it asked for.
    fn listen(port: u16, addr_file: Option<&Path>) -> Result<(TcpListener, SocketAddr), String> {
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
        Ok((listener, addr))
    }

    /// A listener on loopback TCP that never starts a thread, for the headless server, which runs one.
    ///
    /// The listener and every connection are nonblocking: [`Transport::poll`], called once a frame by the thread
    /// that owns the channel, accepts, reads whole lines and writes what the sockets take of the replies; nothing
    /// waits for a peer. A connection keeps the reply bytes its peer has not taken yet, up to [`MAX_PENDING`]; a peer
    /// that lets more pile up has stopped reading and is dropped, so it cannot hold up the frame that answers it or
    /// grow the server's memory without bound.
    pub struct PolledTcpTransport {
        listener: TcpListener,
        conns: BTreeMap<u64, Conn>,
        next: u64,
        addr: SocketAddr,
        pending_limit: usize,
    }

    /// One accepted connection and the bytes it has in flight either way.
    struct Conn {
        stream: TcpStream,
        /// The start of a request line whose newline has not arrived.
        partial: Vec<u8>,
        /// The line being read is longer than [`MAX_LINE`]: its bytes are dropped up to its newline.
        skipping: bool,
        /// Reply bytes; `out[sent..]` are the ones the socket has not taken yet.
        out: Vec<u8>,
        sent: usize,
    }

    impl Conn {
        fn pending(&self) -> usize {
            self.out.len() - self.sent
        }

        /// Appends one reply line, newline included, behind the bytes still waiting.
        fn queue(&mut self, line: &str) {
            // Compact once the sent prefix is at least half the buffer, so the copy is paid for by the sends.
            if self.sent > 0 && self.sent * 2 >= self.out.len() {
                self.out.drain(..self.sent);
                self.sent = 0;
            }
            self.out.extend_from_slice(line.as_bytes());
            self.out.push(b'\n');
        }

        /// Writes as much of the waiting reply bytes as the socket takes without blocking.
        fn flush(&mut self) -> std::io::Result<()> {
            while self.sent < self.out.len() {
                match self.stream.write(&self.out[self.sent..]) {
                    Ok(0) => return Err(ErrorKind::WriteZero.into()),
                    Ok(n) => self.sent += n,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
            if self.sent == self.out.len() {
                self.out.clear();
                self.sent = 0;
            }
            Ok(())
        }

        /// Reads what has arrived, at most [`MAX_LINE`] bytes a call, and appends its whole lines to `lines`.
        /// `Ok(false)` once the peer has closed.
        fn read(&mut self, conn: u64, lines: &mut Vec<(u64, String)>) -> std::io::Result<bool> {
            let mut buf = [0u8; 16 * 1024];
            let mut budget = MAX_LINE;
            while budget > 0 {
                let n = match self.stream.read(&mut buf) {
                    Ok(0) => return Ok(false),
                    Ok(n) => n,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(true),
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                };
                budget = budget.saturating_sub(n);
                self.split(conn, &buf[..n], lines);
            }
            Ok(true)
        }

        /// Cuts `bytes` into lines, carrying an unfinished one to the next read.
        fn split(&mut self, conn: u64, mut bytes: &[u8], lines: &mut Vec<(u64, String)>) {
            while let Some(end) = bytes.iter().position(|&b| b == b'\n') {
                if self.skipping {
                    self.skipping = false;
                } else if self.partial.len() + end > MAX_LINE {
                    // The line ends in this read but is still too long: refuse it as one that never ended here.
                    self.partial.clear();
                    self.refuse_overlong();
                } else {
                    self.partial.extend_from_slice(&bytes[..end]);
                    let text = String::from_utf8_lossy(&self.partial);
                    let text = text.trim_end_matches('\r');
                    if !text.trim().is_empty() {
                        lines.push((conn, text.to_owned()));
                    }
                    self.partial.clear();
                }
                bytes = &bytes[end + 1..];
            }
            if self.skipping {
                return;
            }
            self.partial.extend_from_slice(bytes);
            if self.partial.len() > MAX_LINE {
                self.partial = Vec::new();
                self.skipping = true;
                self.refuse_overlong();
            }
        }

        fn refuse_overlong(&mut self) {
            let why = format!("the request is longer than {MAX_LINE} bytes");
            self.queue(&Reply::failed(Value::Null, why).line());
        }
    }

    impl PolledTcpTransport {
        /// Listens on `port` of loopback as [`TcpTransport::bind`] does, address file included, without a thread.
        pub fn bind(port: u16, addr_file: Option<&Path>) -> Result<Self, String> {
            let (listener, addr) = listen(port, addr_file)?;
            listener
                .set_nonblocking(true)
                .map_err(|e| format!("cannot make the control listener nonblocking: {e}"))?;
            Ok(Self {
                listener,
                conns: BTreeMap::new(),
                next: 0,
                addr,
                pending_limit: MAX_PENDING,
            })
        }

        /// Sets the most reply bytes a connection may leave untaken before it is dropped (default [`MAX_PENDING`]).
        pub fn with_pending_limit(mut self, bytes: usize) -> Self {
            self.pending_limit = bytes;
            self
        }

        /// The address the listener took.
        pub fn addr(&self) -> SocketAddr {
            self.addr
        }

        /// The connections open now.
        pub fn connections(&self) -> usize {
            self.conns.len()
        }

        /// Takes every connection waiting on the listener.
        fn accept(&mut self) {
            loop {
                match self.listener.accept() {
                    Ok((stream, peer)) => {
                        if let Err(e) = stream.set_nonblocking(true) {
                            tracing::warn!(error = %e, "control: cannot make a connection nonblocking");
                            continue;
                        }
                        self.next += 1;
                        tracing::debug!(conn = self.next, %peer, "control: a connection opened");
                        self.conns.insert(
                            self.next,
                            Conn {
                                stream,
                                partial: Vec::new(),
                                skipping: false,
                                out: Vec::new(),
                                sent: 0,
                            },
                        );
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => return,
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(e) => {
                        tracing::debug!(error = %e, "control: accept failed");
                        return;
                    }
                }
            }
        }
    }

    /// Writes what `c` can and says whether it stays open: not when its socket failed, nor when its peer left more
    /// than `limit` reply bytes untaken.
    fn keep(conn: u64, c: &mut Conn, limit: usize) -> bool {
        if let Err(e) = c.flush() {
            tracing::debug!(conn, error = %e, "control: cannot answer a connection");
            return false;
        }
        if c.pending() > limit {
            tracing::warn!(
                conn,
                pending = c.pending(),
                limit,
                "control: dropping a connection that does not read its replies"
            );
            return false;
        }
        true
    }

    impl Transport for PolledTcpTransport {
        fn poll(&mut self) -> Vec<(u64, String)> {
            self.accept();
            let mut lines = Vec::new();
            let limit = self.pending_limit;
            self.conns.retain(|&conn, c| {
                let open = match c.read(conn, &mut lines) {
                    Ok(open) => open,
                    Err(e) => {
                        tracing::debug!(conn, error = %e, "control: a connection ended");
                        false
                    }
                };
                open && keep(conn, c, limit)
            });
            lines
        }

        fn reply(&mut self, conn: u64, line: &str) {
            let Some(c) = self.conns.get_mut(&conn) else {
                return;
            };
            c.queue(line);
            if !keep(conn, c, self.pending_limit) {
                self.conns.remove(&conn);
            }
        }
    }

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
            let (listener, addr) = listen(port, addr_file)?;
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

    /// Polls `t` until a request line arrives, for at most five seconds.
    #[cfg(not(target_arch = "wasm32"))]
    fn next_line(t: &mut PolledTcpTransport) -> (u64, String) {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(x) = t.poll().into_iter().next() {
                return x;
            }
            assert!(std::time::Instant::now() < end, "no request arrived");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn polled_tcp_joins_split_lines_and_answers_the_connection_that_asked() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpStream;
        let mut t = PolledTcpTransport::bind(0, None).unwrap();
        let mut c = TcpStream::connect(t.addr()).unwrap();
        c.write_all(b"{\"cmd\":").unwrap();
        c.flush().unwrap();
        while t.connections() == 0 {
            assert!(t.poll().is_empty());
        }
        c.write_all(b"\"ping\"}\r\n").unwrap();
        let (conn, line) = next_line(&mut t);
        assert_eq!(line, r#"{"cmd":"ping"}"#);
        t.reply(conn, r#"{"ok":true}"#);
        let mut back = String::new();
        BufReader::new(c).read_line(&mut back).unwrap();
        assert_eq!(back, "{\"ok\":true}\n");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn polled_tcp_drops_a_connection_that_stops_reading_without_blocking() {
        use std::io::Write;
        use std::net::TcpStream;
        use std::time::{Duration, Instant};
        let limit = 256 * 1024;
        let mut t = PolledTcpTransport::bind(0, None)
            .unwrap()
            .with_pending_limit(limit);
        // The peer asks once and never reads.
        let mut c = TcpStream::connect(t.addr()).unwrap();
        c.write_all(b"{\"cmd\":\"state\"}\n").unwrap();
        let (conn, _) = next_line(&mut t);
        let big = format!("{{\"ok\":true,\"pad\":\"{}\"}}", "x".repeat(64 * 1024));
        let started = Instant::now();
        let mut sent = 0usize;
        while t.connections() == 1 {
            // Each round is one frame's reply and the next frame's poll; neither may wait for the peer.
            let round = Instant::now();
            t.reply(conn, &big);
            t.poll();
            assert!(
                round.elapsed() < Duration::from_secs(1),
                "a frame waited for the peer"
            );
            sent += big.len() + 1;
            // Kernel buffers take a few MB at most; a transport that never drops runs past this.
            assert!(sent < 256 * 1024 * 1024, "the connection was never dropped");
        }
        assert!(sent > limit);
        assert!(started.elapsed() < Duration::from_secs(10));
        // Replies to the dropped connection go nowhere and cost nothing.
        t.reply(conn, &big);
        assert_eq!(t.connections(), 0);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn polled_tcp_refuses_an_overlong_line_and_reads_the_next() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpStream;
        let mut t = PolledTcpTransport::bind(0, None).unwrap();
        let c = TcpStream::connect(t.addr()).unwrap();
        let mut w = c.try_clone().unwrap();
        let writer = std::thread::spawn(move || {
            w.write_all("x".repeat(crate::MAX_LINE + 1).as_bytes())
                .unwrap();
            w.write_all(b"\n{\"cmd\":\"ping\"}\n").unwrap();
        });
        let (_, line) = next_line(&mut t);
        assert_eq!(line, r#"{"cmd":"ping"}"#);
        writer.join().unwrap();
        let mut back = String::new();
        BufReader::new(c).read_line(&mut back).unwrap();
        assert!(back.contains(r#""ok":false"#), "{back}");
        assert!(back.contains("longer than"), "{back}");
    }
}
