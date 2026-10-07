// SPDX-License-Identifier: GPL-3.0-or-later
//! The opcode loop. Conventions are documented in `bytecode.rs`.

use std::rc::Rc;

use super::*;
use crate::bytecode::{CALL_METHOD, CALL_THREAD};
use crate::value::{Array, Key, NEntry, Ref, Root, pool_full};

const POOL_FULL: &str = "exceeded maximum number of script variables";

fn rd16(code: &[u8], at: usize) -> usize {
    usize::from(u16::from_le_bytes([code[at], code[at + 1]]))
}

fn rd32(code: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([code[at], code[at + 1], code[at + 2], code[at + 3]])
}

fn key(v: &Value) -> Result<Key, String> {
    match v {
        Value::Int(i) => Ok(Key::Int(*i)),
        Value::Str(s) => Ok(Key::Str(s.clone())),
        other => Err(format!("{} is not an array index", other.type_name())),
    }
}

/// Stores `v` below `path` in `slot`, creating arrays on the way (copy on write).
fn set_path(slot: &mut Value, path: &[Key], v: Value) -> Result<(), String> {
    let Some((k, rest)) = path.split_first() else {
        *slot = v;
        return Ok(());
    };
    if matches!(slot, Value::Undefined) {
        *slot = Value::Array(Rc::new(Array::new()));
    }
    match slot {
        Value::Array(a) => {
            let a = Rc::make_mut(a);
            if rest.is_empty() {
                a.set(k.clone(), v);
            } else {
                let next = a.get_mut_or_insert(k.clone());
                set_path(next, rest, v)?;
                if matches!(next, Value::Undefined) {
                    a.remove(k);
                }
            }
            Ok(())
        }
        Value::Str(_) => Err("string characters cannot be individually changed".into()),
        Value::Vector(_) => Err("vector components cannot be individually changed".into()),
        other => Err(format!("{} is not an array", other.type_name())),
    }
}

fn event_target(obj: Value, name: Value) -> Result<(Obj, Str), String> {
    let Value::Object(obj) = obj else {
        return Err(format!("{} is not an object", obj.type_name()));
    };
    let Value::Str(name) = name else {
        return Err(format!("{} is not a string", name.type_name()));
    };
    Ok((obj, name))
}

impl Vm {
    fn read_ref(&self, host: &mut dyn Host, t: &Thread, r: &Ref) -> Result<Value, String> {
        let mut cur = match &r.root {
            Root::Local(i) => t.locals[*i].clone(),
            Root::Game => self.game.clone(),
            Root::Field(o, name) => match o.entity().and_then(|e| host.get_field(e, name)) {
                Some(v) => v,
                None => o.get(name).unwrap_or(Value::Undefined),
            },
        };
        for k in &r.path {
            cur = match cur {
                Value::Array(a) => a.get(k).cloned().unwrap_or(Value::Undefined),
                Value::Undefined => Value::Undefined,
                other => return Err(format!("{} is not an array", other.type_name())),
            };
        }
        Ok(cur)
    }

    fn write_ref(
        &mut self,
        host: &mut dyn Host,
        t: &mut Thread,
        r: &Ref,
        v: Value,
    ) -> Result<(), String> {
        if pool_full() {
            return Err(POOL_FULL.into());
        }
        match &r.root {
            Root::Local(i) => set_path(&mut t.locals[*i], &r.path, v),
            Root::Game => set_path(&mut self.game, &r.path, v),
            Root::Field(o, name) if r.path.is_empty() => {
                if let Some(e) = o.entity()
                    && host.set_field(e, name, &v)?
                {
                    return Ok(());
                }
                o.set(name, v);
                Ok(())
            }
            Root::Field(o, name) => {
                let mut r2 = Ok(());
                o.with_field(name, |slot| r2 = set_path(slot, &r.path, v));
                r2
            }
        }
    }

    /// Runs `t` from its top frame until the thread yields, ends or fails.
    #[allow(clippy::too_many_lines)]
    pub(super) fn run(
        &mut self,
        host: &mut dyn Host,
        tid: ThreadId,
        t: &mut Thread,
    ) -> Result<Exit, VmError> {
        let prog = Rc::clone(&self.prog);
        let top = t.frames.last().expect("a thread has a frame");
        let mut code: &[u8] = &prog.functions[top.func as usize].code;
        let mut pc = top.pc;
        let mut lbase = top.lbase;

        macro_rules! save {
            () => {
                t.frames.last_mut().expect("frame").pc = pc
            };
        }
        macro_rules! bail {
            ($m:expr) => {{
                save!();
                return Err(script_error($m));
            }};
        }
        macro_rules! pop {
            () => {
                match t.stack.pop() {
                    Some(v) => v,
                    None => {
                        save!();
                        return Err(fault("value stack underflow"));
                    }
                }
            };
        }
        macro_rules! push {
            ($v:expr) => {{
                if self.stack_base + t.stack.len() >= MAX_STACK {
                    bail!("Internal script stack overflow");
                }
                t.stack.push($v);
            }};
        }
        macro_rules! tri {
            ($e:expr) => {
                match $e {
                    Ok(v) => v,
                    Err(m) => bail!(m),
                }
            };
        }
        macro_rules! check_pending {
            () => {
                if let Some(k) = self.pending_kill(tid, t) {
                    save!();
                    if k == 0 {
                        return Ok(Exit::Dead);
                    }
                    let n = self.unwind_to(t, k);
                    self.depth -= n;
                    t.state = State::Sched(self.time);
                    self.bucket_head(self.time, tid);
                    return Ok(Exit::Yield);
                }
            };
        }
        macro_rules! backward_guard {
            () => {
                self.jumps = self.jumps.wrapping_add(1);
                if self.jumps & 0xFF == 0 && self.timer.elapsed() >= self.timeout {
                    if self.loading {
                        self.guard_warnings += 1;
                        self.reset_timeout();
                    } else {
                        bail!("potential infinite loop in script");
                    }
                }
            };
        }
        // Calls script function `$fid`: a new frame, or a new thread run to its first yield.
        macro_rules! enter {
            ($fid:expr, $flags:expr, $argc:expr, $this:expr) => {{
                let fid = $fid as usize;
                let argc = $argc as usize;
                if self.depth >= MAX_DEPTH {
                    bail!("script stack overflow (too many embedded function calls)");
                }
                let callee = &prog.functions[fid];
                let params = callee.param_count as usize;
                let this: Obj = match $this {
                    Some(o) => o,
                    None => t.frames.last().expect("frame").this.clone(),
                };
                save!();
                if $flags & CALL_THREAD != 0 {
                    let mut nt = self.new_thread(fid as FuncId, this);
                    for i in 0..argc {
                        let v = pop!();
                        if i < params {
                            nt.locals[i] = v;
                        }
                    }
                    let ntid = self.alloc_slot();
                    self.slots[ntid.idx as usize].state = SlotState::Running(Vec::new());
                    let held = t.stack.len();
                    self.stack_base += held;
                    let r = self.exec(host, ntid, nt);
                    self.stack_base -= held;
                    r?;
                    push!(Value::Undefined);
                    check_pending!();
                } else {
                    let base = t.locals.len();
                    t.locals
                        .resize(base + callee.local_count as usize, Value::Undefined);
                    for i in 0..argc {
                        let v = pop!();
                        if i < params {
                            t.locals[base + i] = v;
                        }
                    }
                    let uid = self.uid();
                    t.frames.push(Frame {
                        func: fid as FuncId,
                        pc: 0,
                        lbase: base,
                        sbase: t.stack.len(),
                        this,
                        uid,
                        endons: Vec::new(),
                    });
                    self.depth += 1;
                    code = &callee.code;
                    pc = 0;
                    lbase = base;
                }
            }};
        }
        macro_rules! do_return {
            ($v:expr) => {{
                let v = $v;
                let mut f = t.frames.pop().expect("frame");
                Self::release_frame(&mut f);
                self.depth -= 1;
                t.locals.truncate(f.lbase);
                t.stack.truncate(f.sbase);
                match t.frames.last() {
                    None => return Ok(Exit::Done(v)),
                    Some(c) => {
                        code = &prog.functions[c.func as usize].code;
                        pc = c.pc;
                        lbase = c.lbase;
                        push!(v);
                    }
                }
            }};
        }

        loop {
            self.ops += 1;
            let op = Op::ALL[usize::from(code[pc])];
            pc += 1;
            match op {
                Op::PushInt => {
                    push!(Value::Int(rd32(code, pc) as i32));
                    pc += 4;
                }
                Op::PushFloat => {
                    push!(Value::Float(f32::from_bits(rd32(code, pc))));
                    pc += 4;
                }
                Op::PushStr => {
                    push!(Value::Str(self.strings[rd32(code, pc) as usize].clone()));
                    pc += 4;
                }
                Op::PushLocStr => {
                    push!(Value::LocStr(self.strings[rd32(code, pc) as usize].clone()));
                    pc += 4;
                }
                Op::PushAnimRef => {
                    push!(Value::Anim(self.strings[rd32(code, pc) as usize].clone()));
                    pc += 4;
                }
                Op::PushAnimTree => {
                    push!(Value::AnimTree(
                        self.strings[rd32(code, pc) as usize].clone()
                    ));
                    pc += 4;
                }
                Op::PushUndefined => push!(Value::Undefined),
                Op::PushSelf => {
                    let this = t.frames.last().expect("frame").this.clone();
                    push!(Value::Object(this));
                }
                Op::PushLevel => push!(Value::Object(self.level.clone())),
                Op::PushGame => push!(self.game.clone()),
                Op::PushAnimGlobal => push!(Value::Object(self.anim.clone())),
                Op::PushVector => {
                    let x = pop!();
                    let y = pop!();
                    let z = pop!();
                    let v = tri!(ops::vector(&x, &y, &z));
                    push!(v);
                }
                Op::PushEmptyArray => {
                    if pool_full() {
                        bail!(POOL_FULL);
                    }
                    push!(Value::Array(Rc::new(Array::new())));
                }
                Op::PushFuncPtr => {
                    push!(Value::Func(rd32(code, pc)));
                    pc += 4;
                }
                Op::GetLocal => {
                    let v = t.locals[lbase + rd16(code, pc)].clone();
                    pc += 2;
                    push!(v);
                }
                Op::SetLocal => {
                    let v = pop!();
                    t.locals[lbase + rd16(code, pc)] = v;
                    pc += 2;
                }
                Op::GetField => {
                    let si = rd32(code, pc);
                    pc += 4;
                    let base = pop!();
                    let v = if self.size_string == Some(si) {
                        tri!(ops::size(&base))
                    } else {
                        let name = &self.strings[si as usize];
                        match &base {
                            Value::Object(o) => {
                                match o.entity().and_then(|e| host.get_field(e, name)) {
                                    Some(v) => v,
                                    None => o.get(name).unwrap_or(Value::Undefined),
                                }
                            }
                            other => bail!(format!("{} is not a field object", other.type_name())),
                        }
                    };
                    push!(v);
                }
                Op::GetIndex => {
                    let k = pop!();
                    let base = pop!();
                    let v = tri!(ops::index(&base, &k));
                    push!(v);
                }
                Op::RefLocal => {
                    let r = Ref {
                        root: Root::Local(lbase + rd16(code, pc)),
                        path: Vec::new(),
                    };
                    pc += 2;
                    push!(Value::Ref(Box::new(r)));
                }
                Op::RefGame => {
                    let r = Ref {
                        root: Root::Game,
                        path: Vec::new(),
                    };
                    push!(Value::Ref(Box::new(r)));
                }
                Op::RefField => {
                    let name = self.strings[rd32(code, pc) as usize].clone();
                    pc += 4;
                    let base = pop!();
                    let base = match base {
                        Value::Ref(r) => tri!(self.read_ref(host, t, &r)),
                        v => v,
                    };
                    let Value::Object(o) = base else {
                        bail!(format!("{} is not a field object", base.type_name()));
                    };
                    let r = Ref {
                        root: Root::Field(o, name),
                        path: Vec::new(),
                    };
                    push!(Value::Ref(Box::new(r)));
                }
                Op::RefIndex => {
                    let k = tri!(key(&pop!()));
                    match pop!() {
                        Value::Ref(mut r) => {
                            r.path.push(k);
                            push!(Value::Ref(r));
                        }
                        other => bail!(format!("{} is not assignable", other.type_name())),
                    }
                }
                Op::LoadRef => {
                    let Some(Value::Ref(r)) = t.stack.last() else {
                        bail!("LoadRef without a reference");
                    };
                    let r = r.clone();
                    let v = tri!(self.read_ref(host, t, &r));
                    push!(v);
                }
                Op::Store => {
                    let Value::Ref(r) = pop!() else {
                        save!();
                        return Err(fault("Store without a reference"));
                    };
                    let v = pop!();
                    tri!(self.write_ref(host, t, &r, v));
                }
                Op::Swap => {
                    let n = t.stack.len();
                    if n < 2 {
                        save!();
                        return Err(fault("value stack underflow"));
                    }
                    t.stack.swap(n - 1, n - 2);
                }
                Op::Pop => {
                    pop!();
                }
                Op::Neg => {
                    let v = pop!();
                    t.stack.push(tri!(ops::neg(v)));
                }
                Op::Not => {
                    let v = pop!();
                    t.stack
                        .push(Value::Int(i32::from(!tri!(ops::cast_bool(&v)))));
                }
                Op::BitNot => {
                    let v = pop!();
                    t.stack.push(tri!(ops::bit_not(v)));
                }
                Op::ToBool => {
                    let v = pop!();
                    t.stack
                        .push(Value::Int(i32::from(tri!(ops::cast_bool(&v)))));
                }
                Op::Add
                | Op::Sub
                | Op::Mul
                | Op::Div
                | Op::Mod
                | Op::BitAnd
                | Op::BitOr
                | Op::BitXor
                | Op::Shl
                | Op::Shr
                | Op::Eq
                | Op::Ne
                | Op::Lt
                | Op::Le
                | Op::Gt
                | Op::Ge => {
                    let b = pop!();
                    let a = pop!();
                    t.stack.push(tri!(ops::binary(op, a, b)));
                }
                Op::Jump => {
                    let rel = rd32(code, pc) as i32;
                    pc += 4;
                    if rel < 0 {
                        backward_guard!();
                    }
                    pc = (pc as i64 + i64::from(rel)) as usize;
                }
                Op::JumpIfFalse | Op::JumpIfTrue => {
                    let rel = rd32(code, pc) as i32;
                    pc += 4;
                    let v = pop!();
                    if tri!(ops::cast_bool(&v)) == (op == Op::JumpIfTrue) {
                        if rel < 0 {
                            backward_guard!();
                        }
                        pc = (pc as i64 + i64::from(rel)) as usize;
                    }
                }
                Op::AndJump | Op::OrJump => {
                    let rel = rd32(code, pc) as i32;
                    pc += 4;
                    let v = pop!();
                    if tri!(ops::cast_bool(&v)) == (op == Op::OrJump) {
                        t.stack.push(Value::Int(i32::from(op == Op::OrJump)));
                        pc = (pc as i64 + i64::from(rel)) as usize;
                    }
                }
                Op::CallFunc => {
                    let flags = code[pc];
                    let fid = rd32(code, pc + 1);
                    let argc = code[pc + 5];
                    pc += 6;
                    let this = if flags & CALL_METHOD != 0 {
                        match pop!() {
                            Value::Object(o) => Some(o),
                            other => bail!(format!("{} is not an object", other.type_name())),
                        }
                    } else {
                        None
                    };
                    enter!(fid, flags, argc, this);
                }
                Op::CallPtr => {
                    let flags = code[pc];
                    let argc = code[pc + 1];
                    pc += 2;
                    let this = if flags & CALL_METHOD != 0 {
                        match pop!() {
                            Value::Object(o) => Some(o),
                            other => bail!(format!("{} is not an object", other.type_name())),
                        }
                    } else {
                        None
                    };
                    let Value::Func(fid) = pop!() else {
                        bail!("not a function pointer");
                    };
                    enter!(fid, flags, argc, this);
                }
                Op::CallBuiltin => {
                    let idx = rd16(code, pc) as u16;
                    let n = usize::from(code[pc + 2]);
                    pc += 3;
                    if t.stack.len() < n {
                        save!();
                        return Err(fault("value stack underflow"));
                    }
                    let start = t.stack.len() - n;
                    t.stack[start..].reverse();
                    save!();
                    let held = t.stack.len();
                    self.stack_base += held;
                    let r = host.call_function(self, idx, &t.stack[start..]);
                    self.stack_base -= held;
                    let v = tri!(r);
                    t.stack.truncate(start);
                    push!(v);
                    check_pending!();
                }
                Op::CallBuiltinMethod => {
                    let idx = rd16(code, pc) as u16;
                    let n = usize::from(code[pc + 2]);
                    pc += 3;
                    let ent = match pop!() {
                        Value::Object(o) => match o.entity() {
                            Some(e) => e,
                            None => bail!(format!("{} is not an entity", o.kind_name())),
                        },
                        other => bail!(format!("{} is not an entity", other.type_name())),
                    };
                    if t.stack.len() < n {
                        save!();
                        return Err(fault("value stack underflow"));
                    }
                    let start = t.stack.len() - n;
                    t.stack[start..].reverse();
                    save!();
                    let held = t.stack.len();
                    self.stack_base += held;
                    let r = host.call_method(self, idx, ent, &t.stack[start..]);
                    self.stack_base -= held;
                    let v = tri!(r);
                    t.stack.truncate(start);
                    push!(v);
                    check_pending!();
                }
                Op::Return => {
                    let v = pop!();
                    do_return!(v);
                }
                Op::ReturnUndefined => do_return!(Value::Undefined),
                Op::Wait => {
                    let v = pop!();
                    let ticks = tri!(wait_ticks(&v)) as u32;
                    if ticks > 0 {
                        self.reset_timeout();
                    }
                    let tick = (self.time + ticks) & TICK_MASK;
                    save!();
                    t.state = State::Sched(tick);
                    self.bucket_head(tick, tid);
                    return Ok(Exit::Yield);
                }
                Op::WaittillFrameEnd => {
                    save!();
                    t.state = State::Sched(self.time);
                    self.bucket_tail(self.time, tid);
                    return Ok(Exit::Yield);
                }
                Op::Notify => {
                    let n = usize::from(code[pc]);
                    pc += 1;
                    let obj = pop!();
                    let name = pop!();
                    let (obj, name) = tri!(event_target(obj, name));
                    if t.stack.len() < n {
                        save!();
                        return Err(fault("value stack underflow"));
                    }
                    let at = t.stack.len() - n;
                    let mut payload: Vec<Value> = t.stack.drain(at..).collect();
                    payload.reverse();
                    save!();
                    self.notify(&obj, &name, &payload);
                    check_pending!();
                }
                Op::Endon => {
                    let obj = pop!();
                    let name = pop!();
                    let (obj, name) = tri!(event_target(obj, name));
                    let id = self.uid();
                    let f = t.frames.last_mut().expect("frame");
                    obj.add_entry(
                        &name,
                        NEntry {
                            id,
                            thread: tid,
                            what: NWhat::Endon(f.uid),
                        },
                    );
                    f.endons.push((obj, name, id));
                }
                Op::Waittill | Op::WaittillMatch => {
                    let n = code[pc];
                    pc += 1;
                    let obj = pop!();
                    let name = pop!();
                    let (obj, name) = tri!(event_target(obj, name));
                    let what = if op == Op::Waittill {
                        NWhat::Waittill(n)
                    } else {
                        let mut want = Vec::with_capacity(usize::from(n));
                        for _ in 0..n {
                            want.push(pop!());
                        }
                        NWhat::Match(want)
                    };
                    let id = self.uid();
                    obj.add_entry(
                        &name,
                        NEntry {
                            id,
                            thread: tid,
                            what,
                        },
                    );
                    t.wait = Some(WaitReg {
                        obj,
                        name,
                        entry: id,
                    });
                    t.state = State::Waiting;
                    save!();
                    return Ok(Exit::Yield);
                }
            }
        }
    }
}
