// SPDX-License-Identifier: GPL-3.0-or-later
//! Script values and the objects they point to.
//!
//! Arrays have value semantics (copy on write, as in the original: writing through a shared
//! array copies it first). Structs, `level`, `anim` and entities are shared objects.
//!
//! The original keeps every object and every field or array element in one fixed pool
//! (0x8000 parents, 0xFFFE children). The same two counters are kept here and checked where
//! scripts allocate; locals, thread stacks and notify registrations are not counted.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::bytecode::FuncId;

pub type Str = Rc<str>;

pub const MAX_OBJECTS: u32 = 0x8000;
pub const MAX_VALUES: u32 = 0xFFFE;

thread_local! {
    static OBJECTS: Cell<u32> = const { Cell::new(0) };
    static VALUES: Cell<u32> = const { Cell::new(0) };
}

fn bump(counter: &'static std::thread::LocalKey<Cell<u32>>, by: i64) {
    counter.with(|c| c.set((i64::from(c.get()) + by) as u32));
}

/// True when another object or value would not fit the original's pools.
pub(crate) fn pool_full() -> bool {
    OBJECTS.with(Cell::get) >= MAX_OBJECTS || VALUES.with(Cell::get) >= MAX_VALUES
}

/// The live object and value counts of this thread's pools.
pub fn pool_usage() -> (u32, u32) {
    (OBJECTS.with(Cell::get), VALUES.with(Cell::get))
}

#[derive(Clone)]
pub enum Value {
    Undefined,
    Int(i32),
    Float(f32),
    Str(Str),
    /// Localized string reference (`&"X"`).
    LocStr(Str),
    Vector([f32; 3]),
    Array(Rc<Array>),
    Object(Obj),
    Func(FuncId),
    Anim(Str),
    AnimTree(Str),
    /// An lvalue under construction; only ever lives on the VM stack.
    Ref(Box<Ref>),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Undefined => "undefined",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "string",
            Value::LocStr(_) => "localized string",
            Value::Vector(_) => "vector",
            Value::Array(_) => "array",
            Value::Object(o) if o.is_entity() => "entity",
            Value::Object(_) => "object",
            Value::Func(_) => "function",
            Value::Anim(_) => "animation",
            Value::AnimTree(_) => "animtree",
            Value::Ref(_) => "reference",
        }
    }

    pub fn str(s: &str) -> Value {
        Value::Str(s.into())
    }
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Undefined => write!(f, "undefined"),
            Value::Int(v) => write!(f, "{v}"),
            Value::Float(v) => write!(f, "{v:?}"),
            Value::Str(s) => write!(f, "{s:?}"),
            Value::LocStr(s) => write!(f, "&{s:?}"),
            Value::Vector(v) => write!(f, "({}, {}, {})", v[0], v[1], v[2]),
            Value::Array(a) => f.debug_list().entries(a.entries.iter()).finish(),
            Value::Object(o) => write!(f, "<{}>", o.kind_name()),
            Value::Func(id) => write!(f, "function#{id}"),
            Value::Anim(s) => write!(f, "%{s}"),
            Value::AnimTree(s) => write!(f, "#animtree {s}"),
            Value::Ref(_) => write!(f, "<ref>"),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Key {
    Int(i32),
    Str(Str),
}

/// An assignable location: a root plus array indices below it.
#[derive(Clone)]
pub struct Ref {
    pub(crate) root: Root,
    pub(crate) path: Vec<Key>,
}

#[derive(Clone)]
pub(crate) enum Root {
    /// Absolute index into the thread's local slots.
    Local(usize),
    Game,
    Field(Obj, Str),
}

/// Map with int and string keys, stored in insertion order.
///
/// The original keeps an array's elements as a child list of the array object. A new element
/// is linked in at the **head** (`GetNewVariableIndexInternal2`, used by every ordinary array
/// store), removal unlinks in place, and `getarraykeys` (`Scr_AddArrayKeys`) walks the list
/// head to tail. So keys come out newest first: see [`Array::keys_original_order`]. Copying
/// an array (copy on write, `CopyArray`) walks the source list head to tail and head-inserts
/// into the copy, which **reverses** the order; [`Clone`] does the same.
#[derive(Default)]
pub struct Array {
    entries: Vec<(Key, Value)>,
    index: HashMap<Key, usize>,
}

impl Array {
    pub fn new() -> Array {
        bump(&OBJECTS, 1);
        Array::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, k: &Key) -> Option<&Value> {
        self.index.get(k).map(|&i| &self.entries[i].1)
    }

    /// Elements in insertion order (oldest first), which is the *reverse* of the order the
    /// original's scripts see; use [`Self::keys_original_order`] for that.
    pub fn iter(&self) -> impl Iterator<Item = (&Key, &Value)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    /// The keys in the order the original's `getarraykeys` returns them: the child list from
    /// head to tail, i.e. most recently inserted first (research: `Scr_AddArrayKeys` walks
    /// `FindFirstSibling`/`FindNextSibling`; new elements are head-inserted). A key whose
    /// value is overwritten keeps its place; a removed and re-added key counts as new.
    pub fn keys_original_order(&self) -> impl Iterator<Item = &Key> {
        self.entries.iter().rev().map(|(k, _)| k)
    }

    pub fn get_mut_or_insert(&mut self, k: Key) -> &mut Value {
        let i = match self.index.get(&k) {
            Some(&i) => i,
            None => {
                bump(&VALUES, 1);
                self.index.insert(k.clone(), self.entries.len());
                self.entries.push((k, Value::Undefined));
                self.entries.len() - 1
            }
        };
        &mut self.entries[i].1
    }

    /// Sets `k`; storing `undefined` removes the element, as in the original.
    pub fn set(&mut self, k: Key, v: Value) {
        if matches!(v, Value::Undefined) {
            self.remove(&k);
        } else {
            *self.get_mut_or_insert(k) = v;
        }
    }

    pub fn remove(&mut self, k: &Key) {
        if let Some(i) = self.index.remove(k) {
            self.entries.remove(i);
            bump(&VALUES, -1);
            for (j, (key, _)) in self.entries.iter().enumerate().skip(i) {
                self.index.insert(key.clone(), j);
            }
        }
    }
}

impl Clone for Array {
    /// Reverses the element order, as the original's `CopyArray` does (see the type docs).
    fn clone(&self) -> Array {
        bump(&OBJECTS, 1);
        bump(&VALUES, self.entries.len() as i64);
        let entries: Vec<(Key, Value)> = self.entries.iter().rev().cloned().collect();
        let index = entries
            .iter()
            .enumerate()
            .map(|(i, (k, _))| (k.clone(), i))
            .collect();
        Array { entries, index }
    }
}

impl Drop for Array {
    fn drop(&mut self) {
        bump(&OBJECTS, -1);
        bump(&VALUES, -(self.entries.len() as i64));
    }
}

/// Entity class of a script object that stands for an engine entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntClass {
    Entity,
    HudElem,
    PathNode,
    VehicleNode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntRef {
    pub num: u16,
    pub class: EntClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Struct,
    Level,
    Anim,
    Entity(EntRef),
}

/// One notify registration, kept in registration order (oldest first).
#[derive(Clone)]
pub(crate) struct NEntry {
    pub id: u64,
    pub thread: crate::vm::ThreadId,
    pub what: NWhat,
}

#[derive(Clone)]
pub(crate) enum NWhat {
    /// `waittill` taking `n` payload values.
    Waittill(u8),
    /// `waittillmatch` comparing the first values.
    Match(Vec<Value>),
    /// `endon` of the frame with this uid.
    Endon(u64),
}

pub struct Object {
    pub(crate) kind: Kind,
    dead: Cell<bool>,
    fields: RefCell<HashMap<Str, Value>>,
    pub(crate) notify: RefCell<HashMap<Str, Vec<NEntry>>>,
}

impl Drop for Object {
    fn drop(&mut self) {
        bump(&OBJECTS, -1);
        bump(&VALUES, -(self.fields.get_mut().len() as i64));
    }
}

/// Shared handle to a script object; equality is identity.
#[derive(Clone)]
pub struct Obj(pub(crate) Rc<Object>);

impl PartialEq for Obj {
    fn eq(&self, o: &Obj) -> bool {
        Rc::ptr_eq(&self.0, &o.0)
    }
}

impl Obj {
    pub(crate) fn new(kind: Kind) -> Obj {
        bump(&OBJECTS, 1);
        Obj(Rc::new(Object {
            kind,
            dead: Cell::new(false),
            fields: RefCell::default(),
            notify: RefCell::default(),
        }))
    }

    /// A fresh plain object, as `spawnstruct()` returns.
    pub fn new_struct() -> Obj {
        Obj::new(Kind::Struct)
    }

    pub fn is_entity(&self) -> bool {
        matches!(self.0.kind, Kind::Entity(_))
    }

    /// The engine entity this object stands for, unless it was freed.
    pub fn entity(&self) -> Option<EntRef> {
        match self.0.kind {
            Kind::Entity(e) if !self.0.dead.get() => Some(e),
            _ => None,
        }
    }

    /// True for the handle of a freed entity.
    pub fn is_dead(&self) -> bool {
        self.0.dead.get()
    }

    pub(crate) fn kind_name(&self) -> &'static str {
        match self.0.kind {
            Kind::Struct => "struct",
            Kind::Level => "level",
            Kind::Anim => "anim",
            Kind::Entity(_) => "entity",
        }
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        self.0.fields.borrow().get(name).cloned()
    }

    /// Sets a field; `undefined` removes it.
    pub fn set(&self, name: &Str, v: Value) {
        let mut f = self.0.fields.borrow_mut();
        if matches!(v, Value::Undefined) {
            if f.remove(name).is_some() {
                bump(&VALUES, -1);
            }
        } else if f.insert(name.clone(), v).is_none() {
            bump(&VALUES, 1);
        }
    }

    pub(crate) fn mark_dead(&self) {
        self.0.dead.set(true);
    }

    pub(crate) fn clear_fields(&self) {
        let mut f = self.0.fields.borrow_mut();
        bump(&VALUES, -(f.len() as i64));
        f.clear();
    }

    pub(crate) fn with_field<R>(&self, name: &Str, f: impl FnOnce(&mut Value) -> R) -> R {
        let mut fields = self.0.fields.borrow_mut();
        let mut fresh = false;
        let slot = fields.entry(name.clone()).or_insert_with(|| {
            fresh = true;
            Value::Undefined
        });
        if fresh {
            bump(&VALUES, 1);
        }
        let r = f(slot);
        if matches!(slot, Value::Undefined) {
            fields.remove(name);
            bump(&VALUES, -1);
        }
        r
    }

    pub(crate) fn add_entry(&self, name: &Str, e: NEntry) {
        self.0
            .notify
            .borrow_mut()
            .entry(name.clone())
            .or_default()
            .push(e);
    }

    pub(crate) fn remove_entry(&self, name: &str, id: u64) {
        let mut n = self.0.notify.borrow_mut();
        if let Some(list) = n.get_mut(name) {
            list.retain(|e| e.id != id);
            if list.is_empty() {
                n.remove(name);
            }
        }
    }
}
