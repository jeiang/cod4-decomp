// SPDX-License-Identifier: GPL-3.0-only
//! `net-vote`: two real UDP clients talk and vote. One says a line to everyone and one to its team: the other hears
//! the first, and the second only on the same team; a verb the server does not know is answered. One calls a vote
//! to kick a bot by slot (`clientkick`); the vote's configstrings, which the client draws as the yellow vote lines,
//! reach the other, who votes yes and the bot is dropped. The first then calls a kick vote against the
//! other, who votes yes: the server drops the second client three seconds after the vote passes. The first
//! finally calls a gametype-and-map vote alone (it is the only voter) and the server changes to that map and
//! gametype. It then renames itself through its userinfo, calls a `g_gametype` vote (`g_gametype war; map_restart`)
//! and the level restarts in that gametype. A `getstatus` from a bare socket lists the players, and `games_mp.log`
//! holds the game start, the chat, the script's join lines and the end of the first level.
use super::net_objective::Human;
use crate::stage::{StageCtx, StageReport, Status};
use net::ui::cs;
use server::client::Team;
use server::server::Server;
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const NAME: &str = "net-vote";
const LIMIT: Duration = Duration::from_secs(120);

/// Puts every bot on the spectators' side: connected, so the vote has someone to be called against, but no voter.
fn sideline_bots(server: &mut Server) {
    let bots: Vec<u16> = server
        .game
        .connected_clients()
        .filter(|(_, c)| c.bot)
        .map(|(n, _)| n)
        .collect();
    for n in bots {
        if let Some(c) = server.game.client_mut(n) {
            c.team = Team::Spectator;
        }
    }
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(install) = ctx.install.as_deref() else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("install not found"));
    };
    let log = ctx.dir.join("games_mp.log");
    let _ = std::fs::remove_file(&log);
    let args: Vec<String> = [
        "+set",
        "net_port",
        "0",
        "+set",
        "g_log",
        &log.to_string_lossy(),
    ]
    .map(String::from)
    .to_vec();
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
        "bots 1",
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

    // Chat: a line to everyone reaches the other person; the speaker hears it too.
    // The clients are on loopback, which the server's flood protection leaves alone.
    let say = |_: &mut Server, people: &mut [Human; 2], line: &str| -> Result<(), String> {
        people[0].c.command(line);
        Ok(())
    };
    if let Err(e) = say(&mut server, &mut people, "say \"hello there\"") {
        return fail(e);
    }
    while !(people[1].chats.iter().any(|c| c.2 == "hello there")
        && people[0].chats.iter().any(|c| c.2 == "hello there"))
    {
        if Instant::now() > deadline {
            return fail("the line a person said never reached both people".into());
        }
        frame(&mut server, &mut people);
    }
    let heard = people[1].chats.iter().find(|c| c.2 == "hello there");
    if heard != Some(&(false, a, "hello there".into())) {
        return fail(format!("the line arrived as {heard:?}"));
    }

    // A team line reaches the team only: the other person is put on the speaker's team, then the other one, and a
    // public marker after each shows what the team line did before it.
    let side = server.game.client(a).map(|c| c.team).unwrap_or_default();
    let other_side = server.game.client(b).map(|c| c.team).unwrap_or_default();
    for (on_team, text) in [(true, "team one"), (false, "team two")] {
        let put = if on_team {
            side
        } else if side == Team::Axis {
            Team::Allies
        } else {
            Team::Axis
        };
        if let Some(c) = server.game.client_mut(b) {
            c.team = put;
        }
        for (verb, line) in [("say_team", text), ("say", "marker")] {
            if let Err(e) = say(&mut server, &mut people, &format!("{verb} \"{line}\"")) {
                return fail(e);
            }
        }
        while !people[1].chats.iter().any(|c| c.2 == "marker") {
            if Instant::now() > deadline {
                return fail("the public marker line never arrived".into());
            }
            frame(&mut server, &mut people);
        }
        let got = people[1].chats.iter().any(|c| c.2 == text);
        if got != on_team {
            return fail(format!(
                "a team line {} the teammate {on_team}: heard {got}",
                if on_team {
                    "must reach"
                } else {
                    "must not reach"
                }
            ));
        }
        people[1].chats.clear();
    }
    if let Some(c) = server.game.client_mut(b) {
        c.team = other_side;
    }

    // A verb the server does not know is answered, not swallowed.
    people[0].c.command("frobnicate now");
    while !people[0]
        .prints
        .iter()
        .any(|p| p.contains("GAME_UNKNOWNCLIENTCOMMAND") && p.contains("frobnicate"))
    {
        if Instant::now() > deadline {
            return fail("the unknown command was not answered".into());
        }
        frame(&mut server, &mut people);
    }

    // `where` tells the person its origin; `kill` needs cheats, as the original's `CheatsOk`, and then
    // ends a living player the way the script `suicide` does.
    people[0].c.command("where");
    while !people[0].prints.iter().any(|p| p.starts_with('(')) {
        if Instant::now() > deadline {
            return fail("`where` was not answered with an origin".into());
        }
        frame(&mut server, &mut people);
    }
    people[0].c.command("kill");
    while !people[0]
        .prints
        .iter()
        .any(|p| p.contains("GAME_CHEATSNOTENABLED"))
    {
        if Instant::now() > deadline {
            return fail("`kill` without cheats was not refused".into());
        }
        frame(&mut server, &mut people);
    }
    if !server
        .game
        .client(a)
        .is_some_and(|c| c.session == server::client::Session::Playing)
    {
        return fail("`kill` without cheats ended the player".into());
    }
    server.game.cvars.force("sv_cheats", "1");
    people[0].c.command("kill");
    while server
        .game
        .client(a)
        .is_some_and(|c| c.session == server::client::Session::Playing)
    {
        if Instant::now() > deadline {
            return fail("`kill` left the player alive".into());
        }
        frame(&mut server, &mut people);
    }

    // A vote to kick a bot by its slot, as the Kick menu sends it. The other person sees the vote's configstrings.
    let Some(bot) = server
        .game
        .connected_clients()
        .find(|(_, c)| c.bot)
        .map(|(n, _)| n)
    else {
        return fail("no bot to vote against".into());
    };
    people[0].c.command(&format!("callvote clientkick {bot}"));
    let shown = |p: &mut Human| {
        p.c.ui()
            .map(|u| {
                (
                    u.config(cs::VOTE_TIME).to_owned(),
                    u.config(cs::VOTE_STRING).to_owned(),
                    u.config(cs::VOTE_YES).to_owned(),
                    u.config(cs::VOTE_NO).to_owned(),
                )
            })
            .unwrap_or_default()
    };
    while shown(&mut people[1]).0.is_empty() {
        if Instant::now() > deadline {
            return fail("the vote never reached the other person's configstrings".into());
        }
        frame(&mut server, &mut people);
    }
    let (time, text, yes, no) = shown(&mut people[1]);
    if !text.starts_with("Kick") || (yes.as_str(), no.as_str()) != ("1", "0") || time.is_empty() {
        return fail(format!("the vote showed {time:?} {text:?} {yes:?} {no:?}"));
    }
    // Both voters say yes: the bot is dropped three seconds after the vote passes.
    people[1].c.command("vote yes");
    while server.game.client(bot).is_some_and(|c| c.connected()) {
        if Instant::now() > deadline {
            return fail("the clientkick vote dropped no one".into());
        }
        frame(&mut server, &mut people);
    }

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

    // A vote with one voter passes alone and changes the gametype and the map. A vote needs two people connected, so a
    // bot joins, and watches (a spectator does not vote).
    if let Err(e) = server.exec_line("bots 1") {
        return fail(format!("bots 1: {e}"));
    }
    // The bot's script picks its team first; only then is it put aside.
    while !server
        .game
        .connected_clients()
        .any(|(_, c)| c.bot && matches!(c.team, Team::Axis | Team::Allies))
    {
        if Instant::now() > deadline {
            return fail("the bot never picked a team".into());
        }
        frame(&mut server, &mut people);
    }
    sideline_bots(&mut server);
    people[0].c.command("callvote typemap sd mp_backlot");
    while server.map_name() != Some("mp_backlot") {
        if Instant::now() > deadline {
            return fail(format!(
                "the typemap vote never ran (map {:?}, voter {a}, prints {:?})",
                server.map_name(),
                people[0].prints.iter().rev().take(3).collect::<Vec<_>>()
            ));
        }
        frame(&mut server, &mut people);
    }
    let gametype = server.game.cvars.string("g_gametype").to_owned();
    if gametype != "sd" {
        return fail(format!("the vote left the gametype at {gametype:?}"));
    }

    // The level changed: the person's slot may be another one now.
    let a = loop {
        if let Some(n) = people[0].own
            && server
                .game
                .client(n)
                .is_some_and(|c| c.connected() && !c.bot)
        {
            break n;
        }
        if Instant::now() > deadline {
            return fail("the person never got a slot in the new level".into());
        }
        frame(&mut server, &mut people);
    };

    // The person renames itself: the server cleans the name, and the others' view (the configstring) follows.
    people[0]
        .c
        .command(&net::client::userinfo_command("^1Zed  Zed", 25_000, 30));
    while server.game.client(a).is_none_or(|c| c.name != "Zed  Zed") {
        if Instant::now() > deadline {
            return fail(format!(
                "the userinfo did not rename the player (it is {:?}, {} s into the stage)",
                server.game.client(a).map(|c| c.name.clone()),
                (Instant::now() + LIMIT - deadline).as_secs()
            ));
        }
        frame(&mut server, &mut people);
    }

    // A `getstatus` from a bare socket lists the players and the server settings.
    let probe = std::net::UdpSocket::bind("127.0.0.1:0")?;
    probe.set_nonblocking(true)?;
    probe.send_to(&net::Oob::GetStatus(77).encode(), addr)?;
    let mut status = None;
    while status.is_none() {
        if Instant::now() > deadline {
            return fail("getstatus was not answered".into());
        }
        frame(&mut server, &mut people);
        let mut buf = [0u8; 4096];
        if let Ok(n) = probe.recv(&mut buf) {
            status = net::Oob::parse(&buf[..n]);
        }
    }
    match status {
        Some(net::Oob::StatusResponse { info, players }) => {
            let has = |k: &str, v: &str| info.iter().any(|(n, x)| n == k && x == v);
            if !has("mapname", "mp_backlot") || !has("challenge", "77") {
                return fail(format!("the status carried {info:?}"));
            }
            if !players.iter().any(|p| p.name == "Zed  Zed") {
                return fail(format!("the status listed {players:?}"));
            }
        }
        other => return fail(format!("getstatus was answered with {other:?}")),
    }

    // A gametype vote with one voter: `g_gametype war; map_restart` starts the level again as war.
    while !server
        .game
        .connected_clients()
        .any(|(_, c)| c.bot && matches!(c.team, Team::Axis | Team::Allies))
    {
        if Instant::now() > deadline {
            return fail("the bot never picked a team after the map change".into());
        }
        frame(&mut server, &mut people);
    }
    sideline_bots(&mut server);
    if let Some(c) = server.game.client_mut(a) {
        c.team = Team::Allies;
    }
    let loads = server.level_loads;
    people[0].c.command("callvote g_gametype war");
    while server.level_loads == loads {
        if Instant::now() > deadline {
            return fail("the g_gametype vote never restarted the level".into());
        }
        frame(&mut server, &mut people);
    }
    let gametype = server.game.cvars.string("g_gametype").to_owned();
    if gametype != "war" || server.map_name() != Some("mp_backlot") {
        return fail(format!(
            "the g_gametype vote left {gametype:?} on {:?}",
            server.map_name()
        ));
    }

    // The game log: the start of each level, what was said, the script's join lines, the end of the first.
    drop(server);
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_file(&log);
    for want in ["InitGame: ", "say;", "sayteam;", "J;", "ShutdownGame:"] {
        if !text.contains(want) {
            return fail(format!("games_mp.log has no {want:?} line:\n{text}"));
        }
    }
    let mut report = StageReport::new(NAME, Status::Passed);
    report
        .notes
        .push("say and say_team reached the right people; where and kill were carried out; the clientkick vote dropped a bot by slot; the kick vote dropped the target; the typemap vote moved to mp_backlot, sd; a userinfo renamed a player; getstatus listed it; the g_gametype vote restarted the level as war; games_mp.log has the game, chat and join lines".into());
    Ok(report)
}
