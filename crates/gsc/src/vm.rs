// SPDX-License-Identifier: GPL-3.0-or-later
//! The bytecode VM and its scheduler.
//!
//! A thread is its saved frames and value stack. Threads park in time buckets (a map from the
//! script tick to a deque) or in object notify lists. The bucket for the current tick is
//! drained by [`Vm::run_current_threads`]; see `research/gsc-sched` (ticket #20) for the rules
//! reproduced here:
//!
//! * a bucket runs last-inserted-first (insertion is at the head), and keeps running whatever
//!   is inserted into it while it drains;
//! * `waittillframeend` inserts at the tail of the current bucket;
//! * `notify` visits registrations oldest first and files each woken thread at the head of the
//!   current bucket, so woken threads run newest registration first; `endon` entries are
//!   visited in the same pass and terminate their frame during the notify;
//! * terminating an inner frame resumes its caller (with `undefined` as the call result) at the
//!   head of the current bucket; terminating a thread's first frame ends the thread;
//! * freeing an entity is deferred to [`Vm::inc_time`]; it cancels every waiter silently.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::bytecode::{FuncId, Op, Program};
use crate::ops;
use crate::value::{Array, EntClass, EntRef, Kind, NWhat, Obj, Str, Value};

/// Calls a script function may nest (original: `function_count < 31`).
pub const MAX_DEPTH: usize = 31;
/// Depth above which an engine-initiated call is refused (original: `function_count > 29`).
pub const MAX_ENGINE_DEPTH: usize = 29;
/// Value stack entries (original: 2048 slots, the last one is the sentinel).
pub const MAX_STACK: usize = 2046;
/// Script ticks wrap at 24 bits.
pub const TICK_MASK: u32 = 0xFF_FFFF;
/// Longest `wait`, in ticks.
pub const MAX_WAIT: u64 = 0xFF_FFFE;
/// Script ticks per second of `wait`.
pub const TICKS_PER_SECOND: u32 = 30;
/// Wall-clock limit between timeout resets (original 1.7 MP: 2500 ms).
pub const LOOP_TIMEOUT: Duration = Duration::from_millis(2500);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThreadId {
    idx: u32,
    generation: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmErrorKind {
    /// A runtime error the original would print and answer by killing the thread chain.
    Script,
    /// Broken bytecode or VM state; not reachable from compiled scripts.
    Fault,
}

#[derive(Debug, Clone)]
pub struct VmError {
    pub kind: VmErrorKind,
    pub message: String,
    /// `file::function line N` of the frames that were killed, innermost first.
    pub trace: Vec<String>,
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        for l in &self.trace {
            write!(f, "\n  at {l}")?;
        }
        Ok(())
    }
}

impl std::error::Error for VmError {}

/// What the engine side provides: builtins and entity fields.
pub trait Host {
    /// Calls builtin function `index` (into `Program::builtins.function_names()`); `args[0]` is
    /// the first script argument.
    fn call_function(&mut self, vm: &mut Vm, index: u16, args: &[Value]) -> Result<Value, String>;

    /// Calls builtin method `index` on a live entity.
    fn call_method(
        &mut self,
        vm: &mut Vm,
        index: u16,
        ent: EntRef,
        args: &[Value],
    ) -> Result<Value, String>;

    /// Reads an engine-owned entity field such as `origin`; `None` falls through to the
    /// fields stored on the script object.
    fn get_field(&mut self, _ent: EntRef, _name: &str) -> Option<Value> {
        None
    }

    /// Writes an engine-owned entity field; `Ok(false)` stores it on the script object.
    fn set_field(&mut self, _ent: EntRef, _name: &str, _value: &Value) -> Result<bool, String> {
        Ok(false)
    }
}

#[derive(Debug, Clone)]
pub enum CallOutcome {
    /// The entry function returned before its thread first yielded.
    Finished(Value),
    /// The thread parked or was terminated.
    Pending,
}

struct Frame {
    func: FuncId,
    pc: usize,
    /// First local slot of this frame in `Thread::locals`.
    lbase: usize,
    /// Stack height when the frame was entered (arguments already popped).
    sbase: usize,
    this: Obj,
    uid: u64,
    /// `endon` registrations: object, event, entry id.
    endons: Vec<(Obj, Str, u64)>,
}

enum State {
    /// In the bucket for this tick.
    Sched(u32),
    /// In an object's notify list.
    Waiting,
}

struct WaitReg {
    obj: Obj,
    name: Str,
    entry: u64,
}

struct Thread {
    frames: Vec<Frame>,
    stack: Vec<Value>,
    locals: Vec<Value>,
    state: State,
    wait: Option<WaitReg>,
}

enum SlotState {
    Free,
    Parked(Box<Thread>),
    /// Executing; frame uids that a notify asked to terminate meanwhile.
    Running(Vec<u64>),
}

struct Slot {
    generation: u32,
    state: SlotState,
}

enum Exit {
    Done(Value),
    /// Parked or rescheduled; the thread is stored back by the caller.
    Yield,
    /// Terminated by `endon` or by a freed entity.
    Dead,
}

pub struct Vm {
    prog: Rc<Program>,
    strings: Vec<Str>,
    /// String index of `size`, which `.size` reads as the element count rather than a field.
    size_string: Option<u32>,
    slots: Vec<Slot>,
    free_slots: Vec<u32>,
    buckets: HashMap<u32, VecDeque<ThreadId>>,
    time: u32,
    level: Obj,
    anim: Obj,
    game: Value,
    entities: HashMap<u16, Obj>,
    free_ents: Vec<Obj>,
    next_uid: u64,
    depth: usize,
    stack_base: usize,
    timeout: Duration,
    timer: Instant,
    loading: bool,
    guard_warnings: u32,
    jumps: u32,
    ops: u64,
}

fn fault(msg: impl Into<String>) -> VmError {
    VmError {
        kind: VmErrorKind::Fault,
        message: msg.into(),
        trace: Vec::new(),
    }
}

fn script_error(msg: impl Into<String>) -> VmError {
    VmError {
        kind: VmErrorKind::Script,
        message: msg.into(),
        trace: Vec::new(),
    }
}

/// `wait n` in ticks: `n * 30`, at least 1 unless `n` is zero.
///
/// The operand is a 32-bit float, so `0.7` is really 0.699999988; flooring its product would
/// lose a tick. The epsilon is relative (a few float ulps) plus the original's absolute 2^-30.
pub fn wait_ticks(v: &Value) -> Result<u64, String> {
    let ticks = match v {
        Value::Int(n) if *n < 0 => return Err("negative wait is not allowed".into()),
        Value::Int(n) => *n as u64 * u64::from(TICKS_PER_SECOND),
        Value::Float(x) if *x < 0.0 => return Err("negative wait is not allowed".into()),
        Value::Float(x) if *x == 0.0 => 0,
        Value::Float(x) => {
            let p = f64::from(*x) * f64::from(TICKS_PER_SECOND);
            ((p + p * 2f64.powi(-22) + 2f64.powi(-30)).floor() as u64).max(1)
        }
        other => return Err(format!("{} is not a number", other.type_name())),
    };
    if ticks > MAX_WAIT {
        return Err("wait is too long".into());
    }
    Ok(ticks)
}

impl Drop for Vm {
    fn drop(&mut self) {
        crate::value::release_objects(&self.game);
    }
}

impl Vm {
    /// Checks every function's bytecode and prepares an empty VM at tick 0.
    pub fn new(prog: Program) -> Result<Vm, VmError> {
        verify(&prog)?;
        let strings = prog.strings.iter().map(|s| Str::from(&**s)).collect();
        let size_string = prog
            .strings
            .iter()
            .position(|s| &**s == "size")
            .map(|i| i as u32);
        Ok(Vm {
            prog: Rc::new(prog),
            strings,
            size_string,
            slots: Vec::new(),
            free_slots: Vec::new(),
            buckets: HashMap::new(),
            time: 0,
            level: Obj::new(Kind::Level),
            anim: Obj::new(Kind::Anim),
            game: Value::Array(Rc::new(Array::new())),
            entities: HashMap::new(),
            free_ents: Vec::new(),
            next_uid: 1,
            depth: 0,
            stack_base: 0,
            timeout: LOOP_TIMEOUT,
            timer: Instant::now(),
            loading: false,
            guard_warnings: 0,
            jumps: 0,
            ops: 0,
        })
    }

    pub fn program(&self) -> &Program {
        &self.prog
    }

    /// The script tick (24 bits); unrelated to `level.time`.
    pub fn time(&self) -> u32 {
        self.time
    }

    pub fn level(&self) -> Obj {
        self.level.clone()
    }

    pub fn anim(&self) -> Obj {
        self.anim.clone()
    }

    pub fn game(&self) -> &Value {
        &self.game
    }

    /// Replaces the `game` variable; the server carries it over `map_restart`.
    pub fn set_game(&mut self, v: Value) {
        self.game = v;
    }

    /// Opcodes executed so far.
    pub fn ops_executed(&self) -> u64 {
        self.ops
    }

    /// The loop guard fires when this much wall-clock time passes between timeout resets.
    pub fn set_loop_timeout(&mut self, d: Duration) {
        self.timeout = d;
    }

    /// While loading, the loop guard only warns (and resets) instead of killing the thread.
    pub fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
    }

    /// Loop-guard warnings issued while loading.
    pub fn guard_warnings(&self) -> u32 {
        self.guard_warnings
    }

    /// Resets the loop-guard timer (`Scr_ResetTimeout`).
    pub fn reset_timeout(&mut self) {
        self.timer = Instant::now();
    }

    /// Number of threads that exist, running or parked.
    pub fn thread_count(&self) -> usize {
        self.slots.len() - self.free_slots.len()
    }

    /// The script object of entity `num`, created on first use.
    pub fn entity(&mut self, num: u16, class: EntClass) -> Obj {
        self.entities
            .entry(num)
            .or_insert_with(|| Obj::new(Kind::Entity(EntRef { num, class })))
            .clone()
    }

    /// Marks entity `num`'s object dead; waiters are cancelled by the next [`Vm::inc_time`].
    /// No event is delivered. A later `entity(num, ..)` returns a fresh object.
    pub fn free_entity(&mut self, num: u16) {
        if let Some(o) = self.entities.remove(&num) {
            o.mark_dead();
            self.free_ents.push(o);
        }
    }

    /// `Scr_NotifyNum`: a no-op when the entity has no script object yet.
    pub fn notify_entity(&mut self, num: u16, name: &str, payload: &[Value]) {
        if let Some(o) = self.entities.get(&num).cloned() {
            self.notify(&o, name, payload);
        }
    }

    fn uid(&mut self) -> u64 {
        self.next_uid += 1;
        self.next_uid
    }

    // ---- slots and buckets ----

    fn alloc_slot(&mut self) -> ThreadId {
        match self.free_slots.pop() {
            Some(idx) => ThreadId {
                idx,
                generation: self.slots[idx as usize].generation,
            },
            None => {
                self.slots.push(Slot {
                    generation: 0,
                    state: SlotState::Free,
                });
                ThreadId {
                    idx: self.slots.len() as u32 - 1,
                    generation: 0,
                }
            }
        }
    }

    fn free_slot(&mut self, tid: ThreadId) {
        let s = &mut self.slots[tid.idx as usize];
        s.state = SlotState::Free;
        s.generation += 1;
        self.free_slots.push(tid.idx);
    }

    fn live(&self, tid: ThreadId) -> bool {
        self.slots
            .get(tid.idx as usize)
            .is_some_and(|s| s.generation == tid.generation && !matches!(s.state, SlotState::Free))
    }

    fn bucket_head(&mut self, tick: u32, tid: ThreadId) {
        self.buckets.entry(tick).or_default().push_front(tid);
    }

    fn bucket_tail(&mut self, tick: u32, tid: ThreadId) {
        self.buckets.entry(tick).or_default().push_back(tid);
    }

    fn bucket_remove(&mut self, tick: u32, tid: ThreadId) {
        if let Some(b) = self.buckets.get_mut(&tick) {
            if let Some(i) = b.iter().position(|t| *t == tid) {
                b.remove(i);
            }
            if b.is_empty() && tick != self.time {
                self.buckets.remove(&tick);
            }
        }
    }

    // ---- registrations ----

    /// Drops every notify registration and bucket membership a parked thread holds.
    fn unpark(&mut self, tid: ThreadId, t: &mut Thread) {
        if let Some(w) = t.wait.take() {
            w.obj.remove_entry(&w.name, w.entry);
        }
        if let State::Sched(tick) = t.state {
            self.bucket_remove(tick, tid);
        }
    }

    fn release_frame(f: &mut Frame) {
        for (obj, name, id) in f.endons.drain(..) {
            obj.remove_entry(&name, id);
        }
    }

    /// Ends a thread that is not running: all registrations go, the slot is freed.
    fn kill_parked(&mut self, tid: ThreadId, mut t: Box<Thread>) {
        self.unpark(tid, &mut t);
        for f in &mut t.frames {
            Self::release_frame(f);
        }
        self.free_slot(tid);
    }

    /// Pops frames `k..` of `t` and resumes frame `k - 1` with `undefined` as the call result.
    /// Returns how many frames died.
    fn unwind_to(&mut self, t: &mut Thread, k: usize) -> usize {
        let sbase = t.frames[k].sbase;
        let lbase = t.frames[k].lbase;
        let dead = t.frames.len() - k;
        for mut f in t.frames.drain(k..) {
            Self::release_frame(&mut f);
        }
        t.stack.truncate(sbase);
        t.locals.truncate(lbase);
        t.stack.push(Value::Undefined);
        dead
    }

    /// `endon` fired for frame `uid` of thread `tid`.
    fn terminate_frame(&mut self, tid: ThreadId, uid: u64) {
        if !self.live(tid) {
            return;
        }
        let slot = &mut self.slots[tid.idx as usize];
        if let SlotState::Running(pending) = &mut slot.state {
            pending.push(uid);
            return;
        }
        let SlotState::Parked(mut t) =
            std::mem::replace(&mut slot.state, SlotState::Running(Vec::new()))
        else {
            return;
        };
        let Some(k) = t.frames.iter().position(|f| f.uid == uid) else {
            self.slots[tid.idx as usize].state = SlotState::Parked(t);
            return;
        };
        self.unpark(tid, &mut t);
        if k == 0 {
            self.kill_parked(tid, t);
        } else {
            self.unwind_to(&mut t, k);
            t.state = State::Sched(self.time);
            self.bucket_head(self.time, tid);
            self.slots[tid.idx as usize].state = SlotState::Parked(t);
        }
    }

    /// Wakes a thread parked in `waittill`, giving it `n` payload values (first on top).
    fn wake(&mut self, tid: ThreadId, payload: &[Value], n: u8) {
        let SlotState::Parked(t) = &mut self.slots[tid.idx as usize].state else {
            return;
        };
        t.wait = None;
        for i in (0..n as usize).rev() {
            t.stack
                .push(payload.get(i).cloned().unwrap_or(Value::Undefined));
        }
        t.state = State::Sched(self.time);
        self.bucket_head(self.time, tid);
    }

    /// Delivers an event. Nothing runs here; woken threads are filed in the current bucket.
    pub fn notify(&mut self, obj: &Obj, name: &str, payload: &[Value]) {
        let mut last = 0;
        loop {
            // The list changes while we walk it (wakes and terminations remove entries), so
            // look up the next entry after the last visited id each time.
            let next = {
                let n = obj.0.notify.borrow();
                let Some(list) = n.get(name) else { return };
                let i = list.partition_point(|e| e.id <= last);
                match list.get(i) {
                    Some(e) => e.clone(),
                    None => return,
                }
            };
            last = next.id;
            if !self.live(next.thread) {
                obj.remove_entry(name, next.id);
                continue;
            }
            match next.what {
                NWhat::Endon(uid) => {
                    obj.remove_entry(name, next.id);
                    self.terminate_frame(next.thread, uid);
                }
                NWhat::Waittill(n) => {
                    obj.remove_entry(name, next.id);
                    self.wake(next.thread, payload, n);
                }
                NWhat::Match(want) => {
                    let hit = want
                        .iter()
                        .enumerate()
                        .all(|(i, w)| payload.get(i).is_some_and(|p| ops::equal(w, p)));
                    if hit {
                        obj.remove_entry(name, next.id);
                        self.wake(next.thread, payload, 0);
                    }
                }
            }
        }
    }

    // ---- driving ----

    /// `Scr_RunCurrentThreads`: drains the bucket of the current tick, including everything
    /// inserted into it meanwhile. Errors are returned; the drain continues after each one.
    pub fn run_current_threads(&mut self, host: &mut dyn Host) -> Vec<VmError> {
        let mut errors = Vec::new();
        if self.depth != 0 {
            errors.push(fault("run_current_threads inside a running thread"));
            return errors;
        }
        self.reset_timeout();
        while let Some(tid) = self
            .buckets
            .get_mut(&self.time)
            .and_then(VecDeque::pop_front)
        {
            let slot = &mut self.slots[tid.idx as usize];
            let SlotState::Parked(t) =
                std::mem::replace(&mut slot.state, SlotState::Running(Vec::new()))
            else {
                errors.push(fault("bucket entry is not a parked thread"));
                continue;
            };
            if let Err(e) = self.exec(host, tid, t) {
                errors.push(e);
            }
        }
        self.buckets.remove(&self.time);
        errors
    }

    /// `Scr_IncTime`: drain the current bucket, free pending entities, advance the tick.
    pub fn inc_time(&mut self, host: &mut dyn Host) -> Vec<VmError> {
        let errors = self.run_current_threads(host);
        self.free_entity_list();
        self.time = (self.time + 1) & TICK_MASK;
        errors
    }

    /// `Scr_FreeEntityList`: cancels every registration on the freed objects. Waiting threads
    /// are discarded and never resume; no event is delivered.
    fn free_entity_list(&mut self) {
        for o in std::mem::take(&mut self.free_ents) {
            let lists = std::mem::take(&mut *o.0.notify.borrow_mut());
            for e in lists.into_values().flatten() {
                if matches!(e.what, NWhat::Endon(_)) || !self.live(e.thread) {
                    continue;
                }
                let slot = &mut self.slots[e.thread.idx as usize];
                let waiting = matches!(
                    &slot.state,
                    SlotState::Parked(t) if matches!(&t.wait, Some(w) if w.entry == e.id)
                );
                if waiting {
                    let SlotState::Parked(mut t) =
                        std::mem::replace(&mut slot.state, SlotState::Running(Vec::new()))
                    else {
                        unreachable!()
                    };
                    t.wait = None;
                    self.kill_parked(e.thread, t);
                }
            }
            o.clear_fields();
        }
    }

    /// Engine-initiated call (`Scr_ExecThread`/`Scr_ExecEntThread`): the function runs to its
    /// first yield before this returns. `this` defaults to `level`.
    pub fn call(
        &mut self,
        host: &mut dyn Host,
        func: FuncId,
        this: Option<Obj>,
        args: &[Value],
    ) -> Result<CallOutcome, VmError> {
        if self.depth > MAX_ENGINE_DEPTH {
            return Err(script_error(
                "script stack overflow (too many embedded function calls)",
            ));
        }
        self.reset_timeout();
        let this = this.unwrap_or_else(|| self.level.clone());
        let f = self
            .prog
            .functions
            .get(func as usize)
            .ok_or_else(|| fault(format!("no function {func}")))?;
        let param_count = f.param_count as usize;
        let mut t = self.new_thread(func, this);
        for (i, a) in args.iter().take(param_count).enumerate() {
            t.locals[i] = a.clone();
        }
        let tid = self.alloc_slot();
        self.slots[tid.idx as usize].state = SlotState::Running(Vec::new());
        Ok(match self.exec(host, tid, t)? {
            Exit::Done(v) => CallOutcome::Finished(v),
            Exit::Yield | Exit::Dead => CallOutcome::Pending,
        })
    }

    fn new_thread(&mut self, func: FuncId, this: Obj) -> Box<Thread> {
        let local_count = self.prog.functions[func as usize].local_count as usize;
        let uid = self.uid();
        Box::new(Thread {
            frames: vec![Frame {
                func,
                pc: 0,
                lbase: 0,
                sbase: 0,
                this,
                uid,
                endons: Vec::new(),
            }],
            stack: Vec::new(),
            locals: vec![Value::Undefined; local_count],
            state: State::Sched(self.time),
            wait: None,
        })
    }

    /// Runs `t` (whose slot is marked running) until it yields or ends, then stores or frees it.
    fn exec(
        &mut self,
        host: &mut dyn Host,
        tid: ThreadId,
        mut t: Box<Thread>,
    ) -> Result<Exit, VmError> {
        self.depth += t.frames.len();
        let r = self.run(host, tid, &mut t);
        self.depth -= t.frames.len();
        match r {
            Ok(Exit::Yield) => {
                self.slots[tid.idx as usize].state = SlotState::Parked(t);
                Ok(Exit::Yield)
            }
            Ok(done) => {
                for f in &mut t.frames {
                    Self::release_frame(f);
                }
                self.free_slot(tid);
                Ok(done)
            }
            Err(mut e) => {
                self.trace(&t, &mut e.trace);
                self.unpark(tid, &mut t);
                for f in &mut t.frames {
                    Self::release_frame(f);
                }
                self.free_slot(tid);
                Err(e)
            }
        }
    }

    fn trace(&self, t: &Thread, out: &mut Vec<String>) {
        for f in t.frames.iter().rev() {
            let func = &self.prog.functions[f.func as usize];
            let file = &self.prog.files[func.file as usize].name;
            out.push(format!(
                "{file}::{} line {}",
                func.name,
                func.line_at(f.pc.saturating_sub(1))
            ));
        }
    }

    /// Reads the termination requests a notify filed against the running thread. Returns the
    /// lowest frame index that must die, if any.
    fn pending_kill(&mut self, tid: ThreadId, t: &Thread) -> Option<usize> {
        let SlotState::Running(p) = &mut self.slots[tid.idx as usize].state else {
            return None;
        };
        if p.is_empty() {
            return None;
        }
        let uids = std::mem::take(p);
        uids.iter()
            .filter_map(|u| t.frames.iter().position(|f| f.uid == *u))
            .min()
    }
}

/// Decodes every function once so the interpreter can index without bounds surprises.
fn verify(prog: &Program) -> Result<(), VmError> {
    use crate::bytecode::Imm;
    for (i, op) in Op::ALL.iter().enumerate() {
        if *op as usize != i {
            return Err(fault("opcode numbering is not dense"));
        }
    }
    for (id, f) in prog.functions.iter().enumerate() {
        let bad =
            |what: &str, pc: usize| fault(format!("function {id} ({}): {what} at {pc}", f.name));
        if f.param_count > f.local_count {
            return Err(bad("more parameters than locals", 0));
        }
        let mut starts = vec![false; f.code.len() + 1];
        let mut jumps = Vec::new();
        let mut pc = 0;
        let mut last = None;
        while pc < f.code.len() {
            starts[pc] = true;
            let op = Op::from_u8(f.code[pc]).ok_or_else(|| bad("unknown opcode", pc))?;
            last = Some(op);
            let mut at = pc + 1;
            let end = at + op.operands().iter().map(|i| i.size()).sum::<usize>();
            if end > f.code.len() {
                return Err(bad("truncated instruction", pc));
            }
            let mut vals = [0u32; 3];
            for (n, imm) in op.operands().iter().enumerate() {
                let b = &f.code[at..at + imm.size()];
                vals[n] = match imm {
                    Imm::U8 => u32::from(b[0]),
                    Imm::U16 => u32::from(u16::from_le_bytes([b[0], b[1]])),
                    _ => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                };
                at += imm.size();
            }
            match op {
                Op::Jump | Op::JumpIfFalse | Op::JumpIfTrue | Op::AndJump | Op::OrJump => {
                    let target = end as i64 + i64::from(vals[0] as i32);
                    jumps.push((pc, target));
                }
                Op::PushStr
                | Op::PushLocStr
                | Op::PushAnimRef
                | Op::PushAnimTree
                | Op::GetField
                | Op::RefField
                    if vals[0] as usize >= prog.strings.len() =>
                {
                    return Err(bad("string index out of range", pc));
                }
                Op::PushFuncPtr | Op::CallFunc => {
                    let target = if op == Op::CallFunc { vals[1] } else { vals[0] };
                    if target as usize >= prog.functions.len() {
                        return Err(bad("function id out of range", pc));
                    }
                }
                Op::GetLocal | Op::SetLocal | Op::RefLocal
                    if vals[0] >= u32::from(f.local_count) =>
                {
                    return Err(bad("local slot out of range", pc));
                }
                Op::CallBuiltin if vals[0] as usize >= prog.builtins.function_names().len() => {
                    return Err(bad("builtin function out of range", pc));
                }
                Op::CallBuiltinMethod if vals[0] as usize >= prog.builtins.method_names().len() => {
                    return Err(bad("builtin method out of range", pc));
                }
                _ => {}
            }
            pc = end;
        }
        if !matches!(last, Some(Op::Return | Op::ReturnUndefined | Op::Jump)) {
            return Err(bad("code does not end in a return or jump", f.code.len()));
        }
        for (pc, target) in jumps {
            if target < 0 || !starts.get(target as usize).copied().unwrap_or(false) {
                return Err(bad("jump target is not an instruction", pc));
            }
        }
    }
    Ok(())
}

mod interp;
