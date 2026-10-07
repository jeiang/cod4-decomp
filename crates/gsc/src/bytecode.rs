// SPDX-License-Identifier: GPL-3.0-or-later
//! Bytecode for the GSC VM.
//!
//! One [`Function`] is one byte string; operands are little-endian and jump offsets are
//! relative to the byte after the jump instruction. The VM is a value stack machine over
//! per-frame local slots, so a thread is just its saved frames and stack: suspending
//! (`Wait`, `Waittill`, `WaittillMatch`, `WaittillFrameEnd`) stores them and resuming
//! continues at the next instruction.
//!
//! Conventions the VM relies on:
//! * Call arguments are pushed last-to-first, then the callee pointer (`CallPtr`), then the
//!   object (method forms), so the object is on top and argument 0 is next. The callee
//!   binds its parameters by popping `param_count` values into locals `0..param_count`.
//!   Missing arguments are `undefined`; extra ones are dropped.
//! * Every call pushes exactly one result; thread calls push `undefined`.
//! * `Jump*` with a negative offset is a backward jump: the infinite-loop guard point.
//! * Lvalues are built as references: `RefLocal`/`RefGame` start a chain, a value on the
//!   stack (`self`, `level`, any expression) can start one too, and `RefField`/`RefIndex`
//!   extend it. `Store` pops the reference, then the value below it. `LoadRef` pushes the
//!   referenced value while keeping the reference (compound assignment).
//! * `Waittill n` pops the event name and object, parks the thread, and on wake pushes the
//!   notify payload padded or truncated to `n` values, first payload value on top.
//! * `WaittillMatch n` pops the event name and object, then `n` match values (first on top).
//! * `Notify n` pops object, event name, then `n` payload values (first on top).
//! * `Endon` pops object and event name.

use crate::builtins::Builtins;

pub type FuncId = u32;

macro_rules! ops {
    ($( $(#[$m:meta])* $name:ident = $code:literal [$($imm:ident),*] ),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u8)]
        pub enum Op { $($(#[$m])* $name = $code),* }

        impl Op {
            pub const ALL: &'static [Op] = &[$(Op::$name),*];

            pub fn from_u8(b: u8) -> Option<Op> {
                Op::ALL.iter().copied().find(|o| *o as u8 == b)
            }

            /// Operand layout following the opcode byte.
            pub fn operands(self) -> &'static [Imm] {
                match self { $(Op::$name => &[$(Imm::$imm),*]),* }
            }
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Imm {
    U8,
    U16,
    U32,
    I32,
    F32,
}

impl Imm {
    pub fn size(self) -> usize {
        match self {
            Imm::U8 => 1,
            Imm::U16 => 2,
            Imm::U32 | Imm::I32 | Imm::F32 => 4,
        }
    }
}

/// `CallFunc`/`CallPtr` flag bits.
pub const CALL_THREAD: u8 = 1;
pub const CALL_CHILD: u8 = 2;
pub const CALL_METHOD: u8 = 4;

ops! {
    PushInt = 0 [I32],
    PushFloat = 1 [F32],
    /// Index into [`Program::strings`].
    PushStr = 2 [U32],
    /// Localized string reference (`&"X"`).
    PushLocStr = 3 [U32],
    PushAnimRef = 4 [U32],
    PushAnimTree = 5 [U32],
    PushUndefined = 6 [],
    PushSelf = 7 [],
    PushLevel = 8 [],
    PushGame = 9 [],
    PushAnimGlobal = 10 [],
    /// Pops x, y, z (x on top).
    PushVector = 11 [],
    PushEmptyArray = 12 [],
    /// Function pointer to a script function.
    PushFuncPtr = 13 [U32],
    GetLocal = 14 [U16],
    SetLocal = 15 [U16],
    GetField = 16 [U32],
    GetIndex = 17 [],
    RefLocal = 18 [U16],
    RefGame = 19 [],
    RefField = 20 [U32],
    RefIndex = 21 [],
    LoadRef = 22 [],
    Store = 23 [],
    Swap = 24 [],
    Pop = 25 [],
    Neg = 26 [],
    Not = 27 [],
    BitNot = 28 [],
    ToBool = 29 [],
    Add = 30 [],
    Sub = 31 [],
    Mul = 32 [],
    Div = 33 [],
    Mod = 34 [],
    BitAnd = 35 [],
    BitOr = 36 [],
    BitXor = 37 [],
    Shl = 38 [],
    Shr = 39 [],
    Eq = 40 [],
    Ne = 41 [],
    Lt = 42 [],
    Le = 43 [],
    Gt = 44 [],
    Ge = 45 [],
    Jump = 46 [I32],
    /// Pops; jumps when falsy.
    JumpIfFalse = 47 [I32],
    /// Pops; jumps when truthy.
    JumpIfTrue = 48 [I32],
    /// `&&`: pops; when falsy pushes int 0 and jumps.
    AndJump = 49 [I32],
    /// `||`: pops; when truthy pushes int 1 and jumps.
    OrJump = 50 [I32],
    /// flags, function id, argument count.
    CallFunc = 51 [U8, U32, U8],
    /// flags, argument count; the callee pointer is on the stack.
    CallPtr = 52 [U8, U8],
    /// builtin function index, argument count.
    CallBuiltin = 53 [U16, U8],
    /// builtin method index, argument count; the object is on top.
    CallBuiltinMethod = 54 [U16, U8],
    Return = 55 [],
    ReturnUndefined = 56 [],
    Wait = 57 [],
    WaittillFrameEnd = 58 [],
    Notify = 59 [U8],
    Endon = 60 [],
    Waittill = 61 [U8],
    WaittillMatch = 62 [U8],
}

#[derive(Debug, Clone, Default)]
pub struct Function {
    pub name: String,
    /// Index into [`Program::files`].
    pub file: u32,
    pub param_count: u16,
    pub local_count: u16,
    pub code: Vec<u8>,
    /// `(code offset, source line)` pairs in increasing offset order.
    pub lines: Vec<(u32, u32)>,
}

impl Function {
    pub fn line_at(&self, pc: usize) -> u32 {
        let i = self.lines.partition_point(|(off, _)| *off as usize <= pc);
        i.checked_sub(1).map_or(0, |i| self.lines[i].1)
    }

    /// One line per instruction, for tests and VM diagnostics.
    pub fn disassemble(&self, prog: &Program) -> Vec<String> {
        let mut out = Vec::new();
        let mut pc = 0;
        while pc < self.code.len() {
            let start = pc;
            let Some(op) = Op::from_u8(self.code[pc]) else {
                out.push(format!("{start:04} ?{:#x}", self.code[pc]));
                break;
            };
            pc += 1;
            let mut s = format!("{start:04} {op:?}");
            let mut next = pc;
            for imm in op.operands() {
                next += imm.size();
            }
            for imm in op.operands() {
                let b = &self.code[pc..pc + imm.size()];
                pc += imm.size();
                let v = match imm {
                    Imm::U8 => b[0].to_string(),
                    Imm::U16 => u16::from_le_bytes([b[0], b[1]]).to_string(),
                    Imm::U32 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]).to_string(),
                    Imm::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]).to_string(),
                    Imm::I32 => {
                        let rel = i32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                        match op {
                            Op::PushInt => rel.to_string(),
                            _ => format!("->{}", next as i64 + i64::from(rel)),
                        }
                    }
                };
                s.push(' ');
                s.push_str(&v);
                if matches!(op, Op::PushStr | Op::GetField | Op::RefField)
                    && let Some(text) = v.parse::<usize>().ok().and_then(|i| prog.strings.get(i))
                {
                    s.push_str(&format!(" {text:?}"));
                }
            }
            out.push(s);
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct File {
    /// Canonical name (lowercase, backslashes, no extension).
    pub name: String,
    pub functions: std::ops::Range<FuncId>,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub files: Vec<File>,
    pub functions: Vec<Function>,
    pub strings: Vec<Box<str>>,
    /// The builtin tables the indices in the bytecode refer to.
    pub builtins: Builtins,
}

impl Program {
    /// Looks up `name` defined directly in `file` (both case-insensitive).
    pub fn find(&self, file: &str, name: &str) -> Option<FuncId> {
        let file = crate::canonical_file(file);
        let name = name.to_ascii_lowercase();
        let f = self.files.iter().find(|f| f.name == file)?;
        f.functions
            .clone()
            .find(|&id| self.functions[id as usize].name == name)
    }
}
