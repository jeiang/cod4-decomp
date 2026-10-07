// SPDX-License-Identifier: GPL-3.0-or-later
//! AST to bytecode. All files are compiled together so cross-file calls bind to function ids.

use std::collections::HashMap;

use crate::ast::*;
use crate::builtins::Builtins;
use crate::bytecode::{self, CALL_CHILD, CALL_METHOD, CALL_THREAD, File, Function as Code, Op};
use crate::error::{CompileError, ErrorKind};
use crate::parser::{canonical_file, parse};

type Result<T, E = CompileError> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Compile `/# ... #/` blocks (the original does so only with `developer_script`).
    pub developer: bool,
}

/// Compiles `(file name, source text)` pairs into one [`bytecode::Program`].
/// Names may be given as `maps/mp/foo.gsc`; they are canonicalized. Returns every error found.
pub fn compile(
    sources: &[(&str, &str)],
    builtins: &Builtins,
    opts: Options,
) -> Result<bytecode::Program, Vec<CompileError>> {
    let mut errors = Vec::new();
    let mut scripts = Vec::with_capacity(sources.len());
    let mut names: Vec<String> = Vec::with_capacity(sources.len());
    for (name, text) in sources {
        let name = canonical_file(name);
        match parse(text) {
            Ok(s) => scripts.push(s),
            Err(mut e) => {
                e.file = name.clone();
                errors.push(e);
                scripts.push(Script::default());
            }
        }
        if names.contains(&name) {
            errors.push(file_error(&name, 0, "file given twice"));
        }
        names.push(name);
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    // Symbols: function ids in file order, includes resolved to file indices.
    let mut symbols: Vec<FileSyms> = Vec::with_capacity(scripts.len());
    let mut next_id = 0u32;
    for (script, name) in scripts.iter().zip(&names) {
        let start = next_id;
        let mut funcs = HashMap::new();
        for f in &script.functions {
            if funcs.insert(f.name.clone(), next_id).is_some() {
                errors.push(file_error(
                    name,
                    f.line,
                    format!("function `{}` is defined twice", f.name),
                ));
            }
            next_id += 1;
        }
        symbols.push(FileSyms {
            funcs,
            direct: HashMap::new(),
            includes: Vec::new(),
            range: start..next_id,
        });
    }
    // Include lists, resolved to file indices.
    let mut includes: Vec<Vec<(usize, u32)>> = vec![Vec::new(); scripts.len()];
    for (i, script) in scripts.iter().enumerate() {
        for inc in &script.includes {
            match names.iter().position(|n| *n == *inc.file) {
                Some(j) => includes[i].push((j, inc.line)),
                None => errors.push(CompileError {
                    kind: ErrorKind::UnknownFile,
                    ..file_error(
                        &names[i],
                        inc.line,
                        format!("#include of unknown file `{}`", inc.file),
                    )
                }),
            }
        }
    }
    // The original copies an included file's functions into the includer; a name that is
    // already taken (by the file's own functions or another include) is a compile error. A
    // file included twice, directly or through another include, is the same functions and
    // is accepted. Stock scripts only call what they include directly; calls through a
    // second level of includes are accepted too, resolved after the direct ones.
    for (i, list) in includes.iter().enumerate() {
        let mut table = symbols[i].funcs.clone();
        for &(j, line) in list {
            for (name, id) in &symbols[j].funcs {
                if let Some(old) = table.insert(name.clone(), *id)
                    && old != *id
                {
                    errors.push(file_error(
                        &names[i],
                        line,
                        format!("function `{name}` already defined"),
                    ));
                }
            }
            symbols[i].includes.push(j);
        }
        symbols[i].direct = table;
    }

    let mut strings = Interner::default();
    let mut functions = Vec::with_capacity(next_id as usize);
    for (fi, script) in scripts.iter().enumerate() {
        for f in &script.functions {
            let mut fc = FnCompiler {
                files: &symbols,
                names: &names,
                file: fi,
                builtins,
                strings: &mut strings,
                opts,
                code: Vec::new(),
                lines: Vec::new(),
                locals: HashMap::new(),
                local_count: 0,
                loops: Vec::new(),
                animtree: f.animtree.as_ref(),
                line: f.line,
            };
            match fc.function(f) {
                Ok(()) => functions.push(Code {
                    name: f.name.to_string(),
                    file: fi as u32,
                    param_count: f.params.len() as u16,
                    local_count: fc.local_count,
                    code: fc.code,
                    lines: fc.lines,
                }),
                Err(mut e) => {
                    e.file = names[fi].clone();
                    errors.push(e);
                    functions.push(Code::default());
                }
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(bytecode::Program {
        files: names
            .into_iter()
            .zip(&symbols)
            .map(|(name, s)| File {
                name,
                functions: s.range.clone(),
            })
            .collect(),
        functions,
        strings: strings.list,
        builtins: builtins.clone(),
    })
}

fn file_error(file: &str, line: u32, msg: impl Into<String>) -> CompileError {
    CompileError {
        file: file.to_string(),
        ..CompileError::at(ErrorKind::Semantic, line, 0, msg)
    }
}

struct FileSyms {
    /// The file's own functions.
    funcs: HashMap<Name, u32>,
    /// Own plus directly included functions.
    direct: HashMap<Name, u32>,
    includes: Vec<usize>,
    range: std::ops::Range<u32>,
}

#[derive(Default)]
struct Interner {
    list: Vec<Box<str>>,
    map: HashMap<Box<str>, u32>,
}

impl Interner {
    fn get(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.map.get(s) {
            return i;
        }
        let i = self.list.len() as u32;
        self.list.push(s.into());
        self.map.insert(s.into(), i);
        i
    }
}

struct LoopCtx {
    is_switch: bool,
    breaks: Vec<usize>,
    continues: Vec<usize>,
}

struct FnCompiler<'a> {
    files: &'a [FileSyms],
    names: &'a [String],
    file: usize,
    builtins: &'a Builtins,
    strings: &'a mut Interner,
    opts: Options,
    code: Vec<u8>,
    lines: Vec<(u32, u32)>,
    locals: HashMap<Name, u16>,
    local_count: u16,
    loops: Vec<LoopCtx>,
    animtree: Option<&'a Name>,
    line: u32,
}

impl FnCompiler<'_> {
    fn err<T>(&self, kind: ErrorKind, msg: impl Into<String>) -> Result<T> {
        Err(CompileError::at(kind, self.line, 0, msg))
    }

    fn emit(&mut self, op: Op) {
        self.code.push(op as u8);
    }

    fn emit_u8(&mut self, op: Op, a: u8) {
        self.emit(op);
        self.code.push(a);
    }

    fn emit_u16(&mut self, op: Op, a: u16) {
        self.emit(op);
        self.code.extend_from_slice(&a.to_le_bytes());
    }

    fn emit_u32(&mut self, op: Op, a: u32) {
        self.emit(op);
        self.code.extend_from_slice(&a.to_le_bytes());
    }

    fn emit_str(&mut self, op: Op, s: &str) {
        let i = self.strings.get(s);
        self.emit_u32(op, i);
    }

    /// Emits a jump with a placeholder offset and returns the operand position.
    fn emit_jump(&mut self, op: Op) -> usize {
        self.emit_u32(op, 0);
        self.code.len() - 4
    }

    fn patch(&mut self, at: usize, target: usize) {
        let rel = target as i64 - (at + 4) as i64;
        self.code[at..at + 4].copy_from_slice(&(rel as i32).to_le_bytes());
    }

    fn patch_here(&mut self, at: usize) {
        self.patch(at, self.code.len());
    }

    fn jump_to(&mut self, op: Op, target: usize) {
        let at = self.emit_jump(op);
        self.patch(at, target);
    }

    fn mark(&mut self, line: u32) {
        self.line = line;
        if self.lines.last().is_none_or(|l| l.1 != line) {
            self.lines.push((self.code.len() as u32, line));
        }
    }

    fn slot(&mut self, name: &Name) -> Result<u16> {
        if let Some(&s) = self.locals.get(name) {
            return Ok(s);
        }
        let s = self.local_count;
        self.local_count = match s.checked_add(1) {
            Some(n) => n,
            None => return self.err(ErrorKind::Semantic, "too many local variables"),
        };
        self.locals.insert(name.clone(), s);
        Ok(s)
    }

    fn temp(&mut self) -> Result<u16> {
        let s = self.local_count;
        self.local_count = match s.checked_add(1) {
            Some(n) => n,
            None => return self.err(ErrorKind::Semantic, "too many local variables"),
        };
        Ok(s)
    }

    fn function(&mut self, f: &Function) -> Result<()> {
        if f.params.len() > 255 {
            return self.err(ErrorKind::Semantic, "parameter count exceeds 255");
        }
        for p in &f.params {
            // Parameters occupy slots 0..n even when a name repeats.
            let s = self.temp()?;
            self.locals.insert(p.clone(), s);
        }
        self.stmts(&f.body)?;
        self.emit(Op::ReturnUndefined);
        Ok(())
    }

    /// Own and directly included functions first, then deeper includes (depth first).
    fn lookup(&self, file: usize, name: &str) -> Option<u32> {
        fn deeper(
            files: &[FileSyms],
            file: usize,
            name: &str,
            seen: &mut Vec<usize>,
        ) -> Option<u32> {
            if seen.contains(&file) {
                return None;
            }
            seen.push(file);
            files[file].includes.iter().find_map(|&i| {
                files[i]
                    .direct
                    .get(name)
                    .copied()
                    .or_else(|| deeper(files, i, name, seen))
            })
        }
        let f = &self.files[file];
        f.direct
            .get(name)
            .copied()
            .or_else(|| deeper(self.files, file, name, &mut Vec::new()))
    }

    /// A full canonical name, or a bare last path segment (`_utility::f`).
    fn find_file(&self, f: &str) -> Option<usize> {
        self.names.iter().position(|n| n == f).or_else(|| {
            let tail = format!("\\{f}");
            self.names.iter().position(|n| n.ends_with(&tail))
        })
    }

    fn resolve(&self, file: Option<&str>, name: &str) -> Result<u32> {
        let (fi, shown) = match file {
            None => (self.file, None),
            Some(f) => match self.find_file(f) {
                Some(i) => (i, Some(f)),
                None => {
                    return self.err(ErrorKind::UnknownFile, format!("unknown file `{f}`"));
                }
            },
        };
        match self.lookup(fi, name) {
            Some(id) => Ok(id),
            None => match shown {
                Some(f) => self.err(
                    ErrorKind::UnknownFunction,
                    format!("unknown function `{f}::{name}`"),
                ),
                None => self.err(
                    ErrorKind::UnknownFunction,
                    format!("unknown function `{name}`"),
                ),
            },
        }
    }

    fn stmts(&mut self, v: &[Stmt]) -> Result<()> {
        v.iter().try_for_each(|s| self.stmt(s))
    }

    fn stmt(&mut self, s: &Stmt) -> Result<()> {
        self.mark(s.line);
        match &s.kind {
            StmtKind::Empty => {}
            StmtKind::Block(v) => self.stmts(v)?,
            StmtKind::Dev(v) => {
                if self.opts.developer {
                    self.stmts(v)?;
                }
            }
            StmtKind::Expr(e) => {
                self.expr(e)?;
                self.emit(Op::Pop);
            }
            StmtKind::Assign { target, op, value } => {
                self.assign(target, *op, |c| c.expr(value))?;
            }
            StmtKind::IncDec { target, delta } => {
                self.lvalue(target)?;
                self.emit(if *delta > 0 { Op::Inc } else { Op::Dec });
            }
            StmtKind::If {
                cond,
                then,
                otherwise,
            } => {
                self.expr(cond)?;
                let to_else = self.emit_jump(Op::JumpIfFalse);
                self.stmt(then)?;
                match otherwise {
                    Some(o) => {
                        let to_end = self.emit_jump(Op::Jump);
                        self.patch_here(to_else);
                        self.stmt(o)?;
                        self.patch_here(to_end);
                    }
                    None => self.patch_here(to_else),
                }
            }
            StmtKind::While { cond, body } => {
                let start = self.code.len();
                self.expr(cond)?;
                let exit = self.emit_jump(Op::JumpIfFalse);
                self.loops.push(LoopCtx {
                    is_switch: false,
                    breaks: vec![],
                    continues: vec![],
                });
                self.stmt(body)?;
                self.jump_to(Op::Jump, start);
                self.end_loop(vec![exit], start);
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(i) = init {
                    self.stmt(i)?;
                }
                let start = self.code.len();
                let exit = match cond {
                    Some(c) => {
                        self.expr(c)?;
                        Some(self.emit_jump(Op::JumpIfFalse))
                    }
                    None => None,
                };
                self.loops.push(LoopCtx {
                    is_switch: false,
                    breaks: vec![],
                    continues: vec![],
                });
                self.stmt(body)?;
                let step_at = self.code.len();
                if let Some(st) = step {
                    self.stmt(st)?;
                }
                self.jump_to(Op::Jump, start);
                self.end_loop(exit.into_iter().collect(), step_at);
            }
            StmtKind::Switch { value, items } => self.switch(value, items)?,
            StmtKind::Break => {
                let at = self.emit_jump(Op::Jump);
                match self.loops.last_mut() {
                    Some(l) => l.breaks.push(at),
                    None => return self.err(ErrorKind::Semantic, "break outside loop or switch"),
                }
            }
            StmtKind::Continue => {
                let at = self.emit_jump(Op::Jump);
                match self.loops.iter_mut().rev().find(|l| !l.is_switch) {
                    Some(l) => l.continues.push(at),
                    None => return self.err(ErrorKind::Semantic, "continue outside loop"),
                }
            }
            StmtKind::Return(v) => match v {
                Some(e) => {
                    self.expr(e)?;
                    self.emit(Op::Return);
                }
                None => self.emit(Op::ReturnUndefined),
            },
            StmtKind::Wait(e) => {
                self.expr(e)?;
                self.emit(Op::Wait);
            }
            StmtKind::WaittillFrameEnd => self.emit(Op::WaittillFrameEnd),
            StmtKind::Event { kind, object, args } => self.event(*kind, object, args)?,
        }
        Ok(())
    }

    /// Pops the loop context, patches `exits` to here, breaks to here and continues to `cont`.
    fn end_loop(&mut self, exits: Vec<usize>, cont: usize) {
        let l = self.loops.pop().expect("loop context");
        for at in exits.into_iter().chain(l.breaks) {
            self.patch_here(at);
        }
        for at in l.continues {
            self.patch(at, cont);
        }
    }

    fn switch(&mut self, value: &Expr, items: &[SwitchItem]) -> Result<()> {
        self.expr(value)?;
        let tmp = self.temp()?;
        self.emit_u16(Op::SetLocal, tmp);
        let mut case_jumps = Vec::new();
        for it in items {
            if let SwitchItem::Case(c) = it {
                self.emit_u16(Op::GetLocal, tmp);
                match &c.kind {
                    ExprKind::Int(_) | ExprKind::Str(_) | ExprKind::Unary(UnOp::Neg, _) => {
                        self.expr(c)?
                    }
                    _ => return self.err(ErrorKind::Semantic, "case label must be a constant"),
                }
                self.emit(Op::Eq);
                case_jumps.push(self.emit_jump(Op::JumpIfTrue));
            }
        }
        let no_match = self.emit_jump(Op::Jump);
        self.loops.push(LoopCtx {
            is_switch: true,
            breaks: vec![],
            continues: vec![],
        });
        let mut next_case = case_jumps.into_iter();
        let mut default = None;
        for it in items {
            match it {
                SwitchItem::Case(_) => {
                    let at = next_case.next().expect("one jump per case");
                    self.patch_here(at);
                }
                SwitchItem::Default => default = Some(self.code.len()),
                SwitchItem::Stmt(s) => self.stmt(s)?,
            }
        }
        match default {
            Some(d) => self.patch(no_match, d),
            None => self.patch_here(no_match),
        }
        let l = self.loops.pop().expect("switch context");
        for at in l.breaks {
            self.patch_here(at);
        }
        // `continue` inside a switch targets the enclosing loop.
        if !l.continues.is_empty() {
            match self.loops.iter_mut().rev().find(|l| !l.is_switch) {
                Some(outer) => outer.continues.extend(l.continues),
                None => return self.err(ErrorKind::Semantic, "continue outside loop"),
            }
        }
        Ok(())
    }

    fn event(&mut self, kind: EventKind, object: &Expr, args: &[Expr]) -> Result<()> {
        let rest = &args[1..];
        match kind {
            EventKind::Endon => {
                if !rest.is_empty() {
                    return self.err(ErrorKind::Semantic, "endon takes one event name");
                }
                self.expr(&args[0])?;
                self.expr(object)?;
                self.emit(Op::Endon);
            }
            EventKind::Notify | EventKind::WaittillMatch => {
                let n = self.count(rest.len())?;
                for a in rest.iter().rev() {
                    self.expr(a)?;
                }
                self.expr(&args[0])?;
                self.expr(object)?;
                self.emit_u8(
                    if kind == EventKind::Notify {
                        Op::Notify
                    } else {
                        Op::WaittillMatch
                    },
                    n,
                );
            }
            EventKind::Waittill => {
                let n = self.count(rest.len())?;
                self.expr(&args[0])?;
                self.expr(object)?;
                self.emit_u8(Op::Waittill, n);
                for t in rest {
                    match &t.kind {
                        ExprKind::Var(name) => {
                            let s = self.slot(name)?;
                            self.emit_u16(Op::SetLocal, s);
                        }
                        _ => {
                            self.lvalue(t)?;
                            self.emit(Op::Store);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn count(&self, n: usize) -> Result<u8> {
        match u8::try_from(n) {
            Ok(n) => Ok(n),
            Err(_) => self.err(ErrorKind::Semantic, "parameter count exceeds 255"),
        }
    }

    fn emit_int(&mut self, v: i32) {
        self.emit(Op::PushInt);
        self.code.extend_from_slice(&v.to_le_bytes());
    }

    /// `target = value` or `target op= value`; `rhs` pushes the value.
    fn assign(
        &mut self,
        target: &Expr,
        op: Option<BinOp>,
        rhs: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        match (&target.kind, op) {
            (ExprKind::Var(n), None) => {
                rhs(self)?;
                let s = self.slot(n)?;
                self.emit_u16(Op::SetLocal, s);
            }
            (ExprKind::Var(n), Some(op)) => {
                let s = self.slot(n)?;
                self.emit_u16(Op::GetLocal, s);
                rhs(self)?;
                self.emit(binop(op));
                self.emit_u16(Op::SetLocal, s);
            }
            (_, None) => {
                rhs(self)?;
                self.lvalue(target)?;
                self.emit(Op::Store);
            }
            (_, Some(op)) => {
                // The original reads the target, applies the operator, then builds the
                // reference again for the store.
                self.expr(target)?;
                rhs(self)?;
                self.emit(binop(op));
                self.lvalue(target)?;
                self.emit(Op::Store);
            }
        }
        Ok(())
    }

    fn lvalue(&mut self, e: &Expr) -> Result<()> {
        self.line = e.line;
        match &e.kind {
            ExprKind::Var(n) => {
                let s = self.slot(n)?;
                self.emit_u16(Op::RefLocal, s);
            }
            ExprKind::Game => self.emit(Op::RefGame),
            ExprKind::Field(b, name) => {
                self.lvalue_base(b)?;
                self.emit_str(Op::RefField, name);
            }
            ExprKind::Index(b, k) => {
                self.expr(k)?;
                self.lvalue_base(b)?;
                self.emit(Op::RefIndex);
            }
            _ => return self.err(ErrorKind::Semantic, "expression is not assignable"),
        }
        Ok(())
    }

    fn lvalue_base(&mut self, b: &Expr) -> Result<()> {
        match b.kind {
            ExprKind::Var(_) | ExprKind::Game | ExprKind::Field(..) | ExprKind::Index(..) => {
                self.lvalue(b)
            }
            _ => self.expr(b),
        }
    }

    fn expr(&mut self, e: &Expr) -> Result<()> {
        self.line = e.line;
        match &e.kind {
            ExprKind::Int(v) => self.emit_int(*v),
            ExprKind::Float(v) => {
                self.emit(Op::PushFloat);
                self.code.extend_from_slice(&v.to_le_bytes());
            }
            ExprKind::Str(s) => self.emit_str(Op::PushStr, s),
            ExprKind::LocStr(s) => self.emit_str(Op::PushLocStr, s),
            ExprKind::AnimRef(s) => {
                self.need_animtree()?;
                self.emit_str(Op::PushAnimRef, s);
            }
            ExprKind::AnimTree => {
                let name = self.need_animtree()?;
                self.emit_str(Op::PushAnimTree, &name);
            }
            ExprKind::Undefined => self.emit(Op::PushUndefined),
            ExprKind::SelfRef => self.emit(Op::PushSelf),
            ExprKind::Level => self.emit(Op::PushLevel),
            ExprKind::Game => self.emit(Op::PushGame),
            ExprKind::Anim => self.emit(Op::PushAnimGlobal),
            ExprKind::Var(n) => {
                let s = self.slot(n)?;
                self.emit_u16(Op::GetLocal, s);
            }
            ExprKind::Vector(parts) => {
                for p in parts.iter().rev() {
                    self.expr(p)?;
                }
                self.emit(Op::PushVector);
            }
            ExprKind::EmptyArray => self.emit(Op::PushEmptyArray),
            ExprKind::Field(b, name) => {
                self.expr(b)?;
                self.emit_str(Op::GetField, name);
            }
            ExprKind::Index(b, k) => {
                self.expr(b)?;
                self.expr(k)?;
                self.emit(Op::GetIndex);
            }
            ExprKind::FuncRef { file, name } => {
                let id = self.resolve(file.as_deref().map(canonical_file).as_deref(), name)?;
                self.emit_u32(Op::PushFuncPtr, id);
            }
            ExprKind::Call(c) => self.call(c)?,
            ExprKind::Unary(op, x) => match (op, &x.kind) {
                (UnOp::Neg, ExprKind::Int(v)) => self.emit_int(v.wrapping_neg()),
                (UnOp::Neg, ExprKind::Float(v)) => {
                    self.emit(Op::PushFloat);
                    self.code.extend_from_slice(&(-v).to_le_bytes());
                }
                _ => {
                    self.expr(x)?;
                    self.emit(match op {
                        UnOp::Neg => Op::Neg,
                        UnOp::Not => Op::Not,
                        UnOp::BitNot => Op::BitNot,
                    });
                }
            },
            ExprKind::Binary(op @ (BinOp::And | BinOp::Or), l, r) => {
                self.expr(l)?;
                let j = self.emit_jump(if *op == BinOp::And {
                    Op::AndJump
                } else {
                    Op::OrJump
                });
                self.expr(r)?;
                self.emit(Op::ToBool);
                self.patch_here(j);
            }
            ExprKind::Binary(op, l, r) => {
                self.expr(l)?;
                self.expr(r)?;
                self.emit(binop(*op));
            }
        }
        Ok(())
    }

    fn need_animtree(&self) -> Result<Name> {
        match self.animtree {
            Some(n) => Ok(n.clone()),
            None => self.err(
                ErrorKind::Semantic,
                "animation reference without a preceding #using_animtree",
            ),
        }
    }

    fn call(&mut self, c: &Call) -> Result<()> {
        self.line = c.line;
        let argc = self.count(c.args.len())?;
        let mut flags = match c.mode {
            CallMode::Call => 0,
            CallMode::Thread => CALL_THREAD,
            CallMode::ChildThread => CALL_THREAD | CALL_CHILD,
        };
        if c.object.is_some() {
            flags |= CALL_METHOD;
        }
        let target = match &c.callee {
            // Builtins are resolved first, and only for plain calls: `thread name()` always
            // means a script function.
            Callee::Name(n) => {
                let builtin = c.mode == CallMode::Call
                    && match &c.object {
                        None => self.builtins.function(n).is_some(),
                        Some(_) => self.builtins.method(n).is_some(),
                    };
                match self.lookup(self.file, n) {
                    Some(id) if !builtin => Some(id),
                    _ => return self.builtin_call(c, n, argc),
                }
            }
            Callee::Path { file, name } => Some(self.resolve(Some(&canonical_file(file)), name)?),
            Callee::Pointer(_) => None,
        };
        for a in c.args.iter().rev() {
            self.expr(a)?;
        }
        // The receiver is evaluated before the pointer, and ends up below it.
        if let Some(o) = &c.object {
            self.expr(o)?;
        }
        if let Callee::Pointer(p) = &c.callee {
            self.expr(p)?;
        }
        self.line = c.line;
        match target {
            Some(id) => {
                self.emit(Op::CallFunc);
                self.code.push(flags);
                self.code.extend_from_slice(&id.to_le_bytes());
                self.code.push(argc);
            }
            None => {
                self.emit(Op::CallPtr);
                self.code.push(flags);
                self.code.push(argc);
            }
        }
        Ok(())
    }

    fn builtin_call(&mut self, c: &Call, name: &str, argc: u8) -> Result<()> {
        if c.mode != CallMode::Call {
            return self.err(
                ErrorKind::Semantic,
                format!("builtin `{name}` cannot be threaded"),
            );
        }
        let (op, idx) = match &c.object {
            None => (Op::CallBuiltin, self.builtins.function(name)),
            Some(_) => (Op::CallBuiltinMethod, self.builtins.method(name)),
        };
        let Some(idx) = idx else {
            let what = if c.object.is_some() {
                "method"
            } else {
                "function"
            };
            return self.err(
                ErrorKind::UnknownFunction,
                format!("unknown {what} `{name}`"),
            );
        };
        for a in c.args.iter().rev() {
            self.expr(a)?;
        }
        if let Some(o) = &c.object {
            self.expr(o)?;
        }
        self.line = c.line;
        self.emit_u16(op, idx);
        self.code.push(argc);
        Ok(())
    }
}

fn binop(op: BinOp) -> Op {
    match op {
        BinOp::Add => Op::Add,
        BinOp::Sub => Op::Sub,
        BinOp::Mul => Op::Mul,
        BinOp::Div => Op::Div,
        BinOp::Mod => Op::Mod,
        BinOp::BitAnd => Op::BitAnd,
        BinOp::BitOr => Op::BitOr,
        BinOp::BitXor => Op::BitXor,
        BinOp::Shl => Op::Shl,
        BinOp::Shr => Op::Shr,
        BinOp::Eq => Op::Eq,
        BinOp::Ne => Op::Ne,
        BinOp::Lt => Op::Lt,
        BinOp::Le => Op::Le,
        BinOp::Gt => Op::Gt,
        BinOp::Ge => Op::Ge,
        BinOp::And | BinOp::Or => unreachable!("short-circuit ops are compiled separately"),
    }
}
