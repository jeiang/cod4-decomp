// SPDX-License-Identifier: GPL-3.0-only
//! The datagram transport everything above it uses: UDP for the network, an in-memory switchboard
//! for the listen server and tests, and [`Joined`], which lets a server take its datagrams from UDP
//! and from further transports (the [`wt`](crate::wt) WebTransport endpoint) at once.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Largest datagram any peer sends or accepts.
pub const MAX_DATAGRAM: usize = 1400;

/// Largest message a [`Joined`] or WebTransport session delivers; a UDP datagram or a WebTransport
/// stream message beyond it is cut or refused.
pub const MAX_MESSAGE: usize = 8192;

/// Datagrams a [`Joined`] holds for a server that is not reading.
const INBOX_CAP: usize = 4096;

/// Unreliable, unordered datagrams to and from peers named by socket address.
/// Reliability, ordering and fragmentation are the [`Netchan`](crate::Netchan)'s job.
pub trait Transport {
    fn local_addr(&self) -> SocketAddr;

    /// Sends one datagram. A full or failing path drops it silently: loss is normal.
    fn send_to(&mut self, to: SocketAddr, data: &[u8]);

    /// Waits up to `timeout` (`None` = do not wait) for a datagram and returns its length.
    fn recv_from(
        &mut self,
        buf: &mut [u8],
        timeout: Option<Duration>,
    ) -> io::Result<Option<(usize, SocketAddr)>>;

    /// Whether `peer` is a session this transport owns (so a [`Joined`] sends to it here, not
    /// over UDP). Only transports with sessions of their own say yes.
    fn carries(&self, _peer: SocketAddr) -> bool {
        false
    }

    /// Peers whose session ended (a tab closed, a connection lost) since the last call, so the server frees their
    /// slots at once. Only transports with sessions of their own report any.
    fn take_closed(&mut self) -> Vec<SocketAddr> {
        Vec::new()
    }
}

impl<T: Transport + ?Sized> Transport for Box<T> {
    fn local_addr(&self) -> SocketAddr {
        (**self).local_addr()
    }

    fn send_to(&mut self, to: SocketAddr, data: &[u8]) {
        (**self).send_to(to, data);
    }

    fn recv_from(
        &mut self,
        buf: &mut [u8],
        timeout: Option<Duration>,
    ) -> io::Result<Option<(usize, SocketAddr)>> {
        (**self).recv_from(buf, timeout)
    }

    fn carries(&self, peer: SocketAddr) -> bool {
        (**self).carries(peer)
    }

    fn take_closed(&mut self) -> Vec<SocketAddr> {
        (**self).take_closed()
    }
}

pub struct UdpTransport {
    socket: UdpSocket,
    local: SocketAddr,
    /// The read timeout currently set on the socket, to avoid a syscall per call.
    timeout: Option<Duration>,
    nonblocking: bool,
}

impl UdpTransport {
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(addr)?;
        let local = socket.local_addr()?;
        Ok(Self {
            socket,
            local,
            timeout: None,
            nonblocking: false,
        })
    }

    /// Lets the socket send to the broadcast address (LAN server discovery).
    pub fn set_broadcast(&self, on: bool) -> io::Result<()> {
        self.socket.set_broadcast(on)
    }
}

impl Transport for UdpTransport {
    fn local_addr(&self) -> SocketAddr {
        self.local
    }

    fn send_to(&mut self, to: SocketAddr, data: &[u8]) {
        let _ = self.socket.send_to(data, to);
    }

    fn recv_from(
        &mut self,
        buf: &mut [u8],
        timeout: Option<Duration>,
    ) -> io::Result<Option<(usize, SocketAddr)>> {
        let timeout = timeout.filter(|t| !t.is_zero());
        loop {
            let r = match timeout {
                None => {
                    if !self.nonblocking {
                        self.socket.set_nonblocking(true)?;
                        self.nonblocking = true;
                    }
                    self.socket.recv_from(buf)
                }
                Some(t) => {
                    if self.nonblocking {
                        self.socket.set_nonblocking(false)?;
                        self.nonblocking = false;
                    }
                    if self.timeout != Some(t) {
                        self.socket.set_read_timeout(Some(t))?;
                        self.timeout = Some(t);
                    }
                    self.socket.recv_from(buf)
                }
            };
            return match r {
                Ok((n, from)) => Ok(Some((n, from))),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    Ok(None)
                }
                // A previous send to a closed port can surface here on some systems.
                Err(e) if e.kind() == io::ErrorKind::ConnectionReset => continue,
                Err(e) => Err(e),
            };
        }
    }
}

type Datagrams = VecDeque<(SocketAddr, Vec<u8>)>;

/// Datagrams received from other threads, waiting for the one that reads them. Cloning shares
/// the queue. Pushing to a full inbox drops the datagram: a stalled reader must not grow memory.
#[derive(Clone)]
pub struct Inbox {
    q: Arc<(Mutex<Datagrams>, Condvar)>,
    cap: usize,
}

impl Inbox {
    pub fn new(cap: usize) -> Self {
        Self {
            q: Arc::default(),
            cap,
        }
    }

    pub fn push(&self, from: SocketAddr, data: &[u8]) {
        let mut q = self.q.0.lock().unwrap();
        if q.len() < self.cap {
            q.push_back((from, data.to_vec()));
            self.q.1.notify_one();
        }
    }

    /// Takes the oldest datagram, waiting up to `timeout` (`None` or zero: do not wait). One
    /// longer than `buf` is cut to fit, like a UDP read.
    pub fn pop(&self, buf: &mut [u8], timeout: Option<Duration>) -> Option<(usize, SocketAddr)> {
        let (lock, cv) = &*self.q;
        let mut q = lock.lock().unwrap();
        if q.is_empty()
            && let Some(t) = timeout.filter(|t| !t.is_zero())
        {
            q = cv.wait_timeout_while(q, t, |q| q.is_empty()).unwrap().0;
        }
        q.pop_front().map(|(from, d)| {
            let n = d.len().min(buf.len());
            buf[..n].copy_from_slice(&d[..n]);
            (n, from)
        })
    }
}

/// UDP on one socket plus any number of [`Transport`]s that bring their own sessions (WebTransport),
/// read through one blocking `recv_from`: a thread per source feeds one [`Inbox`]. Sends go to the
/// transport that [`carries`](Transport::carries) the peer, else out the UDP socket.
pub struct Joined {
    udp: UdpTransport,
    inbox: Inbox,
    others: Vec<Box<dyn Transport + Send>>,
    stop: Arc<AtomicBool>,
}

impl Joined {
    /// The receive queue for transports to deliver into (see [`Self::add`]).
    pub fn inbox(&self) -> Inbox {
        self.inbox.clone()
    }

    pub fn new(udp: UdpTransport) -> io::Result<Self> {
        let socket = udp.socket.try_clone()?;
        // The read timeout is how the thread notices the drop.
        socket.set_read_timeout(Some(Duration::from_millis(200)))?;
        let inbox = Inbox::new(INBOX_CAP);
        let stop = Arc::new(AtomicBool::new(false));
        let (q, flag) = (inbox.clone(), stop.clone());
        std::thread::Builder::new()
            .name("udp-recv".into())
            .spawn(move || {
                let mut buf = [0u8; MAX_MESSAGE];
                while !flag.load(Ordering::Relaxed) {
                    match socket.recv_from(&mut buf) {
                        Ok((n, from)) => q.push(from, &buf[..n]),
                        Err(e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::WouldBlock
                                    | io::ErrorKind::TimedOut
                                    | io::ErrorKind::ConnectionReset
                            ) => {}
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Self {
            udp,
            inbox,
            others: Vec::new(),
            stop,
        })
    }

    /// Adds a transport whose received datagrams land in [`Self::inbox`].
    pub fn add(&mut self, t: Box<dyn Transport + Send>) {
        self.others.push(t);
    }
}

impl Drop for Joined {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Transport for Joined {
    fn local_addr(&self) -> SocketAddr {
        self.udp.local
    }

    fn send_to(&mut self, to: SocketAddr, data: &[u8]) {
        match self.others.iter_mut().find(|t| t.carries(to)) {
            Some(t) => t.send_to(to, data),
            None => self.udp.send_to(to, data),
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
        self.others.iter().any(|t| t.carries(peer))
    }

    fn take_closed(&mut self) -> Vec<SocketAddr> {
        self.others
            .iter_mut()
            .flat_map(|t| t.take_closed())
            .collect()
    }
}

/// An in-process switchboard: endpoints made by [`MemNet::endpoint`] exchange datagrams by
/// address without touching the network. Cloneable and shareable across threads.
#[derive(Clone, Default)]
pub struct MemNet {
    routes: Arc<Mutex<HashMap<SocketAddr, Inbox>>>,
}

impl MemNet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens the endpoint `addr`; sending to an address nobody holds drops the datagram.
    pub fn endpoint(&self, addr: SocketAddr) -> MemTransport {
        let q = Inbox::new(usize::MAX);
        self.routes.lock().unwrap().insert(addr, q.clone());
        MemTransport {
            net: self.clone(),
            addr,
            inbox: q,
        }
    }
}

pub struct MemTransport {
    net: MemNet,
    addr: SocketAddr,
    inbox: Inbox,
}

impl Drop for MemTransport {
    fn drop(&mut self) {
        self.net.routes.lock().unwrap().remove(&self.addr);
    }
}

impl Transport for MemTransport {
    fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    fn send_to(&mut self, to: SocketAddr, data: &[u8]) {
        let dest = self.net.routes.lock().unwrap().get(&to).cloned();
        if let Some(q) = dest {
            q.push(self.addr, data);
        }
    }

    fn recv_from(
        &mut self,
        buf: &mut [u8],
        timeout: Option<Duration>,
    ) -> io::Result<Option<(usize, SocketAddr)>> {
        Ok(self.inbox.pop(buf, timeout))
    }
}

/// Wraps a transport and drops, duplicates and reorders outgoing datagrams deterministically,
/// to test the layers above under bad paths.
pub struct Impaired<T> {
    pub inner: T,
    pub loss: f32,
    pub duplicate: f32,
    /// Chance a datagram is held back and sent after the next one.
    pub reorder: f32,
    rng: u64,
    held: Option<(SocketAddr, Vec<u8>)>,
    pub sent: u64,
    pub dropped: u64,
}

impl<T: Transport> Impaired<T> {
    pub fn new(inner: T, seed: u64, loss: f32, duplicate: f32, reorder: f32) -> Self {
        Self {
            inner,
            loss,
            duplicate,
            reorder,
            rng: seed | 1,
            held: None,
            sent: 0,
            dropped: 0,
        }
    }

    fn chance(&mut self, p: f32) -> bool {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let r = self.rng.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 40;
        (r as f32 / (1u64 << 24) as f32) < p
    }
}

impl<T: Transport> Transport for Impaired<T> {
    fn local_addr(&self) -> SocketAddr {
        self.inner.local_addr()
    }

    fn send_to(&mut self, to: SocketAddr, data: &[u8]) {
        self.sent += 1;
        if self.chance(self.loss) {
            self.dropped += 1;
            return;
        }
        if self.held.is_none() && self.chance(self.reorder) {
            self.held = Some((to, data.to_vec()));
            return;
        }
        self.inner.send_to(to, data);
        if self.chance(self.duplicate) {
            self.inner.send_to(to, data);
        }
        if let Some((t, d)) = self.held.take() {
            self.inner.send_to(t, &d);
        }
    }

    fn recv_from(
        &mut self,
        buf: &mut [u8],
        timeout: Option<Duration>,
    ) -> io::Result<Option<(usize, SocketAddr)>> {
        self.inner.recv_from(buf, timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(p: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], p))
    }

    #[test]
    fn mem_endpoints_exchange_datagrams() {
        let net = MemNet::new();
        let mut a = net.endpoint(addr(1));
        let mut b = net.endpoint(addr(2));
        a.send_to(addr(2), b"hi");
        a.send_to(addr(9), b"nobody");
        let mut buf = [0u8; 16];
        assert_eq!(b.recv_from(&mut buf, None).unwrap(), Some((2, addr(1))));
        assert_eq!(&buf[..2], b"hi");
        assert_eq!(b.recv_from(&mut buf, None).unwrap(), None);
    }

    #[test]
    fn udp_loopback_round_trip() {
        let mut a = UdpTransport::bind(addr(0)).unwrap();
        let mut b = UdpTransport::bind(addr(0)).unwrap();
        a.send_to(b.local_addr(), b"ping");
        let mut buf = [0u8; 16];
        let (n, from) = b
            .recv_from(&mut buf, Some(Duration::from_secs(2)))
            .unwrap()
            .unwrap();
        assert_eq!(
            (&buf[..n], from.port()),
            (&b"ping"[..], a.local_addr().port())
        );
        assert_eq!(b.recv_from(&mut buf, None).unwrap(), None);
    }
}
