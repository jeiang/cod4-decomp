// SPDX-License-Identifier: GPL-3.0-or-later
//! WebTransport endpoint for browser clients: the same datagrams a UDP client exchanges (connect
//! handshake packets and netchan packets), carried over a WebTransport session.
//!
//! [`WtTransport`] implements [`Transport`]. A client is named by the remote `SocketAddr` of its
//! QUIC connection (QUIC and UDP ports differ, so the names never collide); migration is off so
//! that name is stable. Sending to an address with no session drops the datagram silently. The
//! endpoint runs on its own thread with its own small tokio runtime; received messages land in a
//! shared [`Inbox`], so a server reading [`Joined`](crate::transport::Joined) waits on UDP and
//! WebTransport with one blocking call.
//!
//! # Framing (both directions; the browser client implements the same rule)
//!
//! One engine datagram is one message:
//!
//! * `len <=` [`datagram_limit`] (the smaller of [`BROWSER_DATAGRAM`] and the session's maximum
//!   datagram size; in a browser, `transport.datagrams.maxDatagramSize` capped at
//!   [`BROWSER_DATAGRAM`]): send it as one WebTransport datagram (`datagrams.writable`).
//! * longer, up to [`MAX_MESSAGE`] (8192) bytes: open one **unidirectional stream**
//!   (`createUnidirectionalStream`), write exactly the datagram's bytes and close the stream
//!   (FIN). No length prefix, no header: the stream's end delimits the message.
//! * longer than [`MAX_MESSAGE`] is never sent; a received stream longer than that is discarded.
//!
//! Receivers accept both forms whatever the size: every incoming datagram is one message, and
//! every incoming unidirectional stream, read to its end, is one message. Bidirectional streams
//! are ignored. Streams and datagrams race each other, so messages can arrive out of order, as
//! over UDP; the netchan reassembles fragments in any order. Open one stream per message in send
//! order and read incoming streams in the order `incomingUnidirectionalStreams` yields them, each
//! to its end, which keeps delivery close to send order.
//!
//! With no host the endpoint listens on every address, IPv4 and IPv6.
//!
//! Browsers cannot trust a self-signed certificate except by `serverCertificateHashes`: without
//! configured PEM files the endpoint makes an ECDSA P-256 certificate valid [`SELF_SIGNED_DAYS`]
//! days (the limit is 14) and reports its SHA-256; see [`Started`].

use crate::transport::{Inbox, MAX_MESSAGE, Transport};
use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wtransport::tls::{Certificate, Identity};
use wtransport::{Connection, Endpoint, ServerConfig};

/// Browsers send and receive WebTransport datagrams of at most about this many bytes.
pub const BROWSER_DATAGRAM: usize = 1200;

/// Validity of a generated certificate; browsers accept hashed certificates for at most 14 days.
pub const SELF_SIGNED_DAYS: u32 = 13;

/// Stream messages queued for one peer; more are dropped like a congested datagram, so a slow
/// peer cannot make the server pile up memory.
const MAX_QUEUED: usize = 128;

/// How long a peer may take to finish a stream message.
const STREAM_TIMEOUT: Duration = Duration::from_secs(3);

/// Where and how to listen.
pub struct WtConfig {
    pub bind: SocketAddr,
    /// PEM certificate chain and private key files; `None` makes a self-signed certificate.
    pub tls: Option<(PathBuf, PathBuf)>,
}

/// What the endpoint came up with.
pub struct Started {
    pub addr: SocketAddr,
    /// SHA-256 of the generated certificate's DER: the value for `serverCertificateHashes`.
    /// `None` when a certificate was loaded (browsers verify it normally).
    pub cert_hash: Option<[u8; 32]>,
}

/// A session and the queue of stream messages waiting to be sent on it.
#[derive(Clone)]
struct Session {
    conn: Connection,
    queue: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    queued: Arc<AtomicUsize>,
}

type Sessions = Arc<Mutex<HashMap<SocketAddr, Session>>>;

pub struct WtTransport {
    local: SocketAddr,
    sessions: Sessions,
    inbox: Inbox,
    /// Dropping this stops the endpoint's thread.
    _stop: tokio::sync::oneshot::Sender<()>,
}

/// The datagram size at or below which `conn` sends QUIC datagrams; larger messages use a stream.
pub fn datagram_limit(conn: &Connection) -> usize {
    conn.max_datagram_size()
        .map_or(0, |m| m.min(BROWSER_DATAGRAM))
}

impl WtTransport {
    /// Starts the endpoint delivering received messages into `inbox`.
    pub fn start(cfg: WtConfig, inbox: Inbox) -> io::Result<(Self, Started)> {
        // Bind before the thread starts so a bad address fails here.
        let socket = bind(cfg.bind)?;
        let sessions = Sessions::default();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (s2, q2) = (sessions.clone(), inbox.clone());
        std::thread::Builder::new()
            .name("webtransport".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => return drop(ready_tx.send(Err(e))),
                };
                let up = rt.block_on(open(socket, cfg.tls));
                let (endpoint, started) = match up {
                    Ok(v) => v,
                    Err(e) => return drop(ready_tx.send(Err(e))),
                };
                let _ = ready_tx.send(Ok(started));
                rt.block_on(async {
                    tokio::select! {
                        () = accept_loop(endpoint, s2, q2) => {}
                        _ = stopped => {}
                    }
                });
            })?;
        let started = ready_rx
            .recv()
            .map_err(|_| io::Error::other("webtransport thread died"))??;
        let t = Self {
            local: started.addr,
            sessions,
            inbox,
            _stop: stop,
        };
        Ok((t, started))
    }

    /// The datagram limit of `peer`'s session, if it has one (see [`datagram_limit`]).
    pub fn datagram_limit_of(&self, peer: SocketAddr) -> Option<usize> {
        self.sessions
            .lock()
            .unwrap()
            .get(&peer)
            .map(|s| datagram_limit(&s.conn))
    }
}

/// Binds `addr`; the unspecified IPv4 address means every address of both families (browsers
/// resolve `localhost` to `::1` first), falling back to IPv4 only where IPv6 is unavailable.
fn bind(addr: SocketAddr) -> io::Result<UdpSocket> {
    if addr.ip() == std::net::Ipv4Addr::UNSPECIFIED {
        let dual = || {
            let s = socket2::Socket::new(
                socket2::Domain::IPV6,
                socket2::Type::DGRAM,
                Some(socket2::Protocol::UDP),
            )?;
            s.set_only_v6(false)?;
            s.bind(&SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, addr.port())).into())?;
            Ok::<_, io::Error>(UdpSocket::from(s))
        };
        if let Ok(s) = dual() {
            return Ok(s);
        }
    }
    UdpSocket::bind(addr)
}

async fn open(
    socket: UdpSocket,
    tls: Option<(PathBuf, PathBuf)>,
) -> io::Result<(
    Endpoint<wtransport::endpoint::endpoint_side::Server>,
    Started,
)> {
    let addr = socket.local_addr()?;
    let (identity, cert_hash) = match tls {
        Some((cert, key)) => (
            Identity::load_pemfiles(cert, key)
                .await
                .map_err(io::Error::other)?,
            None,
        ),
        None => {
            let mut names = vec!["localhost".to_owned(), "127.0.0.1".into(), "::1".into()];
            if !addr.ip().is_unspecified() {
                names.push(addr.ip().to_string());
            }
            let id = Identity::self_signed_builder()
                .subject_alt_names(names)
                .from_now_utc()
                .validity_days(SELF_SIGNED_DAYS)
                .build()
                .map_err(io::Error::other)?;
            let hash = *Certificate::hash(&id.certificate_chain().as_slice()[0]).as_ref();
            (id, Some(hash))
        }
    };
    let config = ServerConfig::builder()
        .with_bind_socket(socket)
        .with_identity(identity)
        .allow_migration(false)
        .build();
    let endpoint = Endpoint::server(config)?;
    Ok((endpoint, Started { addr, cert_hash }))
}

/// Anyone may connect: WebTransport has no CORS and pages are served from anywhere. Bans, rate
/// limits and the password apply above the transport, on the connect packets.
async fn accept_loop(
    endpoint: Endpoint<wtransport::endpoint::endpoint_side::Server>,
    sessions: Sessions,
    inbox: Inbox,
) {
    loop {
        let incoming = endpoint.accept().await;
        tokio::spawn(session(incoming, sessions.clone(), inbox.clone()));
    }
}

async fn session(
    incoming: wtransport::endpoint::IncomingSession,
    sessions: Sessions,
    inbox: Inbox,
) {
    let Ok(request) = incoming.await else { return };
    let Ok(conn) = request.accept().await else {
        return;
    };
    let peer = conn.remote_address();
    let (queue, rx) = tokio::sync::mpsc::unbounded_channel();
    let queued = Arc::new(AtomicUsize::new(0));
    sessions.lock().unwrap().insert(
        peer,
        Session {
            conn: conn.clone(),
            queue,
            queued: queued.clone(),
        },
    );
    tokio::select! {
        () = datagrams(&conn, peer, &inbox) => {}
        () = streams(&conn, peer, &inbox) => {}
        () = send_streams(&conn, rx, &queued) => {}
    }
    let mut s = sessions.lock().unwrap();
    if s.get(&peer)
        .is_some_and(|c| c.conn.stable_id() == conn.stable_id())
    {
        s.remove(&peer);
    }
}

async fn datagrams(conn: &Connection, peer: SocketAddr, inbox: &Inbox) {
    while let Ok(d) = conn.receive_datagram().await {
        inbox.push(peer, &d.payload());
    }
}

/// Reads each incoming stream to its end before the next: streams are accepted in the order the
/// peer opened them, and a netchan message's fragments must be handled in order.
async fn streams(conn: &Connection, peer: SocketAddr, inbox: &Inbox) {
    while let Ok(mut rx) = conn.accept_uni().await {
        let read = async {
            let mut msg = Vec::new();
            let mut chunk = [0u8; 2048];
            while let Some(n) = rx.read(&mut chunk).await.ok()? {
                msg.extend_from_slice(&chunk[..n]);
                if msg.len() > MAX_MESSAGE {
                    return None;
                }
            }
            Some(msg)
        };
        if let Ok(Some(msg)) = tokio::time::timeout(STREAM_TIMEOUT, read).await {
            inbox.push(peer, &msg);
        }
    }
}

/// Sends queued messages one stream each, opening them in queue order (without waiting for the
/// peer to acknowledge one before opening the next).
async fn send_streams(
    conn: &Connection,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    queued: &AtomicUsize,
) {
    while let Some(data) = rx.recv().await {
        queued.fetch_sub(1, Ordering::SeqCst);
        let Ok(opening) = conn.open_uni().await else {
            return;
        };
        let Ok(mut tx) = opening.await else { return };
        if tx.write_all(&data).await.is_ok() {
            tokio::spawn(async move {
                let _ = tx.finish().await;
            });
        }
    }
}

impl Transport for WtTransport {
    fn local_addr(&self) -> SocketAddr {
        self.local
    }

    fn send_to(&mut self, to: SocketAddr, data: &[u8]) {
        let Some(Session {
            conn,
            queue,
            queued,
        }) = self.sessions.lock().unwrap().get(&to).cloned()
        else {
            return;
        };
        if data.len() <= datagram_limit(&conn) {
            let _ = conn.send_datagram(data);
        } else if data.len() <= MAX_MESSAGE && queued.load(Ordering::SeqCst) < MAX_QUEUED {
            queued.fetch_add(1, Ordering::SeqCst);
            let _ = queue.send(data.to_vec());
        }
    }

    fn recv_from(
        &mut self,
        buf: &mut [u8],
        timeout: Option<Duration>,
    ) -> io::Result<Option<(usize, SocketAddr)>> {
        Ok(self.inbox.pop(buf, timeout))
    }

    fn carries(&self, peer: SocketAddr) -> bool {
        self.sessions.lock().unwrap().contains_key(&peer)
    }
}

/// Standard base64 with padding, for the page-facing certificate hash.
pub fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
