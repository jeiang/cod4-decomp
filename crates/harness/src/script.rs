// SPDX-License-Identifier: GPL-3.0-only
//! Console-script scenarios.
//!
//! A script is console commands separated by newlines or `;`, with `//` and
//! `#` comments (outside quotes). Arguments are whitespace separated; double
//! quotes group. The harness runs `wait <duration>` and `echo` itself; every
//! other command goes to the engine console. Commands the engine does not have
//! yet are reported as skipped, so one script grows with the engine.
//!
//! ```text
//! map mp_crash; bots 17; wait 30s
//! screenshot "after wait"   // comment
//! ```
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub line: usize,
    pub name: String,
    pub args: Vec<String>,
}

impl Command {
    pub fn text(&self) -> String {
        std::iter::once(self.name.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

pub fn parse(src: &str) -> Result<Vec<Command>, ParseError> {
    let mut out = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let line_no = i + 1;
        let err = |message: &str| ParseError {
            line: line_no,
            message: message.to_owned(),
        };
        let mut words: Vec<String> = Vec::new();
        let (mut cur, mut in_word, mut quoted) = (String::new(), false, false);
        let flush = |words: &mut Vec<String>, cur: &mut String, in_word: &mut bool| {
            if *in_word {
                words.push(std::mem::take(cur));
                *in_word = false;
            }
        };
        let mut push_command = |words: &mut Vec<String>| {
            if !words.is_empty() {
                let mut it = std::mem::take(words).into_iter();
                out.push(Command {
                    line: line_no,
                    name: it.next().unwrap_or_default().to_ascii_lowercase(),
                    args: it.collect(),
                });
            }
        };
        let chars: Vec<char> = line.chars().collect();
        let mut j = 0;
        while j < chars.len() {
            let c = chars[j];
            if quoted {
                if c == '"' {
                    quoted = false;
                } else {
                    cur.push(c);
                }
            } else if c == '"' {
                quoted = true;
                in_word = true;
            } else if c == '#' || (c == '/' && chars.get(j + 1) == Some(&'/')) {
                break;
            } else if c == ';' {
                flush(&mut words, &mut cur, &mut in_word);
                push_command(&mut words);
            } else if c.is_whitespace() {
                flush(&mut words, &mut cur, &mut in_word);
            } else {
                cur.push(c);
                in_word = true;
            }
            j += 1;
        }
        if quoted {
            return Err(err("unterminated quote"));
        }
        flush(&mut words, &mut cur, &mut in_word);
        push_command(&mut words);
    }
    Ok(out)
}

/// `30s`, `500ms`, `2m`; a bare number is seconds.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let n: f64 = s[..split].parse().ok()?;
    let secs = match &s[split..] {
        "" | "s" => n,
        "ms" => n / 1000.0,
        "m" => n * 60.0,
        _ => return None,
    };
    (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs))
}

/// The engine console as seen by the runner.
pub trait Console {
    fn has_command(&self, name: &str) -> bool;
    fn exec(&mut self, cmd: &Command) -> Result<(), String>;
}

/// The console before any engine exists: no commands.
pub struct NoEngine;

impl Console for NoEngine {
    fn has_command(&self, _: &str) -> bool {
        false
    }
    fn exec(&mut self, cmd: &Command) -> Result<(), String> {
        Err(format!("no engine command {}", cmd.name))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "detail", rename_all = "snake_case")]
pub enum Outcome {
    Done,
    Skipped(String),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandReport {
    pub line: usize,
    pub command: String,
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// Run `cmds`. `sleep` performs waits (tests substitute a fake).
///
/// Once a command has been skipped and no engine command has run, there is nothing real to
/// wait for, so later waits are skipped as well instead of idling.
pub fn run(
    cmds: &[Command],
    console: &mut dyn Console,
    mut sleep: impl FnMut(Duration),
) -> Vec<CommandReport> {
    let mut skipped_any = false;
    let mut ran_engine = false;
    cmds.iter()
        .map(|c| {
            let outcome = match c.name.as_str() {
                "echo" => {
                    println!("{}", c.args.join(" "));
                    Outcome::Done
                }
                "wait" => match c.args.as_slice() {
                    [d] => match parse_duration(d) {
                        _ if skipped_any && !ran_engine => {
                            Outcome::Skipped("nothing to wait for".into())
                        }
                        Some(d) => {
                            sleep(d);
                            Outcome::Done
                        }
                        None => Outcome::Failed(format!("bad duration {d:?}")),
                    },
                    _ => Outcome::Failed("usage: wait <duration>".into()),
                },
                name if !console.has_command(name) => {
                    skipped_any = true;
                    Outcome::Skipped(format!("command not implemented: {name}"))
                }
                _ => match console.exec(c) {
                    Ok(()) => {
                        ran_engine = true;
                        Outcome::Done
                    }
                    Err(e) => Outcome::Failed(e),
                },
            };
            CommandReport {
                line: c.line,
                command: c.text(),
                outcome,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(src: &str) -> Vec<(String, Vec<String>)> {
        parse(src)
            .unwrap()
            .into_iter()
            .map(|c| (c.name, c.args))
            .collect()
    }

    #[test]
    fn parses_separators_quotes_and_comments() {
        let got =
            names("MAP mp_crash; bots 17;wait 30s\n# whole line\nshot \"a b\" \"\"  // tail ; x\n");
        assert_eq!(
            got,
            vec![
                ("map".into(), vec!["mp_crash".into()]),
                ("bots".into(), vec!["17".into()]),
                ("wait".into(), vec!["30s".into()]),
                ("shot".into(), vec!["a b".into(), String::new()]),
            ]
        );
        assert_eq!(names(r#"echo "x // y; z""#)[0].1, ["x // y; z"]);
        assert_eq!(parse("echo \"oops").unwrap_err().line, 1);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration("1.5"), Some(Duration::from_millis(1500)));
        assert_eq!(parse_duration("x"), None);
        assert_eq!(parse_duration("5h"), None);
    }

    #[test]
    fn unknown_commands_skip_and_waits_after_them_do_not_idle() {
        let cmds = parse("echo hi; wait 1s; map mp_crash; wait 30s; screenshot").unwrap();
        let mut slept = Vec::new();
        let r = run(&cmds, &mut NoEngine, |d| slept.push(d));
        assert_eq!(slept, vec![Duration::from_secs(1)]);
        let out: Vec<_> = r.iter().map(|r| r.outcome.clone()).collect();
        assert_eq!(out[0], Outcome::Done);
        assert_eq!(out[1], Outcome::Done);
        assert!(matches!(&out[2], Outcome::Skipped(m) if m.contains("map")));
        assert!(matches!(&out[3], Outcome::Skipped(_)));
        assert!(matches!(&out[4], Outcome::Skipped(_)));
    }

    #[test]
    fn engine_commands_run_when_present() {
        struct E(Vec<String>);
        impl Console for E {
            fn has_command(&self, n: &str) -> bool {
                n == "map"
            }
            fn exec(&mut self, c: &Command) -> Result<(), String> {
                self.0.push(c.text());
                if c.args.is_empty() {
                    Err("needs a map".into())
                } else {
                    Ok(())
                }
            }
        }
        let mut e = E(vec![]);
        let r = run(
            &parse("map mp_crash; map; wait 1s; wait nope").unwrap(),
            &mut e,
            |_| {},
        );
        assert_eq!(e.0, ["map mp_crash", "map"]);
        assert_eq!(r[0].outcome, Outcome::Done);
        assert_eq!(r[1].outcome, Outcome::Failed("needs a map".into()));
        assert_eq!(r[2].outcome, Outcome::Done);
        assert!(matches!(r[3].outcome, Outcome::Failed(_)));
    }
}
