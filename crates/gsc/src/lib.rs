// SPDX-License-Identifier: GPL-3.0-or-later
//! GSC compiler and bytecode VM.
//!
//! Source text goes through [`lexer`], [`parser`] (AST in [`ast`]) and [`compiler`] into a
//! [`bytecode::Program`]. Builtins are bound by index through [`Builtins`].

pub mod ast;
pub mod builtins;
pub mod bytecode;
pub mod compiler;
pub mod error;
mod inventory;
pub mod lexer;
pub mod parser;

pub use builtins::Builtins;
pub use bytecode::{Function, Op, Program};
pub use compiler::{Options, compile};
pub use error::{CompileError, ErrorKind};
pub use parser::canonical_file;
