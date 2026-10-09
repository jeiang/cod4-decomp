// SPDX-License-Identifier: GPL-3.0-only
//! `callvote` and `vote` (`g_oldVoting`: the engine runs the vote, not the scripts): a player calls a vote for
//! `map`, `typemap`, `g_gametype`, `map_restart`, `map_rotate`, `kick` or `tempBanUser`, the other players
//! answer `vote y` or `vote n`, and a majority of the voting players runs the command three seconds later.
//! Bots do not vote (and do not count); spectators neither.

use std::collections::HashSet;

use net::ui::{PrintKind, ServerCmd, cs};

use crate::client::Team;
use crate::game::Game;
use crate::ui::Dest;

/// How long a vote runs, and how long a passed vote waits before it acts.
const VOTE_MS: i32 = 30_000;
const EXECUTE_DELAY_MS: i32 = 3_000;

/// A name without its `^n` colour codes.
pub(crate) fn clean_name(name: &str) -> String {
    let mut out = String::new();
    let mut it = name.chars().peekable();
    while let Some(c) = it.next() {
        if c == '^' && it.peek().is_some_and(char::is_ascii_digit) {
            it.next();
        } else {
            out.push(c);
        }
    }
    out
}

pub struct Vote {
    /// Console lines the vote runs when it passes.
    commands: Vec<String>,
    end: i32,
    yes: i32,
    no: i32,
    cast: HashSet<u16>,
}

#[derive(Default)]
pub struct VoteState {
    current: Option<Vote>,
    /// A passed vote's lines and when they run.
    pending: Option<(i32, Vec<String>)>,
}

impl Game {
    fn tell(&mut self, n: u16, text: &str) {
        self.send(
            Dest::Client(n),
            ServerCmd::Print {
                kind: PrintKind::Console,
                text: format!("{text}\n"),
            },
        );
    }

    /// Players who may vote: connected people on a team.
    fn voters(&self) -> Vec<u16> {
        self.connected_clients()
            .filter(|(_, c)| !c.bot && c.team != Team::Spectator)
            .map(|(n, _)| n)
            .collect()
    }

    /// A client's `callvote <what> [args]`: what is refused is told to the caller.
    pub fn call_vote(&mut self, n: u16, argv: &[String]) {
        if let Err(why) = self.start_vote(n, argv) {
            self.tell(n, &why);
        }
    }

    fn start_vote(&mut self, n: u16, argv: &[String]) -> Result<(), String> {
        if !self.cvars.bool("g_allowvote") {
            return Err("Voting is not enabled on this server.".into());
        }
        if self.connected_clients().count() < 2 {
            return Err("Not enough players to call a vote.".into());
        }
        if self.vote.current.is_some() {
            return Err("A vote is already in progress.".into());
        }
        if self.client(n).is_none_or(|c| c.team == Team::Spectator) {
            return Err("Spectators cannot call a vote.".into());
        }
        let arg = |i: usize| argv.get(i).map_or("", String::as_str);
        if argv.iter().any(|a| a.contains(';') || a.contains('"')) {
            return Err("Invalid vote string.".into());
        }
        let gametype_ok = |g: &str| {
            self.content
                .rawfile(&format!("maps/mp/gametypes/{}.gsc", g.to_ascii_lowercase()))
                .is_some()
                && !g.starts_with('_')
        };
        let map_ok = |m: &str| self.map_exists(m);
        let (commands, shown): (Vec<String>, String) = match arg(0).to_ascii_lowercase().as_str() {
            "map" => {
                if !map_ok(arg(1)) {
                    return Err(format!("{} is not a valid map.", arg(1)));
                }
                (
                    vec![format!("map {}", arg(1))],
                    format!("Change map to {}", arg(1)),
                )
            }
            "typemap" => {
                let (gt, map) = (arg(1), arg(2));
                if !gametype_ok(gt) {
                    return Err(format!("{gt} is not a valid gametype."));
                }
                if !map_ok(map) {
                    return Err(format!("{map} is not a valid map."));
                }
                (
                    vec![format!("g_gametype {gt}"), format!("map {map}")],
                    format!("Change gametype to {gt} on {map}"),
                )
            }
            "g_gametype" => {
                if !gametype_ok(arg(1)) {
                    return Err(format!("{} is not a valid gametype.", arg(1)));
                }
                (
                    vec![format!("g_gametype {}", arg(1)), "map_restart".into()],
                    format!("Change gametype to {}", arg(1)),
                )
            }
            "map_restart" => (vec!["fast_restart".into()], "Restart the map".into()),
            "map_rotate" => (vec!["map_rotate".into()], "Play the next map".into()),
            what @ ("kick" | "tempbanuser" | "clientkick" | "tempbanclient") => {
                // `clientkick` and `tempBanClient` take a slot; `kick` and `tempBanUser` (the stock menu) a name.
                let by_number = matches!(what, "clientkick" | "tempbanclient");
                let target = self.connected_clients().find(|(k, c)| {
                    if by_number {
                        arg(1).parse::<u16>().ok() == Some(*k)
                    } else {
                        clean_name(&c.name).eq_ignore_ascii_case(&clean_name(arg(1)))
                    }
                });
                let Some((k, c)) = target else {
                    return Err("That client is not on the server.".into());
                };
                let ban = what.starts_with("tempban");
                (
                    vec![format!(
                        "{} {k}",
                        if ban { "tempbanclient" } else { "clientkick" }
                    )],
                    format!(
                        "{} {}",
                        if ban { "Temporarily ban" } else { "Kick" },
                        c.name
                    ),
                )
            }
            _ => {
                return Err(
                    "Invalid vote string. Votes: map_restart, map_rotate, typemap, map, g_gametype, kick, tempBanUser."
                        .into(),
                );
            }
        };
        let end = self.level.time + VOTE_MS;
        let caller = self.client(n).map_or_else(String::new, |c| c.name.clone());
        self.send(
            Dest::All,
            ServerCmd::Print {
                kind: PrintKind::Normal,
                text: format!("{caller} called a vote: {shown}"),
            },
        );
        self.vote.current = Some(Vote {
            commands,
            end,
            yes: 1,
            no: 0,
            cast: HashSet::from([n]),
        });
        self.set_configstring(cs::VOTE_TIME, &format!("{end} 0"));
        self.set_configstring(cs::VOTE_STRING, &shown);
        self.set_configstring(cs::VOTE_YES, "1");
        self.set_configstring(cs::VOTE_NO, "0");
        Ok(())
    }

    /// A client's `vote y|n`.
    pub fn cast_vote(&mut self, n: u16, answer: &str) {
        let spectator = self.client(n).is_none_or(|c| c.team == Team::Spectator);
        let Some(v) = self.vote.current.as_mut() else {
            return self.tell(n, "No vote in progress.");
        };
        if spectator {
            return self.tell(n, "Spectators cannot vote.");
        }
        if !v.cast.insert(n) {
            return self.tell(n, "Vote already cast.");
        }
        if matches!(answer.as_bytes().first(), Some(b'y' | b'Y' | b'1')) {
            v.yes += 1;
            let s = v.yes.to_string();
            self.set_configstring(cs::VOTE_YES, &s);
        } else {
            v.no += 1;
            let s = v.no.to_string();
            self.set_configstring(cs::VOTE_NO, &s);
        }
        self.tell(n, "Vote cast.");
    }

    /// `CheckVote`, once a frame: ends a decided vote and returns the console lines that are due.
    pub fn vote_frame(&mut self) -> Vec<String> {
        let time = self.level.time;
        let mut due = Vec::new();
        if self.vote.pending.as_ref().is_some_and(|(at, _)| *at < time) {
            due = self.vote.pending.take().map(|(_, c)| c).unwrap_or_default();
        }
        let voting = self.voters().len() as i32;
        let Some(v) = self.vote.current.as_ref() else {
            return due;
        };
        let need = voting / 2 + 1;
        let passed = if time >= v.end {
            // Out of time: the abstentions count for half.
            let abstain = (voting - v.yes - v.no).max(0) as f32 * 0.5;
            Some(v.yes > (abstain + 0.4999) as i32 + v.no)
        } else if v.yes >= need {
            Some(true)
        } else if v.no > voting - need {
            Some(false)
        } else {
            None
        };
        let Some(passed) = passed else { return due };
        let commands = std::mem::take(&mut self.vote.current.as_mut().unwrap().commands);
        self.vote.current = None;
        self.set_configstring(cs::VOTE_TIME, "");
        self.send(
            Dest::All,
            ServerCmd::Print {
                kind: PrintKind::Normal,
                text: if passed {
                    "Vote passed."
                } else {
                    "Vote failed."
                }
                .into(),
            },
        );
        if passed {
            self.vote.pending = Some((time + EXECUTE_DELAY_MS, commands));
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{Client, Conn};
    use crate::content::Content;
    use crate::cvar::Cvars;

    fn game() -> Game {
        let mut g = Game::new(Cvars::new(), Content::default());
        g.cvars.set("g_allowvote", "1");
        g.clients = ["Ann", "^1Bo^7", "Cy"]
            .iter()
            .enumerate()
            .map(|(n, name)| Client::new(n as u16, false, (*name).to_owned()))
            .collect();
        for c in &mut g.clients {
            c.conn = Conn::Connected;
            c.team = Team::Allies;
        }
        g
    }

    /// What the vote would run, or why it was refused.
    fn call(g: &mut Game, line: &[&str]) -> Result<Vec<String>, String> {
        let argv: Vec<String> = line.iter().map(|s| (*s).to_owned()).collect();
        g.start_vote(0, &argv)?;
        let v = g.vote.current.take().expect("a vote started");
        Ok(v.commands)
    }

    #[test]
    fn clientkick_and_tempbanclient_take_a_slot_and_kick_and_tempbanuser_a_name() {
        let mut g = game();
        assert_eq!(
            call(&mut g, &["clientkick", "1"]),
            Ok(vec!["clientkick 1".into()])
        );
        assert_eq!(
            call(&mut g, &["tempBanClient", "2"]),
            Ok(vec!["tempbanclient 2".into()])
        );
        // The name is matched without its colour codes, as the stock menu sends it.
        assert_eq!(
            call(&mut g, &["kick", "bo"]),
            Ok(vec!["clientkick 1".into()])
        );
        assert_eq!(
            call(&mut g, &["tempBanUser", "CY"]),
            Ok(vec!["tempbanclient 2".into()])
        );
        // A name is not a slot and a slot is not a name.
        assert!(call(&mut g, &["clientkick", "Bo"]).is_err());
        assert!(call(&mut g, &["kick", "1"]).is_err());
        assert!(call(&mut g, &["clientkick", "9"]).is_err());
    }
}
