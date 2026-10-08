// SPDX-License-Identifier: GPL-3.0-only
//! Config-file syntax: tokenizing command lines, quoting on write, and the platform config path.
//!
//! A line holds commands separated by `;`. Tokens are whitespace separated or `"quoted"`; inside quotes `\"`, `\\`
//! and `\n` are escapes (the original had no way to put a quote in a value; we do). `//` starts a comment outside
//! quotes.

use std::path::PathBuf;

/// Split a line into commands, each a token list.
pub fn split_commands(line: &str) -> Vec<Vec<String>> {
    let mut cmds = Vec::new();
    let mut toks: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_tok = false;
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' => quoted = false,
                '\\' => match chars.peek() {
                    Some('"') => {
                        cur.push('"');
                        chars.next();
                    }
                    Some('\\') => {
                        cur.push('\\');
                        chars.next();
                    }
                    Some('n') => {
                        cur.push('\n');
                        chars.next();
                    }
                    _ => cur.push('\\'),
                },
                c => cur.push(c),
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                in_tok = true;
            }
            '/' if chars.peek() == Some(&'/') => break,
            ';' | '\n' | '\r' | '\t' | ' ' => {
                if in_tok {
                    toks.push(std::mem::take(&mut cur));
                    in_tok = false;
                }
                if (c == ';' || c == '\n') && !toks.is_empty() {
                    cmds.push(std::mem::take(&mut toks));
                }
            }
            c => {
                cur.push(c);
                in_tok = true;
            }
        }
    }
    if in_tok {
        toks.push(cur);
    }
    if !toks.is_empty() {
        cmds.push(toks);
    }
    cmds
}

/// `"text"` with `"`, `\` and newlines escaped, so [`split_commands`] reads it back unchanged.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Re-join tokens into a command line, quoting those that need it.
pub fn join(tokens: &[String]) -> String {
    let plain = |t: &str| {
        !t.is_empty() && !t.contains(|c: char| c.is_whitespace() || matches!(c, '"' | ';' | '\\'))
    };
    tokens
        .iter()
        .map(|t| if plain(t) { t.clone() } else { quote(t) })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `<platform config dir>/cod4e`: where the user's profiles live.
pub fn default_dir() -> Option<PathBuf> {
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    #[cfg(target_os = "macos")]
    let base = env("HOME").map(|h| h.join("Library/Application Support"));
    #[cfg(windows)]
    let base = env("APPDATA");
    #[cfg(not(any(target_os = "macos", windows)))]
    let base = env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config")));
    base.map(|b| b.join("cod4e"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(line: &str) -> Vec<String> {
        let mut c = split_commands(line);
        assert_eq!(c.len(), 1, "{line}");
        c.remove(0)
    }

    #[test]
    fn tokens_quotes_comments_and_semicolons() {
        assert_eq!(one(r#"bind w "+forward""#), ["bind", "w", "+forward"]);
        assert_eq!(
            one(r#"seta name "a b" // trailing"#),
            ["seta", "name", "a b"]
        );
        assert_eq!(one(r#"set x """#), ["set", "x", ""]);
        assert_eq!(
            split_commands(r#"bind x "say hi; +attack"; unbind y"#),
            [vec!["bind", "x", "say hi; +attack"], vec!["unbind", "y"]]
        );
        assert!(split_commands("// only a comment").is_empty());
        // `//` inside quotes is data.
        assert_eq!(one(r#"set u "http://x""#), ["set", "u", "http://x"]);
    }

    #[test]
    fn quote_escapes_round_trip() {
        for s in [
            "plain",
            "with \"quote\"",
            "back\\slash",
            "a\nb",
            "x; y // z",
            "",
        ] {
            assert_eq!(one(&format!("set k {}", quote(s)))[2], s, "{s:?}");
        }
        let toks: Vec<String> = ["say", "hello world", "a\"b"].map(String::from).into();
        assert_eq!(split_commands(&join(&toks)), [toks]);
    }
}
