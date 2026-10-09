// SPDX-License-Identifier: GPL-3.0-only
//! `net-hardening`: the server's network capacity and the ways a client leaves or is kept out. A server that
//! latched `sv_maxclients 40` at the first map takes 34 real UDP clients (more than the 32 the peer table had at
//! boot), reports them and the free slots in `getinfo`, and frees a slot at once when a client says `disconnect`
//! (not after a timeout). After a map change with `sv_maxclients 4`, `sv_privateClients 2` and a private password,
//! the public slots fill up and refuse the next person with "Server is full.", while a person who gives the
//! password takes a private slot.
use super::net_objective::Human;
use crate::stage::{StageCtx, StageReport, Status};
use net::oob::Oob;
use net::{Transport, UdpTransport};
use server::client::Conn;
use server::server::Server;
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const NAME: &str = "net-hardening";
const LIMIT: Duration = Duration::from_secs(120);
/// Past the 32 clients the peer table had when the server booted.
const MANY: u16 = 34;

/// Steps the server and the people until `done` holds; false when `limit` runs out first.
fn until(
    server: &mut Server,
    people: &mut [Human],
    limit: Duration,
    done: impl Fn(&Server, &[Human]) -> bool,
) -> bool {
    let deadline = Instant::now() + limit;
    while !done(server, people) {
        if Instant::now() > deadline {
            return false;
        }
        server.run_for(Duration::from_millis(16));
        for p in people.iter_mut() {
            p.step();
        }
    }
    true
}

/// The answer to a `getinfo` from a plain socket.
fn getinfo(
    server: &mut Server,
    t: &mut UdpTransport,
    to: SocketAddr,
) -> Option<Vec<(String, String)>> {
    t.send_to(to, &Oob::GetInfo(7).encode());
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buf = vec![0u8; 4096];
    while Instant::now() < deadline {
        server.run_for(Duration::from_millis(10));
        while let Ok(Some((n, _))) = t.recv_from(&mut buf, Some(Duration::from_millis(1))) {
            if let Some(Oob::InfoResponse(kv)) = Oob::parse(&buf[..n]) {
                return Some(kv);
            }
        }
    }
    None
}

fn value<'a>(kv: &'a [(String, String)], key: &str) -> &'a str {
    kv.iter()
        .find(|(k, _)| k == key)
        .map_or("", |(_, v)| v.as_str())
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(install) = ctx.install.as_deref() else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("install not found"));
    };
    let args: Vec<String> = ["+set", "net_port", "0"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let mut server = match Server::boot(install, &args, false) {
        Ok(s) => s,
        Err(e) => return Ok(StageReport::new(NAME, Status::Failed).with_reason(e)),
    };
    let Some(addr) = server.net_addr() else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("cannot bind a UDP socket"));
    };
    let addr = SocketAddr::from(([127, 0, 0, 1], addr.port()));
    let fail = |why: String| Ok(StageReport::new(NAME, Status::Failed).with_reason(why));
    let exec = |server: &mut Server, lines: &[&str]| -> Result<(), String> {
        for line in lines {
            server.exec_line(line).map_err(|e| format!("{line}: {e}"))?;
        }
        Ok(())
    };
    // The cfg a server runs sets sv_maxclients after the socket is bound; it applies at the first map.
    if let Err(e) = exec(
        &mut server,
        &[
            "set g_gametype war",
            "set scr_war_timelimit 0",
            "set scr_war_scorelimit 0",
            "set sv_maxclients 40",
            "map mp_crash",
        ],
    ) {
        return fail(e);
    }
    if server.game.max_clients != 40 {
        return fail(format!(
            "sv_maxclients did not apply: {}",
            server.game.max_clients
        ));
    }
    let mut probe = UdpTransport::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;

    let mut people = Vec::new();
    for id in 1..=MANY {
        people.push(Human::new(addr, id)?);
    }
    if !until(&mut server, &mut people, LIMIT, |s, _| {
        s.net_clients() == usize::from(MANY)
    }) {
        return fail(format!(
            "{} of {MANY} clients got a slot",
            server.net_clients()
        ));
    }
    let Some(kv) = getinfo(&mut server, &mut probe, addr) else {
        return fail("getinfo was not answered".into());
    };
    if value(&kv, "clients") != MANY.to_string() || value(&kv, "sv_maxclients") != "40" {
        return fail(format!(
            "getinfo: clients {:?} sv_maxclients {:?}",
            value(&kv, "clients"),
            value(&kv, "sv_maxclients")
        ));
    }

    // A client that says `disconnect` frees its slot at once, not after the timeout.
    let leaver = people.remove(0);
    let mut leaver = [leaver];
    leaver[0].c.disconnect();
    if !until(&mut server, &mut people, Duration::from_secs(10), |s, _| {
        s.net_clients() == usize::from(MANY) - 1
    }) {
        return fail("a client's disconnect did not free its slot".into());
    }
    drop(leaver);
    for p in &mut people {
        p.c.disconnect();
    }
    if !until(&mut server, &mut people, Duration::from_secs(20), |s, _| {
        s.net_clients() == 0
    }) {
        return fail(format!(
            "{} clients left after everyone disconnected",
            server.net_clients()
        ));
    }
    drop(people);

    // Private slots: two of four are kept for a person with the password.
    if let Err(e) = exec(
        &mut server,
        &[
            "set sv_maxclients 4",
            "set sv_privateClients 2",
            "set sv_privatePassword s3cret",
            "map mp_crash",
        ],
    ) {
        return fail(e);
    }
    let mut public = vec![Human::new(addr, 101)?, Human::new(addr, 102)?];
    if !until(&mut server, &mut public, LIMIT, |s, _| s.net_clients() == 2) {
        return fail("the public slots did not fill".into());
    }
    let taken: Vec<usize> = (0..server.game.clients.len())
        .filter(|n| server.game.clients[*n].conn != Conn::Free)
        .collect();
    if taken.len() != 2 || taken.iter().any(|n| *n < 2) {
        return fail(format!("the public clients hold slots {taken:?}"));
    }
    let mut late = vec![Human::new(addr, 103)?];
    if !until(&mut server, &mut late, LIMIT, |_, p| {
        p[0].c.refused().is_some()
    }) || late[0].c.refused() != Some("Server is full.")
    {
        return fail(format!(
            "a third public client was not refused as full: {:?}",
            late[0].c.refused()
        ));
    }
    let Some(kv) = getinfo(&mut server, &mut probe, addr) else {
        return fail("getinfo was not answered".into());
    };
    if value(&kv, "sv_maxclients") != "2" {
        return fail(format!(
            "getinfo with the private slots free: sv_maxclients {:?}",
            value(&kv, "sv_maxclients")
        ));
    }
    let mut vip = vec![Human::with_password(addr, 104, "s3cret")?];
    if !until(&mut server, &mut vip, LIMIT, |s, _| s.net_clients() == 3) {
        return fail("the person with the private password was not let in".into());
    }
    let slot = server
        .game
        .clients
        .iter()
        .position(|c| c.conn != Conn::Free && c.name == "human104");
    if !slot.is_some_and(|n| n < 2) {
        return fail(format!("the private password gave slot {slot:?}"));
    }

    let mut report = StageReport::new(NAME, Status::Passed);
    report.notes.push(format!(
        "{MANY} clients joined a server booted for 32 after sv_maxclients 40 was latched; getinfo showed them; disconnect freed a slot at once; private slots: public clients kept out of them and refused as full, the password gave slot {}",
        slot.unwrap_or_default()
    ));
    Ok(report)
}
