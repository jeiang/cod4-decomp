// SPDX-License-Identifier: GPL-3.0-only
//! The text the player types outside the menus: the chat line (`chatmodepublic`, `chatmodeteam`) and the console
//! (`~`). One field serves both, as at the original's `Message_Key` and `Console_Key`; this module is the keys and
//! the field's text, the drawing is [`crate::hud::draw_console`].

use crate::ui::UiKey;

/// What the original's chat field holds: `say` carries at most this many characters to the other players.
pub const CHAT_MAX: usize = 149;
/// Lines the console remembers to recall with the arrow keys.
const HISTORY: usize = 64;

/// Who carries out a client command. Typed, bound, menu and config lines all reach the owner the same way: the
/// shell's command loop and the input layer route by this table, so a word known to one is known to both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// The input layer itself (`Input::exec_tokens`, `Input::run`): binds, dvars, stances, console channels.
    Input,
    /// The shell's command loop (`HostCx::run_buffer`), which a bind hands it to through `pending_commands`.
    Shell,
    /// The app's `console_action`.
    App,
    /// Read from the input frame's `pending_commands` by the app or the net play (`weapnext`, `togglemenu`, ...).
    Frame,
    /// The server's, sent through [`server_line`] (`say`, `callvote`, ...).
    Server,
    /// A cgame command with no handler (`CG_InitConsoleCommands`, KisakCOD cg_consolecmds_mp.cpp:84-110): in a
    /// match it goes to the server as typed, otherwise it is unknown, as the cgame is not loaded.
    Forward,
    /// Stock, but not carried out by this build: a warning names it and the issue that tracks it ([`MISSING`]).
    Missing,
}

/// Every command the client knows besides the `+`/`-` buttons ([`BUTTONS`]), by its registered name: the stock
/// multiplayer client's `Cmd_AddCommand` names (KisakCOD 1.8, checked against the strings and registrations of
/// `iw3mp.exe` 1.7) and this build's own (`bots`, `name`, `playdemo`, `toggleconsole`, `drawvehicles`, the stats
/// verbs the menus run, ...).
const COMMANDS: &[(&str, Owner)] = {
    use Owner::*;
    &[
        // cl_keys.cpp:1462-1465, dvar_cmds.cpp:525-537, cmd.cpp:441-446, con_channels.cpp:281-284.
        ("bind", Input),
        ("bindlist", Input),
        ("unbind", Input),
        ("unbindall", Input),
        ("set", Input),
        ("seta", Input),
        ("sets", Input),
        ("setu", Input),
        ("toggle", Input),
        ("togglep", Input),
        ("reset", Input),
        ("dvarlist", Input),
        ("dvardump", Input),
        ("cmdlist", Input),
        ("exec", Input),
        ("vstr", Input),
        ("wait", Input),
        ("con_channellist", Input),
        ("con_showchannel", Input),
        ("con_hidechannel", Input),
        ("con_visiblechannellist", Input),
        // dvar_cmds.cpp:540-660, cl_main_mp.cpp:602 (setenv), win_main.cpp:591 (in_restart).
        ("dvar_bool", Input),
        ("dvar_int", Input),
        ("dvar_float", Input),
        ("setenv", Input),
        ("in_restart", Input),
        // cl_input.cpp:973-1047: the stance and ADS commands.
        ("gocrouch", Input),
        ("goprone", Input),
        ("togglecrouch", Input),
        ("toggleprone", Input),
        ("lowerstance", Input),
        ("raisestance", Input),
        ("toggleads", Input),
        ("leaveads", Input),
        // cl_main_mp.cpp, cl_console.cpp:364, ui_main_mp.cpp, dvar_cmds.cpp, win_storage.cpp, r_cmds.cpp.
        ("cinematic", Shell),
        ("unskippablecinematic", Shell),
        ("logo", Shell),
        ("clear", Shell),
        ("closemenu", Shell),
        ("openmenu", Shell),
        ("openScriptMenu", Shell),
        ("connect", Shell),
        ("reconnect", Shell),
        ("disconnect", Shell),
        ("demo", Shell),
        ("playdemo", Shell),
        ("map", Shell),
        ("devmap", Shell),
        ("vid_restart", Shell),
        ("r_applyPicmip", Shell),
        ("snd_restart", Shell),
        ("writeconfig", Shell),
        ("setfromdvar", Shell),
        ("setdvartotime", Shell),
        ("setfromlocString", Shell),
        ("selectStringTableEntryInDvar", Shell),
        ("statSet", Shell),
        ("statGetInDvar", Shell),
        ("statsetusingtable", Shell),
        ("statclearbitmask", Shell),
        ("statclearperknew", Shell),
        ("setprofile", Shell),
        ("updatedvarsfromprofile", Shell),
        ("startSingleplayer", Shell),
        // common.cpp:1665, com_files.cpp:1819-1925, cl_main_mp.cpp:2879-3762, cl_main_pc_mp.cpp:422,
        // win_storage.cpp:802-809.
        ("writedefaults", Shell),
        ("dir", Shell),
        ("fdir", Shell),
        ("path", Shell),
        ("fullpath", Shell),
        ("fs_openedList", Shell),
        ("localservers", Shell),
        ("globalservers", Shell),
        ("ping", Shell),
        ("serverstatus", Shell),
        ("readStats", Shell),
        ("uploadStats", Shell),
        ("resetStats", Shell),
        // cl_main_mp.cpp (rcon, record, stoprecord, cmd), cl_input.cpp (centerview), cg_consolecmds_mp.cpp
        // (mp_QuickMessage, viewpos); `bots` and `name` are this build's.
        ("rcon", App),
        ("record", App),
        ("stoprecord", App),
        ("cmd", App),
        ("centerview", App),
        ("mp_QuickMessage", App),
        ("viewpos", App),
        ("bots", App),
        ("name", App),
        // cl_main_mp.cpp:821-843, 3922, com_memory.cpp:282, cg_consolecmds_mp.cpp:279-330, snd.cpp:285-500 and
        // 3344, com_sndalias.cpp:542.
        ("clientinfo", App),
        ("configstrings", App),
        ("showip", App),
        ("meminfo", App),
        ("VoiceChat", App),
        ("VoiceTeamChat", App),
        ("snd_list", App),
        ("snd_playLocal", App),
        ("snd_setEq", App),
        ("snd_setEqFreq", App),
        ("snd_setEqGain", App),
        ("snd_setEqQ", App),
        ("snd_setEqType", App),
        ("snd_deactivateEq", App),
        ("quit", Frame),
        ("toggleMenu", Frame),
        ("chatmodepublic", Frame),
        ("chatmodeteam", Frame),
        ("toggleconsole", Frame),
        ("togglefullscreen", Frame),
        ("screenshot", Frame),
        ("screenshotJpeg", Frame),
        ("weapnext", Frame),
        ("weapprev", Frame),
        ("actionslot", Frame),
        // This build's same-camera comparison switches (the net play reads them).
        ("drawvehicles", Frame),
        ("drawprops", Frame),
        ("drawscriptmodels", Frame),
        ("say", Server),
        ("say_team", Server),
        ("callvote", Server),
        ("vote", Server),
        ("kill", Server),
        ("follownext", Server),
        ("followprev", Server),
        ("where", Server),
        ("mr", Forward),
        ("give", Forward),
        ("take", Forward),
        ("god", Forward),
        ("demigod", Forward),
        ("notarget", Forward),
        ("noclip", Forward),
        ("ufo", Forward),
        ("levelshot", Forward),
        ("setviewpos", Forward),
        ("jumptonode", Forward),
        ("stats", Forward),
        ("team", Forward),
        ("follow", Forward),
        ("printentities", Forward),
        ("muteplayer", Forward),
        ("unmuteplayer", Forward),
        ("VisionSetNaked", Forward),
        ("VisionSetNight", Forward),
    ]
};

/// The stock multiplayer client commands this build accepts but does not carry out, each with the issue that
/// tracks it: the warning names both instead of `Unknown command`. Every one is registered by `iw3mp.exe` 1.7
/// with a handler that is not empty. KisakCOD 1.8 names 1.7 lacks (`gfx_stats`, `devgui_dvar`, `loadzone`,
/// `statGet`, `listmenus`, `fxTest`, ...) are not here and are unknown, as in 1.7. `error`, `crash`, `freeze`
/// and `assert` are not here either: the original registers them only with `com_developer`
/// (common.cpp:1328-1334).
const MISSING: &[(&str, u32)] = &[
    ("cubemapShot", 527),
    ("imagelist", 527),
    ("reloadmaterialtextures", 527),
    ("r_loadsun", 527),
    ("r_savesun", 527),
    ("r_smc_flush", 527),
    ("r_smc_stats", 527),
    ("devgui", 528),
    ("updatehunkusage", 528),
    ("phys_go", 529),
    ("phys_stop", 529),
    ("ragdoll_baselerp_bone", 529),
    ("ragdoll_bone", 529),
    ("ragdoll_clear", 529),
    ("ragdoll_joint", 529),
    ("ragdoll_limit", 529),
    ("ragdoll_pin_bone", 529),
    ("ragdoll_selfpair", 529),
    ("touchFile", 530),
    ("fs_referencedList", 530),
    ("net_dumpprofile", 531),
    ("net_restart", 531),
    ("snd_refreshSpeakerMaps", 532),
    ("snd_refreshVolumeModGroups", 532),
    ("snd_setEnvironmentEffects", 532),
    ("snd_deactivateEnvironmentEffects", 532),
    ("localizeSoundAliasFiles", 532),
    ("cg_shellshock", 533),
    ("cg_shellshock_load", 533),
    ("cg_shellshock_save", 533),
    ("timedemo", 534),
];

/// The `+`/`-` button commands (KisakCOD cl_input.cpp:973-1047, and the cgame's `+scores` and `+actionslot`,
/// cg_consolecmds_mp.cpp). A `+` word not among them is unknown, typed or bound, as the original never forwards
/// one (`CL_ForwardCommandToServer`).
pub const BUTTONS: &[&str] = &[
    "actionslot",
    "activate",
    "attack",
    "back",
    "breath_sprint",
    "forward",
    "frag",
    "gostand",
    "holdbreath",
    "leanleft",
    "leanright",
    "left",
    "lookdown",
    "lookup",
    "melee",
    "melee_breath",
    "mlook",
    "movedown",
    "moveleft",
    "moveright",
    "moveup",
    "prone",
    "reload",
    "right",
    "scores",
    "smoke",
    "speed",
    "speed_throw",
    "sprint",
    "stance",
    "strafe",
    "talk",
    "throw",
    "toggleads_throw",
    "usereload",
];

/// Who carries out `name` (any case); `None` for a word that is no client command (a dvar, or unknown).
pub fn owner(name: &str) -> Option<Owner> {
    if let Some(b) = name.strip_prefix(['+', '-']) {
        return BUTTONS
            .iter()
            .any(|n| n.eq_ignore_ascii_case(b))
            .then_some(Owner::Input);
    }
    COMMANDS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|&(_, o)| o)
        .or_else(|| missing_issue(name).map(|_| Owner::Missing))
}

/// The issue that tracks a stock command this build does not carry out (any case); `None` for any other word.
fn missing_issue(name: &str) -> Option<u32> {
    MISSING
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|&(_, issue)| issue)
}

/// Every command name the client registers, as registered (`cmdlist`, Tab): the commands, the missing ones and
/// the `+`/`-` buttons.
pub fn names() -> impl Iterator<Item = String> {
    COMMANDS
        .iter()
        .map(|(n, _)| (*n).to_owned())
        .chain(MISSING.iter().map(|(n, _)| (*n).to_owned()))
        .chain(
            BUTTONS
                .iter()
                .flat_map(|b| [format!("+{b}"), format!("-{b}")]),
        )
}

/// A stock command this build does not carry out: one warning per command name, naming the issue that tracks it,
/// which the console shows.
pub fn note_missing(name: &str) {
    let issue = missing_issue(name).unwrap_or_default();
    logging::dedup!(
        WARN,
        name,
        command = name,
        issue = issue,
        "stock command not implemented"
    );
}

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
                let names = names()
                    .filter(|c| c.to_ascii_lowercase().starts_with(&want))
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

/// Whether a console word is a client command at all (a dvar is another matter).
pub fn is_command(name: &str) -> bool {
    owner(name).is_some()
}

/// An unknown word typed at the console: a warning keyed by the command name (never its arguments, which may be a
/// password).
pub fn note_unknown(name: &str) {
    logging::dedup!(WARN, name, command = name, "unknown command");
}

/// Whether the app's `console_action` carries `name` out from a bind or config line (its own commands, and the
/// cgame's forwarded ones), not the input layer, the shell or the net play.
pub fn is_own_verb(name: &str) -> bool {
    matches!(owner(name), Some(Owner::App | Owner::Forward))
}

/// Whether `name` is a command the shell's command loop carries out (`HostCx::run_buffer`), which a bind or a
/// config line hands over to it through the input layer's `pending_commands`.
pub fn is_shell_verb(name: &str) -> bool {
    owner(name) == Some(Owner::Shell)
}

/// Whether the shell hands `name` to the app as a console action (`console_action`) instead of the input layer:
/// the server's verbs, the forwarded ones and the commands the app answers itself.
pub fn is_app_verb(name: &str) -> bool {
    matches!(
        owner(name),
        Some(Owner::App | Owner::Server | Owner::Forward)
    ) || name.eq_ignore_ascii_case("togglemenu")
}

/// Whether `name` (a command's first word) goes to the server.
pub fn is_server_verb(name: &str) -> bool {
    owner(name) == Some(Owner::Server)
}

/// Whether `name` is a cgame command without a handler, forwarded to the server as typed while in a match.
pub fn is_forward_verb(name: &str) -> bool {
    owner(name) == Some(Owner::Forward)
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

/// The `set`/`seta` line a variable write is handed to the listen server as (one dvar store in the original, #366):
/// the value quoted, with the server's tokenizer's limits ([`server_line`]).
pub fn set_line(name: &str, value: &str, archive: bool) -> String {
    let value: String = value
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if c == '"' { '\'' } else { c })
        .collect();
    let verb = if archive { "seta" } else { "set" };
    format!("{verb} {name} \"{value}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_in(c: &mut Console, s: &str) {
        for ch in s.chars() {
            c.key(UiKey::Char(ch), |_| Vec::new());
        }
    }

    #[test]
    fn an_unknown_command_warns_by_name() {
        let (_, events) = logging::capture(|| note_unknown("no_such_cmd_x"));
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert!(e.level == tracing::Level::WARN && e.field("command") == Some("no_such_cmd_x"));
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
        // `bots` once fell through to the input layer, which ignored it without a word.
        assert!(
            is_app_verb("bots") && is_app_verb("RCON") && is_app_verb("say") && !is_app_verb("map")
        );
        assert!(is_command("MAP") && is_command("+attack") && is_command("rcon"));
        assert!(!is_command("frobnicate"));
        // The video commands and the screenshot ones are known, so the console answers them itself.
        assert!(
            ["cinematic", "logo", "screenshotJpeg"]
                .iter()
                .all(|c| is_command(c))
        );
    }

    /// One name, one owner: a command listed twice (or both carried out and missing) would route by whichever
    /// came first.
    #[test]
    fn every_command_has_one_owner() {
        let mut seen = std::collections::HashSet::new();
        for n in names() {
            assert!(seen.insert(n.to_ascii_lowercase()), "{n} listed twice");
        }
        assert!(BUTTONS.iter().all(|b| *b == b.to_ascii_lowercase()));
        assert_eq!(owner("MeminFo"), Some(Owner::App));
        assert_eq!(owner("IMAGELIST"), Some(Owner::Missing));
        assert_eq!(owner("gfx_stats"), None, "1.8 only: unknown, as in 1.7");
        assert_eq!(owner("openscriptmenu"), Some(Owner::Shell));
        assert_eq!(owner("TOGGLEPRONE"), Some(Owner::Input));
        assert_eq!(owner("give"), Some(Owner::Forward));
        assert!(is_own_verb("viewpos") && is_own_verb("give") && !is_own_verb("say"));
    }

    /// Only the stock buttons are `+`/`-` commands: `+frobnicate` is unknown, typed or bound, as the original never
    /// forwards a `+` word (`CL_ForwardCommandToServer`).
    #[test]
    fn only_stock_buttons_are_plus_commands() {
        assert!(is_command("+attack") && is_command("-SCORES") && is_command("+melee_breath"));
        assert!(!is_command("+frobnicate") && !is_command("-frobnicate"));
        assert!(!is_command("+nightvision") && !is_command("+binoculars"));
    }

    #[test]
    fn a_missing_command_warns_naming_its_issue() {
        // A name no other test runs: the warning is rate limited per name across the test threads.
        let (_, events) = logging::capture(|| note_missing("r_smc_flush"));
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert!(e.level == tracing::Level::WARN && e.field("command") == Some("r_smc_flush"));
        assert_eq!(e.field("issue"), Some("527"));
    }
}
