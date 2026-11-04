// SPDX-License-Identifier: GPL-3.0-only
//! The console's print channels and their commands (`con_channels.cpp`): `con_showchannel`, `con_hidechannel`,
//! `con_channellist` and `con_visiblechannellist`, shared by the client's console and the server's.
//!
//! Only the 25 built-in channels exist (`builtinChannels`, KisakCOD con_channels.cpp:240-264); the game message
//! window channels the original opens next are not modelled. Channel 0 (`dontfilter`) is never listed, shown or
//! hidden, as the original's loops start at 1.

/// The built-in channel names, by number (`builtinChannels`).
pub const NAMES: [&str; 25] = [
    "dontfilter",
    "error",
    "gamenotify",
    "boldgame",
    "subtitle",
    "obituary",
    "logfile_only",
    "console_only",
    "gfx",
    "sound",
    "files",
    "devgui",
    "profile",
    "ui",
    "client",
    "server",
    "system",
    "playerweap",
    "ai",
    "anim",
    "physics",
    "fx",
    "leaderboards",
    "parserscript",
    "script",
];

/// `CON_CHANNEL_ERROR`.
pub const ERROR: usize = 1;

/// Which channels the console shows (`pcGlob.filters[CON_DEST_CONSOLE]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Channels {
    shown: u32,
}

impl Default for Channels {
    /// `Con_InitChannels`: the console shows `con_default_console_filter`, whose default is `*`.
    fn default() -> Self {
        let mut c = Self { shown: 0 };
        c.filter("*", true);
        c
    }
}

impl Channels {
    /// `Con_IsChannelVisible(CON_DEST_CONSOLE, ..)`: `dontfilter` always is.
    pub fn visible(&self, channel: usize) -> bool {
        channel == 0 || (channel < NAMES.len() && self.shown & (1 << channel) != 0)
    }

    /// `Con_IsChannelVisible` for a message printed by `Com_PrintError` or `Com_PrintWarning`: its own channel, or
    /// the `error` channel when that is shown (KisakCOD con_channels.cpp:100-103).
    pub fn visible_error(&self, channel: usize) -> bool {
        self.visible(channel) || self.visible(ERROR)
    }

    /// `Con_FilterShowChannel`: shows or hides every channel `pattern` matches (`*` and `?`, any case); a line per
    /// channel that changed, or `No channels added or hidden`.
    fn filter(&mut self, pattern: &str, show: bool) -> Vec<String> {
        let mut out = Vec::new();
        for (ch, name) in NAMES.iter().enumerate().skip(1) {
            if !wild(pattern, name) || self.visible(ch) == show {
                continue;
            }
            self.shown ^= 1 << ch;
            out.push(format!(
                "{} channel: {name}",
                if show { "Adding" } else { "Hiding" }
            ));
        }
        if out.is_empty() {
            out.push("No channels added or hidden".to_owned());
        }
        out
    }

    /// Runs one of the channel commands (`Con_FilterAdd_f`, `Con_FilterRemove_f`, `Con_ChannelList_f`,
    /// `Con_FilterList_f`; con_channels.cpp:297-357): the lines it prints, or `None` when `command` is none of
    /// them.
    pub fn command(&mut self, command: &str, args: &[&str]) -> Option<Vec<String>> {
        let command_l = command.to_ascii_lowercase();
        let show = match command_l.as_str() {
            "con_showchannel" => true,
            "con_hidechannel" => false,
            "con_channellist" => return Some(NAMES[1..].iter().map(|n| (*n).to_owned()).collect()),
            "con_visiblechannellist" => {
                return Some(
                    (1..NAMES.len())
                        .filter(|&c| self.visible(c))
                        .map(|c| NAMES[c].to_owned())
                        .collect(),
                );
            }
            _ => return None,
        };
        if args.is_empty() {
            return Some(vec![
                format!("USAGE: {command} <channel>"),
                "<channel> may include wildcards */?".to_owned(),
            ]);
        }
        Some(args.iter().flat_map(|a| self.filter(a, show)).collect())
    }
}

/// `I_stricmpwild`: `*` any run, `?` any one character, any case.
pub fn wild(pattern: &str, name: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match p.split_first() {
            None => s.is_empty(),
            Some((b'*', rest)) => {
                rest.is_empty() || go(rest, s) || (!s.is_empty() && go(p, &s[1..]))
            }
            Some((&c, rest)) => s.split_first().is_some_and(|(&d, srest)| {
                (c == b'?' || c.eq_ignore_ascii_case(&d)) && go(rest, srest)
            }),
        }
    }
    go(pattern.as_bytes(), name.as_bytes())
}

/// The lines of `cmdlist [match]` (`Cmd_List_f`, KisakCOD cmd.cpp:1268-1288): every name `filter` matches
/// (`Com_Filter`, as [`wild`]) in the order given, then the count.
pub fn cmd_list<'a>(names: impl IntoIterator<Item = &'a str>, filter: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = names
        .into_iter()
        .filter(|n| filter.is_none_or(|f| wild(f, n)))
        .map(str::to_owned)
        .collect();
    let n = out.len();
    out.push(format!("{n} commands"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_or_warning_shows_on_its_channel_or_the_error_channel() {
        let mut c = Channels::default();
        c.command("con_hidechannel", &["system"]);
        assert!(!c.visible(16) && c.visible_error(16));
        c.command("con_hidechannel", &["error"]);
        assert!(!c.visible_error(16) && c.visible_error(8));
    }

    #[test]
    fn every_channel_starts_visible_and_hiding_is_by_pattern() {
        let mut c = Channels::default();
        assert!((0..NAMES.len()).all(|ch| c.visible(ch)));
        assert_eq!(
            c.command("con_hidechannel", &["error", "ph*"]).unwrap(),
            ["Hiding channel: error", "Hiding channel: physics"]
        );
        assert!(!c.visible(ERROR) && c.visible(0));
        assert_eq!(
            c.command("con_hidechannel", &["error"]).unwrap(),
            ["No channels added or hidden"]
        );
        let visible = c.command("con_visiblechannellist", &[]).unwrap();
        assert_eq!(visible.len(), 22);
        assert!(!visible.iter().any(|n| n == "error" || n == "dontfilter"));
        assert_eq!(
            c.command("con_showchannel", &["ERR?R"]).unwrap(),
            ["Adding channel: error"]
        );
        assert_eq!(c.command("con_channellist", &[]).unwrap().len(), 24);
        assert_eq!(
            c.command("con_showchannel", &[]).unwrap(),
            [
                "USAGE: con_showchannel <channel>",
                "<channel> may include wildcards */?"
            ]
        );
        assert_eq!(c.command("con_frob", &[]), None);
    }

    #[test]
    fn wildcards_match_as_the_original() {
        assert!(wild("*", "error") && wild("e*r", "error") && wild("?rror", "Error"));
        assert!(!wild("e*x", "error") && !wild("err", "error"));
        assert_eq!(
            cmd_list(["bind", "bindlist", "unbind"], Some("bind*")),
            ["bind", "bindlist", "2 commands"]
        );
    }
}
