// SPDX-License-Identifier: GPL-3.0-only
//! `cod4e-master [--port N] [--bind addr]`: the master server public cod4e servers announce themselves to and
//! clients ask for the Internet list. It needs no game files.

// Not yet converted to `tracing` (#377): the print lints come on for this crate when its prints are gone.
#![allow(clippy::print_stderr, clippy::print_stdout)]

use master::Master;
use net::master::MASTER_PORT;
use net::oob::Oob;
use net::{Transport, UdpTransport};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::ExitCode;
use std::time::{Duration, Instant};

fn main() -> ExitCode {
    let _log = logging::init(logging::Config::new("master"));
    let mut bind = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
    let mut port = MASTER_PORT;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let value = |args: &mut dyn Iterator<Item = String>| {
            args.next().ok_or(format!("{a} needs a value"))
        };
        let r = match a.as_str() {
            "--port" => value(&mut args).and_then(|v| {
                v.parse()
                    .map(|p| port = p)
                    .map_err(|e| format!("--port: {e}"))
            }),
            "--bind" => value(&mut args).and_then(|v| {
                v.parse()
                    .map(|b| bind = b)
                    .map_err(|e| format!("--bind: {e}"))
            }),
            "-h" | "--help" => {
                println!(
                    "usage: cod4e-master [--port N] [--bind addr]\n\
                     Servers opt in with `sv_public 1` and `sv_master1 host:port`; clients set `cl_masters host:port`.\n\
                     Default port {MASTER_PORT} (UDP)."
                );
                return ExitCode::SUCCESS;
            }
            _ => Err(format!("unknown argument {a}")),
        };
        if let Err(e) = r {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    }
    let mut sock = match UdpTransport::bind(SocketAddr::new(bind, port)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot bind udp {bind}:{port}: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("cod4e-master listening on udp {}", sock.local_addr());
    let mut m = Master::new(Instant::now());
    let mut buf = vec![0u8; 2048];
    let mut next_expire = Instant::now();
    let mut listed = 0;
    loop {
        let got = sock.recv_from(&mut buf, Some(Duration::from_millis(250)));
        let now = Instant::now();
        if let Ok(Some((n, from))) = got
            && let Some(oob) = Oob::parse(&buf[..n])
        {
            for (to, reply) in m.handle(from, oob, now) {
                sock.send_to(to, &reply.encode());
            }
        }
        if now >= next_expire {
            m.expire(now);
            next_expire = now + Duration::from_secs(1);
        }
        if m.len() != listed {
            listed = m.len();
            println!("{listed} servers listed");
        }
    }
}
