// SPDX-License-Identifier: GPL-3.0-only
//! `net-rcon`: the server is administered over UDP. A plain socket sends `rcon` packets: a wrong
//! password is refused and changes nothing, the right one runs `status`, `set`, `say` and the kick
//! and ban commands, with the console output coming back as `print` packets. A person kicked is told so and
//! refused on reconnecting until `unbanUser`; `banClient` is written to the ban file.
use super::net_objective::Human;
use crate::stage::{StageCtx, StageReport, Status};
use net::oob::{Oob, PRINT_CHUNK};
use net::{Transport, UdpTransport};
use server::client::Session;
use server::server::Server;
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const NAME: &str = "net-rcon";
const LIMIT: Duration = Duration::from_secs(120);
/// A little over the server's one-request-per-500-ms rule.
const SPACING: Duration = Duration::from_millis(600);

struct Admin {
    t: UdpTransport,
    server: SocketAddr,
    last: Instant,
}

impl Admin {
    /// Sends one `rcon` packet (waiting out the spacing first) and returns the text of the
    /// `print` packets that answer; `None` when nothing came back.
    fn rcon(
        &mut self,
        s: &mut Server,
        people: &mut [Human],
        password: &str,
        cmd: &str,
    ) -> Option<String> {
        while self.last.elapsed() < SPACING {
            step(s, people);
        }
        self.send(password, cmd);
        self.last = Instant::now();
        self.collect(s, people)
    }

    fn send(&mut self, password: &str, cmd: &str) {
        let p = Oob::Rcon {
            password: password.into(),
            command: cmd.into(),
        };
        self.t.send_to(self.server, &p.encode());
    }

    fn collect(&mut self, s: &mut Server, people: &mut [Human]) -> Option<String> {
        let mut got: Option<String> = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut quiet = 0;
        let mut buf = vec![0u8; 4096];
        while Instant::now() < deadline && quiet < 4 {
            step(s, people);
            let mut any = false;
            while let Ok(Some((n, _))) = self.t.recv_from(&mut buf, Some(Duration::ZERO)) {
                if let Some(Oob::Print(t)) = Oob::parse(&buf[..n]) {
                    assert!(t.len() <= PRINT_CHUNK);
                    got.get_or_insert_default().push_str(&t);
                    any = true;
                }
            }
            // Once an answer has started, a few quiet frames mean it is complete.
            quiet = if got.is_some() && !any { quiet + 1 } else { 0 };
        }
        got
    }
}

fn step(s: &mut Server, people: &mut [Human]) {
    s.run_for(Duration::from_millis(16));
    for p in people {
        p.step();
    }
}

fn playing(s: &Server, h: &Human) -> bool {
    h.own
        .and_then(|n| s.game.client(n))
        .is_some_and(|c| c.session == Session::Playing)
}

/// A password made up for this run.
fn password() -> String {
    format!("rc{:x}x", std::process::id().wrapping_mul(2_654_435_761))
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let r = run_in(ctx);
    let _ = std::fs::remove_dir_all(
        std::env::temp_dir().join(format!("cod4e-net-rcon-{}", std::process::id())),
    );
    r
}

fn run_in(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(install) = ctx.install.as_deref() else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("install not found"));
    };
    let password = password();
    let dir = std::env::temp_dir().join(format!("cod4e-net-rcon-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let ban_file = dir.join("ban.txt");
    let args: Vec<String> = [
        "+set".into(),
        "net_port".into(),
        "0".into(),
        "+set".into(),
        "rcon_password".into(),
        password.clone(),
        "+set".into(),
        "sv_banFile".into(),
        ban_file.display().to_string(),
    ]
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
    let fail = |why: String| Ok(StageReport::new(NAME, Status::Failed).with_reason(why));
    let mut admin = Admin {
        t: UdpTransport::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?,
        server: addr,
        last: Instant::now(),
    };
    let mut none: [Human; 0] = [];

    // A wrong password is refused and the command does not run.
    let r = admin.rcon(&mut server, &mut none, "wrong", "set rcon_probe 1");
    if !r.as_deref().is_some_and(|t| t.contains("Invalid password"))
        || server.game.cvars.exists("rcon_probe")
    {
        return fail(format!(
            "a wrong rcon password was not refused: {r:?}, probe {}",
            server.game.cvars.exists("rcon_probe")
        ));
    }
    if !server
        .log
        .iter()
        .any(|l| l.starts_with("Bad Rcon from 127.0.0.1:"))
    {
        return fail("the bad rcon attempt was not logged".into());
    }

    // The right one runs commands and the output comes back.
    let r = admin.rcon(
        &mut server,
        &mut none,
        &password,
        "set rcon_probe 7; say hello",
    );
    if server.game.cvars.string("rcon_probe") != "7"
        || !r.as_deref().is_some_and(|t| t.contains("console: hello"))
    {
        return fail(format!(
            "rcon set/say: probe {:?}, reply {r:?}",
            server.game.cvars.string("rcon_probe")
        ));
    }
    let r = admin
        .rcon(&mut server, &mut none, &password, "status")
        .unwrap_or_default();
    if !r.contains("map: mp_crash") || !r.contains("num score ping guid name") || !r.contains("BOT")
    {
        return fail(format!("rcon status reply: {r:?}"));
    }
    // More output than one packet holds arrives whole.
    let r = admin
        .rcon(&mut server, &mut none, &password, "cvarlist")
        .unwrap_or_default();
    if r.len() <= PRINT_CHUNK || !r.contains("g_gametype") || r.contains(&password) {
        return fail(format!(
            "rcon cvarlist: {} bytes, password leaked: {}",
            r.len(),
            r.contains(&password)
        ));
    }

    // Two requests inside the interval: the second is dropped without an answer.
    admin.rcon(&mut server, &mut none, &password, "set rcon_rate 1");
    admin.send(&password, "set rcon_rate 2");
    let again = admin.collect(&mut server, &mut none);
    if server.game.cvars.string("rcon_rate") != "1" || again.is_some() {
        return fail(format!(
            "rcon was not rate limited: rate {:?}, reply {again:?}",
            server.game.cvars.string("rcon_rate")
        ));
    }

    // A person on the server: `status` shows the address, `kick` bans briefly, `unbanUser` lifts it.
    let mut people = [Human::new(addr, 1)?];
    let deadline = Instant::now() + LIMIT;
    while !playing(&server, &people[0]) {
        if Instant::now() > deadline {
            return fail("the client never got a body".into());
        }
        step(&mut server, &mut people);
    }
    let slot = people[0].own.unwrap_or_default();
    let r = admin
        .rcon(&mut server, &mut people, &password, "status")
        .unwrap_or_default();
    let row = r
        .lines()
        .find(|l| l.contains("human1"))
        .unwrap_or_default()
        .to_owned();
    let fields: Vec<&str> = row.split_whitespace().collect();
    // num score ping guid name lastmsg address qport rate
    if fields.len() != 9
        || !fields[6].starts_with("127.0.0.1:")
        || fields[2].parse::<u32>().is_err() && fields[2] != "CNCT"
    {
        return fail(format!("status row of the person: {row:?}"));
    }
    admin.rcon(&mut server, &mut people, &password, "kick human1");
    let dropped = Instant::now() + Duration::from_secs(30);
    while server.game.client(slot).is_some_and(|c| c.connected()) {
        if Instant::now() > dropped {
            return fail("kick did not remove the person".into());
        }
        step(&mut server, &mut people);
    }
    // The person is told why, not left in a match that no longer has them.
    while people[0].c.dropped() != Some("Player kicked") {
        if Instant::now() > dropped {
            return fail(format!(
                "the kicked person was told {:?}",
                people[0].c.dropped()
            ));
        }
        step(&mut server, &mut people);
    }
    let mut again = [Human::new(addr, 2)?];
    while again[0].c.refused().is_none() {
        if Instant::now() > dropped + Duration::from_secs(30) {
            return fail("a kicked address was allowed back in".into());
        }
        step(&mut server, &mut again);
    }
    admin.rcon(&mut server, &mut again, &password, "unbanUser 127.0.0.1");
    let mut back = [Human::new(addr, 3)?];
    let deadline = Instant::now() + LIMIT;
    while !playing(&server, &back[0]) {
        if Instant::now() > deadline {
            return fail("the address was not let back in after unbanUser".into());
        }
        step(&mut server, &mut back);
    }

    // A server that goes quiet ends the client's connection with a timeout message, not a frozen world.
    back[0].c.set_timeout(Duration::from_secs(1));
    let limit = Instant::now() + Duration::from_secs(30);
    while back[0].c.dropped().is_none() {
        if Instant::now() > limit {
            return fail("a silent server did not end the client's connection".into());
        }
        back[0].step();
    }
    if back[0].c.dropped() != Some(net::client::TIMED_OUT) {
        return fail(format!("silent server: {:?}", back[0].c.dropped()));
    }
    let mut back = [Human::new(addr, 4)?];
    let deadline = Instant::now() + LIMIT;
    while !playing(&server, &back[0]) {
        if Instant::now() > deadline {
            return fail("the client could not rejoin after the timeout".into());
        }
        step(&mut server, &mut back);
    }

    // `banClient` is for good: it reaches the ban file, which a restart reads.
    let slot = back[0].own.unwrap_or_default();
    admin.rcon(
        &mut server,
        &mut back,
        &password,
        &format!("banClient {slot}"),
    );
    let kept = std::fs::read_to_string(&ban_file).unwrap_or_default();
    if !kept.lines().any(|l| l == "127.0.0.1") {
        return fail(format!("banClient left the ban file at {kept:?}"));
    }
    let mut report = StageReport::new(NAME, Status::Passed);
    report.notes.push(
        "rcon: wrong password refused and logged; status/set/say/cvarlist answered in print packets; rate limited; kick told the person and banned the address until unbanUser; a silent server times the client out; banClient written to the ban file".into(),
    );
    Ok(report)
}
