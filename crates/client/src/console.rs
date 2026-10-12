// SPDX-License-Identifier: GPL-3.0-only
//! The text the player types outside the menus: the chat line (`chatmodepublic`, `chatmodeteam`) and the console
//! (`~`). One field serves both, as at the original's `Message_Key` and `Console_Key`; this module is the keys and
//! the field's text, the drawing is [`crate::hud::draw_console`].

use crate::ui::UiKey;

/// What the original's chat field holds: `say` carries at most this many characters to the other players.
pub const CHAT_MAX: usize = 149;
/// Lines the console remembers to recall with the arrow keys.
const HISTORY: usize = 64;

/// Commands the client runs for the console (Tab completes them).
const COMMANDS: &[&str] = &[
    "bind",
    "callvote",
    "connect",
    "devmap",
    "disconnect",
    "dvardump",
    "dvarlist",
    "exec",
    "map",
    "playdemo",
    "quit",
    "rcon",
    "record",
    "say",
    "say_team",
    "screenshot",
    "screenshotjpeg",
    "set",
    "seta",
    "stoprecord",
    "toggle",
    "unbind",
    "unbindall",
    "vid_restart",
    "vote",
    "vstr",
];

/// What a console word names: what Tab completes it from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arg {
    Cvar,
    Map,
    Demo,
    /// A config file of the install (`exec`).
    Cfg,
}

/// A completion request: the names of `kind` that start with the lowercase `prefix`.
pub struct Want<'a> {
    pub kind: Arg,
    pub prefix: &'a str,
}

/// What the first argument of a command names, if it completes.
fn arg_kind(command: &str) -> Option<Arg> {
    match command {
        "map" | "devmap" => Some(Arg::Map),
        "playdemo" | "demo" => Some(Arg::Demo),
        "exec" => Some(Arg::Cfg),
        "set" | "seta" | "sets" | "setu" | "toggle" | "vstr" | "setfromdvar" => Some(Arg::Cvar),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Closed,
    Chat {
        team: bool,
    },
    Console,
}

/// What Enter on a non-empty field asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Entered {
    Say { team: bool, text: String },
    Line(String),
}

#[derive(Default)]
pub struct Console {
    pub mode: Mode,
    pub text: Vec<char>,
    /// Where the next character goes, in characters.
    pub cursor: usize,
    history: Vec<String>,
    /// The history line shown in the field (counted from the oldest), while the arrows are walking it.
    recall: Option<usize>,
}

impl Console {
    /// The keys go here and not to the game or the menus.
    pub fn active(&self) -> bool {
        self.mode != Mode::Closed
    }

    pub fn open_chat(&mut self, team: bool) {
        self.mode = Mode::Chat { team };
        self.clear();
    }

    /// `~`: opens the console, or closes it (a chat line open is dropped for it).
    pub fn toggle(&mut self) {
        self.mode = if self.mode == Mode::Console {
            Mode::Closed
        } else {
            Mode::Console
        };
        self.clear();
    }

    pub fn close(&mut self) {
        self.mode = Mode::Closed;
        self.clear();
    }

    fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.recall = None;
    }

    pub fn field(&self) -> String {
        self.text.iter().collect()
    }

    fn set_field(&mut self, s: &str) {
        self.text = s.chars().collect();
        self.cursor = self.text.len();
    }

    /// One key. `complete` lists the names a console word may be completed to (cvars; the commands are known here).
    pub fn key(&mut self, key: UiKey, complete: impl Fn(Want) -> Vec<String>) -> Option<Entered> {
        let console = self.mode == Mode::Console;
        let max = if console { 255 } else { CHAT_MAX };
        match key {
            UiKey::Escape => self.close(),
            UiKey::Enter => {
                let line = self.field();
                let line = line.trim();
                if line.is_empty() {
                    if !console {
                        self.close();
                    }
                    return None;
                }
                let line = line.to_owned();
                let entered = match self.mode {
                    Mode::Chat { team } => Entered::Say { team, text: line },
                    _ => {
                        if self.history.last() != Some(&line) {
                            self.history.push(line.clone());
                            if self.history.len() > HISTORY {
                                self.history.remove(0);
                            }
                        }
                        Entered::Line(line)
                    }
                };
                if console {
                    self.clear();
                } else {
                    self.close();
                }
                return Some(entered);
            }
            UiKey::Char(c) if !c.is_control() && self.text.len() < max => {
                self.text.insert(self.cursor, c);
                self.cursor += 1;
            }
            UiKey::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.text.remove(self.cursor);
            }
            UiKey::Delete if self.cursor < self.text.len() => {
                self.text.remove(self.cursor);
            }
            UiKey::Left => self.cursor = self.cursor.saturating_sub(1),
            UiKey::Right => self.cursor = (self.cursor + 1).min(self.text.len()),
            UiKey::Home => self.cursor = 0,
            UiKey::End => self.cursor = self.text.len(),
            UiKey::Up if console => {
                let at = self.recall.unwrap_or(self.history.len()).saturating_sub(1);
                if let Some(line) = self.history.get(at).cloned() {
                    self.recall = Some(at);
                    self.set_field(&line);
                }
            }
            UiKey::Down if console => match self.recall {
                Some(at) if at + 1 < self.history.len() => {
                    self.recall = Some(at + 1);
                    let line = self.history[at + 1].clone();
                    self.set_field(&line);
                }
                Some(_) => self.clear(),
                None => {}
            },
            UiKey::Tab if console => self.complete_word(&complete),
            _ => {}
        }
        None
    }

    /// Tab: the first word becomes the longest start the commands and cvars that begin with it share; the word
    /// after a command that takes a map, demo, config or cvar name completes to those.
    fn complete_word(&mut self, complete: &impl Fn(Want) -> Vec<String>) {
        let line = self.field();
        if line.is_empty() {
            return;
        }
        let (head, mut names) = match line.split_once(char::is_whitespace) {
            None => {
                let want = line.to_ascii_lowercase();
                let names = COMMANDS
                    .iter()
                    .filter(|c| c.starts_with(&want))
                    .map(|c| (*c).to_owned())
                    .chain(complete(Want {
                        kind: Arg::Cvar,
                        prefix: &want,
                    }))
                    .collect();
                (String::new(), names)
            }
            Some((command, rest)) => {
                let Some(kind) = arg_kind(&command.to_ascii_lowercase()) else {
                    return;
                };
                if rest.contains(char::is_whitespace) {
                    return;
                }
                let prefix = rest.to_ascii_lowercase();
                (
                    format!("{command} "),
                    complete(Want {
                        kind,
                        prefix: &prefix,
                    }),
                )
            }
        };
        names.sort();
        names.dedup();
        let Some(first) = names.first() else { return };
        let mut common = first.len();
        for n in &names[1..] {
            common = common.min(
                first
                    .bytes()
                    .zip(n.bytes())
                    .take_while(|(a, b)| a == b)
                    .count(),
            );
        }
        let mut done = format!("{head}{}", &first[..common]);
        if names.len() == 1 {
            done.push(' ');
        }
        if done.len() > line.len() {
            self.set_field(&done);
        }
    }
}

/// The client commands the server carries out; every other command is the client's own.
const SERVER_VERBS: &[&str] = &[
    "say",
    "say_team",
    "callvote",
    "vote",
    "kill",
    "follownext",
    "followprev",
    "where",
];

/// Every command the client's menus, binds and console carry out (besides the `+`/`-` ones and the server's), for
/// telling a typed word that is none of them.
const OWN_COMMANDS: &[&str] = &[
    "actionslot",
    "bind",
    "bots",
    "chatmodepublic",
    "chatmodeteam",
    "connect",
    "devmap",
    "demo",
    "disconnect",
    "drawvehicles",
    "dvardump",
    "dvarlist",
    "exec",
    "gocrouch",
    "goprone",
    "loc_warnings",
    "map",
    "name",
    "playdemo",
    "quit",
    "r_applypicmip",
    "rcon",
    "reconnect",
    "record",
    "screenshot",
    "screenshotjpeg",
    "seta",
    "selectstringtableentryindvar",
    "set",
    "setdvartotime",
    "setfromdvar",
    "sets",
    "setu",
    "setprofile",
    "snd_restart",
    "statclearbitmask",
    "statclearperknew",
    "statgetindvar",
    "statset",
    "statsetusingtable",
    "stoprecord",
    "toggle",
    "toggleconsole",
    "togglefullscreen",
    "togglemenu",
    "unbind",
    "unbindall",
    "updatedvarsfromprofile",
    "vid_restart",
    "vstr",
    "wait",
    "weapnext",
    "weapprev",
    "writeconfig",
];

/// Whether a console word is a command at all (a cvar is another matter).
pub fn is_command(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with(['+', '-']) || is_server_verb(&n) || OWN_COMMANDS.contains(&n.as_str())
}

/// Whether `name` (a command's first word) goes to the server.
pub fn is_server_verb(name: &str) -> bool {
    SERVER_VERBS.iter().any(|v| name.eq_ignore_ascii_case(v))
}

/// The line a server command is sent as: `say "text"`, `callvote clientkick 3`. The server's tokenizer has no
/// escapes, so a quote in a word becomes an apostrophe, and control characters (which steer the localizer) go.
/// `None` for a `say` with nothing to say.
pub fn server_line(cmd: &[String]) -> Option<String> {
    let name = cmd.first()?.to_ascii_lowercase();
    let clean = |s: &str| -> String {
        s.chars()
            .filter(|c| !c.is_control())
            .map(|c| if c == '"' { '\'' } else { c })
            .collect()
    };
    if name.starts_with("say") {
        let text = clean(&cmd[1..].join(" "));
        return (!text.trim().is_empty()).then(|| format!("{name} \"{text}\""));
    }
    let words: Vec<String> = cmd[1..]
        .iter()
        .map(|w| {
            let w = clean(w);
            if w.is_empty() || w.contains(char::is_whitespace) {
                format!("\"{w}\"")
            } else {
                w
            }
        })
        .collect();
    Some(format!("{name} {}", words.join(" ")).trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_in(c: &mut Console, s: &str) {
        for ch in s.chars() {
            c.key(UiKey::Char(ch), |_| Vec::new());
        }
    }

    fn enter(c: &mut Console) -> Option<Entered> {
        c.key(UiKey::Enter, |_| Vec::new())
    }

    #[test]
    fn a_chat_line_goes_out_as_typed_and_closes_the_field() {
        let mut c = Console::default();
        c.open_chat(true);
        type_in(&mut c, "hello  team");
        assert_eq!(
            enter(&mut c),
            Some(Entered::Say {
                team: true,
                text: "hello  team".into()
            })
        );
        assert!(!c.active());
    }

    #[test]
    fn an_empty_or_escaped_chat_line_says_nothing() {
        let mut c = Console::default();
        c.open_chat(false);
        assert_eq!(enter(&mut c), None);
        assert!(!c.active(), "Enter on nothing leaves the field");
        c.open_chat(false);
        type_in(&mut c, "oops");
        c.key(UiKey::Escape, |_| Vec::new());
        assert!(!c.active());
        c.open_chat(false);
        assert!(c.text.is_empty(), "a closed line is not kept");
    }

    #[test]
    fn a_chat_line_is_no_longer_than_the_server_keeps() {
        let mut c = Console::default();
        c.open_chat(false);
        type_in(&mut c, &"x".repeat(CHAT_MAX + 30));
        assert_eq!(c.text.len(), CHAT_MAX);
    }

    #[test]
    fn editing_moves_the_cursor_through_the_text() {
        let mut c = Console::default();
        c.toggle();
        type_in(&mut c, "mp");
        c.key(UiKey::Left, |_| Vec::new());
        type_in(&mut c, "a");
        c.key(UiKey::Home, |_| Vec::new());
        type_in(&mut c, "m");
        c.key(UiKey::End, |_| Vec::new());
        c.key(UiKey::Backspace, |_| Vec::new());
        assert_eq!(c.field(), "mma");
    }

    #[test]
    fn the_console_stays_open_and_recalls_lines_with_the_arrows() {
        let mut c = Console::default();
        c.toggle();
        for l in ["set a 1", "map mp_crash", "map mp_crash"] {
            type_in(&mut c, l);
            assert_eq!(enter(&mut c), Some(Entered::Line(l.into())));
            assert!(c.active());
        }
        c.key(UiKey::Up, |_| Vec::new());
        assert_eq!(c.field(), "map mp_crash");
        c.key(UiKey::Up, |_| Vec::new());
        assert_eq!(c.field(), "set a 1", "a repeated line is kept once");
        c.key(UiKey::Down, |_| Vec::new());
        c.key(UiKey::Down, |_| Vec::new());
        assert!(c.text.is_empty(), "past the newest line is a blank field");
    }

    #[test]
    fn tab_completes_commands_and_cvars_to_what_they_share() {
        let names = |w: Want| -> Vec<String> {
            let all: &[&str] = match w.kind {
                Arg::Cvar => &["cg_fov", "cg_fovscale"],
                Arg::Map => &["mp_crossfire", "mp_crossroads", "mp_strike"],
                Arg::Cfg => &["default_mp.cfg"],
                Arg::Demo => &[],
            };
            all.iter()
                .filter(|n| n.starts_with(w.prefix))
                .map(|n| (*n).to_owned())
                .collect()
        };
        let mut c = Console::default();
        c.toggle();
        type_in(&mut c, "cg_f");
        c.key(UiKey::Tab, names);
        assert_eq!(c.field(), "cg_fov", "two names share cg_fov");
        c.clear();
        type_in(&mut c, "disc");
        c.key(UiKey::Tab, names);
        assert_eq!(c.field(), "disconnect ", "one match completes the word");
        // The word after `map` is a map, after `exec` a config file; a command with no such argument stays.
        c.clear();
        type_in(&mut c, "map mp_cr");
        c.key(UiKey::Tab, names);
        assert_eq!(c.field(), "map mp_cross", "two maps share mp_cross");
        c.clear();
        type_in(&mut c, "exec def");
        c.key(UiKey::Tab, names);
        assert_eq!(c.field(), "exec default_mp.cfg ");
        c.clear();
        type_in(&mut c, "say mp_cr");
        c.key(UiKey::Tab, names);
        assert_eq!(c.field(), "say mp_cr");
    }

    #[test]
    fn toggling_the_console_drops_a_chat_line_and_back() {
        let mut c = Console::default();
        c.open_chat(false);
        type_in(&mut c, "half");
        c.toggle();
        assert_eq!(c.mode, Mode::Console);
        assert!(c.text.is_empty());
        c.toggle();
        assert!(!c.active());
    }

    #[test]
    fn server_lines_keep_what_the_server_tokenizer_can_read() {
        let w = |s: &[&str]| s.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            server_line(&w(&["say", "he said", "\"no\""])).as_deref(),
            Some("say \"he said 'no'\"")
        );
        assert_eq!(server_line(&w(&["say"])), None);
        assert_eq!(
            server_line(&w(&["callvote", "clientkick", "3"])).as_deref(),
            Some("callvote clientkick 3")
        );
        assert_eq!(
            server_line(&w(&["CALLVOTE", "map", "mp crash"])).as_deref(),
            Some("callvote map \"mp crash\"")
        );
        assert_eq!(server_line(&w(&["vote"])).as_deref(), Some("vote"));
        assert!(is_server_verb("Say_Team") && is_server_verb("kill") && !is_server_verb("map"));
        assert!(is_command("MAP") && is_command("+attack") && is_command("rcon"));
        assert!(!is_command("frobnicate"));
    }
}
