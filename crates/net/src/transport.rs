// SPDX-License-Identifier: GPL-3.0-or-later
//! The datagram transport everything above it uses: UDP for the network, an in-memory switchboard
//! for the listen server and tests. A WebTransport adapter would implement the same trait.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Largest datagram any peer sends or accepts.
pub const MAX_DATAGRAM: usize = 1400;

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

type Queue = Arc<(Mutex<VecDeque<(SocketAddr, Vec<u8>)>>, std::sync::Condvar)>;

/// An in-process switchboard: endpoints made by [`MemNet::endpoint`] exchange datagrams by
/// address without touching the network. Cloneable and shareable across threads.
#[derive(Clone, Default)]
pub struct MemNet {
    routes: Arc<Mutex<HashMap<SocketAddr, Queue>>>,
}

impl MemNet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens the endpoint `addr`; sending to an address nobody holds drops the datagram.
    pub fn endpoint(&self, addr: SocketAddr) -> MemTransport {
        let q: Queue = Arc::default();
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
    inbox: Queue,
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
            q.0.lock().unwrap().push_back((self.addr, data.to_vec()));
            q.1.notify_one();
        }
    }

    fn recv_from(
        &mut self,
        buf: &mut [u8],
        timeout: Option<Duration>,
    ) -> io::Result<Option<(usize, SocketAddr)>> {
        let (lock, cv) = &*self.inbox;
        let mut q = lock.lock().unwrap();
        if q.is_empty()
            && let Some(t) = timeout.filter(|t| !t.is_zero())
        {
            q = cv.wait_timeout_while(q, t, |q| q.is_empty()).unwrap().0;
        }
        Ok(q.pop_front().map(|(from, d)| {
            let n = d.len().min(buf.len());
            buf[..n].copy_from_slice(&d[..n]);
            (n, from)
        }))
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
