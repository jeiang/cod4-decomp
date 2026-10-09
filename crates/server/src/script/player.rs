// SPDX-License-Identifier: GPL-3.0-only
//! Player builtins (`PlayerCmd_*`) and the client fields scripts read and write
//! (`self.sessionstate`, `self.score`, ...; `g_client_fields.cpp` of the original).

use gsc::{EntClass, EntRef, Value, Vm};
use sim::pm::{PmType, button, pmf};

use super::Impl::{self, Real};
use super::{Args, MethFn, uicmd};
use crate::client::{Client, Session, Team};
use crate::combat::{self, Damage};
use crate::game::Game;

type R = Result<Value, String>;

const fn r(f: MethFn) -> Impl<MethFn> {
    Real(f)
}

/// `bg_perkNames`: the perk bit is the name's index.
pub const PERK_NAMES: [&str; 20] = [
    "specialty_gpsjammer",
    "specialty_bulletaccuracy",
    "specialty_fastreload",
    "specialty_rof",
    "specialty_holdbreath",
    "specialty_bulletpenetration",
    "specialty_grenadepulldeath",
    "specialty_pistoldeath",
    "specialty_quieter",
    "specialty_parabolic",
    "specialty_longersprint",
    "specialty_detectexplosive",
    "specialty_explosivedamage",
    "specialty_exposeenemy",
    "specialty_bulletdamage",
    "specialty_extraammo",
    "specialty_twoprimaries",
    "specialty_armorvest",
    "specialty_fraggrenade",
    "specialty_specialgrenade",
];

pub const METHODS: &[(&str, Impl<MethFn>)] = &[
    ("spawn", r(spawn)),
    ("suicide", r(suicide)),
    ("finishplayerdamage", r(finish_player_damage)),
    ("setorigin", r(set_origin)),
    ("getplayerangles", r(get_player_angles)),
    ("setplayerangles", r(set_player_angles)),
    ("getvelocity", r(get_velocity)),
    ("geteye", r(get_eye)),
    ("getstance", r(get_stance)),
    (
        "isonground",
        r(|g, _, e, _| flag(g, e, |c| c.ps.ground_entity_num != sim::cm::ENTITYNUM_NONE)),
    ),
    (
        "isonladder",
        r(|g, _, e, _| flag(g, e, |c| c.ps.pm_flags & pmf::LADDER != 0)),
    ),
    (
        "ismantling",
        r(|g, _, e, _| flag(g, e, |c| c.ps.pm_flags & pmf::MANTLE != 0)),
    ),
    ("getspeed", r(get_speed)),
    ("freezecontrols", r(freeze_controls)),
    ("allowads", r(|g, _, e, a| allow(g, e, a, AllowKind::Ads))),
    ("allowjump", r(|g, _, e, a| allow(g, e, a, AllowKind::Jump))),
    (
        "allowsprint",
        r(|g, _, e, a| allow(g, e, a, AllowKind::Sprint)),
    ),
    ("allowspectateteam", r(allow_spectate_team)),
    ("setmovespeedscale", r(set_move_speed_scale)),
    (
        "attackbuttonpressed",
        r(|g, _, e, _| flag(g, e, |c| c.buttons & button::ATTACK != 0)),
    ),
    (
        "adsbuttonpressed",
        r(|g, _, e, _| flag(g, e, |c| c.buttons & button::ADS != 0)),
    ),
    (
        "meleebuttonpressed",
        r(|g, _, e, _| flag(g, e, |c| c.buttons & button::MELEE != 0)),
    ),
    (
        "usebuttonpressed",
        r(|g, _, e, _| {
            flag(g, e, |c| {
                c.buttons & (button::USE | button::USE_RELOAD) != 0
            })
        }),
    ),
    (
        "fragbuttonpressed",
        r(|g, _, e, _| flag(g, e, |c| c.buttons & button::FRAG != 0)),
    ),
    (
        "secondaryoffhandbuttonpressed",
        r(|g, _, e, _| flag(g, e, |c| c.buttons & button::SMOKE != 0)),
    ),
    ("setclientdvar", r(uicmd::set_client_dvar)),
    ("setclientdvars", r(uicmd::set_client_dvars)),
    ("openmenu", r(uicmd::open_menu)),
    ("openmenunomouse", r(uicmd::open_menu_no_mouse)),
    ("closemenu", r(uicmd::close_menu)),
    ("closeingamemenu", r(uicmd::close_ingame_menu)),
    ("iprintln", r(uicmd::client_print)),
    ("iprintlnbold", r(uicmd::client_print_bold)),
    ("sayall", r(uicmd::say_all)),
    ("sayteam", r(uicmd::say_team)),
    ("showscoreboard", r(show_scoreboard)),
    ("updatescores", r(|_, _, _, _| Ok(Value::Undefined))),
    ("updatedmscores", r(|_, _, _, _| Ok(Value::Undefined))),
    // Upstream stores `sess.enterTime` and nothing ever reads it: no observable effect, so no state kept here.
    ("setentertime", r(|_, _, _, _| Ok(Value::Undefined))),
    ("setrank", r(set_rank)),
    ("getguid", r(get_guid)),
    ("getxuid", r(get_guid)),
    ("getclanid", r(|_, _, _, _| Ok(Value::Int(0)))),
    ("getclanname", r(|_, _, _, _| Ok(Value::str("")))),
    ("getstat", r(get_stat)),
    ("setstat", r(set_stat)),
    ("shellshock", r(shell_shock)),
    ("stopshellshock", r(stop_shell_shock)),
    ("viewkick", r(view_kick)),
    ("vibrate", r(|_, _, _, _| Ok(Value::Undefined))),
    ("setdepthoffield", r(|_, _, _, _| Ok(Value::Undefined))),
    (
        "setviewmodeldepthoffield",
        r(|_, _, _, _| Ok(Value::Undefined)),
    ),
    ("playlocalsound", r(super::sound::play_local_sound)),
    ("stoplocalsound", r(super::sound::stop_local_sound)),
    (
        "pingplayer",
        r(|g, _, e, _| {
            let n = client_of(g, e)?;
            let until = g.level.time + 3000;
            if let Some(c) = g.client_mut(n) {
                c.compass_ping_until = until;
            }
            Ok(Value::Undefined)
        }),
    ),
    ("istalking", r(|_, _, _, _| Ok(Value::Int(0)))),
    ("setperk", r(set_perk)),
    ("unsetperk", r(unset_perk)),
    ("hasperk", r(has_perk)),
    ("clearperks", r(clear_perks)),
];

fn client_of(g: &Game, e: EntRef) -> Result<u16, String> {
    if e.class != EntClass::Entity {
        return Err("not an entity".into());
    }
    if g.is_client(e.num) {
        Ok(e.num)
    } else {
        Err(format!("entity {} is not a player", e.num))
    }
}

fn flag(g: &mut Game, e: EntRef, f: fn(&Client) -> bool) -> R {
    let n = client_of(g, e)?;
    Ok(Value::Int(i32::from(f(g.client(n).expect("client")))))
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Int(n) => *n != 0,
        Value::Float(f) => *f != 0.0,
        Value::Undefined => false,
        _ => true,
    }
}

/// `PlayerCmd_spawn`: `ClientSpawn` at the given origin and angles.
fn spawn(g: &mut Game, vm: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client_of(g, e)?;
    let (origin, angles) = (a.vector(0)?, a.vector(1)?);
    g.client_spawn(vm, n, origin, angles);
    Ok(Value::Undefined)
}

/// `PlayerCmd_Suicide`: kills a living player the way a console `kill` does.
fn suicide(g: &mut Game, vm: &mut Vm, e: EntRef, _: Args) -> R {
    let n = client_of(g, e)?;
    g.kill_self(vm, n)?;
    Ok(Value::Undefined)
}

impl Game {
    /// `Cmd_Kill_f`: a living player dies of `MOD_SUICIDE`; anyone else is left alone.
    pub fn kill_self(&mut self, vm: &mut Vm, n: u16) -> Result<(), String> {
        let alive = self
            .client(n)
            .is_some_and(|c| c.session == Session::Playing && c.ps.pm_type == PmType::Normal);
        if alive {
            let mut d = Damage::new(100_000, combat::MOD_SUICIDE);
            d.attacker = Some(n);
            d.inflictor = Some(n);
            self.finish_player_damage(vm, n, d)?;
        }
        Ok(())
    }

    /// `Cmd_Where_f`: tells `n` where it stands.
    pub fn where_am_i(&mut self, n: u16) {
        let Some(c) = self.client(n) else { return };
        let [x, y, z] = c.ps.origin;
        self.send(
            crate::ui::Dest::Client(n),
            net::ui::ServerCmd::Print {
                kind: net::ui::PrintKind::Console,
                text: format!("({} {} {})\n", x as i32, y as i32, z as i32),
            },
        );
    }
}

/// `PlayerCmd_finishPlayerDamage(inflictor, attacker, damage, flags, mod, weapon, point, dir,
/// hitloc, timeOffset)`.
fn finish_player_damage(g: &mut Game, vm: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client_of(g, e)?;
    let ent = |v: Option<&Value>| match v {
        Some(Value::Object(o)) => o.entity().map(|e| e.num),
        _ => None,
    };
    let mean = combat::mod_from_name(a.string(4)?)
        .ok_or_else(|| format!("Unknown means of death \"{}\"\n", a.string(4).unwrap_or("")))?;
    let vec = |i: usize| match a.opt(i) {
        Some(Value::Vector(v)) => Some(*v),
        _ => None,
    };
    let mut d = Damage::new(a.int(2)?, mean);
    d.inflictor = ent(a.opt(0)).or(Some(sim::cm::ENTITYNUM_WORLD));
    d.attacker = ent(a.opt(1)).or(Some(sim::cm::ENTITYNUM_WORLD));
    d.flags = a.int(3)?;
    d.weapon = g.weapon_index(a.string(5).unwrap_or(""));
    d.point = vec(6);
    d.dir = vec(7);
    d.hitloc = combat::hitloc_from_name(a.string(8).unwrap_or("none")).unwrap_or(0);
    d.time_offset = a.int(9)?;
    g.finish_player_damage(vm, n, d)?;
    Ok(Value::Undefined)
}

fn set_origin(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client_of(g, e)?;
    let o = a.vector(0)?;
    if let Some(c) = g.client_mut(n) {
        c.ps.origin = o;
    }
    if let Some(en) = g.ent_mut(n) {
        en.origin = o;
    }
    g.relink(n);
    Ok(Value::Undefined)
}

fn get_player_angles(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let n = client_of(g, e)?;
    Ok(Value::Vector(g.client(n).expect("client").ps.viewangles))
}

fn set_player_angles(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client_of(g, e)?;
    g.set_client_view_angle(n, a.vector(0)?);
    Ok(Value::Undefined)
}

fn get_velocity(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let n = client_of(g, e)?;
    Ok(Value::Vector(g.client(n).expect("client").ps.velocity))
}

fn get_eye(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let n = client_of(g, e)?;
    let ps = &g.client(n).expect("client").ps;
    let o = ps.origin;
    Ok(Value::Vector([o[0], o[1], o[2] + ps.view_height_current]))
}

fn get_stance(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let n = client_of(g, e)?;
    let c = g.client(n).expect("client");
    Ok(Value::str(match c.ps.stance() {
        sim::pm::Stance::Stand => "stand",
        sim::pm::Stance::Crouch => "crouch",
        sim::pm::Stance::Prone => "prone",
    }))
}

fn get_speed(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let n = client_of(g, e)?;
    let v = g.client(n).expect("client").ps.velocity;
    Ok(Value::Float((v[0] * v[0] + v[1] * v[1]).sqrt()))
}

fn freeze_controls(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client_of(g, e)?;
    g.client_mut(n).expect("client").frozen = truthy(a.get(0)?);
    Ok(Value::Undefined)
}

enum AllowKind {
    Ads,
    Jump,
    Sprint,
}

fn allow(g: &mut Game, e: EntRef, a: Args, kind: AllowKind) -> R {
    let n = client_of(g, e)?;
    let on = truthy(a.get(0)?);
    let c = g.client_mut(n).expect("client");
    match kind {
        AllowKind::Ads => c.allow_ads = on,
        AllowKind::Jump => {
            if on {
                c.ps.pm_flags &= !pmf::NO_JUMP;
            } else {
                c.ps.pm_flags |= pmf::NO_JUMP;
            }
        }
        AllowKind::Sprint => {
            if on {
                c.ps.pm_flags &= !pmf::NO_SPRINT;
            } else {
                c.ps.pm_flags |= pmf::NO_SPRINT;
            }
        }
    }
    Ok(Value::Undefined)
}

fn set_move_speed_scale(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client_of(g, e)?;
    g.client_mut(n).expect("client").move_speed_scale = a.float(0)?;
    Ok(Value::Undefined)
}

/// Chat-area print to one client: bots hear nothing; the console shows nothing extra.
/// `shellshock(name, seconds)`: the player's screen and ears take the named shell shock.
fn shell_shock(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client_of(g, e)?;
    let name = a.string(0)?.to_owned();
    let ms = (a.float(1)? * 1000.0).round() as i32;
    g.send(
        crate::ui::Dest::Client(n),
        net::ui::ServerCmd::ShellShock { name, ms },
    );
    Ok(Value::Undefined)
}

/// `viewkick(force, source)`: the player's view is hit as by `force` percent of the health from `source`
/// (`GScr_ViewKick`); the end frame shows it.
fn view_kick(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client_of(g, e)?;
    let force = a.int(0)?;
    let from = a.vector(1)?;
    let c = g.client_mut(n).expect("client");
    c.damage_blood = (c.max_health * force + 50) / 100;
    if c.damage_blood < 0 {
        return Err(format!("viewkick: damage {force} < 0"));
    }
    for (d, (o, f)) in c.damage_from.iter_mut().zip(c.ps.origin.iter().zip(from)) {
        *d = o - f;
    }
    Ok(Value::Undefined)
}

fn stop_shell_shock(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let n = client_of(g, e)?;
    g.send(
        crate::ui::Dest::Client(n),
        net::ui::ServerCmd::ShellShock {
            name: String::new(),
            ms: 0,
        },
    );
    Ok(Value::Undefined)
}

fn get_guid(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let n = client_of(g, e)?;
    Ok(Value::Int(1_000_000 + i32::from(n)))
}

fn get_stat(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client_of(g, e)?;
    let i = a.int(0)?;
    Ok(Value::Int(
        g.client(n)
            .expect("client")
            .stats
            .get(&i)
            .copied()
            .unwrap_or(0),
    ))
}

fn set_stat(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client_of(g, e)?;
    let (i, v) = (a.int(0)?, a.int(1)?);
    let old = g.client_mut(n).expect("client").stats.insert(i, v);
    if old != Some(v) {
        g.send(
            crate::ui::Dest::Client(n),
            net::ui::ServerCmd::Stat { index: i, value: v },
        );
    }
    Ok(Value::Undefined)
}

fn allow_spectate_team(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client_of(g, e)?;
    let name = a.string(0)?;
    let bit = crate::client::spec::from_name(name)
        .ok_or_else(|| format!("Unknown team '{name}'. Must be allies, axis, none or freelook."))?;
    let on = a.int(1)? != 0;
    let c = g.client_mut(n).expect("client");
    if on {
        c.spec_allow |= bit;
    } else {
        c.spec_allow &= !bit;
    }
    Ok(Value::Undefined)
}

/// `setrank(rank[, prestige])`: both are bytes, published to everybody in the client info.
fn set_rank(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client_of(g, e)?;
    let byte = |v: i32, what: &str| {
        u8::try_from(v)
            .map_err(|_| format!("'{v}' is an illegal {what} value.  Must be less than 256.\n"))
    };
    let rank = byte(a.int(0)?, "rank")?;
    // `Scr_GetNumParam() >= 2`: an explicit argument, whatever its value, counts as given.
    let prestige = if a.len() >= 2 {
        Some(byte(a.int(1)?, "prestige")?)
    } else {
        None
    };
    let c = g.client_mut(n).ok_or("not a player")?;
    c.rank = rank;
    if let Some(p) = prestige {
        c.prestige = p;
    }
    Ok(Value::Undefined)
}

fn show_scoreboard(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let n = client_of(g, e)?;
    g.ui.score_requests.push(n);
    Ok(Value::Undefined)
}

fn perk_bit(a: &Args) -> Result<u32, String> {
    let name = a.string(0)?;
    PERK_NAMES
        .iter()
        .position(|p| p.eq_ignore_ascii_case(name))
        .map(|i| 1u32 << i)
        .ok_or_else(|| format!("Unknown perk: {name}\n"))
}

fn set_perk(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client_of(g, e)?;
    g.client_mut(n).expect("client").ps.perks |= perk_bit(&a)?;
    Ok(Value::Undefined)
}

fn unset_perk(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client_of(g, e)?;
    g.client_mut(n).expect("client").ps.perks &= !perk_bit(&a)?;
    Ok(Value::Undefined)
}

fn has_perk(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = client_of(g, e)?;
    let bit = perk_bit(&a)?;
    Ok(Value::Int(i32::from(
        g.client(n).expect("client").ps.perks & bit != 0,
    )))
}

fn clear_perks(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let n = client_of(g, e)?;
    g.client_mut(n).expect("client").ps.perks = 0;
    Ok(Value::Undefined)
}

// ---- client fields ----

fn want_str(v: &Value) -> Result<&str, String> {
    match v {
        Value::Str(s) => Ok(s),
        o => Err(format!("type {} is not a string", o.type_name())),
    }
}

fn want_int(v: &Value) -> Result<i32, String> {
    match v {
        Value::Int(i) => Ok(*i),
        Value::Float(f) => Ok(*f as i32),
        o => Err(format!("type {} is not an int", o.type_name())),
    }
}

/// `Scr_GetClientField`: `None` when `name` is not a client field.
pub fn get_client_field(g: &Game, n: u16, name: &str) -> Option<Value> {
    let c = g.client(n)?;
    Some(match name {
        "name" => Value::str(&c.name),
        "sessionteam" => Value::str(c.team.name()),
        "sessionstate" => Value::str(c.session.name()),
        "maxhealth" => Value::Int(c.max_health),
        "score" => Value::Int(c.score),
        "deaths" => Value::Int(c.deaths),
        "kills" => Value::Int(c.kills),
        "assists" => Value::Int(c.assists),
        "hasradar" => Value::Int(i32::from(c.has_radar)),
        "statusicon" => Value::str(&c.status_icon),
        "headicon" => Value::str(&c.head_icon),
        "headiconteam" => Value::str(&c.head_icon_team),
        "spectatorclient" => Value::Int(c.spectator_client),
        "killcamentity" => Value::Int(c.kill_cam_entity),
        "archivetime" => Value::Float(c.archive_time),
        "psoffsettime" => Value::Int(c.ps_offset_time),
        _ => return None,
    })
}

/// `Scr_SetClientField`: `None` when `name` is not a client field, else the outcome. Setting
/// `origin`/`angles` on a player moves the player state too.
pub fn set_client_field(g: &mut Game, n: u16, name: &str, v: &Value) -> Option<Result<(), String>> {
    let mut run = || -> Result<bool, String> {
        match name {
            "name" | "pers" => Err(format!("player field {name} is read-only")),
            "sessionteam" => {
                let s = want_str(v)?;
                let t = Team::from_name(s).ok_or_else(|| {
                    format!("'{s}' is an illegal sessionteam string. Must be allies, axis, none, or spectator.")
                })?;
                g.client_mut(n).expect("client").team = t;
                Ok(true)
            }
            "sessionstate" => {
                let s = want_str(v)?;
                let st = Session::from_name(s).ok_or_else(|| {
                    format!("'{s}' is an illegal sessionstate string. Must be playing, dead, spectator, or intermission.")
                })?;
                g.client_mut(n).expect("client").session = st;
                g.set_client_contents(n);
                Ok(true)
            }
            "maxhealth" => {
                g.client_mut(n).expect("client").max_health = want_int(v)?;
                Ok(true)
            }
            "score" => {
                g.client_mut(n).expect("client").score = want_int(v)?;
                Ok(true)
            }
            "deaths" => {
                g.client_mut(n).expect("client").deaths = want_int(v)?;
                Ok(true)
            }
            "kills" => {
                g.client_mut(n).expect("client").kills = want_int(v)?;
                Ok(true)
            }
            "assists" => {
                g.client_mut(n).expect("client").assists = want_int(v)?;
                Ok(true)
            }
            "hasradar" => {
                g.client_mut(n).expect("client").has_radar = want_int(v)? != 0;
                Ok(true)
            }
            "statusicon" => {
                g.client_mut(n).expect("client").status_icon = want_str(v)?.to_owned();
                Ok(true)
            }
            "headicon" => {
                let icon = want_str(v)?.to_owned();
                g.precache(crate::ui::Table::Material, &icon)?;
                g.client_mut(n).expect("client").head_icon = icon;
                Ok(true)
            }
            "headiconteam" => {
                g.client_mut(n).expect("client").head_icon_team = want_str(v)?.to_owned();
                Ok(true)
            }
            "spectatorclient" => {
                g.client_mut(n).expect("client").spectator_client = want_int(v)?;
                Ok(true)
            }
            "killcamentity" => {
                g.client_mut(n).expect("client").kill_cam_entity = want_int(v)?;
                Ok(true)
            }
            "archivetime" => {
                g.client_mut(n).expect("client").archive_time = match v {
                    Value::Float(f) => *f,
                    Value::Int(i) => *i as f32,
                    o => return Err(format!("type {} is not a float", o.type_name())),
                };
                Ok(true)
            }
            "psoffsettime" => {
                g.client_mut(n).expect("client").ps_offset_time = want_int(v)?;
                Ok(true)
            }
            "origin" => {
                if let Value::Vector(o) = v {
                    g.client_mut(n).expect("client").ps.origin = *o;
                }
                Ok(false)
            }
            "angles" => {
                if let Value::Vector(a) = v {
                    g.set_client_view_angle(n, *a);
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    };
    let known = matches!(
        name,
        "name"
            | "pers"
            | "sessionteam"
            | "sessionstate"
            | "maxhealth"
            | "score"
            | "deaths"
            | "kills"
            | "assists"
            | "hasradar"
            | "statusicon"
            | "headicon"
            | "headiconteam"
            | "spectatorclient"
            | "killcamentity"
            | "archivetime"
            | "psoffsettime"
    );
    match run() {
        Err(e) => Some(Err(e)),
        Ok(_) if known => Some(Ok(())),
        Ok(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gsc::{Builtins, Options, compile};

    /// Clients pick the Dead Silence sound families by this bit of the entity state's perks.
    #[test]
    fn the_quieter_perk_bit_is_the_one_clients_test() {
        let i = PERK_NAMES.iter().position(|n| *n == "specialty_quieter");
        assert_eq!(i.map(|i| 1u32 << i), Some(sim::pm::PERK_QUIETER));
    }

    #[test]
    fn setrank_keeps_the_prestige_unless_given_and_rejects_a_byte_overflow() {
        let prog = compile(
            &[("t.gsc", "main() {}")],
            &Builtins::stock_mp(),
            Options::default(),
        )
        .unwrap();
        let mut vm = Vm::new(prog).unwrap();
        let mut g = Game::new(crate::cvar::Cvars::new(), Default::default());
        g.reset_level(4);
        let n = g.connect_client(&mut vm, true, "Ann").unwrap();
        let e = EntRef {
            num: n,
            class: EntClass::Entity,
        };
        let mut call = |args: &[i32]| {
            let v: Vec<Value> = args.iter().map(|n| Value::Int(*n)).collect();
            set_rank(&mut g, &mut vm, e, Args::new("setrank", &v))
        };
        call(&[30, 2]).unwrap();
        call(&[31]).unwrap();
        assert!(call(&[256]).is_err() && call(&[1, -1]).is_err());
        let c = g.client(n).unwrap();
        assert_eq!((c.rank, c.prestige), (31, 2));
    }

    #[test]
    fn a_players_head_icon_reaches_clients_as_a_material_and_a_team() {
        let prog = compile(
            &[("t.gsc", "main() {}")],
            &Builtins::stock_mp(),
            Options::default(),
        )
        .unwrap();
        let mut vm = Vm::new(prog).unwrap();
        let mut g = Game::new(crate::cvar::Cvars::new(), Default::default());
        g.reset_level(4);
        let n = g.connect_client(&mut vm, true, "Ann").unwrap();
        let c = g.client_mut(n).unwrap();
        c.conn = crate::client::Conn::Connected;
        c.session = Session::Playing;
        let shown = |g: &Game| {
            let e = crate::netsv::world_entities(g)
                .into_iter()
                .find(|e| e.number == n)
                .expect("the player is published");
            (e.head_icon, e.head_icon_team)
        };
        assert_eq!(shown(&g), (0, 0), "no icon until the scripts set one");
        for (field, v) in [("headicon", "waypoint_bomb"), ("headiconteam", "axis")] {
            set_client_field(&mut g, n, field, &Value::str(v))
                .unwrap()
                .unwrap();
        }
        let (icon, team) = shown(&g);
        assert_eq!(team, Team::Axis as u8);
        assert_eq!(
            g.configstrings[&(u32::from(net::ui::cs::MATERIALS) + u32::from(icon))],
            "waypoint_bomb"
        );
    }
}
