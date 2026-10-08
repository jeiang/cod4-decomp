// SPDX-License-Identifier: GPL-3.0-or-later
//! Browser clients over WebTransport: a real `wtransport` client, pinning the server's certificate
//! hash the way a page's `serverCertificateHashes` does, talks to a [`NetSv`] next to UDP clients.
//! [`WtClient`] is written from the framing rule in the `net::wt` docs, not from the server code.

use net::Snapshot;
use net::client::NetClient;
use net::entity::{EntityState, etype};
use net::transport::{Inbox, Joined, MAX_MESSAGE, Transport, UdpTransport};
use net::wt::{BROWSER_DATAGRAM, WtConfig, WtTransport};
use server::netsv::{Inbound, NetSv};
use sim::pm::UserCmd;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use wtransport::tls::Sha256Digest;
use wtransport::{ClientConfig, Connection, Endpoint};

const LO: [u8; 4] = [127, 0, 0, 1];

fn lo(port: u16) -> SocketAddr {
    SocketAddr::from((LO, port))
}

fn until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(30);
    while !f() {
        assert!(Instant::now() < end, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A browser's side of the session: datagrams up to the datagram limit, longer messages as one
/// unidirectional stream each.
struct WtClient {
    rt: tokio::runtime::Runtime,
    conn: Connection,
    _endpoint: Endpoint<wtransport::endpoint::endpoint_side::Client>,
    inbox: Inbox,
    streams_in: Arc<AtomicUsize>,
    datagrams_in: Arc<AtomicUsize>,
}

impl WtClient {
    fn connect(server: SocketAddr, pin: [u8; 32]) -> Result<Self, String> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let inbox = Inbox::new(1024);
        let (streams_in, datagrams_in) =
            (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (endpoint, conn) = rt.block_on(async {
            let cfg = ClientConfig::builder()
                .with_bind_default()
                .with_server_certificate_hashes([Sha256Digest::new(pin)])
                .build();
            let endpoint = Endpoint::client(cfg).map_err(|e| e.to_string())?;
            let url = format!("https://127.0.0.1:{}/", server.port());
            let conn = endpoint.connect(url).await.map_err(|e| e.to_string())?;
            Ok::<_, String>((endpoint, conn))
        })?;
        let (c, q, n) = (conn.clone(), inbox.clone(), datagrams_in.clone());
        rt.spawn(async move {
            while let Ok(d) = c.receive_datagram().await {
                n.fetch_add(1, Ordering::SeqCst);
                q.push(server, &d.payload());
            }
        });
        let (c, q, n) = (conn.clone(), inbox.clone(), streams_in.clone());
        rt.spawn(async move {
            while let Ok(mut rx) = c.accept_uni().await {
                let mut msg = Vec::new();
                let mut chunk = [0u8; 1500];
                while let Ok(Some(k)) = rx.read(&mut chunk).await {
                    msg.extend_from_slice(&chunk[..k]);
                }
                n.fetch_add(1, Ordering::SeqCst);
                q.push(server, &msg);
            }
        });
        Ok(Self {
            rt,
            conn,
            _endpoint: endpoint,
            inbox,
            streams_in,
            datagrams_in,
        })
    }
}

impl Drop for WtClient {
    fn drop(&mut self) {
        self.conn.close(0u8.into(), b"bye");
        self.rt.block_on(self._endpoint.wait_idle());
    }
}

impl Transport for WtClient {
    fn local_addr(&self) -> SocketAddr {
        self._endpoint.local_addr().unwrap()
    }

    fn send_to(&mut self, _to: SocketAddr, data: &[u8]) {
        let limit = self
            .conn
            .max_datagram_size()
            .map_or(0, |m| m.min(BROWSER_DATAGRAM));
        if data.len() <= limit {
            let _ = self.conn.send_datagram(data);
        } else {
            let conn = self.conn.clone();
            let data = data.to_vec();
            self.rt.block_on(async move {
                let mut tx = conn.open_uni().await.unwrap().await.unwrap();
                tx.write_all(&data).await.unwrap();
                // The server may stop the stream once it has read it all.
                let _ = tx.finish().await;
            });
        }
    }

    fn recv_from(
        &mut self,
        buf: &mut [u8],
        timeout: Option<Duration>,
    ) -> std::io::Result<Option<(usize, SocketAddr)>> {
        Ok(self.inbox.pop(buf, timeout))
    }
}

/// A server endpoint with UDP and WebTransport on loopback.
struct Endpoints {
    joined: Joined,
    udp: SocketAddr,
    wt: SocketAddr,
    hash: [u8; 32],
}

fn endpoints() -> Endpoints {
    let udp = UdpTransport::bind(lo(0)).unwrap();
    let udp_addr = udp.local_addr();
    let mut joined = Joined::new(udp).unwrap();
    let (wt, up) = WtTransport::start(
        WtConfig {
            bind: lo(0),
            tls: None,
        },
        joined.inbox(),
    )
    .unwrap();
    joined.add(Box::new(wt));
    Endpoints {
        joined,
        udp: udp_addr,
        wt: up.addr,
        hash: up.cert_hash.expect("self-signed"),
    }
}

#[test]
fn messages_cross_both_ways_as_datagrams_or_streams() {
    let mut e = endpoints();
    let mut c = WtClient::connect(e.wt, e.hash).unwrap();
    let mut buf = vec![0u8; MAX_MESSAGE + 1];

    // Client to server: small as a datagram; over the limit, and up to the cap, as a stream.
    let msgs: Vec<Vec<u8>> = [30, 1200, 1208, 3000, MAX_MESSAGE]
        .iter()
        .map(|&n| (0..n).map(|i| (i * 7 + n) as u8).collect())
        .collect();
    let mut from = None;
    for m in &msgs {
        c.send_to(e.wt, m);
        let (n, f) = e
            .joined
            .recv_from(&mut buf, Some(Duration::from_secs(5)))
            .unwrap()
            .expect("message arrives");
        assert_eq!(&buf[..n], &m[..], "{} bytes", m.len());
        from = Some(f);
    }
    let from = from.unwrap();
    assert_ne!(
        from.port(),
        e.wt.port(),
        "named by the client's QUIC address"
    );
    assert!(e.joined.carries(from) && !e.joined.carries(lo(1)));

    // Server to client, same rule.
    for m in &msgs {
        e.joined.send_to(from, m);
        let (n, f) = c
            .recv_from(&mut buf, Some(Duration::from_secs(5)))
            .unwrap()
            .expect("reply arrives");
        assert_eq!((f, &buf[..n]), (e.wt, &m[..]), "{} bytes", m.len());
    }
    assert!(
        c.streams_in.load(Ordering::SeqCst) >= 3,
        "large replies use streams"
    );
    assert!(
        c.datagrams_in.load(Ordering::SeqCst) >= 1,
        "small replies use datagrams"
    );

    // Over the cap is refused both ways; the session survives.
    c.send_to(e.wt, &vec![1u8; MAX_MESSAGE + 1]);
    c.send_to(e.wt, b"after");
    let (n, _) = e
        .joined
        .recv_from(&mut buf, Some(Duration::from_secs(5)))
        .unwrap()
        .unwrap();
    assert_eq!(&buf[..n], b"after");
    e.joined.send_to(from, &vec![1u8; MAX_MESSAGE + 1]);
    e.joined.send_to(from, b"back");
    let (n, _) = c
        .recv_from(&mut buf, Some(Duration::from_secs(5)))
        .unwrap()
        .unwrap();
    assert_eq!(&buf[..n], b"back");
}

#[test]
fn sends_route_by_peer_and_unknown_peers_drop() {
    let mut e = endpoints();
    let mut c = WtClient::connect(e.wt, e.hash).unwrap();
    c.send_to(e.wt, b"hello");
    let mut buf = [0u8; 64];
    let (_, wt_peer) = e
        .joined
        .recv_from(&mut buf, Some(Duration::from_secs(5)))
        .unwrap()
        .unwrap();

    let mut u = UdpTransport::bind(lo(0)).unwrap();
    u.send_to(e.udp, b"udp");
    let (n, udp_peer) = e
        .joined
        .recv_from(&mut buf, Some(Duration::from_secs(5)))
        .unwrap()
        .unwrap();
    assert_eq!((&buf[..n], udp_peer), (&b"udp"[..], u.local_addr()));

    e.joined.send_to(udp_peer, b"to-udp");
    e.joined.send_to(wt_peer, b"to-wt");
    e.joined.send_to(lo(9), b"nobody"); // neither a session nor listening: dropped
    let (n, _) = u
        .recv_from(&mut buf, Some(Duration::from_secs(5)))
        .unwrap()
        .unwrap();
    assert_eq!(&buf[..n], b"to-udp");
    let (n, _) = c
        .recv_from(&mut buf, Some(Duration::from_secs(5)))
        .unwrap()
        .unwrap();
    assert_eq!(&buf[..n], b"to-wt");
    assert!(
        c.recv_from(&mut buf, Some(Duration::from_millis(200)))
            .unwrap()
            .is_none()
    );
    assert!(
        u.recv_from(&mut buf, Some(Duration::from_millis(200)))
            .unwrap()
            .is_none()
    );

    // A session that is gone is no longer addressable.
    drop(c);
    until("session closes", || !e.joined.carries(wt_peer));
    e.joined.send_to(wt_peer, b"late");
}

#[test]
fn recv_waits_for_either_source_without_spinning() {
    let mut e = endpoints();
    let mut c = WtClient::connect(e.wt, e.hash).unwrap();
    let mut buf = [0u8; 64];

    let t0 = Instant::now();
    assert!(
        e.joined
            .recv_from(&mut buf, Some(Duration::from_millis(300)))
            .unwrap()
            .is_none()
    );
    assert!(t0.elapsed() >= Duration::from_millis(290));
    assert!(e.joined.recv_from(&mut buf, None).unwrap().is_none());
    assert!(
        e.joined
            .recv_from(&mut buf, Some(Duration::ZERO))
            .unwrap()
            .is_none()
    );

    // A datagram from either source wakes a long wait at once.
    let udp = e.udp;
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        UdpTransport::bind(lo(0)).unwrap().send_to(udp, b"u");
        std::thread::sleep(Duration::from_millis(150));
        c.send_to(e.wt, b"w");
        c
    });
    let t0 = Instant::now();
    let (n, _) = e
        .joined
        .recv_from(&mut buf, Some(Duration::from_secs(20)))
        .unwrap()
        .unwrap();
    assert_eq!(&buf[..n], b"u");
    let (n, _) = e
        .joined
        .recv_from(&mut buf, Some(Duration::from_secs(20)))
        .unwrap()
        .unwrap();
    assert_eq!(&buf[..n], b"w");
    assert!(t0.elapsed() < Duration::from_secs(2));
    drop(sender.join().unwrap());
}

#[test]
fn a_client_must_pin_the_certificate_it_was_given() {
    let e = endpoints();
    let mut wrong = e.hash;
    wrong[0] ^= 1;
    assert!(WtClient::connect(e.wt, wrong).is_err());
    assert!(WtClient::connect(e.wt, e.hash).is_ok());
}

/// Runs a [`NetSv`] the way the server's frame does for its clients: poll, give connects a slot,
/// pass on what clients send, and send each client an (unacknowledged, so full) world snapshot.
struct Frames {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    /// `(slot, console line)` from clients and `(slot, usercmds taken)`.
    lines: mpsc::Receiver<(u16, String)>,
    cmds: mpsc::Receiver<(u16, usize)>,
    slots: mpsc::Receiver<(u16, SocketAddr)>,
}

impl Frames {
    fn run(mut net: NetSv) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (ltx, lines) = mpsc::channel();
        let (ctx, cmds) = mpsc::channel();
        let (stx, slots) = mpsc::channel();
        let flag = stop.clone();
        let join = std::thread::spawn(move || {
            let info = || vec![("hostname".into(), "wt-test".into())];
            let mut tick = 0u32;
            let mut next = Instant::now();
            while !flag.load(Ordering::Relaxed) {
                for i in net.poll(Duration::from_millis(20), &info) {
                    if let Inbound::Connect(req) = i
                        && net.slot_of(req.from).is_none()
                    {
                        let slot = net.peers.iter().position(Option::is_none).unwrap() as u16;
                        net.add_peer(slot, &req, &req.name);
                        let _ = stx.send((slot, req.from));
                    }
                }
                for l in std::mem::take(&mut net.inbox) {
                    let _ = ltx.send(l);
                }
                for slot in 0..net.peers.len() as u16 {
                    if net.peers[usize::from(slot)].is_some() {
                        let n = net.take_cmds(slot).len();
                        if n > 0 {
                            let _ = ctx.send((slot, n));
                        }
                    }
                }
                // A server frame (30 Hz) is not driven faster by client packets.
                next += Duration::from_millis(33);
                std::thread::sleep(next.saturating_duration_since(Instant::now()));
                tick += 1;
                let NetSv { peers, t, .. } = &mut net;
                for p in peers.iter_mut().flatten() {
                    p.link.send(t, Some(world(tick)));
                }
            }
        });
        Self {
            stop,
            join: Some(join),
            lines,
            cmds,
            slots,
        }
    }
}

impl Drop for Frames {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn world(tick: u32) -> Snapshot {
    let mut s = Snapshot::empty();
    s.server_time = tick as i32 * 33;
    s.ps.command_time = tick as i32 * 33;
    // Enough entities that a full snapshot is several full-size netchan fragments.
    for k in 0..WORLD_ENTITIES {
        s.entities.push(EntityState {
            number: k,
            etype: etype::PLAYER,
            origin: [f32::from(k) * 7.3 + tick as f32, -2.2 * f32::from(k), 9.0],
            angles: [1.0, f32::from(k), 0.0],
            model: k % 50,
            ..EntityState::default()
        });
    }
    s.canonical()
}

const WORLD_ENTITIES: u16 = 200;

fn play<T: Transport>(
    c: &mut NetClient<T>,
    what: &str,
    mut done: impl FnMut(&mut NetClient<T>) -> bool,
) {
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        c.pump(Duration::from_millis(20));
        assert!(c.refused().is_none(), "{what}: refused {:?}", c.refused());
        if c.connected() {
            c.send_cmd(UserCmd::default());
        }
        if done(c) {
            return;
        }
        assert!(Instant::now() < end, "timed out: {what}");
    }
}

#[test]
fn a_browser_client_and_a_udp_client_join_and_play_through_the_connect_path() {
    let e = endpoints();
    let (udp, wt, hash) = (e.udp, e.wt, e.hash);
    let frames = Frames::run(NetSv::new(Box::new(e.joined), 4));

    let mut b = NetClient::new(
        WtClient::connect(wt, hash).unwrap(),
        wt,
        "browser",
        "",
        7001,
    );
    let mut u = NetClient::new(UdpTransport::bind(lo(0)).unwrap(), udp, "native", "", 7002);
    play(&mut b, "browser connects and gets a snapshot", |c| {
        c.latest().is_some()
    });
    play(&mut u, "udp connects and gets a snapshot", |c| {
        c.latest().is_some()
    });
    assert_ne!(
        frames.slots.try_recv().unwrap().0,
        frames.slots.try_recv().unwrap().0
    );

    // Netchan both ways: the full world snapshot is fragmented above the datagram limit, so it
    // reached the browser as streams; the client's reliable command and usercmds reached the server.
    assert!(b.latest().unwrap().entities.len() == usize::from(WORLD_ENTITIES));
    assert!(b.stats().is_some());
    b.command("say hello from the browser");
    play(&mut b, "reliable command arrives", |_| {
        frames
            .lines
            .try_recv()
            .is_ok_and(|(_, l)| l == "say hello from the browser")
    });
    assert!(frames.cmds.try_iter().count() > 0);
}

#[test]
fn full_fragments_to_a_browser_use_streams() {
    let e = endpoints();
    let (wt, hash) = (e.wt, e.hash);
    let frames = Frames::run(NetSv::new(Box::new(e.joined), 4));
    let t = WtClient::connect(wt, hash).unwrap();
    let (streams, datagrams) = (t.streams_in.clone(), t.datagrams_in.clone());
    let mut b = NetClient::new(t, wt, "browser", "", 7003);
    play(&mut b, "snapshot", |c| c.latest().is_some());
    assert!(
        streams.load(Ordering::SeqCst) > 0,
        "fragments of {} bytes",
        net::netchan::FRAGMENT_SIZE
    );
    assert!(
        datagrams.load(Ordering::SeqCst) > 0,
        "the connect replies are small"
    );
    drop(frames);
}

fn unbase64(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        _ => 63,
    };
    let s = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::new();
    for c in s.chunks(4) {
        let n = c.iter().fold(0u32, |a, &b| a << 6 | u32::from(val(b))) << (6 * (4 - c.len()));
        out.extend_from_slice(&n.to_be_bytes()[1..c.len()]);
    }
    out
}

/// The whole path a page takes: the server's command-line options set cvars, the endpoint comes
/// up, the info file names its URL and certificate hash, and a client built from only that file
/// joins a real match. Needs the game install (`COD4_PATH`).
#[test]
fn a_client_joins_a_booted_server_using_only_the_info_file() {
    let Some(root) = std::env::var_os("COD4_PATH") else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let info = std::env::temp_dir().join(format!("cod4e-wt-info-{}.json", std::process::id()));
    let args: Vec<String> = [
        "+set",
        "net_port",
        "0",
        "+set",
        "net_wt",
        "127.0.0.1:0",
        "+set",
        "net_wt_info",
        info.to_str().unwrap(),
        "+map",
        "mp_crash",
    ]
    .map(String::from)
    .to_vec();
    let mut s =
        server::server::Server::boot(std::path::Path::new(&root), &args, false).expect("boot");

    let json = std::fs::read_to_string(&info).expect("info file");
    let _ = std::fs::remove_file(&info);
    let field = |key: &str| {
        let rest = &json[json.find(&format!("\"{key}\":\"")).expect(key) + key.len() + 4..];
        rest[..rest.find('"').unwrap()].to_owned()
    };
    let url = field("url");
    let port: u16 = url
        .rsplit(':')
        .next()
        .unwrap()
        .trim_end_matches('/')
        .parse()
        .unwrap();
    let hash: [u8; 32] = unbase64(&field("certHashSha256Base64")).try_into().unwrap();
    assert!(url.starts_with("https://127.0.0.1:"), "{url}");

    let wt = lo(port);
    let client = std::thread::spawn(move || {
        let mut c = NetClient::new(
            WtClient::connect(wt, hash).unwrap(),
            wt,
            "browser",
            "",
            7100,
        );
        play(&mut c, "joins the match", |c| c.latest().is_some());
        c.connected()
    });
    while !client.is_finished() {
        s.run_for(Duration::from_millis(100));
    }
    assert!(client.join().unwrap());
    assert_eq!(s.net_clients(), 1);
}

#[test]
fn a_certificate_from_pem_files_is_served_and_reports_no_hash() {
    let dir = std::env::temp_dir().join(format!("cod4e-wt-pem-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let pin = rt.block_on(async {
        let id = wtransport::Identity::self_signed(["localhost"]).unwrap();
        let chain = id.certificate_chain();
        chain.store_pemfile(&cert).await.unwrap();
        id.private_key().store_secret_pemfile(&key).await.unwrap();
        *chain.as_slice()[0].hash().as_ref()
    });
    let inbox = Inbox::new(16);
    let (_wt, up) = WtTransport::start(
        WtConfig {
            bind: lo(0),
            tls: Some((cert, key)),
        },
        inbox.clone(),
    )
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(up.cert_hash.is_none());
    let mut c = WtClient::connect(up.addr, pin).expect("the loaded certificate is the one served");
    c.send_to(up.addr, b"hi");
    let mut buf = [0u8; 8];
    assert_eq!(
        inbox
            .pop(&mut buf, Some(Duration::from_secs(5)))
            .map(|(n, _)| n),
        Some(2)
    );
}
