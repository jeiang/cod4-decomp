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

/// The file `exec <name>` reads: `.cfg` is added when the name has no extension (`Com_DefaultExtension`).
pub fn exec_name(name: &str) -> String {
    let mut file = name.to_owned();
    if !file.rsplit(['/', '\\']).next().unwrap_or("").contains('.') {
        file.push_str(".cfg");
    }
    file
}

/// `atoi`: the integer part of a number, 0 for anything else.
pub fn atoi(s: &str) -> i32 {
    s.trim().parse::<f64>().map_or(0, |v| v as i32)
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

/// The value a config text gives the cvar `name` (the last `set`, `seta`, `sets` or `setu` of it), for what must be
/// known before the config is run: whether a download is allowed, before a join starts.
#[cfg(any(target_arch = "wasm32", test))]
pub fn saved_value(text: &str, name: &str) -> Option<String> {
    text.lines()
        .flat_map(split_commands)
        .filter(|t| {
            t.len() >= 3
                && matches!(
                    t[0].to_ascii_lowercase().as_str(),
                    "set" | "seta" | "sets" | "setu"
                )
                && t[1].eq_ignore_ascii_case(name)
        })
        .next_back()
        .map(|t| t[2].clone())
}

/// `<platform config dir>/cod4e`: where the user's profiles live.
pub fn default_dir() -> Option<PathBuf> {
    #[cfg(not(target_arch = "wasm32"))]
    return install::default_config_dir();
    // The browser has no profile folder: the page keeps each profile's config (see `Input::use_profile`).
    #[cfg(target_arch = "wasm32")]
    None
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
    fn a_saved_cvar_is_the_last_one_set() {
        let text = "// c\nseta sensitivity 3\nseta cl_allowDownload \"0\"\nset CL_ALLOWDOWNLOAD 1; seta x y\n";
        assert_eq!(saved_value(text, "cl_allowdownload").as_deref(), Some("1"));
        assert_eq!(saved_value(text, "sensitivity").as_deref(), Some("3"));
        assert_eq!(saved_value(text, "missing"), None);
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
