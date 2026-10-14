// SPDX-License-Identifier: GPL-3.0-only
//! Scrubbing secrets from log text. Every sink (log file, stderr, console, browser console) passes its text through
//! [`redact`], so a command line or a field that carries a password never reaches a file or the screen.
//!
//! What is masked: the value after any word containing `password`, `passwd`, `cdkey` or `cd_key` (`set
//! rcon_password x`, `rcon_password=x`, `\password\x` in a userinfo string), the word after `rcon`, and the word
//! after the address in `connect <address> <password>`. It errs on the side of masking more.

use std::borrow::Cow;

const MASK: &str = "***";

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_separator(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'=' | b':' | b'"' | b'\'' | b'\\')
}

fn is_value(b: u8) -> bool {
    !(b.is_ascii_whitespace() || matches!(b, b'"' | b'\'' | b'\\' | b','))
}

/// The value starting after the separators at `from`: its byte range. A value that opens with a quote runs to the
/// closing quote (`rcon_password "a b"`), a bare one to the next space or quote.
fn value_after(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    let seps = bytes[from..]
        .iter()
        .take_while(|&&b| is_separator(b))
        .count();
    let start = from + seps;
    let quote = bytes[from..start]
        .iter()
        .rev()
        .find(|&&b| b == b'"' || b == b'\'')
        .copied()
        .filter(|&b| b != b'\\');
    let len = match quote {
        // To the closing quote; a backslash escaping it (a Debug-formatted field) stays out of the mask.
        Some(q) => {
            let n = bytes[start..].iter().position(|&b| b == q);
            let n = n.unwrap_or(bytes.len() - start);
            n - usize::from(n > 0 && bytes[start + n - 1] == b'\\')
        }
        None => bytes[start..].iter().take_while(|&&b| is_value(b)).count(),
    };
    (len > 0).then_some((start, start + len))
}

/// `text` with secrets replaced by `***`; borrowed when there is none.
pub fn redact(text: &str) -> Cow<'_, str> {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut masked: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !is_word(bytes[i]) {
            i += 1;
            continue;
        }
        let end = i + bytes[i..].iter().take_while(|&&b| is_word(b)).count();
        let word = &lower[i..end];
        let secret = ["password", "passwd", "cdkey", "cd_key"]
            .iter()
            .any(|k| word.contains(k));
        // `rcon` and `connect` are commands when they stand as a word: after a space, `=`, `:` or a quote, not inside
        // a longer path or span token (`net::connect:`, `{name=net-rcon}`).
        let verb = i == 0
            || match bytes[i - 1] {
                b':' => i < 2 || bytes[i - 2] != b':',
                b'-' | b'.' | b'/' | b'{' | b'_' => false,
                _ => true,
            };
        let found = if secret || (word == "rcon" && verb) {
            value_after(bytes, end)
        } else if word == "connect" && verb {
            value_after(bytes, end).and_then(|(_, e)| value_after(bytes, e))
        } else {
            None
        };
        match found {
            Some((s, e)) => {
                masked.push((s, e));
                i = e;
            }
            None => i = end,
        }
    }
    if masked.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (s, e) in masked {
        out.push_str(&text[at..s]);
        out.push_str(MASK);
        at = e;
    }
    out.push_str(&text[at..]);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_masked_and_the_rest_kept() {
        for (input, want) in [
            ("set rcon_password hunter2", "set rcon_password ***"),
            (
                "cmd=\"rcon hunter2 map mp_crash\"",
                "cmd=\"rcon *** map mp_crash\"",
            ),
            (
                "connect 10.0.0.1:28960 hunter2",
                "connect 10.0.0.1:28960 ***",
            ),
            (
                "userinfo \\name\\bob\\password\\hunter2\\rate\\25000",
                "userinfo \\name\\bob\\password\\***\\rate\\25000",
            ),
            (
                "sv_password=hunter2 map=mp_crash",
                "sv_password=*** map=mp_crash",
            ),
            ("seta cdkey ABCD-1234", "seta cdkey ***"),
            ("Connect HOST PW", "Connect HOST ***"),
            (
                "set rcon_password \"a b c\" map x",
                "set rcon_password \"***\" map x",
            ),
            (
                "cmd=\"set rcon_password \\\"a b\\\"\"",
                "cmd=\"set rcon_password \\\"***\\\"\"",
            ),
            ("cmd=rcon hunter2 map x", "cmd=rcon *** map x"),
            ("cmd=connect 1.2.3.4 hunter2", "cmd=connect 1.2.3.4 ***"),
            ("rcon=hunter2", "rcon=***"),
            ("rcon: hunter2", "rcon: ***"),
            (
                "net::connect: connection refused",
                "net::connect: connection refused",
            ),
            ("stage{name=net-rcon}: x y", "stage{name=net-rcon}: x y"),
            ("unknown command foo", "unknown command foo"),
            ("connected to 10.0.0.1", "connected to 10.0.0.1"),
        ] {
            assert_eq!(redact(input), want, "{input}");
        }
        assert!(matches!(redact("nothing secret"), Cow::Borrowed(_)));
    }
}
