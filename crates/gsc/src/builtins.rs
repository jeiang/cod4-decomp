// SPDX-License-Identifier: GPL-3.0-or-later
//! Builtin binding tables. The compiler turns every builtin call into an index into one of
//! two tables; the server registers the implementations under the same indices.
//!
//! The original resolves methods over five class tables searched in a fixed order (player,
//! script entity, hud element, helicopter, entity) with first match winning. Here that is
//! registration order: register methods in the original's table order and the first
//! registration of a name keeps its index.

use std::collections::HashMap;

use crate::inventory;

#[derive(Debug, Clone, Default)]
pub struct Builtins {
    functions: Vec<Box<str>>,
    methods: Vec<Box<str>>,
    function_index: HashMap<Box<str>, u16>,
    method_index: HashMap<Box<str>, u16>,
}

fn add(names: &mut Vec<Box<str>>, index: &mut HashMap<Box<str>, u16>, name: &str) -> u16 {
    let name = name.to_ascii_lowercase();
    if let Some(&i) = index.get(name.as_str()) {
        return i;
    }
    let i = u16::try_from(names.len()).expect("more than 65535 builtins");
    names.push(name.as_str().into());
    index.insert(name.into(), i);
    i
}

impl Builtins {
    pub fn new() -> Self {
        Self::default()
    }

    /// The 295 names the stock MP scripts call (the 296 of research/gsc minus the two statements, plus `abs`, which the sheet missed) (plus the `prof_begin`/`prof_end`
    /// statements, which are language syntax), in a stable but unspecified order.
    pub fn stock_mp() -> Self {
        let mut b = Self::new();
        for n in inventory::FUNCTIONS {
            b.add_function(n);
        }
        for n in inventory::METHODS {
            b.add_method(n);
        }
        b
    }

    /// Registers a function builtin and returns its index; an existing name keeps its index.
    pub fn add_function(&mut self, name: &str) -> u16 {
        add(&mut self.functions, &mut self.function_index, name)
    }

    /// Registers a method builtin and returns its index; an existing name keeps its index.
    pub fn add_method(&mut self, name: &str) -> u16 {
        add(&mut self.methods, &mut self.method_index, name)
    }

    pub fn function(&self, name: &str) -> Option<u16> {
        self.function_index.get(name).copied()
    }

    pub fn method(&self, name: &str) -> Option<u16> {
        self.method_index.get(name).copied()
    }

    pub fn function_names(&self) -> &[Box<str>] {
        &self.functions
    }

    pub fn method_names(&self) -> &[Box<str>] {
        &self.methods
    }
}
