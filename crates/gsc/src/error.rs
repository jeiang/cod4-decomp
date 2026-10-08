// SPDX-License-Identifier: GPL-3.0-only
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Syntax,
    /// Call to a name that is not a script function or a registered builtin.
    UnknownFunction,
    /// `#include` or `path::fn` naming a file that is not in the compiled set.
    UnknownFile,
    /// Everything else the compiler rejects (duplicate functions, bad lvalues, ...).
    Semantic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    pub kind: ErrorKind,
    /// Canonical file name; filled in by the compiler driver.
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub message: String,
}

impl CompileError {
    pub(crate) fn at(kind: ErrorKind, line: u32, col: u32, message: impl Into<String>) -> Self {
        CompileError {
            kind,
            file: String::new(),
            line,
            col,
            message: message.into(),
        }
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}",
            self.file, self.line, self.col, self.message
        )
    }
}

impl std::error::Error for CompileError {}
