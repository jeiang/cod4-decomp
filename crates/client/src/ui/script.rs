// SPDX-License-Identifier: GPL-3.0-only
//! The menu action language: `"open" "main_text" ; "setDvar" ui_x 1 ; ...`.
//!
//! A script is a list of commands separated by `;`. A command is a name followed by arguments, each a quoted string or
//! a bare word. The menu compiler stores `uiScript` calls, expressions for `statsetusingtable` and everything else in
//! this one form, so the tokenizer keeps parentheses and commas as words of their own.

/// One command: its words (quotes removed). `words[0]` is the command name.
pub type Command = Vec<String>;

/// How many words follow each command (`commandList` handlers read this many with `String_Parse`/`Float_Parse`).
/// Commands not listed here take everything up to the next `;`.
fn arity(name: &str) -> Option<usize> {
    Some(match name.to_ascii_lowercase().as_str() {
        "focusfirst" | "wait" | "getautoupdate" => 0,
        "open" | "close" | "ingameopen" | "ingameclose" | "show" | "hide" | "showmenu"
        | "hidemenu" | "fadein" | "fadeout" | "setfocus" | "setfocusbydvar" | "exec"
        | "execnow" | "play" | "scriptmenuresponse" | "feedertop" | "feederbottom"
        | "openforgametype" | "closeforgametype" | "setbackground" => 1,
        "setdvar" | "set" | "setlocalvarbool" | "setlocalvarint" | "setlocalvarfloat"
        | "setlocalvarstring" => 2,
        "execondvarstringvalue"
        | "execondvarintvalue"
        | "execondvarfloatvalue"
        | "execnowondvarstringvalue"
        | "execnowondvarintvalue"
        | "execnowondvarfloatvalue"
        | "scriptmenurespondondvarstringvalue"
        | "scriptmenurespondondvarintvalue"
        | "scriptmenurespondondvarfloatvalue" => 3,
        // `setcolor <name> r g b a` colours the running item, `setitemcolor <group> <name> r g b a` a group.
        "setcolor" => 5,
        "setitemcolor" => 6,
        _ => return None,
    })
}

/// Splits `src` into commands: `;` ends one, and a command with a fixed number of words ends after them (the menu
/// compiler writes `"open" "class" "close" "self"` with no separator); empty commands are dropped.
pub fn parse(src: &str) -> Vec<Command> {
    let words = words(src);
    let mut cmds = Vec::new();
    let mut i = 0;
    while i < words.len() {
        if words[i] == Word::Semi {
            i += 1;
            continue;
        }
        let mut cmd: Command = Vec::new();
        let limit = match &words[i] {
            Word::Text(n) => arity(n).map(|a| a + 1),
            Word::Semi => None,
        };
        while i < words.len() {
            match &words[i] {
                Word::Semi => break,
                Word::Text(t) => cmd.push(t.clone()),
            }
            i += 1;
            if limit.is_some_and(|l| cmd.len() >= l) {
                break;
            }
        }
        cmds.push(cmd);
    }
    cmds
}

#[derive(PartialEq)]
enum Word {
    Text(String),
    Semi,
}

fn words(src: &str) -> Vec<Word> {
    let mut out = Vec::new();
    let mut it = src.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        match c {
            ' ' | '\t' | '\r' | '\n' => {}
            ';' => out.push(Word::Semi),
            '"' => {
                let start = i + 1;
                let mut end = src.len();
                for (j, d) in it.by_ref() {
                    if d == '"' {
                        end = j;
                        break;
                    }
                }
                out.push(Word::Text(src[start..end].to_owned()));
            }
            '(' | ')' | ',' => out.push(Word::Text(c.to_string())),
            _ => {
                let start = i;
                let mut end = src.len();
                while let Some(&(j, d)) = it.peek() {
                    if matches!(d, ' ' | '\t' | '\r' | '\n' | ';' | '"' | '(' | ')' | ',') {
                        end = j;
                        break;
                    }
                    it.next();
                }
                out.push(Word::Text(src[start..end].to_owned()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_split_on_semicolons_and_quotes_group_words() {
        let c = parse(r#""open" "main_text" ; "exec" "set a 1; set b 2" ; ; setdvar x 2 ;"#);
        assert_eq!(
            c,
            vec![
                vec!["open", "main_text"],
                vec!["exec", "set a 1; set b 2"],
                vec!["setdvar", "x", "2"]
            ]
        );
    }

    #[test]
    fn fixed_arity_commands_need_no_separator() {
        let c = parse(
            r#""setLocalVarString" "ui_team" "marines" "open" "class" "close" "self" "focusFirst" ;"#,
        );
        assert_eq!(
            c,
            vec![
                vec!["setLocalVarString", "ui_team", "marines"],
                vec!["open", "class"],
                vec!["close", "self"],
                vec!["focusFirst"]
            ]
        );
    }

    #[test]
    fn expression_arguments_keep_their_punctuation_as_words() {
        let c = parse(
            r#""statsetusingtable" ( "201" , "tablelookup" ( "mp/statstable.csv" , 4 , "m16" , 0 ) ) ;"#,
        );
        assert_eq!(c.len(), 1);
        assert_eq!(
            c[0],
            [
                "statsetusingtable",
                "(",
                "201",
                ",",
                "tablelookup",
                "(",
                "mp/statstable.csv",
                ",",
                "4",
                ",",
                "m16",
                ",",
                "0",
                ")",
                ")"
            ]
        );
    }

    #[test]
    fn setcolor_takes_a_name_and_four_numbers_setitemcolor_a_group_too() {
        let c = parse(
            "setcolor backcolor 1 0 0 1 open x setitemcolor g bordercolor 0 0 0 1 close self",
        );
        assert_eq!(c[0].len(), 6);
        assert_eq!(c[1], ["open", "x"]);
        assert_eq!(c[2].len(), 7);
        assert_eq!(c[3], ["close", "self"]);
    }
}
