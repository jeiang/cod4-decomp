// SPDX-License-Identifier: GPL-3.0-or-later
//! GSC compiler and bytecode VM.
//!
//! Source text goes through [`lexer`], [`parser`] (AST in [`ast`]) and [`compiler`] into a
//! [`bytecode::Program`]. Builtins are bound by index through [`Builtins`].
//!
//! [`Vm`] runs a program under the original engine's scheduling rules: saved-stack threads,
//! LIFO tick buckets, `notify`/`endon`/`waittill*` ordering and deferred entity free. The
//! server supplies builtins and entity fields through [`Host`]. See [`vm`] for the rules.

pub mod ast;
pub mod builtins;
pub mod bytecode;
pub mod compiler;
pub mod error;
mod inventory;
pub mod lexer;
pub mod ops;
pub mod parser;
pub mod value;
pub mod vm;

pub use builtins::Builtins;
pub use bytecode::{Function, Op, Program};
pub use compiler::{Options, compile};
pub use error::{CompileError, ErrorKind};
pub use parser::canonical_file;
pub use value::{Array, EntClass, EntRef, Key, Obj, Value};
pub use vm::{CallOutcome, Host, Vm, VmError, VmErrorKind};
