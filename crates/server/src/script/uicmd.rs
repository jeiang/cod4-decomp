// SPDX-License-Identifier: GPL-3.0-only
//! The builtins that talk to players' screens: client dvars, script menus, print lines,
//! announcements, chat, the winner configstring and the compass objectives.

use gsc::{EntClass, EntRef, Value, Vm};
use net::ui::{self, PrintKind, ServerCmd, cs, obj};

use super::Args;
use super::args::display;
use crate::client::{Session, Team};
use crate::game::Game;
use crate::ui::{Dest, ObjSlot, Table, clip};

type R = Result<Value, String>;

fn player(g: &Game, e: EntRef) -> Result<u16, String> {
    if e.class != EntClass::Entity {
        return Err("not an entity".into());
    }
    if g.is_client(e.num) {
        Ok(e.num)
    } else {
        Err(format!("entity {} is not a player", e.num))
    }
}

/// One argument of a message: a localized reference, or text (a player's name already with its `^7`).
enum Part {
    Loc(String),
    Text(String),
}

/// The parts as one message (`Scr_ConstructMessageString`), in the form the clients localize
/// (`hud::localize`): `\x14` before a localized reference, `\x15` before text. The first
/// reference needs no mark. Marks inside a part become `.`.
fn construct(parts: &[Part]) -> String {
    let mut s = String::new();
    for p in parts {
        let (mark, text) = match p {
            Part::Loc(n) => (if s.is_empty() { None } else { Some('\x14') }, n),
            Part::Text(t) => (Some('\x15'), t),
        };
        if let Some(m) = mark.filter(|_| !text.is_empty()) {
            s.push(m);
        }
        s.extend(text.chars().map(|c| {
            if ('\x14'..='\x16').contains(&c) {
                '.'
            } else {
                c
            }
        }));
    }
    clip(&s)
}

/// Arguments `from..` as one message; a player entity is its name.
fn message(g: &Game, a: Args, from: usize) -> Result<String, String> {
    let mut parts = Vec::new();
    for v in a.v.iter().skip(from) {
        parts.push(match v {
            Value::LocStr(n) => Part::Loc(n.to_string()),
            Value::Object(o) if o.entity().is_some() => {
                let e = o.entity().expect("checked");
                let c = (e.class == EntClass::Entity)
                    .then(|| g.client(e.num))
                    .flatten()
                    .ok_or("Entity is not a player")?;
                Part::Text(format!("{}^7", c.name))
            }
            v => Part::Text(display(v)),
        });
    }
    Ok(construct(&parts))
}

/// Arguments `from..` run together, localized references keeping their `&`: chat and dvar values.
fn joined(a: Args, from: usize) -> String {
    let mut s = String::new();
    for v in a.v.iter().skip(from) {
        match v {
            Value::LocStr(n) => {
                s.push('&');
                s.push_str(n);
            }
            v => s.push_str(&display(v)),
        }
    }
    clip(&s)
}

fn valid_dvar_name(n: &str) -> bool {
    !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn clean(value: &str) -> String {
    clip(&value.replace('"', "'"))
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

fn to_client(g: &mut Game, n: u16, cmd: ServerCmd) {
    if g.client(n).is_some_and(|c| !c.bot) {
        g.send(Dest::Client(n), cmd);
    }
}

pub fn set_client_dvar(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = player(g, e)?;
    let name = a.string(0)?;
    if !valid_dvar_name(name) {
        return Err(format!("Dvar {name} has an invalid dvar name"));
    }
    let value = if a.len() > 2 {
        joined(a, 1)
    } else {
        display(a.get(1)?)
    };
    to_client(
        g,
        n,
        ServerCmd::SetDvar {
            name: name.to_owned(),
            value: clean(&value),
        },
    );
    Ok(Value::Undefined)
}

pub fn set_client_dvars(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = player(g, e)?;
    if !a.len().is_multiple_of(2) {
        return Err("Not enough parameters to setclientdvar() - must be an even number of parameters (dvar, value, dvar, value, etc.)\n".into());
    }
    for i in (0..a.len()).step_by(2) {
        let name = a.string(i)?;
        if !valid_dvar_name(name) {
            return Err(format!("Dvar {name} has an invalid dvar name"));
        }
        let cmd = ServerCmd::SetDvar {
            name: name.to_owned(),
            value: clean(&a.display(i + 1)?),
        };
        to_client(g, n, cmd);
    }
    Ok(Value::Undefined)
}

fn open(g: &mut Game, e: EntRef, a: Args, mouse: bool) -> R {
    let n = player(g, e)?;
    let name = a.string(0)?.to_owned();
    if !g.connected_clients().any(|(c, _)| c == n) {
        return Ok(Value::Int(0));
    }
    if g.menus.find(&name) == 0 {
        g.print(format!("openmenu: menu '{name}' was not precached\n"));
    }
    to_client(g, n, ServerCmd::OpenMenu { name, mouse });
    Ok(Value::Int(1))
}

pub fn open_menu(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    open(g, e, a, true)
}

pub fn open_menu_no_mouse(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    open(g, e, a, false)
}

pub fn close_menu(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let n = player(g, e)?;
    to_client(
        g,
        n,
        ServerCmd::CloseMenu {
            name: String::new(),
        },
    );
    Ok(Value::Undefined)
}

pub fn close_ingame_menu(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let n = player(g, e)?;
    to_client(g, n, ServerCmd::CloseIngameMenu);
    Ok(Value::Undefined)
}

fn print_to(g: &mut Game, dest: Dest, kind: PrintKind, text: String) {
    g.send(dest, ServerCmd::Print { kind, text });
}

/// `iprintln` / `iprintlnbold` called on a player.
pub fn client_print(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = player(g, e)?;
    print_to(g, Dest::Client(n), PrintKind::Normal, message(g, a, 0)?);
    Ok(Value::Undefined)
}

pub fn client_print_bold(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = player(g, e)?;
    print_to(g, Dest::Client(n), PrintKind::Bold, message(g, a, 0)?);
    Ok(Value::Undefined)
}

/// `iprintln` / `iprintlnbold` called on the level: everybody.
pub fn level_print(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let text = message(g, a, 0)?;
    g.print(format!("{}\n", text.replace(['\x14', '\x15', '\x16'], "")));
    print_to(g, Dest::All, PrintKind::Normal, text);
    Ok(Value::Undefined)
}

pub fn level_print_bold(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let text = message(g, a, 0)?;
    g.print(format!("{}\n", text.replace(['\x14', '\x15', '\x16'], "")));
    print_to(g, Dest::All, PrintKind::Bold, text);
    Ok(Value::Undefined)
}

/// `clientprint(player, text)`: the player's console.
pub fn client_print_console(g: &mut Game, _: &mut Vm, a: Args) -> R {
    if a.is_empty() {
        return Ok(Value::Undefined);
    }
    let n = player(g, a.entity(0)?)?;
    print_to(g, Dest::Client(n), PrintKind::Console, clip(a.string(1)?));
    Ok(Value::Undefined)
}

pub fn announcement(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let text = message(g, a, 0)?;
    g.send(Dest::All, ServerCmd::Announce { text });
    Ok(Value::Undefined)
}

pub fn client_announcement(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let n = player(g, a.entity(0)?)?;
    let text = message(g, a, 1)?;
    g.send(Dest::Client(n), ServerCmd::Announce { text });
    Ok(Value::Undefined)
}

fn say(g: &mut Game, e: EntRef, a: Args, team: bool) -> R {
    let n = player(g, e)?;
    g.chat(n, &joined(a, 0), team);
    Ok(Value::Undefined)
}

impl Game {
    /// `G_Say`: `n` says `text` to everyone, or to their team (a player on no team speaks to everyone). A dead
    /// player's line reaches only the others who are not playing, unless `g_deadChat`.
    pub fn chat(&mut self, n: u16, text: &str, team: bool) {
        let Some(c) = self.client(n) else { return };
        let (name, side, session) = (c.name.clone(), c.team, c.session);
        let team = team && matches!(side, Team::Axis | Team::Allies);
        // `G_SayTo`: anyone not playing is marked, a spectator by the team they are on.
        let tag = match (side, session) {
            (Team::Spectator, _) => ui::chat_tag::SPECTATOR,
            (_, Session::Playing) => ui::chat_tag::NORMAL,
            _ => ui::chat_tag::DEAD,
        };
        let text: String = text.chars().take(149).collect();
        self.print(format!(
            "{}: {name}: {text}\n",
            if team { "sayteam" } else { "say" }
        ));
        let dead_chat = self.cvars.bool("g_deadChat");
        let hears: Vec<u16> = self
            .connected_clients()
            .filter(|(_, o)| !team || o.team == side)
            .filter(|(_, o)| {
                dead_chat || session == Session::Playing || o.session != Session::Playing
            })
            .map(|(k, _)| k)
            .collect();
        for k in hears {
            self.send(
                Dest::Client(k),
                ServerCmd::Chat {
                    team,
                    client: n,
                    tag,
                    text: text.clone(),
                },
            );
        }
    }
}

pub fn say_all(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    say(g, e, a, false)
}

pub fn say_team(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    say(g, e, a, true)
}

fn set_winner(g: &mut Game, who: i32) {
    g.set_configstring(cs::MULTI_MAPWINNER, &format!("\\winner\\{who}"));
}

pub fn set_winning_player(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let e = a.entity(0)?;
    set_winner(g, i32::from(e.num) + 1);
    Ok(Value::Undefined)
}

pub fn set_winning_team(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let who = match a.string(0)? {
        "allies" => -2,
        "axis" => -1,
        "none" => 0,
        o => {
            return Err(format!(
                "Illegal team string '{o}'. Must be allies, axis, or none."
            ));
        }
    };
    set_winner(g, who);
    Ok(Value::Undefined)
}

// ---- objectives -------------------------------------------------------------------------------

fn objective_index(a: Args) -> Result<usize, String> {
    let n = a.int(0)?;
    usize::try_from(n)
        .ok()
        .filter(|n| *n < ui::MAX_OBJECTIVES)
        .ok_or_else(|| {
            format!(
                "index {n} is an illegal objective index. Valid indexes are 0 to {}\n",
                ui::MAX_OBJECTIVES - 1
            )
        })
}

fn objective_state_of(s: &str) -> Result<u8, String> {
    Ok(match s {
        "empty" => obj::EMPTY,
        "invisible" => obj::INVISIBLE,
        "current" => obj::CURRENT,
        "active" => obj::ACTIVE,
        o => {
            return Err(format!(
                "Illegal objective state \"{o}\". Valid states are \"empty\", \"invisible\", \"current\", \"active\"\n"
            ));
        }
    })
}

fn set_icon(g: &mut Game, i: usize, name: &str) -> Result<(), String> {
    if let Some(c) = name
        .chars()
        .find(|c| (*c as u32) <= 31 || (*c as u32) >= 127)
    {
        return Err(format!(
            "Illegal character '{c}'(ascii {}) in objective icon name: {name}\n",
            c as u32
        ));
    }
    if name.len() >= 64 {
        return Err(format!("Objective icon name is too long (> 63): {name}\n"));
    }
    let m = g.precache(Table::Material, name)?;
    g.ui.objectives[i].o.icon = m;
    Ok(())
}

fn whole(v: [f32; 3]) -> [f32; 3] {
    v.map(f32::trunc)
}

pub fn objective_add(g: &mut Game, _: &mut Vm, a: Args) -> R {
    if a.len() < 2 {
        return Err("objective_add needs at least the first two parameters out of its parameter list of: index state [string] [position]\n".into());
    }
    let i = objective_index(a)?;
    let state = objective_state_of(a.string(1)?)?;
    let o = &mut g.ui.objectives[i];
    o.o.entity = ui::NO_ENTITY;
    o.o.state = state;
    if a.len() >= 3 {
        o.o.origin = whole(a.vector(2)?);
        if a.len() >= 4 {
            set_icon(g, i, a.string(3)?)?;
        }
    }
    g.ui.objectives[i].team = Team::Free;
    Ok(Value::Undefined)
}

pub fn objective_delete(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let i = objective_index(a)?;
    g.ui.objectives[i] = ObjSlot::cleared();
    Ok(Value::Undefined)
}

pub fn objective_state(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let i = objective_index(a)?;
    let state = objective_state_of(a.string(1)?)?;
    let o = &mut g.ui.objectives[i].o;
    o.state = state;
    if matches!(state, obj::EMPTY | obj::INVISIBLE) {
        o.entity = ui::NO_ENTITY;
    }
    Ok(Value::Undefined)
}

pub fn objective_icon(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let i = objective_index(a)?;
    set_icon(g, i, a.string(1)?)?;
    Ok(Value::Undefined)
}

pub fn objective_position(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let i = objective_index(a)?;
    let origin = whole(a.vector(1)?);
    let o = &mut g.ui.objectives[i].o;
    o.entity = ui::NO_ENTITY;
    o.origin = origin;
    Ok(Value::Undefined)
}

pub fn objective_on_entity(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let i = objective_index(a)?;
    let e = a.entity(1)?;
    g.ui.objectives[i].o.entity = e.num.min(ui::NO_ENTITY);
    Ok(Value::Undefined)
}

pub fn objective_team(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let i = objective_index(a)?;
    g.ui.objectives[i].team = match a.string(1)? {
        "allies" => Team::Allies,
        "axis" => Team::Axis,
        "none" => Team::Free,
        o => {
            return Err(format!(
                "Illegal team string '{o}'. Must be allies, axis, or none."
            ));
        }
    };
    Ok(Value::Undefined)
}

pub fn objective_current(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let mut make = [false; ui::MAX_OBJECTIVES];
    for p in 0..a.len() {
        let n = a.int(p)?;
        let i = usize::try_from(n)
            .ok()
            .filter(|n| *n < ui::MAX_OBJECTIVES)
            .ok_or_else(|| {
                format!(
                    "index {n} is an illegal objective index. Valid indexes are 0 to {}\n",
                    ui::MAX_OBJECTIVES - 1
                )
            })?;
        make[i] = true;
    }
    for (o, m) in g.ui.objectives.iter_mut().zip(make) {
        if m {
            o.o.state = obj::CURRENT;
        } else if o.o.state == obj::CURRENT {
            o.o.state = obj::ACTIVE;
        }
    }
    Ok(Value::Undefined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_marked_the_way_the_clients_read_them() {
        let loc = |k: &str| Part::Loc(k.into());
        let text = |t: &str| Part::Text(t.into());
        // `iprintln(&"MP_CONNECTED", player)`: the key, then the name as an argument.
        assert_eq!(
            construct(&[loc("MP_CONNECTED"), text("Bob^7")]),
            "MP_CONNECTED\x15Bob^7"
        );
        assert_eq!(
            construct(&[loc("MP_WAR_RADAR_ACQUIRED_ENEMY"), text("30")]),
            "MP_WAR_RADAR_ACQUIRED_ENEMY\x1530"
        );
        // Plain text is an argument too, and a later reference is marked as a key.
        assert_eq!(construct(&[text("hi")]), "\x15hi");
        assert_eq!(construct(&[loc("A"), loc("B")]), "A\x14B");
        // A mark a script puts in its text cannot start a part.
        assert_eq!(construct(&[loc("A"), text("x\x14y")]), "A\x15x.y");
    }
}

#[cfg(test)]
mod chat_tests {
    use super::*;
    use crate::client::{Client, Conn};
    use crate::content::Content;
    use crate::cvar::Cvars;

    /// Slots 0 and 1 on the allies, 2 on the axis, 3 a spectator; all playing.
    fn game() -> Game {
        let mut g = Game::new(Cvars::new(), Content::default());
        g.clients = (0..4)
            .map(|n| Client::new(n, false, format!("p{n}")))
            .collect();
        for (n, team) in [
            (0, Team::Allies),
            (1, Team::Allies),
            (2, Team::Axis),
            (3, Team::Spectator),
        ] {
            let c = &mut g.clients[n];
            c.conn = Conn::Connected;
            c.team = team;
            c.session = Session::Playing;
        }
        g
    }

    /// Who was sent a chat line, with the line's team flag and tag.
    fn heard(g: &mut Game) -> Vec<(u16, bool, u8, String)> {
        std::mem::take(&mut g.ui.out)
            .into_iter()
            .filter_map(|o| match (o.to, o.cmd) {
                (
                    Dest::Client(k),
                    ServerCmd::Chat {
                        team, tag, text, ..
                    },
                ) => Some((k, team, tag, text)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_team_line_reaches_the_team_and_a_public_line_everyone() {
        let mut g = game();
        g.chat(0, "to all", false);
        assert_eq!(
            heard(&mut g).iter().map(|h| h.0).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
        g.chat(0, "to us", true);
        let h = heard(&mut g);
        assert_eq!(h.iter().map(|h| h.0).collect::<Vec<_>>(), [0, 1]);
        assert!(h.iter().all(|h| h.1), "marked as a team line");
    }

    #[test]
    fn a_player_on_no_team_speaks_to_everyone_even_when_asking_for_the_team() {
        let mut g = game();
        g.chat(3, "psst", true);
        let h = heard(&mut g);
        assert_eq!(h.len(), 4);
        assert!(h.iter().all(|h| !h.1 && h.2 == ui::chat_tag::SPECTATOR));
    }

    #[test]
    fn the_dead_are_heard_only_by_those_not_playing_unless_dead_chat_is_on() {
        let mut g = game();
        g.clients[0].session = Session::Dead;
        g.clients[3].session = Session::Spectator;
        g.chat(0, "ghost", false);
        let h = heard(&mut g);
        assert_eq!(h.iter().map(|h| h.0).collect::<Vec<_>>(), [0, 3]);
        assert!(h.iter().all(|h| h.2 == ui::chat_tag::DEAD));
        g.cvars.set("g_deadChat", "1");
        g.chat(0, "ghost", false);
        assert_eq!(heard(&mut g).len(), 4);
    }

    #[test]
    fn a_line_is_cut_at_149_characters_and_keeps_a_scripts_localizer_marks() {
        let mut g = game();
        g.chat(0, &format!("\x14KEY\x15b{}", "x".repeat(300)), false);
        let text = heard(&mut g).remove(0).3;
        assert!(text.starts_with("\x14KEY\x15b"), "{text:?}");
        assert_eq!(text.chars().count(), 149);
    }

    #[test]
    fn anyone_not_playing_is_marked_not_only_the_dead() {
        let mut g = game();
        g.clients[1].session = Session::Intermission;
        g.chat(1, "gg", false);
        assert!(heard(&mut g).iter().all(|h| h.2 == ui::chat_tag::DEAD));
    }
}
