// SPDX-License-Identifier: GPL-3.0-only
//! `net-objective`: two people (real UDP clients, no window) play Search and Destroy on mp_crash.
//! One picks up the bomb and holds +activate in the bomb zone, which has to show the zone's hint in
//! the client's own player state and end with the bomb planted; the other holds +activate at the
//! bomb until it is defused. The stage moves each player next to the trigger (`devtele`) so the
//! walk does not decide the result; the use button, the hint and the scripts are the real path.
use crate::stage::{StageCtx, StageReport, Status};
use net::UdpTransport;
use net::client::NetClient;
use net::entity::etype;
use net::ui::{AutoJoin, UiEvent, cs};
use server::client::{Session, Team};
use server::server::Server;
use sim::pm::{ANGLE_UNIT, UserCmd, button};
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const NAME: &str = "net-objective";
/// Waiting for a round to start and a bomb to be planted depends on the scripts' timers.
const LIMIT: Duration = Duration::from_secs(120);

pub(super) struct Human {
    pub(super) c: NetClient<UdpTransport>,
    join: AutoJoin,
    pub(super) own: Option<u16>,
    cmd_time: i32,
    pub(super) hold_use: bool,
    /// The hint the server's player state showed at the zone, and its text.
    hint: Option<(u16, String)>,
    /// Chat lines and console prints the server sent, oldest first.
    pub(super) chats: Vec<(bool, u16, String)>,
    pub(super) prints: Vec<String>,
}

impl Human {
    pub(super) fn new(addr: SocketAddr, id: u16) -> io::Result<Self> {
        Self::with_password(addr, id, "")
    }

    /// A person who gives `password` at connect.
    pub(super) fn with_password(addr: SocketAddr, id: u16, password: &str) -> io::Result<Self> {
        let t = UdpTransport::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
        Ok(Self {
            c: NetClient::new(t, addr, &format!("human{id}"), password, 6000 + id),
            join: AutoJoin::default(),
            own: None,
            cmd_time: 0,
            hold_use: false,
            hint: None,
            chats: Vec::new(),
            prints: Vec::new(),
        })
    }

    /// One client frame: take the packets, answer the join menus, send a command.
    pub(super) fn step(&mut self) {
        self.c.pump(Duration::from_millis(1));
        if let Some(ui) = self.c.ui() {
            let events = ui.drain_events();
            for ev in &events {
                match ev {
                    UiEvent::Chat {
                        team, client, text, ..
                    } => self.chats.push((*team, *client, text.clone())),
                    UiEvent::Print { text, .. } => self.prints.push(text.clone()),
                    _ => {}
                }
                if let Some(a) = self.join.step(ev) {
                    self.c.command(&a);
                }
            }
        }
        let Some(s) = self.c.latest() else { return };
        let n = s.ps.client_num;
        if s.entity(n).is_some_and(|e| e.etype == etype::PLAYER) {
            self.own = Some(n);
        }
        let (hint_ent, hint_str) = (s.ps.cursor_hint_ent_index, s.ps.cursor_hint_string);
        if self.hold_use && hint_ent != 1023 && hint_str >= 0 && self.hint.is_none() {
            let text = self
                .c
                .ui()
                .map(|u| u.config(cs::USE_TRIG_STRINGS + hint_str as u16).to_owned())
                .unwrap_or_default();
            self.hint = Some((hint_ent, text));
        }
        let now = self.c.now_ms();
        let Some(st) = self.c.snaps.server_time(now) else {
            return;
        };
        self.cmd_time = (self.cmd_time + 1).max(st);
        let yaw = self.c.latest().map_or(0.0, |s| s.ps.viewangles[1]);
        let delta = self.c.latest().map_or(0.0, |s| s.ps.delta_angles[1]);
        let cmd = UserCmd {
            server_time: self.cmd_time,
            buttons: if self.hold_use { button::USE } else { 0 },
            angles: [0, (((yaw - delta) / ANGLE_UNIT) as i32) & 0xffff, 0],
            ..UserCmd::default()
        };
        self.c.send_cmd(cmd);
    }
}

/// Puts player `n` at `at` unless it is already within reach of it, so the player is not left
/// falling each time.
fn place(server: &mut Server, n: u16, at: [f32; 3]) {
    let near = server.game.client(n).is_some_and(|c| {
        let o = c.ps.origin;
        ((o[0] - at[0]).powi(2) + (o[1] - at[1]).powi(2)).sqrt() < 24.0
    });
    if !near {
        server.game.teleport(n, at);
    }
}

/// What a player is doing, for a failure message.
fn describe(server: &Server, p: &Human) -> String {
    let Some(c) = p.own.and_then(|n| server.game.client(n)) else {
        return "no body".into();
    };
    format!(
        "origin {:?} buttons {} hint ent {} hint {} weapon {} pm_type {:?} flags {:x} ground {} cmd_time {} command_time {} level {} last_cmd {}",
        c.ps.origin,
        c.buttons,
        c.ps.cursor_hint_ent_index,
        c.ps.cursor_hint,
        c.ps.weapon,
        c.ps.pm_type,
        c.ps.pm_flags,
        c.ps.ground_entity_num,
        c.cmd.server_time,
        c.ps.command_time,
        server.game.level.time,
        c.last_cmd_time
    )
}

fn find(server: &Server, class: &str, name: Option<&str>, team: Option<Team>) -> Option<u16> {
    server.game.ents.iter().enumerate().find_map(|(i, e)| {
        let e = e.as_ref()?;
        (&*e.classname == class
            && name.is_none_or(|n| e.targetname.as_deref().is_some_and(|t| t.starts_with(n)))
            && team.is_none_or(|t| e.x.trigger_team == t)
            && e.origin[2] > -1000.0
            && e.origin[2] < 5000.0)
            .then_some(i as u16)
    })
}

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
        "set g_gametype sd",
        "set scr_sd_timelimit 0",
        "set scr_sd_scorelimit 0",
        "set scr_sd_roundlimit 0",
        "map mp_crash",
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

    // Both people on a team and playing.
    let playing = |server: &Server, people: &[Human; 2]| {
        people.iter().all(|p| {
            p.own.and_then(|n| server.game.client(n)).is_some_and(|c| {
                c.session == Session::Playing && matches!(c.team, Team::Axis | Team::Allies)
            })
        })
    };
    while !playing(&server, &people) {
        if Instant::now() > deadline {
            return fail("the two clients never got a body".into());
        }
        frame(&mut server, &mut people);
    }
    // The attackers are the team the bomb zones were switched on for.
    let zone = loop {
        if Instant::now() > deadline {
            return fail("no bomb zone was switched on for a team".into());
        }
        frame(&mut server, &mut people);
        let z = server.game.ents.iter().enumerate().find_map(|(i, e)| {
            let e = e.as_ref()?;
            (&*e.classname == "trigger_use_touch"
                && e.targetname
                    .as_deref()
                    .is_some_and(|t| t.starts_with("bombzone"))
                && e.x.trigger_team != Team::Free
                && e.origin[2] > -1000.0)
                .then_some((i as u16, e.x.trigger_team))
        });
        if let Some(z) = z {
            break z;
        }
    };
    let (zone_ent, attackers) = zone;
    let team_of =
        |server: &Server, p: &Human| p.own.and_then(|n| server.game.client(n)).map(|c| c.team);
    let Some(planter) = people
        .iter()
        .position(|p| team_of(&server, p) == Some(attackers))
    else {
        return fail("neither person is on the attacking team".into());
    };
    let defender = 1 - planter;
    let (pn, dn) = (
        people[planter].own.unwrap_or_default(),
        people[defender].own.unwrap_or_default(),
    );

    // The planter takes the bomb: standing in the pickup trigger.
    let Some(pickup) = find(&server, "trigger_multiple", Some("sd_bomb_pickup"), None) else {
        return fail("the bomb's pickup trigger is missing".into());
    };
    let Some(at) = server.game.floor_in(pickup) else {
        return fail("the pickup trigger is not linked".into());
    };
    let mut carried = false;
    while !carried {
        if Instant::now() > deadline {
            return fail("the planter never picked the bomb up".into());
        }
        place(&mut server, pn, at);
        for _ in 0..30 {
            frame(&mut server, &mut people);
        }
        carried = server
            .game
            .ent(pickup)
            .is_some_and(|e| e.origin[2] > 5000.0);
    }

    // A script's `player.headicon = ...` on the carrier (the stock S&D scripts set none at pickup, so the stage does
    // what a script would): it has to reach the carrier's own client as a material of the server's table.
    const ICON: &str = "waypoint_bomb";
    let team = server.game.client(pn).map(|c| c.team.name().to_owned());
    let Some(team) = team else {
        return fail("the carrier vanished".into());
    };
    if let Err(e) = server.game.precache(server::ui::Table::Material, ICON) {
        return fail(e);
    }
    if let Some(c) = server.game.client_mut(pn) {
        c.head_icon = ICON.into();
        c.head_icon_team = team;
    }
    let icon = loop {
        if Instant::now() > deadline {
            return fail("the bomb carrier's head icon never reached its client".into());
        }
        frame(&mut server, &mut people);
        let seen = people[planter].c.latest().and_then(|s| {
            let e = s.entity(pn)?;
            let ui = people[planter].c.ui_ref()?;
            (e.head_icon != 0).then(|| ui.material(e.head_icon).to_owned())
        });
        if let Some(name) = seen.filter(|n| !n.is_empty()) {
            break name;
        }
    };
    if icon != ICON {
        return fail(format!("the client saw head icon {icon:?}, not {ICON:?}"));
    }

    // Holding +activate in the zone until the bomb is planted.
    let Some(at) = server.game.floor_in(zone_ent) else {
        return fail("the bomb zone is not linked".into());
    };
    people[planter].hold_use = true;
    while server.game.stats.plants == 0 {
        if Instant::now() > deadline {
            return fail(format!(
                "the bomb was never planted; planter: {}",
                describe(&server, &people[planter])
            ));
        }
        place(&mut server, pn, at);
        for _ in 0..20 {
            frame(&mut server, &mut people);
        }
    }
    people[planter].hold_use = false;
    let Some((hint_ent, text)) = people[planter].hint.clone() else {
        return fail("the planter's player state never showed the zone's hint".into());
    };
    if hint_ent != zone_ent || text.is_empty() {
        return fail(format!(
            "the hint named entity {hint_ent} with text {text:?}, not zone {zone_ent}"
        ));
    }

    // The other person holds +activate at the bomb.
    let defenders = server.game.client(dn).map(|c| c.team);
    let Some(defuse) = defenders.and_then(|t| find(&server, "trigger_use_touch", None, Some(t)))
    else {
        return fail("no defuse trigger was switched on for the defenders".into());
    };
    let Some(at) = server.game.floor_in(defuse) else {
        return fail("the defuse trigger is not linked".into());
    };
    people[defender].hold_use = true;
    while server.game.stats.defuses == 0 {
        if Instant::now() > deadline {
            return fail(format!(
                "the bomb was never defused; defender: {}",
                describe(&server, &people[defender])
            ));
        }
        place(&mut server, dn, at);
        for _ in 0..20 {
            frame(&mut server, &mut people);
        }
    }
    for p in &mut people {
        p.c.disconnect();
    }
    let st = server.game.stats;
    let mut report = StageReport::new(NAME, Status::Passed);
    report.metrics.insert("plants".into(), st.plants as f64);
    report.metrics.insert("defuses".into(), st.defuses as f64);
    report.notes.push(format!("hint at the zone: {text:?}"));
    report.notes.push(format!("carrier head icon: {icon:?}"));
    if !server.script_errors.is_empty() {
        report.status = Status::Failed;
        report.reason = Some(format!("{:?}", server.script_errors));
    }
    Ok(report)
}
