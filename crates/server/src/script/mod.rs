// SPDX-License-Identifier: GPL-3.0-or-later
//! The GSC host: builtin dispatch and entity fields.
//!
//! The compiler binds every builtin call to an index into the program's builtin tables. At
//! startup [`Dispatch`] resolves each name to its implementation: a real one, a logged no-op
//! that names the milestone the feature belongs to, or nothing (calling it is a script error).

use gsc::{EntClass, EntRef, Host, Program, Value, Vm};

use crate::game::{self, Game};

mod args;
mod funcs;
mod hud;
mod methods;

pub use args::Args;

pub type FuncFn = fn(&mut Game, &mut Vm, Args) -> Result<Value, String>;
pub type MethFn = fn(&mut Game, &mut Vm, EntRef, Args) -> Result<Value, String>;

/// One builtin table entry.
#[derive(Clone, Copy)]
pub enum Impl<F> {
    Real(F),
    /// Accepted and ignored; the first call is logged with the milestone that implements it.
    Later(&'static str),
}

pub struct Dispatch {
    funcs: Vec<(Box<str>, Option<Impl<FuncFn>>)>,
    methods: Vec<(Box<str>, Option<Impl<MethFn>>)>,
}

fn resolve<F: Copy>(
    names: &[Box<str>],
    table: &[(&str, Impl<F>)],
) -> Vec<(Box<str>, Option<Impl<F>>)> {
    names
        .iter()
        .map(|n| {
            (
                n.clone(),
                table.iter().find(|(t, _)| *t == &**n).map(|(_, i)| *i),
            )
        })
        .collect()
}

impl Dispatch {
    pub fn new(prog: &Program) -> Self {
        Self {
            funcs: resolve(prog.builtins.function_names(), funcs::TABLE),
            methods: resolve(prog.builtins.method_names(), methods::TABLE),
        }
    }

    /// Builtins the program binds that have no implementation at all.
    pub fn missing(&self) -> Vec<&str> {
        let f = self.funcs.iter().filter(|(_, i)| i.is_none());
        let m = self.methods.iter().filter(|(_, i)| i.is_none());
        f.map(|(n, _)| &**n).chain(m.map(|(n, _)| &**n)).collect()
    }

    /// `(name, milestone)` of every logged no-op the program binds.
    pub fn later(&self) -> Vec<(&str, &'static str)> {
        let f = self.funcs.iter().filter_map(|(n, i)| match i {
            Some(Impl::Later(m)) => Some((&**n, *m)),
            _ => None,
        });
        let m = self.methods.iter().filter_map(|(n, i)| match i {
            Some(Impl::Later(m)) => Some((&**n, *m)),
            _ => None,
        });
        f.chain(m).collect()
    }
}

/// Every logged no-op in the tables, with its milestone.
pub fn later_tables() -> Vec<(&'static str, &'static str)> {
    let f = funcs::TABLE.iter().filter_map(|(n, i)| match i {
        Impl::Later(m) => Some((*n, *m)),
        Impl::Real(_) => None,
    });
    let m = methods::TABLE.iter().filter_map(|(n, i)| match i {
        Impl::Later(m) => Some((*n, *m)),
        Impl::Real(_) => None,
    });
    f.chain(m).collect()
}

/// What the VM calls back into: the game state plus the resolved builtin tables.
pub struct ScriptHost<'a> {
    pub game: &'a mut Game,
    pub dispatch: &'a Dispatch,
}

impl ScriptHost<'_> {
    fn stub(&mut self, name: &str, milestone: &str) {
        let n = self.game.stub_calls.entry(name.to_owned()).or_insert(0);
        *n += 1;
        if *n == 1 {
            self.game.print(format!(
                "script: {name} is not implemented yet ({milestone}); ignored\n"
            ));
        }
    }
}

impl Host for ScriptHost<'_> {
    fn call_function(&mut self, vm: &mut Vm, index: u16, args: &[Value]) -> Result<Value, String> {
        let (name, imp) = &self.dispatch.funcs[usize::from(index)];
        match imp {
            Some(Impl::Real(f)) => f(self.game, vm, Args::new(name, args)),
            Some(Impl::Later(m)) => {
                self.stub(name, m);
                Ok(Value::Undefined)
            }
            None => Err(format!("builtin function '{name}' is not implemented")),
        }
    }

    fn call_method(
        &mut self,
        vm: &mut Vm,
        index: u16,
        ent: EntRef,
        args: &[Value],
    ) -> Result<Value, String> {
        let (name, imp) = &self.dispatch.methods[usize::from(index)];
        match imp {
            Some(Impl::Real(f)) => f(self.game, vm, ent, Args::new(name, args)),
            Some(Impl::Later(m)) => {
                self.stub(name, m);
                Ok(Value::Undefined)
            }
            None => Err(format!("builtin method '{name}' is not implemented")),
        }
    }

    fn get_field(&mut self, ent: EntRef, name: &str) -> Option<Value> {
        match ent.class {
            EntClass::Entity => game::get_ent_field(self.game.ent(ent.num)?, name),
            _ => None,
        }
    }

    fn set_field(&mut self, ent: EntRef, name: &str, value: &Value) -> Result<bool, String> {
        match ent.class {
            EntClass::Entity => match self.game.ent_mut(ent.num) {
                Some(e) => {
                    let set = game::set_ent_field(e, name, value)?;
                    if set && matches!(name, "origin" | "angles") {
                        self.game.relink(ent.num);
                    }
                    Ok(set)
                }
                None => Ok(false),
            },
            _ => Ok(false),
        }
    }
}
