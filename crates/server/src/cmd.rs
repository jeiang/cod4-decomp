// SPDX-License-Identifier: GPL-3.0-or-later
//! Console text: command splitting, tokenizing and the command buffer.
//!
//! Commands are separated by newlines and by `;` outside double quotes. `//` starts a comment
//! outside quotes. Arguments are whitespace separated and double quotes group, as in the
//! original console.

use std::collections::VecDeque;

/// One tokenized command: `argv[0]` is the command name (case preserved).
pub type Argv = Vec<String>;

/// Splits console text into commands.
pub fn split_commands(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            cur.push(c);
            if c == '"' {
                quoted = false;
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                cur.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                while chars.peek().is_some_and(|&n| n != '\n') {
                    chars.next();
                }
            }
            ';' | '\n' => out.push(std::mem::take(&mut cur)),
            '\r' => {}
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.retain(|l| !l.trim().is_empty());
    out
}

/// Tokenizes one command line.
pub fn tokenize(line: &str) -> Argv {
    let mut argv = Vec::new();
    let mut cur = String::new();
    let (mut in_word, mut quoted) = (false, false);
    for c in line.chars() {
        if quoted {
            if c == '"' {
                quoted = false;
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            quoted = true;
            in_word = true;
        } else if c.is_whitespace() {
            if in_word {
                argv.push(std::mem::take(&mut cur));
                in_word = false;
            }
        } else {
            cur.push(c);
            in_word = true;
        }
    }
    if in_word {
        argv.push(cur);
    }
    argv
}

/// Pending console commands.
#[derive(Default)]
pub struct CommandBuffer {
    lines: VecDeque<String>,
    /// `wait` stops execution until the next frame.
    waiting: bool,
}

impl CommandBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends text (`Cbuf_AddText`).
    pub fn add_text(&mut self, text: &str) {
        self.lines.extend(split_commands(text));
    }

    /// Inserts text before everything pending (`Cbuf_InsertText`), used by `exec`.
    pub fn insert_text(&mut self, text: &str) {
        for l in split_commands(text).into_iter().rev() {
            self.lines.push_front(l);
        }
    }

    pub fn wait(&mut self) {
        self.waiting = true;
    }

    /// Next command to run, or `None` when empty or waiting; a wait is consumed by
    /// [`CommandBuffer::end_frame`].
    pub fn pop(&mut self) -> Option<Argv> {
        if self.waiting {
            return None;
        }
        self.lines.pop_front().map(|l| tokenize(&l))
    }

    pub fn end_frame(&mut self) {
        self.waiting = false;
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_semicolons_newlines_and_keeps_quoted_separators() {
        let t = "set a 1; set b \"x;y\" // note ; ignored\r\nmap mp_crash\n\n";
        assert_eq!(
            split_commands(t),
            ["set a 1", " set b \"x;y\" ", "map mp_crash"]
        );
        assert_eq!(tokenize(" set b \"x;y\" "), ["set", "b", "x;y"]);
        assert_eq!(tokenize("set a \"\""), ["set", "a", ""]);
    }

    #[test]
    fn exec_inserts_before_pending_commands() {
        let mut b = CommandBuffer::new();
        b.add_text("map x");
        b.insert_text("set a 1; set b 2");
        let order: Vec<_> = std::iter::from_fn(|| b.pop())
            .map(|a| a[0].clone())
            .collect();
        assert_eq!(order, ["set", "set", "map"]);
    }

    #[test]
    fn wait_defers_the_rest_to_the_next_frame() {
        let mut b = CommandBuffer::new();
        b.add_text("a; wait; b");
        assert_eq!(b.pop().unwrap()[0], "a");
        assert_eq!(b.pop().unwrap()[0], "wait");
        b.wait();
        assert!(b.pop().is_none());
        b.end_frame();
        assert_eq!(b.pop().unwrap()[0], "b");
    }
}
