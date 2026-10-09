// SPDX-License-Identifier: GPL-3.0-only
//! `callvote` and `vote` (`g_oldVoting`: the engine runs the vote, not the scripts): a player calls a vote for
//! `map`, `typemap`, `g_gametype`, `map_restart`, `map_rotate`, `kick` or `tempBanUser`, the other players
//! answer `vote y` or `vote n`, and a majority of the voting players runs the command three seconds later.
//! Everyone connected and not a spectator counts as a voter, bots included, as at the original.
//! With `g_oldVoting 0` the engine runs no vote: the scripts get `call_vote` and `vote` notifies instead.

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
    /// Lines of a passed vote whose wait a new vote cut short: they run at the next frame.
    due_now: Vec<String>,
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

    /// Players who may vote: everyone connected who is not a spectator (`CalculateRanks`' `numVotingClients`).
    fn voters(&self) -> Vec<u16> {
        self.connected_clients()
            .filter(|(_, c)| c.team != Team::Spectator)
            .map(|(n, _)| n)
            .collect()
    }

    /// A client's `callvote <what> [args]`: what is refused is told to the caller. With `g_oldVoting 0` the vote is
    /// the scripts' to run: the result is the `call_vote` notify's arguments.
    pub fn call_vote(&mut self, n: u16, argv: &[String]) -> Option<[String; 3]> {
        match self.start_vote(n, argv) {
            Ok(script) => script,
            Err(why) => {
                self.tell(n, &why);
                None
            }
        }
    }

    fn start_vote(&mut self, n: u16, argv: &[String]) -> Result<Option<[String; 3]>, String> {
        let old_voting = self.cvars.bool("g_oldVoting");
        if !self.cvars.bool("g_allowvote") {
            return Err("Voting is not enabled on this server.".into());
        }
        if self.connected_clients().count() < 2 {
            return Err("Not enough players to call a vote.".into());
        }
        if old_voting && self.vote.current.is_some() {
            return Err("A vote is already in progress.".into());
        }
        if old_voting && self.client(n).is_none_or(|c| c.team == Team::Spectator) {
            return Err("Spectators cannot call a vote.".into());
        }
        let arg = |i: usize| argv.get(i).map_or("", String::as_str);
        if argv.iter().any(|a| a.contains(';') || a.contains('"')) {
            return Err("Invalid vote string.".into());
        }
        if !old_voting {
            return Ok(Some([
                arg(0).to_owned(),
                arg(1).to_owned(),
                arg(2).to_owned(),
            ]));
        }
        if let Some((_, lines)) = self.vote.pending.take() {
            // The last vote was waiting out its delay: it acts now.
            self.vote.due_now.extend(lines);
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
                // What is already running is no change.
                let new_gt = !gt.eq_ignore_ascii_case(self.cvars.string("g_gametype"));
                let new_map = !map.eq_ignore_ascii_case(self.cvars.string("mapname"));
                match (new_gt, new_map) {
                    (false, false) => {
                        return Err("That game type is already on that map.".into());
                    }
                    (true, true) => (
                        vec![format!("g_gametype {gt}"), format!("map {map}")],
                        format!("Change gametype to {gt} on {map}"),
                    ),
                    (false, true) => (vec![format!("map {map}")], format!("Change map to {map}")),
                    (true, false) => (
                        vec![format!("g_gametype {gt}"), "map_restart".into()],
                        format!("Change gametype to {gt}"),
                    ),
                }
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
        self.set_configstring(cs::VOTE_TIME, &format!("{end} {}", self.server_id));
        self.set_configstring(cs::VOTE_STRING, &shown);
        self.set_configstring(cs::VOTE_YES, "1");
        self.set_configstring(cs::VOTE_NO, "0");
        Ok(None)
    }

    /// What `setvote*` repeat of the engine's own vote: its end time and counts, none when no vote runs.
    fn vote_numbers(&self) -> (i32, i32, i32) {
        self.vote
            .current
            .as_ref()
            .map_or((0, 0, 0), |v| (v.end, v.yes, v.no))
    }

    /// `setVoteString`: the text on the vote lines of the screen, with the vote's time and counts.
    pub fn script_vote_string(&mut self, text: &str) {
        let (time, yes, no) = self.vote_numbers();
        self.set_configstring(cs::VOTE_STRING, text);
        self.set_configstring(cs::VOTE_TIME, &format!("{time} {}", self.server_id));
        self.set_configstring(cs::VOTE_YES, &yes.to_string());
        self.set_configstring(cs::VOTE_NO, &no.to_string());
    }

    /// `setVoteTime`: when the vote on the screen ends.
    pub fn script_vote_time(&mut self, time: i32) {
        let (_, yes, no) = self.vote_numbers();
        self.set_configstring(cs::VOTE_TIME, &format!("{time} {}", self.server_id));
        self.set_configstring(cs::VOTE_YES, &yes.to_string());
        self.set_configstring(cs::VOTE_NO, &no.to_string());
    }

    /// `setVoteYesCount`.
    pub fn script_vote_yes(&mut self, yes: i32) {
        let (_, _, no) = self.vote_numbers();
        self.set_configstring(cs::VOTE_YES, &yes.to_string());
        self.set_configstring(cs::VOTE_NO, &no.to_string());
    }

    /// `setVoteNoCount`.
    pub fn script_vote_no(&mut self, no: i32) {
        self.set_configstring(cs::VOTE_NO, &no.to_string());
    }

    /// A client's `vote y|n`. With `g_oldVoting 0` the answer is the scripts' (`"yes"` or `"no"`, the `vote` notify).
    pub fn cast_vote(&mut self, n: u16, answer: &str) -> Option<&'static str> {
        let yes = matches!(answer.as_bytes().first(), Some(b'y' | b'Y' | b'1'));
        if !self.cvars.bool("g_oldVoting") {
            return Some(if yes { "yes" } else { "no" });
        }
        self.cast_engine_vote(n, yes);
        None
    }

    fn cast_engine_vote(&mut self, n: u16, yes: bool) {
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
        if yes {
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
        let mut due = std::mem::take(&mut self.vote.due_now);
        if self.vote.pending.as_ref().is_some_and(|(at, _)| *at < time) {
            due.extend(self.vote.pending.take().map(|(_, c)| c).unwrap_or_default());
        }
        let voting = self.voters().len() as i32;
        let Some(v) = self.vote.current.as_ref() else {
            return due;
        };
        let need = voting / 2 + 1;
        let passed = if time >= v.end {
            // Out of time: the abstentions count for half.
            let abstain = (voting - v.yes - v.no) as f32 * self.cvars.float("g_voteAbstainWeight");
            Some(v.yes > (abstain + 0.5) as i32 + v.no)
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
        g.cvars.set("g_oldVoting", "1");
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
    fn bots_are_voters_and_the_abstain_weight_decides_a_vote_that_runs_out() {
        let mut g = game();
        let mut bot = Client::new(3, true, "Bot".into());
        bot.conn = Conn::Connected;
        bot.team = Team::Axis;
        g.clients.push(bot);
        assert_eq!(g.voters().len(), 4);
        call(&mut g, &["map_restart"]).unwrap();
        for (weight, passes) in [("0", true), ("0.5", true), ("1", false)] {
            g.cvars.set("g_voteAbstainWeight", weight);
            g.level.time = 0;
            g.start_vote(0, &["map_restart".to_owned()]).unwrap();
            // Two yes of four voters, the other two abstain.
            g.cast_vote(1, "y");
            g.level.time = 40_000;
            g.vote_frame();
            assert_eq!(g.vote.pending.take().is_some(), passes, "weight {weight}");
            assert!(g.vote.current.is_none());
        }
    }

    #[test]
    fn a_quorum_counts_everybody_who_is_not_a_spectator() {
        let mut g = game();
        let mut bot = Client::new(3, true, "Bot".into());
        bot.conn = Conn::Connected;
        bot.team = Team::Axis;
        g.clients.push(bot);
        g.start_vote(0, &["map_restart".to_owned()]).unwrap();
        g.vote_frame();
        assert!(
            g.vote.current.is_some(),
            "one yes of four is not a majority"
        );
        g.cast_vote(1, "y");
        g.vote_frame();
        assert!(g.vote.current.is_some(), "two of four is not either");
        g.cast_vote(2, "y");
        g.vote_frame();
        assert!(g.vote.current.is_none());
        assert!(g.vote.pending.is_some());
    }

    #[test]
    fn the_vote_time_carries_the_level_it_was_called_in() {
        let mut g = game();
        g.server_id = 5;
        g.level.time = 1000;
        g.start_vote(0, &["map_restart".to_owned()]).unwrap();
        assert_eq!(
            g.configstrings
                .get(&u32::from(cs::VOTE_TIME))
                .map(String::as_str),
            Some("31000 5")
        );
    }

    #[test]
    fn a_typemap_to_what_is_already_running_changes_nothing_and_a_half_change_is_only_that_half() {
        let mut g = game();
        for gt in ["war", "sd"] {
            g.content
                .add_test_rawfile(&format!("maps/mp/gametypes/{gt}.gsc"), "");
        }
        g.known_maps = vec!["mp_crash".into(), "mp_backlot".into()];
        g.cvars.register("g_gametype", "war", 0);
        g.cvars.register("mapname", "mp_crash", 0);
        assert!(call(&mut g, &["typemap", "war", "mp_crash"]).is_err());
        assert_eq!(
            call(&mut g, &["typemap", "sd", "mp_crash"]),
            Ok(vec!["g_gametype sd".into(), "map_restart".into()])
        );
        assert_eq!(
            call(&mut g, &["typemap", "sd", "mp_backlot"]),
            Ok(vec!["g_gametype sd".into(), "map mp_backlot".into()])
        );
        assert_eq!(
            call(&mut g, &["typemap", "war", "mp_backlot"]),
            Ok(vec!["map mp_backlot".into()])
        );
    }

    #[test]
    fn with_old_voting_off_the_scripts_get_the_vote_to_run() {
        let mut g = game();
        g.cvars.set("g_oldVoting", "0");
        let script = g.call_vote(0, &["kick".to_owned(), "Bo".to_owned()]);
        assert_eq!(script, Some(["kick".into(), "Bo".into(), String::new()]));
        assert!(g.vote.current.is_none());
        assert_eq!(g.cast_vote(1, "yes"), Some("yes"));
        assert_eq!(g.cast_vote(1, "n"), Some("no"));
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
