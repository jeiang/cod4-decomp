// SPDX-License-Identifier: GPL-3.0-only
//! `net-perks`: the perks of a custom class act, the way the stock scripts and the engine apply them. Two real UDP
//! clients (no window) join a headless war match the way a person does: each uploads a profile (`statsync`) whose
//! five custom classes carry the perks under test, picks a team and a custom class in the menus the scripts open,
//! and the stock `_class.gsc` reads the class off the stats at the first spawn. The stage then reads the server's
//! player state for the measurable effects:
//!
//! - the perk bits of the player state are exactly the class's (`register_perks` → `setPerk`; the grenade kit perks
//!   are counts, never bits);
//! - Frag x3 gives three frags and Special Grenades x3 three flashbangs, Bandolier a full stock;
//! - Juggernaut takes a 40 rifle hit down to 30, Stopping Power raises it to 56, Sonic Boom raises a 20 grenade
//!   splash to 25 (`cac_modified_damage`);
//! - Last Stand: a lethal rifle hit leaves the player at 1 health with only the pistol and no frag, crawling
//!   (`PM_LASTSTAND`), with the health held down by `lastStandKeepOverlay`, and the bleed-out kills them ten seconds later with the kill credited to
//!   the attacker (`Callback_PlayerLastStand`, `lastStandTimer`, `LastStandBleedOut`); a grenade kill is no Last
//!   Stand at all (`mayDoLastStand`);
//! - Martyrdom drops a live short-fuse frag where the player died (`DeathGrenadeDrop`).
//!
//! The hits come from the `devkill` console hook, which runs `G_Damage` and the scripts' damage callback like a bullet
//! or a blast that landed; the rest is the player's path.
use crate::stage::{StageCtx, StageReport, Status};
use net::UdpTransport;
use net::client::NetClient;
use net::entity::etype;
use net::ui::{UiEvent, menu_response};
use server::client::{Session, Team};
use server::server::Server;
use sim::pm::{PmType, UserCmd};
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const NAME: &str = "net-perks";
/// Waiting for the scripts (spawns, the ten second bleed-out) depends on the game, not the runner.
const LIMIT: Duration = Duration::from_secs(300);
const FRAME: Duration = Duration::from_millis(16);
/// Frames the stage gives the scripts to answer one event (a hit's callback, a class change).
const SETTLE: usize = 20;
/// Frames a weapon switch or a spawn may take to show in the player state.
const SWITCH: usize = 200;
/// `lastStandTimer( 10 )`: how long a player in Last Stand lives before `LastStandBleedOut`.
const BLEED_OUT_MS: i32 = 10_000;
/// The frames of a Last Stand life the stage reads the health in, past the bleed-out.
const LAST_STAND_FRAMES: usize = 1000;

/// The perk slots of a class, as the stock `level.allowedPerks` lays them out: slot 0 is the kit (grenades, ammo),
/// slot 1 the weapon perk, slot 2 the ability.
#[derive(Clone, Copy)]
struct Class {
    perks: [&'static str; 3],
}

/// No perk in a slot (`statsTable.csv` row 190).
const NONE: &str = "specialty_null";

/// The victim's classes: 1 for the kit and Last Stand, 2 for Bandolier, 3 for Special Grenades and Martyrdom.
const VICTIM: [Class; 3] = [
    Class {
        perks: [
            "specialty_fraggrenade",
            "specialty_armorvest",
            "specialty_pistoldeath",
        ],
    },
    Class {
        perks: [
            "specialty_extraammo",
            "specialty_fastreload",
            "specialty_longersprint",
        ],
    },
    Class {
        perks: [
            "specialty_specialgrenade",
            "specialty_rof",
            "specialty_grenadepulldeath",
        ],
    },
];
/// The attacker's classes: 1 plain, 2 Stopping Power, 3 Sonic Boom.
const ATTACKER: [Class; 3] = [
    Class { perks: [NONE; 3] },
    Class {
        perks: [NONE, "specialty_bulletdamage", NONE],
    },
    Class {
        perks: [NONE, "specialty_explosivedamage", NONE],
    },
];

/// `bg_perkNames`: the bit of a perk in `playerState_t.perks`.
fn perk_bit(name: &str) -> Option<u32> {
    server::script::PERK_NAMES
        .iter()
        .position(|p| *p == name)
        .map(|i| 1 << i)
}

/// The bits `register_perks` sets for a class: the grenade kit perks (Frag x3, Special Grenades x3) never reach
/// `setPerk` in the stock `get_specialtydata`, which keeps them as counts only.
fn perk_bits(c: &Class) -> u32 {
    c.perks
        .iter()
        .filter(|p| !matches!(**p, "specialty_fraggrenade" | "specialty_specialgrenade"))
        .filter_map(|p| perk_bit(p))
        .fold(0, |a, b| a | b)
}

/// `mp/statsTable.csv`: the row (column 0) whose `column` holds `key`, as `tableLookup` finds it.
fn stat_row(server: &Server, column: usize, key: &str) -> Option<i32> {
    let t = server.game.content.string_table("mp/statstable.csv")?;
    let cols = t.column_count as usize;
    let cell = |r: usize, c: usize| t.values.get(r * cols + c).and_then(|n| n.as_deref());
    (0..t.row_count as usize)
        .find(|&r| cell(r, column).is_some_and(|s| s.eq_ignore_ascii_case(key)))
        .and_then(|r| cell(r, 0))
        .and_then(|s| s.parse().ok())
}

/// The `statsync` commands of a profile whose custom classes 1.. are `classes` (the rest repeat the last one): a P90
/// with the M9 as sidearm, a flashbang, no attachment, no camouflage. `200 + 10 * i` is the class's "set up" flag the
/// stock scripts check, the rest follow `cac_getdata`.
fn profile(server: &Server, classes: &[Class]) -> Result<Vec<String>, String> {
    let row = |col, key: &str| stat_row(server, col, key).ok_or(format!("statsTable has no {key}"));
    let (p90, m9, flash) = (row(4, "p90")?, row(4, "beretta")?, row(4, "flash_grenade")?);
    let mut commands = Vec::new();
    for i in 0..5 {
        let class = classes[i.min(classes.len() - 1)];
        let base = 200 + 10 * i as i32;
        let mut pairs = vec![(base, 1), (base + 1, p90), (base + 3, m9)];
        for (slot, perk) in class.perks.iter().enumerate() {
            let r = if *perk == NONE { 190 } else { row(6, perk)? };
            pairs.push((base + 5 + slot as i32, r));
        }
        pairs.push((base + 8, flash));
        // One command per class, as the client's upload chunks them.
        let mut s = String::from("statsync");
        for (i, v) in pairs {
            s.push_str(&format!(" {i} {v}"));
        }
        commands.push(s);
    }
    Ok(commands)
}

/// A client that answers the join menus with a fixed team and class and keeps sending usercmds so its body lives.
struct Player {
    c: NetClient<UdpTransport>,
    team: &'static str,
    /// The class the next class menu is answered with (`custom1`..`custom5`).
    class: String,
    own: Option<u16>,
    cmd_time: i32,
}

impl Player {
    fn new(
        addr: SocketAddr,
        name: &str,
        qport: u16,
        team: &'static str,
        profile: Vec<String>,
    ) -> io::Result<Self> {
        let t = UdpTransport::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
        let mut c = NetClient::new(t, addr, name, "", qport);
        c.set_profile(profile);
        Ok(Self {
            c,
            team,
            class: "custom1".into(),
            own: None,
            cmd_time: 0,
        })
    }

    fn step(&mut self) {
        self.c.pump(Duration::from_millis(1));
        let mut answers = Vec::new();
        if let Some(ui) = self.c.ui() {
            for ev in ui.drain_events() {
                if let UiEvent::OpenMenu { name, .. } = ev {
                    if name.starts_with("team_") {
                        answers.push(menu_response(&name, self.team));
                    } else if name.starts_with("changeclass") {
                        answers.push(menu_response("changeclass", &self.class));
                    }
                }
            }
        }
        for a in answers {
            self.c.command(&a);
        }
        let Some(s) = self.c.latest() else { return };
        let n = s.ps.client_num;
        if s.entity(n).is_some_and(|e| e.etype == etype::PLAYER) {
            self.own = Some(n);
        }
        let now = self.c.now_ms();
        let Some(st) = self.c.snaps.server_time(now) else {
            return;
        };
        self.cmd_time = (self.cmd_time + 1).max(st);
        self.c.send_cmd(UserCmd {
            server_time: self.cmd_time,
            ..UserCmd::default()
        });
    }

    /// Picks `class` for the next spawn, as a click in the class menu does.
    fn choose(&mut self, class: &str) {
        self.class = class.to_owned();
        self.c.command(&menu_response("changeclass", class));
    }
}

/// What the stage reads of a player on the server.
#[derive(Clone, Debug)]
struct Body {
    health: i32,
    perks: u32,
    pm_type: PmType,
    last_stand: bool,
    weapon: String,
    weapons: Vec<String>,
    frag: i32,
    flash: i32,
    p90_stock: i32,
    spawn_count: u16,
    deaths: i32,
    kills: i32,
}

impl Body {
    fn dead(&self) -> bool {
        self.pm_type >= PmType::Dead
    }
}

fn body(server: &Server, n: u16) -> io::Result<Body> {
    let c = server
        .game
        .client(n)
        .ok_or_else(|| io::Error::other(format!("client {n} is gone")))?;
    let t = &server.game.weapons;
    let clip = |name: &str| c.inv.get_clip(t, t.index(name));
    Ok(Body {
        health: server.game.ent(n).map_or(0, |e| e.health),
        perks: c.ps.perks,
        pm_type: c.ps.pm_type,
        last_stand: c.last_stand,
        weapon: t.name(c.ps.weapon as u16).to_owned(),
        weapons: c.inv.list(t).map(|w| t.name(w).to_owned()).collect(),
        frag: clip("frag_grenade_mp"),
        flash: clip("flash_grenade_mp"),
        p90_stock: c.inv.get_stock(t, t.index("p90_mp")),
        spawn_count: c.ps.spawn_count,
        deaths: c.deaths,
        kills: c.kills,
    })
}

fn playing(server: &Server, p: &Player) -> bool {
    p.own.and_then(|n| server.game.client(n)).is_some_and(|c| {
        c.session == Session::Playing
            && matches!(c.team, Team::Axis | Team::Allies)
            && c.ps.pm_type == PmType::Normal
    })
}

/// A player's live grenades on the server, by owner.
fn grenades_of(server: &Server, n: u16) -> usize {
    server
        .game
        .in_use()
        .filter(|(_, e)| e.missile.is_some() && e.owner == Some(n))
        .count()
}

/// The match, and the two players in it.
struct Match {
    server: Server,
    v: Player,
    a: Player,
    deadline: Instant,
}

impl Match {
    fn frame(&mut self) {
        self.server.run_for(FRAME);
        self.v.step();
        self.a.step();
    }

    fn settle(&mut self, frames: usize) {
        for _ in 0..frames {
            self.frame();
        }
    }

    /// Runs until `done`, for at most `frames`; whether it came.
    fn wait_for(&mut self, frames: usize, mut done: impl FnMut(&Server) -> bool) -> bool {
        for _ in 0..frames {
            if done(&self.server) {
                return true;
            }
            self.frame();
        }
        done(&self.server)
    }

    /// Runs until the player's spawn count passed `spawn` and it plays again: a respawn with its chosen class.
    fn respawn(&mut self, n: u16, spawn: u16, victim: bool) -> io::Result<()> {
        loop {
            let b = body(&self.server, n)?;
            let p = if victim { &self.v } else { &self.a };
            if b.spawn_count != spawn && playing(&self.server, p) {
                self.settle(SETTLE);
                return Ok(());
            }
            if Instant::now() > self.deadline {
                return Err(io::Error::other(format!(
                    "client {n} did not respawn (spawn count {}, {:?})",
                    b.spawn_count, b.pm_type
                )));
            }
            self.frame();
        }
    }

    /// `devkill`: `attacker`'s `weapon` hits `victim` for `damage` of `mean`.
    fn hit(
        &mut self,
        victim: u16,
        attacker: u16,
        damage: i32,
        mean: &str,
        weapon: &str,
    ) -> io::Result<()> {
        self.server
            .exec_line(&format!(
                "devkill {victim} {attacker} {damage} {mean} {weapon}"
            ))
            .map_err(|e| io::Error::other(format!("devkill: {e}")))
    }
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
    let fail = |why: String| Ok(StageReport::new(NAME, Status::Failed).with_reason(why));
    for line in [
        "set g_gametype war",
        "set scr_war_timelimit 0",
        "set scr_war_scorelimit 0",
        // Class changes apply at the next spawn, never at once: no grace period.
        "set scr_war_graceperiod 0",
        "set sv_mapRotation \"gametype war map mp_crash\"",
        "map mp_crash",
    ] {
        if let Err(e) = server.exec_line(line) {
            return fail(format!("{line}: {e}"));
        }
    }
    let (victim_profile, attacker_profile) =
        match (profile(&server, &VICTIM), profile(&server, &ATTACKER)) {
            (Ok(v), Ok(a)) => (v, a),
            (Err(e), _) | (_, Err(e)) => return fail(e),
        };
    let v = Player::new(addr, "perkvictim", 7101, "allies", victim_profile)?;
    let a = Player::new(addr, "perkshooter", 7102, "axis", attacker_profile)?;
    let mut m = Match {
        server,
        v,
        a,
        deadline: Instant::now() + LIMIT,
    };
    while !(playing(&m.server, &m.v) && playing(&m.server, &m.a)) {
        if Instant::now() > m.deadline {
            return fail(format!(
                "the clients never got bodies (victim {}, attacker {})",
                playing(&m.server, &m.v),
                playing(&m.server, &m.a)
            ));
        }
        m.frame();
    }
    let (Some(vn), Some(an)) = (m.v.own, m.a.own) else {
        return fail("a client has no body".into());
    };
    m.settle(SETTLE);
    let mut why: Vec<String> = Vec::new();
    let mut report = StageReport::new(NAME, Status::Passed);
    let mut check = |ok: bool, what: String| {
        if !ok {
            why.push(what);
        }
    };

    // Class 1: the perk bits the scripts registered, three frags, one flashbang, the P90's start ammo.
    let vb = body(&m.server, vn)?;
    let ab = body(&m.server, an)?;
    check(
        vb.perks == perk_bits(&VICTIM[0]),
        format!(
            "victim class 1 perks {:#x}, want {:#x} (Frag x3, Juggernaut, Last Stand)",
            vb.perks,
            perk_bits(&VICTIM[0])
        ),
    );
    check(
        ab.perks == 0,
        format!("attacker class 1 perks {:#x}, want none", ab.perks),
    );
    check(
        vb.frag == 3,
        format!("Frag x3 gave {} frags, want 3", vb.frag),
    );
    check(
        vb.flash == 1,
        format!("{} flashbangs without the perk, want 1", vb.flash),
    );
    check(
        vb.p90_stock == 100,
        format!(
            "P90 stock {} without Bandolier, want 100 (150 start ammo less the 50 in the magazine)",
            vb.p90_stock
        ),
    );
    check(
        vb.weapons.iter().any(|w| w == "p90_mp") && vb.weapons.iter().any(|w| w == "beretta_mp"),
        format!(
            "the custom class loadout is {:?}, want a P90 and an M9",
            vb.weapons
        ),
    );
    report
        .metrics
        .insert("class1.frags".into(), f64::from(vb.frag));

    // Juggernaut: a 40 rifle hit does 30.
    m.hit(vn, an, 40, "MOD_RIFLE_BULLET", "ak47_mp")?;
    m.settle(SETTLE);
    let vb = body(&m.server, vn)?;
    check(
        vb.health == 70,
        format!(
            "Juggernaut: health {} after a 40 rifle hit, want 70",
            vb.health
        ),
    );
    report
        .metrics
        .insert("juggernaut.health".into(), f64::from(vb.health));

    // Last Stand: a lethal rifle hit is survived at 1 health with the pistol only and no frag; the body crawls.
    let deaths_before = vb.deaths;
    let kills_before = ab.kills;
    let hit_at = m.server.game.level.time;
    m.hit(vn, an, 1000, "MOD_RIFLE_BULLET", "ak47_mp")?;
    m.wait_for(SWITCH, |s| {
        body(s, vn).is_ok_and(|b| b.weapon == "beretta_mp" || b.dead())
    });
    let vb = body(&m.server, vn)?;
    check(
        vb.last_stand && vb.pm_type == PmType::LastStand,
        format!(
            "Last Stand: lastStand {} and movement {:?} after a lethal rifle hit, want the Last Stand movement",
            vb.last_stand, vb.pm_type
        ),
    );
    check(
        (1..=2).contains(&vb.health),
        format!(
            "Last Stand: health {}, want 1 (2 while the overlay is kept up)",
            vb.health
        ),
    );
    check(
        vb.weapon == "beretta_mp",
        format!("Last Stand: holding {}, want the M9", vb.weapon),
    );
    check(
        !vb.weapons.iter().any(|w| w == "p90_mp"),
        format!("Last Stand: the primary stayed: {:?}", vb.weapons),
    );
    check(
        vb.frag == 0,
        format!("Last Stand: {} frags, want 0", vb.frag),
    );
    // The victim picks its second class now: the bleed-out's respawn gives it.
    m.v.choose("custom2");
    // No heal through the bleed-out, which comes at ten seconds and credits the attacker. The stock
    // `lastStandKeepOverlay` writes the health back to 1 twice a second, so the regeneration of `_healthoverlay.gsc`
    // never keeps what it adds: the health is at 1 or 2 again within a second of any climb, and never full.
    let (mut max_health, mut died_at, mut low_at) = (vb.health, None, hit_at);
    for _ in 0..LAST_STAND_FRAMES {
        m.frame();
        let b = body(&m.server, vn)?;
        if b.dead() || b.deaths > deaths_before {
            died_at = Some(m.server.game.level.time - hit_at);
            break;
        }
        max_health = max_health.max(b.health);
        if b.health <= 2 {
            low_at = m.server.game.level.time;
        }
    }
    let held_down = died_at.is_some_and(|ms| hit_at + ms - low_at <= 1000);
    check(
        max_health < 100 && held_down,
        format!(
            "Last Stand: the health climbed to {max_health} and was last at 1 {} ms before the end, want it held down",
            died_at.map_or(0, |ms| hit_at + ms - low_at)
        ),
    );
    report
        .metrics
        .insert("laststand.max_health".into(), f64::from(max_health));
    match died_at {
        Some(ms) => {
            check(
                (BLEED_OUT_MS - 500..=BLEED_OUT_MS + 2000).contains(&ms),
                format!(
                    "Last Stand: the bleed-out came {ms} ms after the hit, want about {BLEED_OUT_MS}"
                ),
            );
            report
                .metrics
                .insert("laststand.bleed_out_ms".into(), f64::from(ms));
        }
        None => check(false, "Last Stand: no bleed-out".into()),
    }
    m.settle(SETTLE);
    let ab = body(&m.server, an)?;
    check(
        ab.kills == kills_before + 1,
        format!(
            "Last Stand: the attacker has {} kills after the bleed-out, want {}",
            ab.kills,
            kills_before + 1
        ),
    );

    // Class 2 (after the respawn): Bandolier, and Stopping Power against no armour.
    m.respawn(vn, vb.spawn_count, true)?;
    let vb = body(&m.server, vn)?;
    check(
        vb.perks == perk_bits(&VICTIM[1]),
        format!(
            "victim class 2 perks {:#x}, want {:#x} (Bandolier, Sleight of Hand, Extreme Conditioning)",
            vb.perks,
            perk_bits(&VICTIM[1])
        ),
    );
    check(
        vb.p90_stock == 300,
        format!("Bandolier: P90 stock {}, want 300", vb.p90_stock),
    );
    check(
        vb.frag == 1,
        format!("{} frags without Frag x3, want 1", vb.frag),
    );
    check(
        vb.health == 100 && !vb.last_stand,
        format!(
            "the respawn left health {} and lastStand {}",
            vb.health, vb.last_stand
        ),
    );
    // The attacker takes Stopping Power: it dies and comes back with class 2.
    m.a.choose("custom2");
    m.hit(an, vn, 1000, "MOD_RIFLE_BULLET", "p90_mp")?;
    m.respawn(an, ab.spawn_count, false)?;
    let ab = body(&m.server, an)?;
    check(
        ab.perks == perk_bits(&ATTACKER[1]),
        format!(
            "attacker class 2 perks {:#x}, want Stopping Power",
            ab.perks
        ),
    );
    m.hit(vn, an, 40, "MOD_RIFLE_BULLET", "ak47_mp")?;
    m.settle(SETTLE);
    let vb = body(&m.server, vn)?;
    check(
        vb.health == 44,
        format!(
            "Stopping Power: health {} after a 40 rifle hit, want 44",
            vb.health
        ),
    );
    report
        .metrics
        .insert("stopping_power.health".into(), f64::from(vb.health));

    // Sonic Boom: the attacker's class 3 raises a 20 grenade splash to 25.
    m.a.choose("custom3");
    m.hit(an, vn, 1000, "MOD_RIFLE_BULLET", "p90_mp")?;
    m.respawn(an, ab.spawn_count, false)?;
    let before = body(&m.server, vn)?.health;
    m.hit(vn, an, 20, "MOD_GRENADE_SPLASH", "frag_grenade_mp")?;
    m.settle(SETTLE);
    let vb = body(&m.server, vn)?;
    check(
        vb.health == before - 25,
        format!(
            "Sonic Boom: health {} after a 20 grenade splash from {before}, want {}",
            vb.health,
            before - 25
        ),
    );

    // Martyrdom and Special Grenades x3: the victim's class 3, after a death without Last Stand.
    m.v.choose("custom3");
    m.hit(vn, an, 1000, "MOD_RIFLE_BULLET", "ak47_mp")?;
    m.respawn(vn, vb.spawn_count, true)?;
    let vb = body(&m.server, vn)?;
    check(
        vb.perks == perk_bits(&VICTIM[2]),
        format!(
            "victim class 3 perks {:#x}, want {:#x} (Special Grenades x3, Double Tap, Martyrdom)",
            vb.perks,
            perk_bits(&VICTIM[2])
        ),
    );
    check(
        vb.flash == 3,
        format!("Special Grenades x3 gave {} flashbangs, want 3", vb.flash),
    );
    // A lethal grenade: no Last Stand (the class has none, and a grenade never gives one), and a live grenade dropped.
    let grenades_before = grenades_of(&m.server, vn);
    m.hit(vn, an, 1000, "MOD_GRENADE_SPLASH", "frag_grenade_mp")?;
    m.settle(2);
    let vb = body(&m.server, vn)?;
    check(
        vb.dead() && !vb.last_stand,
        format!(
            "a lethal grenade without Last Stand: movement {:?}, lastStand {}",
            vb.pm_type, vb.last_stand
        ),
    );
    let dropped = grenades_of(&m.server, vn);
    check(
        dropped > grenades_before,
        format!("Martyrdom dropped no grenade ({grenades_before} live before, {dropped} after)"),
    );
    report.metrics.insert(
        "martyrdom.grenades".into(),
        dropped.saturating_sub(grenades_before) as f64,
    );

    m.v.c.disconnect();
    m.a.c.disconnect();
    m.settle(5);
    if !why.is_empty() {
        report.status = Status::Failed;
        report.reason = Some(why.join("; "));
    }
    Ok(report)
}
