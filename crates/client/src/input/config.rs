// SPDX-License-Identifier: GPL-3.0-only
// Sun-file parsing follows KisakCOD (GPL-3.0, copyright the KisakCOD contributors and Activision):
// universal/dvar.cpp (Com_LoadDvarsFromBuffer, Com_SaveDvarsToBuffer).
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

/// A command line's `+set`, `+seta`, `+sets` or `+setu` line with a name and a value, as its tokens: the lines
/// applied before the configs run and again after them. Stock's `Com_StartupVariable` takes only `set` and `seta`;
/// `sets` and `setu` are our extension, as on the server.
pub fn startup_set(line: &str) -> Option<Vec<String>> {
    let toks = split_commands(line).into_iter().next()?;
    let verb = toks.first()?.to_ascii_lowercase();
    (matches!(verb.as_str(), "set" | "seta" | "sets" | "setu") && toks.len() >= 3).then_some(toks)
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

/// Guard a relative user filename using the VFS's no-parent/no-drive convention.
pub fn user_filename(name: &str) -> std::io::Result<String> {
    let name = name.replace('\\', "/");
    if name.is_empty()
        || name.ends_with('/')
        || name.starts_with('/')
        || name.contains([':', '\0'])
        || name.split('/').any(|p| p == "..")
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "filename must stay inside the user folder",
        ));
    }
    Ok(name)
}

/// `va("sun/%s.sun", name)`: stock appends `.sun` even when the argument already has an extension.
pub fn sun_qpath(name: &str) -> String {
    format!("sun/{name}.sun")
}

/// `FS_CreatePath` (KisakCOD com_files.cpp:452-459) never creates a path holding `..` or `::`, and prints
/// `refusing to create relative path`. A drive (`:`) and NUL are refused with it, as the server's `writeconfig` does.
pub fn refuses_write(qpath: &str) -> bool {
    qpath.contains("..") || qpath.contains([':', '\0'])
}

/// `FS_SanitizeFilename` (com_files.cpp:644): `\` as `/`, leading and repeated separators collapsed (`sun//x.sun`
/// is `sun/x.sun`) and `.` components skipped. `None` for a `../` (`FS_IsBackupSubStr`), a drive or NUL, which are
/// never opened.
pub fn sanitize_qpath(qpath: &str) -> Option<String> {
    let qpath = qpath.replace('\\', "/");
    if qpath.contains("../") || qpath.contains([':', '\0']) {
        return None;
    }
    let parts: Vec<&str> = qpath
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// `Com_LoadDvarsFromBuffer`'s result. Messages belong on the system console channel; every one is printed by
/// `Com_PrintWarning` or `Com_PrintError`, so it carries their colour prefix and shows when the `error` channel does.
/// Incomplete files still change the dvars, but must not be applied to the world sun.
#[derive(Debug)]
pub struct DvarLoad {
    pub complete: bool,
    pub messages: Vec<String>,
}

/// Reset every listed dvar, then read a name and one value per line. Unlike config scripts,
/// semicolons and backslash escapes have no special meaning in these stock data files.
pub fn load_dvars(
    cvars: &mut super::Cvars,
    names: &[&str],
    text: &str,
    filename: &str,
) -> DvarLoad {
    for &name in names {
        let reset = cvars.reset_value(name).expect("registered dvar").to_owned();
        cvars.set(name, &reset, false);
    }
    let mut read = vec![false; names.len()];
    let mut messages = Vec::new();
    let mut rest = text.split('\0').next().unwrap_or("");
    while let Some(name) = dvar_token(&mut rest, true) {
        if name.is_empty() {
            break;
        }
        if let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(name)) {
            let value = dvar_token(&mut rest, false).unwrap_or("");
            cvars.set(names[i], value, false);
            read[i] = true;
        } else {
            // `Com_PrintWarning` adds `^3`.
            messages.push(format!(
                "^3WARNING: unknown dvar '{name}' in file '{filename}'"
            ));
        }
        rest = rest.find('\n').map_or("", |i| &rest[i + 1..]);
    }
    let complete = read.iter().all(|&r| r);
    if !complete {
        // `Com_PrintError` adds `^1` to a format naming an error and `^1Error: ` to one that does not, so each
        // `"  %s\n"` member line reads `^1Error:   <dvar>` (1.7 0x56d540 through 0x4fcc80).
        messages.push(format!(
            "^1ERROR: the following dvars were not specified in file '{filename}'"
        ));
        for (&name, read) in names.iter().zip(read) {
            if !read {
                messages.push(format!("^1Error:   {name}"));
            }
        }
    }
    DvarLoad { complete, messages }
}

/// `Com_Parse`/`Com_ParseOnLine`: quotes are literal, both C comment forms are skipped.
fn dvar_token<'a>(rest: &mut &'a str, allow_newline: bool) -> Option<&'a str> {
    loop {
        let trimmed = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
        if !allow_newline && rest[..rest.len() - trimmed.len()].contains('\n') {
            return None;
        }
        *rest = trimmed;
        if rest.starts_with("//") {
            let end = rest.find('\n').unwrap_or(rest.len());
            *rest = &rest[end..];
        } else if rest.starts_with("/*") {
            let end = rest.find("*/").map_or(rest.len(), |i| i + 2);
            let newline = rest[..end].contains('\n');
            *rest = &rest[end..];
            if newline && !allow_newline {
                return None;
            }
        } else {
            break;
        }
    }
    if rest.is_empty() {
        return None;
    }
    if let Some(quoted) = rest.strip_prefix('"') {
        let end = quoted.find('"').unwrap_or(quoted.len());
        *rest = quoted.get(end + 1..).unwrap_or("");
        Some(&quoted[..end])
    } else {
        let end = rest
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(rest.len());
        let token = &rest[..end];
        *rest = &rest[end..];
        Some(token)
    }
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

    #[test]
    fn sun_paths_follow_the_stock_write_refusal_and_read_sanitizer() {
        assert_eq!(sun_qpath("dawn.sun"), "sun/dawn.sun.sun");
        assert_eq!(
            sanitize_qpath(&sun_qpath("nested\\dawn")).unwrap(),
            "sun/nested/dawn.sun"
        );
        // `r_savesun /x` writes `sun//x.sun`, the file `r_loadsun x` and `r_loadsun \x` read.
        assert_eq!(sanitize_qpath(&sun_qpath("/x")).unwrap(), "sun/x.sun");
        assert_eq!(sanitize_qpath(&sun_qpath("\\\\x")).unwrap(), "sun/x.sun");
        assert_eq!(
            sanitize_qpath(&sun_qpath("./a..b")).unwrap(),
            "sun/a..b.sun"
        );
        assert!(!refuses_write(&sun_qpath("/x")));
        for name in ["../x", "a/../x", "..\\x", "c:x", "x\0"] {
            assert!(refuses_write(&sun_qpath(name)), "{name:?}");
            assert_eq!(sanitize_qpath(&sun_qpath(name)), None, "{name:?}");
        }
        // `FS_CreatePath` refuses any `..`; the reader only a `../`.
        assert!(refuses_write(&sun_qpath("a..b")));
    }

    #[test]
    fn data_dvars_reset_warn_and_report_missing_in_list_order() {
        let mut c = super::super::Cvars::default();
        c.set("r_sunsprite_size", "99", false);
        c.set("r_sunflare_max_alpha", "0.1", false);
        let report = load_dvars(
            &mut c,
            &["r_sunsprite_size", "r_sunflare_max_alpha"],
            "// comment\nalien 1 r_sunflare_max_alpha 0.8\nR_SUNSPRITE_SIZE \"32\" ignored\n",
            "sun/test.sun",
        );
        assert!(!report.complete);
        assert_eq!(c.get("r_sunsprite_size"), Some("32"));
        assert_eq!(c.get("r_sunflare_max_alpha"), Some("1"));
        assert_eq!(
            report.messages,
            [
                "^3WARNING: unknown dvar 'alien' in file 'sun/test.sun'",
                "^1ERROR: the following dvars were not specified in file 'sun/test.sun'",
                "^1Error:   r_sunflare_max_alpha",
            ]
        );
    }

    #[test]
    fn data_dvars_use_one_literal_value_per_line_and_last_duplicate_wins() {
        let mut c = super::super::Cvars::default();
        let report = load_dvars(
            &mut c,
            &[
                "r_sunsprite_shader",
                "r_sunflare_shader",
                "r_sun_fx_position",
            ],
            "/* header\ncomment */ r_sunsprite_shader \"a\\b; c\"\n\
             r_sunflare_shader\nr_sun_fx_position \"1 2 3\"\n\
             r_sunsprite_shader \"final\\name\"\n",
            "sun/test.sun",
        );
        assert!(report.complete);
        assert!(report.messages.is_empty());
        assert_eq!(c.get("r_sunsprite_shader"), Some("final\\name"));
        assert_eq!(c.get("r_sunflare_shader"), Some(""));
        assert_eq!(c.get("r_sun_fx_position"), Some("1 2 3"));
    }

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
