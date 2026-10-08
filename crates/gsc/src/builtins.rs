// SPDX-License-Identifier: GPL-3.0-only
//! Builtin binding tables. The compiler turns every builtin call into an index into one of
//! two tables; the server registers the implementations under the same indices.
//!
//! The original resolves methods over five class tables searched in a fixed order (player,
//! script entity, hud element, helicopter, entity) with first match winning. Here that is
//! registration order: register methods in the original's table order and the first
//! registration of a name keeps its index. Each method also remembers the table it came
//! from ([`MethodClass`]); the original's method bodies validate the receiver class
//! (e.g. player methods need an entity with a client, hud element methods a hud element).

use std::collections::HashMap;

use crate::inventory;

/// The five method tables of the original, in `Scr_GetMethod` search order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MethodClass {
    /// `PlayerCmd_*`, 83 entries.
    Player,
    /// Movers and models, 18 entries.
    ScriptEnt,
    /// Hud elements, 22 entries.
    HudElem,
    /// Helicopters, 25 entries.
    Helicopter,
    /// The generic entity methods, 82 entries.
    Entity,
}

#[derive(Debug, Clone, Default)]
pub struct Builtins {
    functions: Vec<Box<str>>,
    methods: Vec<Box<str>>,
    method_classes: Vec<MethodClass>,
    function_index: HashMap<Box<str>, u16>,
    method_index: HashMap<Box<str>, u16>,
}

fn add(names: &mut Vec<Box<str>>, index: &mut HashMap<Box<str>, u16>, name: &str) -> (u16, bool) {
    let name = name.to_ascii_lowercase();
    if let Some(&i) = index.get(name.as_str()) {
        return (i, false);
    }
    let i = u16::try_from(names.len()).expect("more than 65535 builtins");
    names.push(name.as_str().into());
    index.insert(name.into(), i);
    (i, true)
}

impl Builtins {
    pub fn new() -> Self {
        Self::default()
    }

    /// The complete original tables of iw3mp 1.7: all 205 function entries (204 distinct names: `weaponfiretime` is listed twice) in
    /// `Scr_GetFunction` order, and all 230 methods in the order Player, ScriptEnt, HudElem, Helicopter, Entity
    /// (each table in its own order), so indices and first-match-wins follow the original.
    pub fn stock_mp() -> Self {
        let mut b = Self::new();
        for n in inventory::FUNCTIONS {
            b.add_function(n);
        }
        let tables: [(MethodClass, &[&str]); 5] = [
            (MethodClass::Player, inventory::PLAYER_METHODS),
            (MethodClass::ScriptEnt, inventory::SCRIPT_ENT_METHODS),
            (MethodClass::HudElem, inventory::HUD_ELEM_METHODS),
            (MethodClass::Helicopter, inventory::HELICOPTER_METHODS),
            (MethodClass::Entity, inventory::ENTITY_METHODS),
        ];
        for (class, names) in tables {
            for n in names {
                b.add_method_in(class, n);
            }
        }
        b
    }

    /// Registers a function builtin and returns its index; an existing name keeps its index.
    pub fn add_function(&mut self, name: &str) -> u16 {
        add(&mut self.functions, &mut self.function_index, name).0
    }

    /// Registers a method builtin of the generic entity class; see [`Self::add_method_in`].
    pub fn add_method(&mut self, name: &str) -> u16 {
        self.add_method_in(MethodClass::Entity, name)
    }

    /// Registers a method builtin from `class` and returns its index. An existing name keeps
    /// its index and its original class (first match wins).
    pub fn add_method_in(&mut self, class: MethodClass, name: &str) -> u16 {
        let (i, new) = add(&mut self.methods, &mut self.method_index, name);
        if new {
            self.method_classes.push(class);
        }
        i
    }

    pub fn function(&self, name: &str) -> Option<u16> {
        self.function_index.get(name).copied()
    }

    pub fn method(&self, name: &str) -> Option<u16> {
        self.method_index.get(name).copied()
    }

    /// The table method `index` came from.
    pub fn method_class(&self, index: u16) -> Option<MethodClass> {
        self.method_classes.get(usize::from(index)).copied()
    }

    pub fn function_names(&self) -> &[Box<str>] {
        &self.functions
    }

    pub fn method_names(&self) -> &[Box<str>] {
        &self.methods
    }
}
