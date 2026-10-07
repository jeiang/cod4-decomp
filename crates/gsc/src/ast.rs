// SPDX-License-Identifier: GPL-3.0-or-later
//! Syntax tree for one GSC file. Names are already lowercased.

pub type Name = Box<str>;

#[derive(Debug, Default)]
pub struct Script {
    pub includes: Vec<Include>,
    pub functions: Vec<Function>,
}

#[derive(Debug)]
pub struct Include {
    /// Canonical file name (lowercase, backslashes, no extension).
    pub file: Name,
    pub line: u32,
}

#[derive(Debug)]
pub struct Function {
    pub name: Name,
    pub params: Vec<Name>,
    pub body: Vec<Stmt>,
    pub line: u32,
    /// The `#using_animtree` in force where the function was defined.
    pub animtree: Option<Name>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallMode {
    Call,
    Thread,
    ChildThread,
}

#[derive(Debug)]
pub enum Callee {
    /// `name`: own file, then includes, then builtins.
    Name(Name),
    /// `file::name`.
    Path { file: Name, name: Name },
    /// `[[ expr ]]`.
    Pointer(Box<Expr>),
}

#[derive(Debug)]
pub struct Call {
    pub object: Option<Box<Expr>>,
    pub mode: CallMode,
    pub callee: Callee,
    pub args: Vec<Expr>,
    pub line: u32,
}

#[derive(Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub line: u32,
}

#[derive(Debug)]
pub enum ExprKind {
    Int(i32),
    Float(f32),
    Str(Name),
    /// `&"LOC_REF"`.
    LocStr(Name),
    /// `%anim`.
    AnimRef(Name),
    /// `#animtree`.
    AnimTree,
    Undefined,
    SelfRef,
    Level,
    Game,
    Anim,
    Var(Name),
    Vector(Box<[Expr; 3]>),
    /// `[]`.
    EmptyArray,
    Field(Box<Expr>, Name),
    Index(Box<Expr>, Box<Expr>),
    /// `::name` or `file::name` used as a value.
    FuncRef {
        file: Option<Name>,
        name: Name,
    },
    Call(Call),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
}

#[derive(Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub line: u32,
}

#[derive(Debug)]
pub enum StmtKind {
    Empty,
    Expr(Expr),
    /// `target = value` or `target op= value`.
    Assign {
        target: Expr,
        op: Option<BinOp>,
        value: Expr,
    },
    /// `target++` / `target--` (`delta` is 1 or -1).
    IncDec {
        target: Expr,
        delta: i32,
    },
    Block(Vec<Stmt>),
    /// `/# ... #/`, compiled only with `Options::developer`.
    Dev(Vec<Stmt>),
    If {
        cond: Expr,
        then: Box<Stmt>,
        otherwise: Option<Box<Stmt>>,
    },
    While {
        cond: Expr,
        body: Box<Stmt>,
    },
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        step: Option<Box<Stmt>>,
        body: Box<Stmt>,
    },
    Switch {
        value: Expr,
        items: Vec<SwitchItem>,
    },
    Break,
    Continue,
    Return(Option<Expr>),
    Wait(Expr),
    WaittillFrameEnd,
    /// `object waittill/waittillmatch/notify/endon (args)`.
    Event {
        kind: EventKind,
        object: Expr,
        args: Vec<Expr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Waittill,
    WaittillMatch,
    Notify,
    Endon,
}

#[derive(Debug)]
pub enum SwitchItem {
    Case(Expr),
    Default,
    Stmt(Stmt),
}
