// SPDX-License-Identifier: GPL-3.0-only
//! The developer-only builtins: script printing and its console channels, asserts, the debug drawings and the
//! script file functions.
//!
//! The compiler keeps a call to any of them outside a `/# ... #/` block only with `developer_script` set
//! (`gsc::Builtins::is_developer`), so what reaches these functions runs as the original's developer build would.
//!
//! Translated from KisakCOD `game_mp/g_scr_main_mp.cpp` (`GScr_CreatePrintChannel`, `GScr_printChannelSet`,
//! `print`, `println`, `GScr_print3d`, `GScr_line`, `assertCmd`, `assertexCmd`, `assertmsgCmd`: lines 467-548,
//! 717-836) and `client/con_channels.cpp` (`Con_OpenChannel`, `Con_ScriptHasPermission`, `Con_GetChannel`,
//! `Con_IsChannelOpen`, `Con_InitChannels`: lines 25-80, 274-282).

use gsc::{Value, Vm};

use super::FuncFn;
use super::Impl::{self, Real};
use super::args::{Args, display};
use crate::debugdraw::{DebugLine, DebugString};
use crate::game::Game;

type R = Result<Value, String>;

const fn r(f: FuncFn) -> Impl<FuncFn> {
    Real(f)
}

pub(super) const FUNCS: &[(&str, Impl<FuncFn>)] = &[
    ("createprintchannel", r(create_print_channel)),
    ("setprintchannel", r(set_print_channel)),
    ("print", r(print)),
    ("println", r(println)),
    ("print3d", r(print3d)),
    ("line", r(line)),
    ("assert", r(assert)),
    ("assertex", r(assertex)),
    ("assertmsg", r(assertmsg)),
    // The 1.7 executable binds all six to one empty function (0x4d74b0, a bare `ret`): they return nothing.
    ("openfile", r(nothing)),
    ("closefile", r(nothing)),
    ("fprintln", r(nothing)),
    ("fprintfields", r(nothing)),
    ("freadln", r(nothing)),
    ("fgetarg", r(nothing)),
];

/// `PrintChannelGlob::openChannels`' size.
pub const MAX_CHANNELS: usize = 256;
/// `CON_CHANNEL_SCRIPT`: the channel scripts print to unless they pick another.
pub const SCRIPT_CHANNEL: usize = 24;
/// `PrintChannel::name` holds 31 characters and the terminator.
const MAX_NAME: usize = 31;

/// `pcGlob.openChannels`: names by number, and whether scripts may print to each.
#[derive(Debug, Clone)]
pub struct PrintChannels {
    open: Vec<(String, bool)>,
}

impl Default for PrintChannels {
    /// `Con_InitChannels`: the built-in channels, of which only `script` takes script prints.
    fn default() -> Self {
        let open = logging::channels::NAMES
            .iter()
            .enumerate()
            .map(|(i, n)| ((*n).to_owned(), i == SCRIPT_CHANNEL))
            .collect();
        Self { open }
    }
}

impl PrintChannels {
    /// `Con_OpenChannel`: false only when every slot is taken; an existing name (any case) is left as it is.
    pub fn open(&mut self, name: &str, allow_script: bool) -> bool {
        let mut name = name.to_owned();
        if name.len() > MAX_NAME {
            let mut end = MAX_NAME;
            while !name.is_char_boundary(end) {
                end -= 1;
            }
            name.truncate(end);
        }
        if self.open.iter().any(|(n, _)| n.eq_ignore_ascii_case(&name)) {
            return true;
        }
        if self.open.len() >= MAX_CHANNELS {
            return false;
        }
        self.open.push((name, allow_script));
        true
    }

    /// `Con_GetChannel`.
    pub fn find(&self, name: &str) -> Option<usize> {
        self.open
            .iter()
            .position(|(n, _)| n.eq_ignore_ascii_case(name))
    }

    /// `Con_IsChannelOpen`.
    pub fn is_open(&self, channel: usize) -> bool {
        channel < self.open.len()
    }

    /// `Con_ScriptHasPermission`.
    pub fn script_may_print(&self, channel: usize) -> bool {
        self.open.get(channel).is_some_and(|(_, allow)| *allow)
    }

    /// The channel's name.
    pub fn name(&self, channel: usize) -> Option<&str> {
        self.open.get(channel).map(|(n, _)| n.as_str())
    }
}

/// `createprintchannel(name)`.
fn create_print_channel(g: &mut Game, _: &mut Vm, a: Args) -> R {
    if a.len() != 1 {
        return Err("illegal call to createprintchannel()".into());
    }
    let name = a.string(0)?;
    if !g.print_channels.open(name, true) {
        return Err("Unable to create new channel.  Maximum number of channels exeeded.".into());
    }
    Ok(Value::Undefined)
}

/// `setprintchannel(name or number)`: the channel scripts print to from now on; returns the one before.
fn set_print_channel(g: &mut Game, _: &mut Vm, a: Args) -> R {
    if a.len() != 1 {
        return Err("illegal call to setprintchannel()".into());
    }
    let channel = match a.get(0)? {
        Value::Str(s) => g.print_channels.find(s),
        Value::Int(n) => usize::try_from(*n)
            .ok()
            .filter(|&n| g.print_channels.is_open(n)),
        _ => None,
    };
    let Some(channel) = channel else {
        return Err("Invalid Print Channel".into());
    };
    if !g.print_channels.script_may_print(channel) {
        return Err("Script does not have permission to print to this channel".into());
    }
    let old = std::mem::replace(&mut g.level.script_print_channel, channel);
    Ok(Value::Int(old as i32))
}

/// `print(...)`/`println(...)`: every argument as text to the script print channel, unless `g_no_script_spam`.
fn print_args(g: &mut Game, a: &Args, newline: bool) -> R {
    if g.cvars.bool("g_no_script_spam") {
        return Ok(Value::Undefined);
    }
    let mut s: String = a.v.iter().map(display).collect();
    if newline {
        s.push('\n');
    }
    // The console has no per-channel filter here; the channel only decides whether scripts may print at all.
    g.print(s);
    Ok(Value::Undefined)
}

fn print(g: &mut Game, _: &mut Vm, a: Args) -> R {
    print_args(g, &a, false)
}

fn println(g: &mut Game, _: &mut Vm, a: Args) -> R {
    print_args(g, &a, true)
}

/// The optional `rgb` and `alpha` arguments at `at` and `at + 1` (white and opaque when absent).
fn color(a: &Args, at: usize) -> Result<[f32; 4], String> {
    let mut c = [1.0; 4];
    if a.len() > at {
        let rgb = a.vector(at)?;
        c[..3].copy_from_slice(&rgb);
    }
    if a.len() > at + 1 {
        c[3] = a.float(at + 1)?;
    }
    Ok(c)
}

/// `print3d(origin, text, [rgb], [alpha], [scale], [duration])`: text in the world for `duration` frames.
fn print3d(g: &mut Game, _: &mut Vm, a: Args) -> R {
    if !(2..=6).contains(&a.len()) {
        return Err("illegal call to print3d()".into());
    }
    let duration = if a.len() > 5 { a.int(5)? } else { 1 };
    let scale = if a.len() > 4 { a.float(4)? } else { 1.0 };
    let color = color(&a, 2)?;
    let text = a.string(1)?.to_owned();
    let origin = a.vector(0)?;
    g.debug_draw.add_string(
        DebugString {
            origin,
            color,
            scale,
            text,
        },
        duration,
    );
    Ok(Value::Undefined)
}

/// `line(start, end, [rgb], [alpha], [depthtest], [duration])`: a line in the world for `duration` frames.
fn line(g: &mut Game, _: &mut Vm, a: Args) -> R {
    if !(2..=6).contains(&a.len()) {
        return Err("illegal call to line()".into());
    }
    let duration = if a.len() > 5 { a.int(5)? } else { 0 };
    let depth_test = if a.len() > 4 { a.int(4)? != 0 } else { false };
    let color = color(&a, 2)?;
    let end = a.vector(1)?;
    let start = a.vector(0)?;
    g.debug_draw.add_line(
        DebugLine {
            start,
            end,
            color,
            depth_test,
        },
        duration,
    );
    Ok(Value::Undefined)
}

/// `assert(condition)`.
fn assert(_: &mut Game, _: &mut Vm, a: Args) -> R {
    if a.int(0)? == 0 {
        return Err("assert fail".into());
    }
    Ok(Value::Undefined)
}

/// `assertex(condition, message)`.
fn assertex(_: &mut Game, _: &mut Vm, a: Args) -> R {
    if a.int(0)? == 0 {
        return Err(format!("assert fail: {}", a.string(1)?));
    }
    Ok(Value::Undefined)
}

/// `assertmsg(message)`: always fails.
fn assertmsg(_: &mut Game, _: &mut Vm, a: Args) -> R {
    Err(format!("assert fail: {}", a.string(0)?))
}

fn nothing(_: &mut Game, _: &mut Vm, _: Args) -> R {
    Ok(Value::Undefined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_script_channel_takes_script_prints_until_scripts_open_their_own() {
        let mut c = PrintChannels::default();
        assert_eq!(c.find("SCRIPT"), Some(SCRIPT_CHANNEL));
        assert!(c.script_may_print(SCRIPT_CHANNEL));
        assert!(!c.script_may_print(1) && c.is_open(1));
        assert!(c.open("mine", true));
        let mine = c.find("Mine").unwrap();
        assert_eq!(mine, logging::channels::NAMES.len());
        assert!(c.script_may_print(mine));
        // Opening a built-in by name changes nothing.
        assert!(c.open("error", true));
        assert!(!c.script_may_print(1));
        while c.open.len() < MAX_CHANNELS {
            let n = format!("c{}", c.open.len());
            assert!(c.open(&n, true));
        }
        assert!(!c.open("one_too_many", true));
        assert!(c.open("mine", true), "an open name still opens");
        assert!(!c.is_open(MAX_CHANNELS));
    }
}
