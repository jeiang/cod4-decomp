// SPDX-License-Identifier: GPL-3.0-only
//! GSC lexer. Identifiers are lowercased (the language is case-insensitive there);
//! string literals keep their case.

use crate::error::{CompileError, ErrorKind};

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(Box<str>),
    Int(i64),
    Float(f32),
    Str(Box<str>),

    // Keywords.
    If,
    Else,
    While,
    For,
    Switch,
    Case,
    Default,
    Break,
    Continue,
    Return,
    Wait,
    Waittill,
    WaittillMatch,
    WaittillFrameEnd,
    Notify,
    Endon,
    Thread,
    ChildThread,
    Undefined,
    True,
    False,
    SelfKw,
    Level,
    Game,
    AnimKw,

    // Directives and developer-block delimiters.
    Include,
    UsingAnimtree,
    AnimTree,
    DevOpen,
    DevClose,

    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Semi,
    Comma,
    Colon,
    ColonColon,
    Dot,

    Assign,
    PlusAssign,
    MinusAssign,
    StarAssign,
    SlashAssign,
    PercentAssign,
    AmpAssign,
    PipeAssign,
    CaretAssign,
    ShlAssign,
    ShrAssign,
    PlusPlus,
    MinusMinus,

    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Bang,
    Shl,
    Shr,
    Lt,
    Le,
    Gt,
    Ge,
    EqEq,
    Ne,
    AndAnd,
    OrOr,

    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub line: u32,
    pub col: u32,
}

fn keyword(s: &str) -> Option<Tok> {
    Some(match s {
        "if" => Tok::If,
        "else" => Tok::Else,
        "while" => Tok::While,
        "for" => Tok::For,
        "switch" => Tok::Switch,
        "case" => Tok::Case,
        "default" => Tok::Default,
        "break" => Tok::Break,
        "continue" => Tok::Continue,
        "return" => Tok::Return,
        "wait" => Tok::Wait,
        "waittill" => Tok::Waittill,
        "waittillmatch" => Tok::WaittillMatch,
        "waittillframeend" => Tok::WaittillFrameEnd,
        "notify" => Tok::Notify,
        "endon" => Tok::Endon,
        "thread" => Tok::Thread,
        "childthread" => Tok::ChildThread,
        "undefined" => Tok::Undefined,
        "true" => Tok::True,
        "false" => Tok::False,
        "self" => Tok::SelfKw,
        "level" => Tok::Level,
        "game" => Tok::Game,
        "anim" => Tok::AnimKw,
        _ => return None,
    })
}

impl Tok {
    /// Spelling of a keyword token, for field names such as `level.wait`.
    pub fn keyword_text(&self) -> Option<&'static str> {
        Some(match self {
            Tok::If => "if",
            Tok::Else => "else",
            Tok::While => "while",
            Tok::For => "for",
            Tok::Switch => "switch",
            Tok::Case => "case",
            Tok::Default => "default",
            Tok::Break => "break",
            Tok::Continue => "continue",
            Tok::Return => "return",
            Tok::Wait => "wait",
            Tok::Waittill => "waittill",
            Tok::WaittillMatch => "waittillmatch",
            Tok::WaittillFrameEnd => "waittillframeend",
            Tok::Notify => "notify",
            Tok::Endon => "endon",
            Tok::Thread => "thread",
            Tok::ChildThread => "childthread",
            Tok::Undefined => "undefined",
            Tok::True => "true",
            Tok::False => "false",
            Tok::SelfKw => "self",
            Tok::Level => "level",
            Tok::Game => "game",
            Tok::AnimKw => "anim",
            _ => return None,
        })
    }
}

struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    line: u32,
    line_start: usize,
    out: Vec<Token>,
}

pub fn lex(src: &str) -> Result<Vec<Token>, CompileError> {
    let mut lx = Lexer {
        src: src.as_bytes(),
        pos: 0,
        line: 1,
        line_start: 0,
        out: Vec::with_capacity(src.len() / 4),
    };
    lx.run()?;
    Ok(lx.out)
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

impl Lexer<'_> {
    fn peek(&self, n: usize) -> u8 {
        self.src.get(self.pos + n).copied().unwrap_or(0)
    }

    fn err(&self, msg: impl Into<String>) -> CompileError {
        CompileError::at(
            ErrorKind::Syntax,
            self.line,
            (self.pos - self.line_start + 1) as u32,
            msg,
        )
    }

    fn newline(&mut self) {
        self.line += 1;
        self.line_start = self.pos + 1;
    }

    fn push(&mut self, tok: Tok, start: usize, line: u32, line_start: usize) {
        self.out.push(Token {
            tok,
            line,
            col: (start - line_start + 1) as u32,
        });
    }

    fn run(&mut self) -> Result<(), CompileError> {
        loop {
            // Skip whitespace and comments.
            loop {
                match self.peek(0) {
                    b'\n' => {
                        self.newline();
                        self.pos += 1;
                    }
                    b' ' | b'\t' | b'\r' | 0x0c => self.pos += 1,
                    b'/' if self.peek(1) == b'/' => {
                        while self.pos < self.src.len() && self.peek(0) != b'\n' {
                            self.pos += 1;
                        }
                    }
                    b'/' if self.peek(1) == b'*' => {
                        let (line, col) = (self.line, self.pos - self.line_start + 1);
                        self.pos += 2;
                        loop {
                            if self.pos >= self.src.len() {
                                return Err(CompileError::at(
                                    ErrorKind::Syntax,
                                    line,
                                    col as u32,
                                    "unterminated /* comment",
                                ));
                            }
                            if self.peek(0) == b'*' && self.peek(1) == b'/' {
                                self.pos += 2;
                                break;
                            }
                            if self.peek(0) == b'\n' {
                                self.newline();
                            }
                            self.pos += 1;
                        }
                    }
                    _ => break,
                }
            }
            let (start, line, line_start) = (self.pos, self.line, self.line_start);
            if self.pos >= self.src.len() {
                self.push(Tok::Eof, start, line, line_start);
                return Ok(());
            }
            let b = self.peek(0);
            let tok = if is_ident_start(b) {
                self.ident()
            } else if b.is_ascii_digit() || (b == b'.' && self.peek(1).is_ascii_digit()) {
                self.number()?
            } else if b == b'"' {
                self.string()?
            } else {
                self.punct()?
            };
            self.push(tok, start, line, line_start);
        }
    }

    /// An identifier, or a backslash-joined path (`maps\mp\_utility`) which is
    /// returned as a single identifier.
    fn ident(&mut self) -> Tok {
        let start = self.pos;
        let mut path = false;
        loop {
            while is_ident_cont(self.peek(0)) {
                self.pos += 1;
            }
            if self.peek(0) == b'\\' && is_ident_start(self.peek(1)) {
                path = true;
                self.pos += 1;
            } else {
                break;
            }
        }
        let text = String::from_utf8_lossy(&self.src[start..self.pos]).to_ascii_lowercase();
        if !path && let Some(k) = keyword(&text) {
            return k;
        }
        Tok::Ident(text.into())
    }

    fn number(&mut self) -> Result<Tok, CompileError> {
        let start = self.pos;
        if self.peek(0) == b'0' && matches!(self.peek(1), b'x' | b'X') {
            self.pos += 2;
            let digits = self.pos;
            while self.peek(0).is_ascii_hexdigit() {
                self.pos += 1;
            }
            let s = std::str::from_utf8(&self.src[digits..self.pos]).unwrap_or("");
            let v = u32::from_str_radix(s, 16).map_err(|_| self.err("bad hex literal"))?;
            return Ok(Tok::Int(v as i32 as i64));
        }
        let mut float = false;
        while self.peek(0).is_ascii_digit() {
            self.pos += 1;
        }
        if self.peek(0) == b'.' && self.peek(1) != b'.' {
            float = true;
            self.pos += 1;
            while self.peek(0).is_ascii_digit() {
                self.pos += 1;
            }
        }
        let s = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("");
        if float {
            let s = s.trim_end_matches('.');
            let v: f32 = s.parse().map_err(|_| self.err("bad float literal"))?;
            Ok(Tok::Float(v))
        } else {
            let v: i64 = s.parse().map_err(|_| self.err("bad integer literal"))?;
            if v > i64::from(i32::MAX) + 1 {
                // Too big for an int: the original reads it as a float.
                return Ok(Tok::Float(v as f32));
            }
            Ok(Tok::Int(v))
        }
    }

    fn string(&mut self) -> Result<Tok, CompileError> {
        let (line, col) = (self.line, (self.pos - self.line_start + 1) as u32);
        self.pos += 1;
        let mut buf = Vec::new();
        loop {
            if self.pos >= self.src.len() {
                return Err(CompileError::at(
                    ErrorKind::Syntax,
                    line,
                    col,
                    "unterminated string",
                ));
            }
            let b = self.peek(0);
            self.pos += 1;
            match b {
                b'"' => break,
                b'\n' => {
                    self.pos -= 1;
                    self.newline();
                    self.pos += 1;
                    buf.push(b);
                }
                b'\\' => {
                    let e = self.peek(0);
                    self.pos += 1;
                    match e {
                        b'n' => buf.push(b'\n'),
                        b't' => buf.push(b'\t'),
                        b'"' => buf.push(b'"'),
                        b'\\' => buf.push(b'\\'),
                        // Unknown escapes keep the backslash, as paths in strings do.
                        _ => {
                            buf.push(b'\\');
                            self.pos -= 1;
                        }
                    }
                }
                _ => buf.push(b),
            }
        }
        Ok(Tok::Str(String::from_utf8_lossy(&buf).into()))
    }

    fn punct(&mut self) -> Result<Tok, CompileError> {
        let (a, b, c) = (self.peek(0), self.peek(1), self.peek(2));
        let (tok, len) = match (a, b, c) {
            (b'<', b'<', b'=') => (Tok::ShlAssign, 3),
            (b'>', b'>', b'=') => (Tok::ShrAssign, 3),
            (b':', b':', _) => (Tok::ColonColon, 2),
            (b'+', b'+', _) => (Tok::PlusPlus, 2),
            (b'-', b'-', _) => (Tok::MinusMinus, 2),
            (b'+', b'=', _) => (Tok::PlusAssign, 2),
            (b'-', b'=', _) => (Tok::MinusAssign, 2),
            (b'*', b'=', _) => (Tok::StarAssign, 2),
            (b'/', b'=', _) => (Tok::SlashAssign, 2),
            (b'%', b'=', _) => (Tok::PercentAssign, 2),
            (b'&', b'=', _) => (Tok::AmpAssign, 2),
            (b'|', b'=', _) => (Tok::PipeAssign, 2),
            (b'^', b'=', _) => (Tok::CaretAssign, 2),
            (b'<', b'<', _) => (Tok::Shl, 2),
            (b'>', b'>', _) => (Tok::Shr, 2),
            (b'<', b'=', _) => (Tok::Le, 2),
            (b'>', b'=', _) => (Tok::Ge, 2),
            (b'=', b'=', _) => (Tok::EqEq, 2),
            (b'!', b'=', _) => (Tok::Ne, 2),
            (b'&', b'&', _) => (Tok::AndAnd, 2),
            (b'|', b'|', _) => (Tok::OrOr, 2),
            (b'/', b'#', _) => (Tok::DevOpen, 2),
            (b'#', b'/', _) => (Tok::DevClose, 2),
            (b'(', ..) => (Tok::LParen, 1),
            (b')', ..) => (Tok::RParen, 1),
            (b'{', ..) => (Tok::LBrace, 1),
            (b'}', ..) => (Tok::RBrace, 1),
            (b'[', ..) => (Tok::LBracket, 1),
            (b']', ..) => (Tok::RBracket, 1),
            (b';', ..) => (Tok::Semi, 1),
            (b',', ..) => (Tok::Comma, 1),
            (b':', ..) => (Tok::Colon, 1),
            (b'.', ..) => (Tok::Dot, 1),
            (b'=', ..) => (Tok::Assign, 1),
            (b'+', ..) => (Tok::Plus, 1),
            (b'-', ..) => (Tok::Minus, 1),
            (b'*', ..) => (Tok::Star, 1),
            (b'/', ..) => (Tok::Slash, 1),
            (b'%', ..) => (Tok::Percent, 1),
            (b'&', ..) => (Tok::Amp, 1),
            (b'|', ..) => (Tok::Pipe, 1),
            (b'^', ..) => (Tok::Caret, 1),
            (b'~', ..) => (Tok::Tilde, 1),
            (b'!', ..) => (Tok::Bang, 1),
            (b'<', ..) => (Tok::Lt, 1),
            (b'>', ..) => (Tok::Gt, 1),
            (b'#', ..) => return self.directive(),
            _ => return Err(self.err(format!("unexpected character {:?}", a as char))),
        };
        self.pos += len;
        Ok(tok)
    }

    fn directive(&mut self) -> Result<Tok, CompileError> {
        let start = self.pos + 1;
        let mut end = start;
        while is_ident_cont(self.src.get(end).copied().unwrap_or(0)) {
            end += 1;
        }
        let name = String::from_utf8_lossy(&self.src[start..end]).to_ascii_lowercase();
        let tok = match name.as_str() {
            "include" => Tok::Include,
            "using_animtree" => Tok::UsingAnimtree,
            "animtree" => Tok::AnimTree,
            _ => return Err(self.err(format!("unknown directive #{name}"))),
        };
        self.pos = end;
        Ok(tok)
    }
}
