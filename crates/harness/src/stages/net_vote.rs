// SPDX-License-Identifier: GPL-3.0-or-later
//! `net-vote`: two real UDP clients vote. One calls a kick vote against the other, who votes yes: the
//! server drops the second client three seconds after the vote passes. The first then calls a
//! gametype-and-map vote alone (it is the only voter) and the server changes to that map and gametype.
use super::net_objective::Human;
use crate::stage::{StageCtx, StageReport, Status};
use server::server::Server;
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const NAME: &str = "net-vote";
const LIMIT: Duration = Duration::from_secs(120);

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(install) = ctx.install.as_deref() else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("install not found"));
    };
    let args: Vec<String> = ["+set", "net_port", "0"].map(String::from).to_vec();
    let mut server = match Server::boot(install, &args, false) {
        Ok(s) => s,
        Err(e) => return Ok(StageReport::new(NAME, Status::Failed).with_reason(e)),
    };
    let Some(addr) = server.net_addr() else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("cannot bind a UDP socket"));
    };
    let addr = SocketAddr::from(([127, 0, 0, 1], addr.port()));
    for line in [
        "set g_gametype war",
        "set scr_war_timelimit 0",
        "set scr_war_scorelimit 0",
        "map mp_crash",
        "bots 2",
    ] {
        if let Err(e) = server.exec_line(line) {
            return Ok(StageReport::new(NAME, Status::Failed).with_reason(format!("{line}: {e}")));
        }
    }
    let mut people = [Human::new(addr, 0)?, Human::new(addr, 1)?];
    let fail = |why: String| Ok(StageReport::new(NAME, Status::Failed).with_reason(why));
    let deadline = Instant::now() + LIMIT;
    let frame = |server: &mut Server, people: &mut [Human; 2]| {
        server.run_for(Duration::from_millis(16));
        for p in people.iter_mut() {
            p.step();
        }
    };
    while !people.iter().all(|p| {
        p.own
            .and_then(|n| server.game.client(n))
            .is_some_and(|c| c.session == server::client::Session::Playing)
    }) {
        if Instant::now() > deadline {
            return fail("the two clients never got a body".into());
        }
        frame(&mut server, &mut people);
    }
    let (a, b) = (
        people[0].own.unwrap_or_default(),
        people[1].own.unwrap_or_default(),
    );
    let name = server
        .game
        .client(b)
        .map(|c| c.name.clone())
        .unwrap_or_default();

    // A kick vote: the caller and the target both say yes.
    people[0].c.command(&format!("callvote kick {name}"));
    for _ in 0..20 {
        frame(&mut server, &mut people);
    }
    people[1].c.command("vote y");
    while server.game.client(b).is_some_and(|c| c.connected()) {
        if Instant::now() > deadline {
            return fail("the kick vote passed no one out".into());
        }
        frame(&mut server, &mut people);
    }

    // A vote with one voter passes alone and changes the gametype and the map.
    people[0].c.command("callvote typemap sd mp_backlot");
    while server.map_name() != Some("mp_backlot") {
        if Instant::now() > deadline {
            return fail(format!(
                "the typemap vote never ran (map {:?}, voter {a})",
                server.map_name()
            ));
        }
        frame(&mut server, &mut people);
    }
    let gametype = server.game.cvars.string("g_gametype").to_owned();
    if gametype != "sd" {
        return fail(format!("the vote left the gametype at {gametype:?}"));
    }
    let mut report = StageReport::new(NAME, Status::Passed);
    report
        .notes
        .push("the kick vote dropped the target; the typemap vote moved to mp_backlot, sd".into());
    Ok(report)
}
