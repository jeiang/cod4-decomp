// SPDX-License-Identifier: GPL-3.0-or-later
//! The transport the client plays on: UDP natively, WebTransport through the page in a browser.

use net::Transport;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

pub enum Wire {
    #[cfg(not(target_arch = "wasm32"))]
    Udp(net::UdpTransport),
    #[cfg(target_arch = "wasm32")]
    Web(crate::web::Wire),
}

impl Wire {
    /// A transport to talk to `server`.
    pub fn open(server: SocketAddr) -> Result<Self, String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = server;
            net::UdpTransport::bind(SocketAddr::from(([0, 0, 0, 0], 0)))
                .map(Self::Udp)
                .map_err(|e| format!("cannot open a UDP socket: {e}"))
        }
        #[cfg(target_arch = "wasm32")]
        Ok(Self::Web(crate::web::Wire::open(
            &crate::web::server_target(),
            server,
        )))
    }
}

impl Transport for Wire {
    fn local_addr(&self) -> SocketAddr {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Udp(t) => t.local_addr(),
            #[cfg(target_arch = "wasm32")]
            Self::Web(t) => t.local_addr(),
        }
    }

    fn send_to(&mut self, to: SocketAddr, data: &[u8]) {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Udp(t) => t.send_to(to, data),
            #[cfg(target_arch = "wasm32")]
            Self::Web(t) => t.send_to(to, data),
        }
    }

    fn recv_from(
        &mut self,
        buf: &mut [u8],
        timeout: Option<Duration>,
    ) -> io::Result<Option<(usize, SocketAddr)>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Udp(t) => t.recv_from(buf, timeout),
            #[cfg(target_arch = "wasm32")]
            Self::Web(t) => t.recv_from(buf, timeout),
        }
    }
}
