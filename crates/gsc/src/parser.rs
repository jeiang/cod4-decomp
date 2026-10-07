// SPDX-License-Identifier: GPL-3.0-or-later
//! Recursive-descent parser: tokens to [`Script`].
//!
//! Object calls have no dot: after a primary expression, an identifier,
//! `thread`, `childthread` or `[[` starts a call on that expression
//! (`self thread foo()`, `level notify(...)`).

use crate::ast::*;
use crate::error::{CompileError, ErrorKind};
use crate::lexer::{Tok, Token, lex};

type Result<T, E = CompileError> = std::result::Result<T, E>;

/// Canonical file name: lowercase, backslash separators, no `.gsc`.
pub fn canonical_file(name: &str) -> String {
    let n = name.to_ascii_lowercase().replace('/', "\\");
    n.strip_suffix(".gsc").map(str::to_string).unwrap_or(n)
}

pub fn parse(src: &str) -> Result<Script> {
    let mut p = Parser {
        toks: lex(src)?,
        pos: 0,
        animtree: None,
    };
    p.script()
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    animtree: Option<Name>,
}

impl Parser {
    fn tok(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn peek_at(&self, n: usize) -> &Tok {
        &self.toks[(self.pos + n).min(self.toks.len() - 1)].tok
    }

    fn line(&self) -> u32 {
        self.toks[self.pos].line
    }

    fn bump(&mut self) -> Tok {
        let t = self.toks[self.pos].tok.clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    fn err<T>(&self, msg: impl Into<String>) -> Result<T> {
        let t = &self.toks[self.pos];
        Err(CompileError::at(ErrorKind::Syntax, t.line, t.col, msg))
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.tok() == t {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: Tok) -> Result<()> {
        if self.eat(&t) {
            Ok(())
        } else {
            self.err(format!("expected {t:?}, found {:?}", self.tok()))
        }
    }

    fn ident(&mut self) -> Result<Name> {
        match self.tok().clone() {
            Tok::Ident(s) => {
                self.bump();
                Ok(s)
            }
            t => self.err(format!("expected identifier, found {t:?}")),
        }
    }

    fn script(&mut self) -> Result<Script> {
        let mut s = Script::default();
        self.top_items(&mut s, false)?;
        Ok(s)
    }

    fn top_items(&mut self, s: &mut Script, in_dev: bool) -> Result<()> {
        loop {
            match self.tok() {
                Tok::Eof if !in_dev => return Ok(()),
                Tok::DevClose if in_dev => {
                    self.bump();
                    return Ok(());
                }
                Tok::DevOpen if !in_dev => {
                    self.bump();
                    self.top_items(s, true)?;
                }
                Tok::Include => {
                    let line = self.line();
                    self.bump();
                    let mut path = self.ident()?.to_string();
                    while self.eat(&Tok::Slash) {
                        path.push('\\');
                        path.push_str(&self.ident()?);
                    }
                    self.expect(Tok::Semi)?;
                    s.includes.push(Include {
                        file: canonical_file(&path).into(),
                        line,
                    });
                }
                Tok::UsingAnimtree => {
                    self.bump();
                    self.expect(Tok::LParen)?;
                    let name = match self.bump() {
                        Tok::Str(n) => n,
                        t => return self.err(format!("expected animtree name, found {t:?}")),
                    };
                    self.expect(Tok::RParen)?;
                    self.expect(Tok::Semi)?;
                    self.animtree = Some(name);
                }
                Tok::Ident(_) => s.functions.push(self.function()?),
                t => return self.err(format!("unexpected {t:?} at top level")),
            }
        }
    }

    fn function(&mut self) -> Result<Function> {
        let line = self.line();
        let name = self.ident()?;
        self.expect(Tok::LParen)?;
        let mut params = Vec::new();
        if !self.eat(&Tok::RParen) {
            loop {
                params.push(self.ident()?);
                if self.eat(&Tok::RParen) {
                    break;
                }
                self.expect(Tok::Comma)?;
            }
        }
        self.expect(Tok::LBrace)?;
        let body = self.stmts_until(&Tok::RBrace)?;
        Ok(Function {
            name,
            params,
            body,
            line,
            animtree: self.animtree.clone(),
        })
    }

    /// Parses statements and consumes the closing token.
    fn stmts_until(&mut self, close: &Tok) -> Result<Vec<Stmt>> {
        let mut v = Vec::new();
        while self.tok() != close {
            if *self.tok() == Tok::Eof {
                return self.err(format!("expected {close:?}, found end of file"));
            }
            v.push(self.stmt()?);
        }
        self.bump();
        Ok(v)
    }

    fn stmt(&mut self) -> Result<Stmt> {
        let line = self.line();
        let kind = match self.tok() {
            Tok::Semi => {
                self.bump();
                StmtKind::Empty
            }
            Tok::LBrace => {
                self.bump();
                StmtKind::Block(self.stmts_until(&Tok::RBrace)?)
            }
            Tok::DevOpen => {
                self.bump();
                StmtKind::Dev(self.stmts_until(&Tok::DevClose)?)
            }
            Tok::If => {
                self.bump();
                let cond = self.paren_expr()?;
                let then = Box::new(self.stmt()?);
                let otherwise = if self.eat(&Tok::Else) {
                    Some(Box::new(self.stmt()?))
                } else {
                    None
                };
                StmtKind::If {
                    cond,
                    then,
                    otherwise,
                }
            }
            Tok::While => {
                self.bump();
                let cond = self.paren_expr()?;
                StmtKind::While {
                    cond,
                    body: Box::new(self.stmt()?),
                }
            }
            Tok::For => {
                self.bump();
                self.expect(Tok::LParen)?;
                let init = self.opt_simple(&Tok::Semi)?;
                self.expect(Tok::Semi)?;
                let cond = if *self.tok() == Tok::Semi {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.expect(Tok::Semi)?;
                let step = self.opt_simple(&Tok::RParen)?;
                self.expect(Tok::RParen)?;
                StmtKind::For {
                    init,
                    cond,
                    step,
                    body: Box::new(self.stmt()?),
                }
            }
            Tok::Switch => {
                self.bump();
                let value = self.paren_expr()?;
                self.expect(Tok::LBrace)?;
                let mut items = Vec::new();
                while !self.eat(&Tok::RBrace) {
                    match self.tok() {
                        Tok::Case => {
                            self.bump();
                            items.push(SwitchItem::Case(self.expr()?));
                            self.expect(Tok::Colon)?;
                        }
                        Tok::Default => {
                            self.bump();
                            self.expect(Tok::Colon)?;
                            items.push(SwitchItem::Default);
                        }
                        Tok::Eof => return self.err("unterminated switch"),
                        _ => items.push(SwitchItem::Stmt(self.stmt()?)),
                    }
                }
                StmtKind::Switch { value, items }
            }
            Tok::Break => {
                self.bump();
                self.expect(Tok::Semi)?;
                StmtKind::Break
            }
            Tok::Continue => {
                self.bump();
                self.expect(Tok::Semi)?;
                StmtKind::Continue
            }
            Tok::Return => {
                self.bump();
                let v = if *self.tok() == Tok::Semi {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.expect(Tok::Semi)?;
                StmtKind::Return(v)
            }
            Tok::Ident(n) if matches!(&**n, "prof_begin" | "prof_end") => {
                // Language statements; the original drops them outside developer mode.
                self.bump();
                self.call_args()?;
                self.expect(Tok::Semi)?;
                StmtKind::Empty
            }
            _ => {
                let k = self.simple()?;
                self.expect(Tok::Semi)?;
                k
            }
        };
        Ok(Stmt { kind, line })
    }

    fn opt_simple(&mut self, end: &Tok) -> Result<Option<Box<Stmt>>> {
        if self.tok() == end {
            return Ok(None);
        }
        let line = self.line();
        let kind = self.simple()?;
        Ok(Some(Box::new(Stmt { kind, line })))
    }

    fn paren_expr(&mut self) -> Result<Expr> {
        self.expect(Tok::LParen)?;
        let e = self.expr()?;
        self.expect(Tok::RParen)?;
        Ok(e)
    }

    /// A statement without its trailing `;`: assignment, `++`/`--`, call, wait or event.
    fn simple(&mut self) -> Result<StmtKind> {
        if self.eat(&Tok::Wait) {
            return Ok(StmtKind::Wait(self.expr()?));
        }
        if self.eat(&Tok::WaittillFrameEnd) {
            self.eat_empty_parens();
            return Ok(StmtKind::WaittillFrameEnd);
        }
        let target = self.postfix()?;
        let op = match self.tok() {
            Tok::Assign => None,
            Tok::PlusAssign => Some(BinOp::Add),
            Tok::MinusAssign => Some(BinOp::Sub),
            Tok::StarAssign => Some(BinOp::Mul),
            Tok::SlashAssign => Some(BinOp::Div),
            Tok::PercentAssign => Some(BinOp::Mod),
            Tok::AmpAssign => Some(BinOp::BitAnd),
            Tok::PipeAssign => Some(BinOp::BitOr),
            Tok::CaretAssign => Some(BinOp::BitXor),
            Tok::ShlAssign => Some(BinOp::Shl),
            Tok::ShrAssign => Some(BinOp::Shr),
            Tok::PlusPlus | Tok::MinusMinus => {
                let delta = if self.bump() == Tok::PlusPlus { 1 } else { -1 };
                return Ok(StmtKind::IncDec { target, delta });
            }
            Tok::Waittill | Tok::WaittillMatch | Tok::Notify | Tok::Endon => {
                let kind = match self.bump() {
                    Tok::Waittill => EventKind::Waittill,
                    Tok::WaittillMatch => EventKind::WaittillMatch,
                    Tok::Notify => EventKind::Notify,
                    _ => EventKind::Endon,
                };
                let args = self.call_args()?;
                if args.is_empty() {
                    return self.err("event statement needs an event name");
                }
                return Ok(StmtKind::Event {
                    kind,
                    object: target,
                    args,
                });
            }
            Tok::WaittillFrameEnd => {
                self.bump();
                self.eat_empty_parens();
                return Ok(StmtKind::WaittillFrameEnd);
            }
            _ => {
                return if matches!(target.kind, ExprKind::Call(_)) {
                    Ok(StmtKind::Expr(target))
                } else {
                    self.err("expression is not a statement")
                };
            }
        };
        self.bump();
        let value = self.expr()?;
        Ok(StmtKind::Assign { target, op, value })
    }

    fn eat_empty_parens(&mut self) {
        if *self.tok() == Tok::LParen && *self.peek_at(1) == Tok::RParen {
            self.bump();
            self.bump();
        }
    }

    fn expr(&mut self) -> Result<Expr> {
        self.binary(1)
    }

    fn binary(&mut self, min_prec: u8) -> Result<Expr> {
        let mut lhs = self.unary()?;
        loop {
            let (op, prec) = match self.tok() {
                Tok::OrOr => (BinOp::Or, 1),
                Tok::AndAnd => (BinOp::And, 2),
                Tok::Pipe => (BinOp::BitOr, 3),
                Tok::Caret => (BinOp::BitXor, 4),
                Tok::Amp => (BinOp::BitAnd, 5),
                Tok::EqEq => (BinOp::Eq, 6),
                Tok::Ne => (BinOp::Ne, 6),
                Tok::Lt => (BinOp::Lt, 7),
                Tok::Le => (BinOp::Le, 7),
                Tok::Gt => (BinOp::Gt, 7),
                Tok::Ge => (BinOp::Ge, 7),
                Tok::Shl => (BinOp::Shl, 8),
                Tok::Shr => (BinOp::Shr, 8),
                Tok::Plus => (BinOp::Add, 9),
                Tok::Minus => (BinOp::Sub, 9),
                Tok::Star => (BinOp::Mul, 10),
                Tok::Slash => (BinOp::Div, 10),
                Tok::Percent => (BinOp::Mod, 10),
                _ => return Ok(lhs),
            };
            if prec < min_prec {
                return Ok(lhs);
            }
            let line = self.line();
            self.bump();
            let rhs = self.binary(prec + 1)?;
            lhs = Expr {
                kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                line,
            };
        }
    }

    fn unary(&mut self) -> Result<Expr> {
        let op = match self.tok() {
            Tok::Bang => UnOp::Not,
            Tok::Minus => UnOp::Neg,
            Tok::Tilde => UnOp::BitNot,
            _ => return self.postfix(),
        };
        let line = self.line();
        self.bump();
        let e = self.unary()?;
        Ok(Expr {
            kind: ExprKind::Unary(op, Box::new(e)),
            line,
        })
    }

    fn postfix(&mut self) -> Result<Expr> {
        let mut e = self.primary()?;
        loop {
            let line = self.line();
            match self.tok() {
                Tok::Dot => {
                    self.bump();
                    let name = match self.bump() {
                        Tok::Ident(n) => n,
                        t => match t.keyword_text() {
                            Some(k) => k.into(),
                            None => return self.err(format!("expected field name, found {t:?}")),
                        },
                    };
                    e = Expr {
                        kind: ExprKind::Field(Box::new(e), name),
                        line,
                    };
                }
                Tok::LBracket if !self.at_ptr_open() => {
                    self.bump();
                    let idx = self.expr()?;
                    self.expect(Tok::RBracket)?;
                    e = Expr {
                        kind: ExprKind::Index(Box::new(e), Box::new(idx)),
                        line,
                    };
                }
                _ if self.starts_object_call() => {
                    let call = self.call_tail(Some(Box::new(e)))?;
                    e = Expr {
                        kind: ExprKind::Call(call),
                        line,
                    };
                }
                _ => return Ok(e),
            }
        }
    }

    /// `[[` (possibly spaced), the start of a call through a pointer.
    fn at_ptr_open(&self) -> bool {
        *self.tok() == Tok::LBracket && *self.peek_at(1) == Tok::LBracket
    }

    fn starts_object_call(&self) -> bool {
        matches!(self.tok(), Tok::Ident(_) | Tok::Thread | Tok::ChildThread) || self.at_ptr_open()
    }

    /// `[thread|childthread] callee(args)` after an optional object.
    fn call_tail(&mut self, object: Option<Box<Expr>>) -> Result<Call> {
        let line = self.line();
        let mode = match self.tok() {
            Tok::Thread => {
                self.bump();
                CallMode::Thread
            }
            Tok::ChildThread => {
                self.bump();
                CallMode::ChildThread
            }
            _ => CallMode::Call,
        };
        let callee = if self.at_ptr_open() {
            self.bump();
            self.bump();
            let e = self.expr()?;
            self.expect(Tok::RBracket)?;
            self.expect(Tok::RBracket)?;
            Callee::Pointer(Box::new(e))
        } else {
            let first = self.ident()?;
            if self.eat(&Tok::ColonColon) {
                Callee::Path {
                    file: first,
                    name: self.ident()?,
                }
            } else {
                Callee::Name(first)
            }
        };
        let args = self.call_args()?;
        Ok(Call {
            object,
            mode,
            callee,
            args,
            line,
        })
    }

    fn call_args(&mut self) -> Result<Vec<Expr>> {
        self.expect(Tok::LParen)?;
        let mut args = Vec::new();
        if self.eat(&Tok::RParen) {
            return Ok(args);
        }
        loop {
            args.push(self.expr()?);
            if self.eat(&Tok::RParen) {
                return Ok(args);
            }
            self.expect(Tok::Comma)?;
        }
    }

    fn primary(&mut self) -> Result<Expr> {
        let line = self.line();
        let kind = match self.tok().clone() {
            Tok::Int(v) => {
                self.bump();
                ExprKind::Int(v as i32)
            }
            Tok::Float(v) => {
                self.bump();
                ExprKind::Float(v)
            }
            Tok::Str(s) => {
                self.bump();
                ExprKind::Str(s)
            }
            Tok::Amp if matches!(self.peek_at(1), Tok::Str(_)) => {
                self.bump();
                match self.bump() {
                    Tok::Str(s) => ExprKind::LocStr(s),
                    _ => unreachable!(),
                }
            }
            Tok::Percent => {
                self.bump();
                ExprKind::AnimRef(self.ident()?)
            }
            Tok::AnimTree => {
                self.bump();
                ExprKind::AnimTree
            }
            Tok::Undefined => {
                self.bump();
                ExprKind::Undefined
            }
            Tok::True => {
                self.bump();
                ExprKind::Int(1)
            }
            Tok::False => {
                self.bump();
                ExprKind::Int(0)
            }
            Tok::SelfKw => {
                self.bump();
                ExprKind::SelfRef
            }
            Tok::Level => {
                self.bump();
                ExprKind::Level
            }
            Tok::Game => {
                self.bump();
                ExprKind::Game
            }
            Tok::AnimKw => {
                self.bump();
                ExprKind::Anim
            }
            Tok::LBracket if *self.peek_at(1) == Tok::RBracket => {
                self.bump();
                self.bump();
                ExprKind::EmptyArray
            }
            Tok::LParen => {
                self.bump();
                let first = self.expr()?;
                if self.eat(&Tok::RParen) {
                    return Ok(first);
                }
                self.expect(Tok::Comma)?;
                let second = self.expr()?;
                self.expect(Tok::Comma)?;
                let third = self.expr()?;
                self.expect(Tok::RParen)?;
                ExprKind::Vector(Box::new([first, second, third]))
            }
            Tok::ColonColon => {
                self.bump();
                ExprKind::FuncRef {
                    file: None,
                    name: self.ident()?,
                }
            }
            Tok::Ident(first) => {
                if *self.peek_at(1) == Tok::LParen {
                    return self.call_expr();
                }
                self.bump();
                if self.eat(&Tok::ColonColon) {
                    let name = self.ident()?;
                    if *self.tok() == Tok::LParen {
                        let args = self.call_args()?;
                        ExprKind::Call(Call {
                            object: None,
                            mode: CallMode::Call,
                            callee: Callee::Path { file: first, name },
                            args,
                            line,
                        })
                    } else {
                        ExprKind::FuncRef {
                            file: Some(first),
                            name,
                        }
                    }
                } else {
                    ExprKind::Var(first)
                }
            }
            Tok::Thread | Tok::ChildThread => return self.call_expr(),
            Tok::LBracket if self.at_ptr_open() => return self.call_expr(),
            t => return self.err(format!("unexpected {t:?} in expression")),
        };
        Ok(Expr { kind, line })
    }

    fn call_expr(&mut self) -> Result<Expr> {
        let line = self.line();
        let call = self.call_tail(None)?;
        Ok(Expr {
            kind: ExprKind::Call(call),
            line,
        })
    }
}
