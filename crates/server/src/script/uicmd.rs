// SPDX-License-Identifier: GPL-3.0-or-later
//! The builtins that talk to players' screens: client dvars, script menus, print lines,
//! announcements, chat, the winner configstring and the compass objectives.

use gsc::{EntClass, EntRef, Value, Vm};
use net::ui::{self, PrintKind, ServerCmd, cs, obj};

use super::Args;
use super::args::display;
use crate::client::Team;
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

/// Arguments `from..` as one message (`Scr_ConstructMessageString`): localized references keep
/// their `&`.
fn message(a: Args, from: usize) -> String {
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
        message(a, 1)
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
    print_to(g, Dest::Client(n), PrintKind::Normal, message(a, 0));
    Ok(Value::Undefined)
}

pub fn client_print_bold(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let n = player(g, e)?;
    print_to(g, Dest::Client(n), PrintKind::Bold, message(a, 0));
    Ok(Value::Undefined)
}

/// `iprintln` / `iprintlnbold` called on the level: everybody.
pub fn level_print(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let text = message(a, 0);
    g.print(format!("{text}\n"));
    print_to(g, Dest::All, PrintKind::Normal, text);
    Ok(Value::Undefined)
}

pub fn level_print_bold(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let text = message(a, 0);
    g.print(format!("{text}\n"));
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
    let text = message(a, 0);
    g.send(Dest::All, ServerCmd::Announce { text });
    Ok(Value::Undefined)
}

pub fn client_announcement(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let n = player(g, a.entity(0)?)?;
    let text = message(a, 1);
    g.send(Dest::Client(n), ServerCmd::Announce { text });
    Ok(Value::Undefined)
}

fn say(g: &mut Game, e: EntRef, a: Args, team: bool) -> R {
    let n = player(g, e)?;
    let text = message(a, 0);
    let c = g.client(n).expect("client");
    let (name, side) = (c.name.clone(), c.team);
    g.print(format!(
        "{}: {name}: {text}\n",
        if team { "sayteam" } else { "say" }
    ));
    let dest = if team { Dest::Team(side) } else { Dest::All };
    g.send(
        dest,
        ServerCmd::Chat {
            team,
            client: n,
            text,
        },
    );
    Ok(Value::Undefined)
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
