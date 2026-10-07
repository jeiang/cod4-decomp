// SPDX-License-Identifier: GPL-3.0-or-later
//! The menu action language: `"open" "main_text" ; "setDvar" ui_x 1 ; ...`.
//!
//! A script is a list of commands separated by `;`. A command is a name followed by arguments, each a quoted string or
//! a bare word. The menu compiler stores `uiScript` calls, expressions for `statsetusingtable` and everything else in
//! this one form, so the tokenizer keeps parentheses and commas as words of their own.

/// One command: its words (quotes removed). `words[0]` is the command name.
pub type Command = Vec<String>;

/// Splits `src` into commands; empty commands (`; ;`) are dropped.
pub fn parse(src: &str) -> Vec<Command> {
    let mut cmds = Vec::new();
    let mut cur: Command = Vec::new();
    let mut it = src.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        match c {
            ' ' | '\t' | '\r' | '\n' => {}
            ';' => {
                if !cur.is_empty() {
                    cmds.push(std::mem::take(&mut cur));
                }
            }
            '"' => {
                let start = i + 1;
                let mut end = src.len();
                for (j, d) in it.by_ref() {
                    if d == '"' {
                        end = j;
                        break;
                    }
                }
                cur.push(src[start..end].to_owned());
            }
            '(' | ')' | ',' => cur.push(c.to_string()),
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
                cur.push(src[start..end].to_owned());
            }
        }
    }
    if !cur.is_empty() {
        cmds.push(cur);
    }
    cmds
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
}
