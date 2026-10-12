// SPDX-License-Identifier: GPL-3.0-only
//! `net-master`: the Internet list end to end. A master starts, two public servers (`sv_public 1`, `sv_master1`) send
//! it heartbeats and a client that asks it (`getchallenge`, `getservers`, then `getinfo` to each server, as the game's
//! Internet list does) lists both. One server is then stopped: its final heartbeat makes the master probe it, and
//! the list drops it while the other stays. A server that is not public never shows up.
use crate::stage::{StageCtx, StageReport, Status};
use master::Master;
use net::oob::Oob;
use net::{Transport, UdpTransport};
use server::server::Server;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

const NAME: &str = "net-master";
const LIMIT: Duration = Duration::from_secs(90);

/// A server on its own thread (the script VM is per thread); it stops, and says goodbye, when `stop` is set.
fn spawn_server(
    install: PathBuf,
    master: SocketAddr,
    name: &'static str,
    public: bool,
    stop: Arc<AtomicBool>,
) -> (
    std::thread::JoinHandle<()>,
    mpsc::Receiver<Result<u16, String>>,
) {
    let (tx, rx) = mpsc::channel();
    let h = std::thread::spawn(move || {
        let mut args: Vec<String> = Vec::new();
        for (k, v) in [
            ("net_port", "0".to_owned()),
            ("sv_hostname", name.to_owned()),
            ("sv_public", u8::from(public).to_string()),
            ("sv_master1", master.to_string()),
        ] {
            args.extend(["+set".to_owned(), k.to_owned(), v]);
        }
        let mut s = match Server::boot(&install, &args, false) {
            Ok(s) => s,
            Err(e) => return drop(tx.send(Err(e))),
        };
        s.announce_to_masters();
        let Some(addr) = s.net_addr() else {
            return drop(tx.send(Err("cannot bind a UDP socket".into())));
        };
        for line in [
            "set g_gametype war",
            "set scr_war_timelimit 0",
            "set scr_war_scorelimit 0",
            "map mp_crash",
        ] {
            if let Err(e) = s.exec_line(line) {
                return drop(tx.send(Err(format!("{line}: {e}"))));
            }
        }
        let _ = tx.send(Ok(addr.port()));
        while !stop.load(Ordering::Relaxed) {
            s.run_for(Duration::from_millis(30));
        }
    });
    (h, rx)
}

/// What the game's Internet list does: the servers `master` names and what each says its `hostname` is.
fn browse(master: SocketAddr, listed: &mut Vec<(SocketAddr, String)>) -> io::Result<()> {
    let mut t = UdpTransport::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
    let masters = [master];
    net::master::begin(&masters, |to, o| t.send_to(to, &o.encode()));
    let mut buf = vec![0u8; 4096];
    let end = Instant::now() + Duration::from_secs(3);
    let mut asked = Vec::new();
    while Instant::now() < end {
        let Ok(Some((n, from))) = t.recv_from(&mut buf, Some(Duration::from_millis(50))) else {
            continue;
        };
        match Oob::parse(&buf[..n]) {
            Some(Oob::InfoResponse(kv)) if asked.contains(&from) => {
                let host = kv
                    .iter()
                    .find(|(k, _)| k == "hostname")
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                listed.push((from, host));
            }
            Some(o) => {
                let mut out = Vec::new();
                let list = net::master::reply(&masters, from, o, |to, o| out.push((to, o)));
                for (to, o) in out {
                    t.send_to(to, &o.encode());
                }
                for a in list.into_iter().flatten() {
                    asked.push(a);
                    t.send_to(a, &Oob::GetInfo(1).encode());
                }
            }
            None => {}
        }
    }
    Ok(())
}

fn hosts(master: SocketAddr) -> io::Result<Vec<String>> {
    let mut v = Vec::new();
    browse(master, &mut v)?;
    let mut h: Vec<String> = v.into_iter().map(|(_, h)| h).collect();
    h.sort();
    Ok(h)
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(install) = ctx.install.as_deref() else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("install not found"));
    };
    let Ok(mut mt) = UdpTransport::bind(SocketAddr::from(([127, 0, 0, 1], 0))) else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("cannot bind a UDP socket"));
    };
    let master = mt.local_addr();
    let stop_master = Arc::new(AtomicBool::new(false));
    let master_thread = {
        let stop = stop_master.clone();
        std::thread::spawn(move || {
            let mut m = Master::new(Instant::now());
            let mut buf = vec![0u8; 2048];
            while !stop.load(Ordering::Relaxed) {
                let got = mt.recv_from(&mut buf, Some(Duration::from_millis(20)));
                let now = Instant::now();
                if let Ok(Some((n, from))) = got
                    && let Some(o) = Oob::parse(&buf[..n])
                {
                    for (to, r) in m.handle(from, o, now) {
                        mt.send_to(to, &r.encode());
                    }
                }
                m.expire(now);
            }
        })
    };
    let (stop_a, stop_b, stop_c) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let servers = [
        spawn_server(install.to_owned(), master, "Alpha", true, stop_a.clone()),
        spawn_server(install.to_owned(), master, "Bravo", true, stop_b.clone()),
        // Not public: it never heartbeats, so it must not be listed.
        spawn_server(install.to_owned(), master, "Hidden", false, stop_c.clone()),
    ];
    let finish = |stops: &[&Arc<AtomicBool>]| {
        for s in stops {
            s.store(true, Ordering::Relaxed);
        }
    };
    let result = (|| -> Result<Vec<String>, String> {
        for (_, rx) in &servers {
            rx.recv_timeout(LIMIT)
                .map_err(|_| "a server did not start".to_owned())??;
        }
        let mut notes = Vec::new();
        let end = Instant::now() + LIMIT;
        let both = ["Alpha".to_owned(), "Bravo".to_owned()];
        loop {
            let h = hosts(master).map_err(|e| e.to_string())?;
            if h == both {
                notes.push("the client lists both public servers and not the private one".into());
                break;
            }
            if Instant::now() > end {
                return Err(format!(
                    "the list never showed Alpha and Bravo; last: {h:?}"
                ));
            }
        }
        // Stop Bravo: it says goodbye, the master probes it, nobody answers.
        stop_b.store(true, Ordering::Relaxed);
        let end = Instant::now() + LIMIT;
        loop {
            let h = hosts(master).map_err(|e| e.to_string())?;
            if h == ["Alpha".to_owned()] {
                notes.push("the stopped server dropped off and the other stayed".into());
                return Ok(notes);
            }
            if Instant::now() > end {
                return Err(format!("the stopped server stayed listed; last: {h:?}"));
            }
        }
    })();
    finish(&[&stop_a, &stop_b, &stop_c]);
    stop_master.store(true, Ordering::Relaxed);
    for (h, _) in servers {
        let _ = h.join();
    }
    let _ = master_thread.join();
    Ok(match result {
        Ok(notes) => {
            let mut r = StageReport::new(NAME, Status::Passed);
            r.notes = notes;
            r
        }
        Err(e) => StageReport::new(NAME, Status::Failed).with_reason(e),
    })
}
