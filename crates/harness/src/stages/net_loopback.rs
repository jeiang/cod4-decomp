// SPDX-License-Identifier: GPL-3.0-or-later
//! `net-loopback`: eight clients connect to one server over real UDP on the loopback interface,
//! through a path that loses, duplicates and reorders datagrams. Asserts the handshake, that
//! reliable commands arrive exactly once and in order, that every snapshot a client accepts equals
//! what the server built, and that after the path clears every client converges on the server's
//! latest state. Reports bytes per client per second.
use crate::stage::{StageCtx, StageReport, Status};
use net::connect::{ConnectRequest, ConnectState, Connector, Gate, serve};
use net::oob::{Challenger, Oob};
use net::transport::{Impaired, UdpTransport};
use net::{ClientLink, EntityState, ServerLink, Snapshot, Transport};
use sim::pm::UserCmd;
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const CLIENTS: usize = 8;
const TICKS: u32 = 300;
const CLEAR_TICKS: u32 = 12;
const TICK_MS: i32 = 33;

fn world(tick: u32, viewer: usize) -> Snapshot {
    let mut s = Snapshot::empty();
    s.server_time = tick as i32 * TICK_MS;
    s.ps.command_time = s.server_time;
    s.ps.origin = [tick as f32 * 3.5 + viewer as f32, 2.0, 8.0];
    s.ps.viewangles = [0.0, (tick * 3 % 360) as f32, 0.0];
    for k in 0..32u16 {
        s.entities.push(EntityState {
            number: k,
            etype: net::entity::etype::PLAYER,
            origin: [f32::from(k) * 50.0 + tick as f32 * 2.0, f32::from(k), 0.0],
            angles: [0.0, (tick * 5 + u32::from(k)) as f32 % 360.0, 0.0],
            ..EntityState::default()
        });
    }
    s.canonical()
}

struct Client {
    link: ClientLink,
    t: Impaired<UdpTransport>,
    commands: Vec<String>,
    last: Option<Snapshot>,
    accepted: u64,
    bad: u64,
}

fn drain(t: &mut dyn Transport, wait: Duration, mut f: impl FnMut(&[u8], SocketAddr)) {
    let mut buf = [0u8; 2048];
    let mut wait = wait;
    while let Ok(Some((n, from))) = t.recv_from(&mut buf, Some(wait)) {
        f(&buf[..n], from);
        wait = Duration::ZERO;
    }
}

pub fn run(_: &StageCtx) -> io::Result<StageReport> {
    let mut report = StageReport::new("net-loopback", Status::Passed);
    let lo = SocketAddr::from(([127, 0, 0, 1], 0));
    let mut st = Impaired::new(UdpTransport::bind(lo)?, 11, 0.1, 0.02, 0.05);
    let server_addr = st.local_addr();
    let challenger = Challenger::new();
    let started = Instant::now();
    let mut conns = Vec::new();
    for i in 0..CLIENTS {
        let t = Impaired::new(UdpTransport::bind(lo)?, 100 + i as u64, 0.1, 0.02, 0.05);
        conns.push((
            Connector::new(server_addr, 1000 + i as u16, &format!("p{i}"), ""),
            t,
        ));
    }
    // Handshake: the loop ends when everyone is connected or after a generous deadline; it
    // never depends on how fast the machine is.
    let mut accepted: Vec<ConnectRequest> = Vec::new();
    let deadline = started + Duration::from_secs(30);
    while conns
        .iter()
        .any(|(c, _)| *c.state() != ConnectState::Connected)
        && Instant::now() < deadline
    {
        let now_ms = started.elapsed().as_millis() as u64;
        for (c, t) in &mut conns {
            c.poll(now_ms, t);
        }
        let mut replies = Vec::new();
        drain(&mut st, Duration::from_millis(2), |p, from| {
            if let Some(o) = Oob::parse(p) {
                match serve(&challenger, now_ms / 1000, from, o, Vec::new) {
                    Gate::Reply(r) => replies.push((from, r)),
                    Gate::Accept(req) => {
                        if !accepted.iter().any(|a| a.from == req.from) {
                            accepted.push(req.clone());
                        }
                        replies.push((from, Oob::ConnectResponse));
                    }
                    _ => {}
                }
            }
        });
        for (to, r) in replies {
            st.send_to(to, &r.encode());
        }
        for (c, t) in &mut conns {
            drain(t, Duration::from_millis(1), |p, from| {
                if let Some(o) = Oob::parse(p) {
                    c.handle(from, o);
                }
            });
        }
    }
    if conns
        .iter()
        .any(|(c, _)| *c.state() != ConnectState::Connected)
    {
        return Ok(StageReport::new("net-loopback", Status::Failed)
            .with_reason("not every client connected within 30 s"));
    }
    let mut srv: Vec<ServerLink> = Vec::new();
    let mut cli: Vec<Client> = Vec::new();
    for (c, t) in conns {
        let from = t.local_addr();
        let Some(req) = accepted.iter().find(|a| a.from.port() == from.port()) else {
            return Ok(StageReport::new("net-loopback", Status::Failed)
                .with_reason("a connected client has no accepted request"));
        };
        srv.push(ServerLink::new(req.from, req.qport));
        cli.push(Client {
            link: ClientLink::new(server_addr, c.qport),
            t,
            commands: Vec::new(),
            last: None,
            accepted: 0,
            bad: 0,
        });
    }

    let mut cmd_total = 0u64;
    for tick in 1..=TICKS + CLEAR_TICKS {
        if tick == TICKS + 1 {
            st.loss = 0.0;
            st.duplicate = 0.0;
            st.reorder = 0.0;
            for c in &mut cli {
                c.t.loss = 0.0;
                c.t.duplicate = 0.0;
                c.t.reorder = 0.0;
            }
        }
        for (i, s) in srv.iter_mut().enumerate() {
            if tick <= 100 {
                let _ = s.command(format!("s{tick}"));
            }
            s.send(&mut st, Some(world(tick, i)));
        }
        for c in &mut cli {
            drain(&mut c.t, Duration::from_millis(2), |p, _| {
                if let Some(m) = c.link.receive(p) {
                    c.commands.extend(m.reliable);
                    if let Some(s) = m.snapshot {
                        c.accepted += 1;
                        c.last = Some(s);
                    }
                }
            });
            for k in 0..4 {
                c.link.push_cmd(UserCmd {
                    server_time: (tick * 4 + k) as i32 * 8,
                    forwardmove: 127,
                    ..UserCmd::default()
                });
            }
            c.link.send(&mut c.t);
        }
        let mut inbox: Vec<(SocketAddr, Vec<u8>)> = Vec::new();
        drain(&mut st, Duration::from_millis(2), |p, from| {
            inbox.push((from, p.to_vec()));
        });
        for (from, p) in inbox {
            if let Some(s) = srv.iter_mut().find(|s| s.addr == from)
                && let Some(pk) = s.receive(&p)
            {
                cmd_total += pk.cmds.len() as u64;
            }
        }
    }

    let expect: Vec<String> = (1..=100).map(|i| format!("s{i}")).collect();
    let mut failures = Vec::new();
    let final_tick = TICKS + CLEAR_TICKS;
    for (i, c) in cli.iter_mut().enumerate() {
        if c.commands != expect {
            failures.push(format!(
                "client {i}: reliable commands wrong ({} of 100 arrived)",
                c.commands.len()
            ));
        }
        match &c.last {
            Some(s) => {
                // The server numbers snapshots per client; the world is a function of the tick.
                let mut want = world(s.server_time as u32 / TICK_MS as u32, i);
                want.num = s.num;
                if *s != want {
                    c.bad += 1;
                }
                if s.server_time != final_tick as i32 * TICK_MS {
                    failures.push(format!(
                        "client {i}: stopped at tick {} of {final_tick}",
                        s.server_time / TICK_MS
                    ));
                }
            }
            None => failures.push(format!("client {i}: no snapshot")),
        }
        if c.bad > 0 {
            failures.push(format!("client {i}: a snapshot differs from the server's"));
        }
        if c.accepted < u64::from(TICKS) * 6 / 10 {
            failures.push(format!("client {i}: only {} snapshots arrived", c.accepted));
        }
    }
    if cmd_total < u64::from(TICKS) * 4 * CLIENTS as u64 / 2 {
        failures.push(format!("server received only {cmd_total} usercmds"));
    }
    let secs = f64::from(final_tick) * f64::from(TICK_MS) / 1000.0;
    let s_bytes: u64 = srv.iter().map(|s| s.stats.bytes_out).sum();
    let c_bytes: u64 = cli.iter().map(|c| c.link.stats.bytes_out).sum();
    let snaps: u64 = srv.iter().map(|s| s.stats.snapshots_out).sum();
    let snap_bytes: u64 = srv.iter().map(|s| s.stats.snapshot_bytes_out).sum();
    let m = &mut report.metrics;
    m.insert(
        "server_to_client.bytes_per_sec".into(),
        s_bytes as f64 / secs / CLIENTS as f64,
    );
    m.insert(
        "client_to_server.bytes_per_sec".into(),
        c_bytes as f64 / secs / CLIENTS as f64,
    );
    m.insert(
        "snapshot.avg_bytes".into(),
        snap_bytes as f64 / snaps.max(1) as f64,
    );
    m.insert("usercmds.received".into(), cmd_total as f64);
    m.insert(
        "snapshots.accepted_per_client".into(),
        cli.iter().map(|c| c.accepted).sum::<u64>() as f64 / CLIENTS as f64,
    );
    report.notes.push(format!(
        "{CLIENTS} clients, 32 moving entities each, 10% loss/2% duplicates/5% reorder both ways for {TICKS} ticks"
    ));
    if !failures.is_empty() {
        report.status = Status::Failed;
        report.reason = Some(failures.join("; "));
    }
    Ok(report)
}
