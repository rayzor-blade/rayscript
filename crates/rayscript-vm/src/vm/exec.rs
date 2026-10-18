//! The interpreter: one fiber, run until it finishes, waits, yields, or its budget is spent. All of its
//! state is in the fiber, so leaving the loop and coming back is only a matter of saving the program
//! counter. The loop polls where a script can run long: on a jump backwards, and on a call. A poll is
//! where the budget is spent, a cancel is seen, and the heap is collected.

use super::ops::{self, Arith, Cmp};
use super::sched::Budget;
use super::{LoadedProto, Vm};
use crate::bytecode::Op;
use crate::fiber::{Failure, Fiber, FiberState, Handler, TraceEntry, Wake};
use crate::object::{Kind, Settled};
use crate::value::{Obj, Value};

pub enum Exit {
    Done(Value),
    /// The fiber failed: what it was, and the value that was thrown.
    Failed(Failure, Value),
    Yield,
    Wait,
    Cancelled,
}

impl Vm {
    /// Takes the top handler off the fiber, if it has one, and lands the error where it points.
    /// Without one, says how the fiber failed.
    fn unwind(&mut self, fo: Obj, error: Value) -> Result<(), (Failure, Value)> {
        let f = &mut fo.fiber().fiber;
        if let Some(h) = f.handlers.pop() {
            f.frames.truncate(h.frame + 1);
            let fr = f.frames[h.frame];
            f.frames[h.frame].pc = h.catch_pc;
            let registers = self.protos[fr.closure.closure().proto as usize].registers as usize;
            f.top = fr.base + registers;
            f.stack[fr.base + h.dst as usize] = error;
            return Ok(());
        }
        let message = ops::display(self, error, 0);
        let mut trace = Vec::new();
        for fr in f.frames.iter().rev() {
            let p = &self.protos[fr.closure.closure().proto as usize];
            let offset = p.offsets.get(fr.pc.saturating_sub(1)).copied().unwrap_or(0);
            trace.push(TraceEntry { function: p.name.to_string(), module: p.module, offset });
        }
        Err((Failure { message, trace }, error))
    }

    pub(crate) fn exec(&mut self, fo: Obj, budget: &mut Budget) -> Exit {
        let fp: *mut Fiber = &mut fo.fiber().fiber;
        macro_rules! f {
            () => {
                unsafe { &mut *fp }
            };
        }
        let mut frame_idx: usize;
        let mut pc: usize;
        let mut base: usize;
        let mut closure: Obj;
        let mut proto: *const LoadedProto;
        macro_rules! load_frame {
            () => {{
                frame_idx = f!().frames.len() - 1;
                let fr = f!().frames[frame_idx];
                pc = fr.pc;
                base = fr.base;
                closure = fr.closure;
                proto = &*self.protos[closure.closure().proto as usize] as *const LoadedProto;
                f!().top = base + unsafe { (*proto).registers } as usize;
            }};
        }
        macro_rules! p {
            () => {
                unsafe { &*proto }
            };
        }
        macro_rules! reg {
            ($i:expr) => {
                f!().stack[base + ($i as usize)]
            };
        }
        macro_rules! save_pc {
            () => {
                f!().frames[frame_idx].pc = pc
            };
        }
        macro_rules! throw {
            ($v:expr) => {{
                let thrown: Value = $v;
                save_pc!();
                match self.unwind(fo, thrown) {
                    Ok(()) => {
                        load_frame!();
                        continue;
                    }
                    Err((failure, error)) => return Exit::Failed(failure, error),
                }
            }};
        }
        macro_rules! fail {
            ($($arg:tt)*) => {{
                let e = self.err(format!($($arg)*));
                throw!(e)
            }};
        }
        // A poll: the budget, a cancel, the heap, and an allocation the heap refused. At a jump the fiber
        // goes on from where it is; before an instruction that has not run yet (`poll_op`), a fiber that
        // gives up its turn goes on from that instruction, not the one after it.
        macro_rules! poll {
            () => {
                poll!(0)
            };
            ($back:expr) => {{
                if self.oom {
                    self.oom = false;
                    save_pc!();
                    let failure = Failure { message: "out of memory".into(), trace: Vec::new() };
                    return Exit::Failed(failure, Value::Nil);
                }
                if f!().cancelled {
                    return Exit::Cancelled;
                }
                if budget.fuel == 0 {
                    f!().frames[frame_idx].pc = pc - $back;
                    return Exit::Yield;
                }
                budget.fuel -= 1;
                budget.polls = budget.polls.wrapping_add(1);
                if budget.polls & 63 == 0 {
                    if let Some(d) = budget.deadline {
                        if std::time::Instant::now() >= d {
                            f!().frames[frame_idx].pc = pc - $back;
                            return Exit::Yield;
                        }
                    }
                }
                // Not inside a call a native made: that native may hold values only it refers to.
                if self.nested == 0 && self.heap.should_collect() {
                    save_pc!();
                    self.collect();
                }
            }};
        }
        macro_rules! native_call {
            ($id:expr, $start:expr, $n:expr, $dst:expr) => {{
                let (start, n, dst): (usize, usize, usize) = ($start, $n, $dst);
                let mut small = [Value::Nil; 8];
                let big: Vec<Value>;
                let args: &[Value] = if n <= 8 {
                    small[..n].copy_from_slice(&f!().stack[start..start + n]);
                    &small[..n]
                } else {
                    big = f!().stack[start..start + n].to_vec();
                    &big
                };
                let func = self.natives[$id as usize].f.clone();
                save_pc!();
                match func(self, args) {
                    Ok(v) => {
                        f!().stack[dst] = v;
                    }
                    Err(e) => throw!(e),
                }
                if self.yield_flag {
                    self.yield_flag = false;
                    return Exit::Yield;
                }
            }};
        }
        macro_rules! do_call {
            ($slot:expr, $argc:expr) => {{
                let (slot, argc): (usize, usize) = ($slot, $argc);
                poll!(1);
                let callee = f!().stack[slot];
                match callee {
                    Value::Obj(c) if c.kind() == Kind::Closure => {
                        save_pc!();
                        match self.push_frame(f!(), c, slot, argc) {
                            Ok(()) => load_frame!(),
                            Err(e) => throw!(e),
                        }
                    }
                    Value::Native(id) => native_call!(id, slot + 1, argc, slot),
                    other => fail!("{} is not a function", ops::type_name(other)),
                }
            }};
        }
        macro_rules! do_return {
            ($v:expr) => {{
                let v: Value = $v;
                let fr = f!().frames.pop().unwrap();
                let depth = f!().frames.len();
                while f!().handlers.last().is_some_and(|h| h.frame >= depth) {
                    f!().handlers.pop();
                }
                f!().stack[fr.base - 1] = v;
                if f!().frames.is_empty() {
                    f!().top = 0;
                    return Exit::Done(v);
                }
                load_frame!();
            }};
        }
        macro_rules! jump {
            ($offset:expr) => {{
                let o: i32 = $offset;
                pc = (pc as i64 + o as i64) as usize;
                if o < 0 {
                    poll!();
                }
            }};
        }
        macro_rules! arith {
            ($op:expr, $dst:expr, $a:expr, $b:expr) => {{
                let (x, y) = (reg!($a), reg!($b));
                match self.arith($op, x, y) {
                    Ok(v) => reg!($dst) = v,
                    Err(e) => throw!(e),
                }
            }};
        }
        macro_rules! compare {
            ($op:expr, $dst:expr, $a:expr, $b:expr) => {{
                let (x, y) = (reg!($a), reg!($b));
                match self.compare($op, x, y) {
                    Ok(v) => reg!($dst) = Value::Bool(v),
                    Err(e) => throw!(e),
                }
            }};
        }

        load_frame!();
        if f!().cancelled {
            return Exit::Cancelled;
        }
        // What woke the fiber lands first: a value in the register it awaited with, or an error to throw.
        let mut pending_throw: Option<Value> = None;
        match std::mem::replace(&mut f!().wake, Wake::None) {
            Wake::None => {}
            Wake::Value(v) => {
                let dst = f!().await_dst;
                reg!(dst) = v;
            }
            Wake::Throw(v) => pending_throw = Some(v),
        }

        loop {
            if let Some(v) = pending_throw.take() {
                throw!(v)
            }
            let op = p!().code[pc];
            pc += 1;
            match op {
                Op::Nop => {}
                Op::Move { dst, src } => {
                    let v = reg!(src);
                    reg!(dst) = v;
                }
                Op::LoadNil { dst } => reg!(dst) = Value::Nil,
                Op::LoadBool { dst, value } => reg!(dst) = Value::Bool(value),
                Op::LoadInt { dst, value } => reg!(dst) = Value::Int(value as i64),
                Op::LoadConst { dst, index } => reg!(dst) = p!().consts[index as usize],

                Op::GetGlobal { dst, name } => {
                    let sym = p!().syms[name as usize];
                    let module = p!().module as usize;
                    match self.modules[module].globals.get(&sym).or_else(|| self.globals.get(&sym)) {
                        Some(v) => reg!(dst) = *v,
                        None => {
                            let n = self.syms.name(sym).to_string();
                            fail!("`{n}` is not defined")
                        }
                    }
                }
                Op::SetGlobal { src, name } => {
                    let sym = p!().syms[name as usize];
                    let module = p!().module as usize;
                    let v = reg!(src);
                    match self.modules[module].globals.get_mut(&sym) {
                        Some(slot) => *slot = v,
                        None => {
                            let n = self.syms.name(sym).to_string();
                            fail!("`{n}` is not defined; declare it with var")
                        }
                    }
                }
                Op::DefGlobal { src, name } => {
                    let sym = p!().syms[name as usize];
                    let module = p!().module as usize;
                    let v = reg!(src);
                    self.modules[module].globals.insert(sym, v);
                }

                Op::NewCell { dst, src } => {
                    let v = reg!(src);
                    reg!(dst) = self.new_cell(v).map(Value::Obj).unwrap_or(Value::Nil);
                }
                Op::GetCell { dst, cell } => match reg!(cell) {
                    Value::Obj(c) if c.kind() == Kind::Cell => reg!(dst) = c.cell().value,
                    _ => fail!("internal error: a captured variable is not in a cell"),
                },
                Op::SetCell { cell, src } => match reg!(cell) {
                    Value::Obj(c) if c.kind() == Kind::Cell => c.cell().value = reg!(src),
                    _ => fail!("internal error: a captured variable is not in a cell"),
                },
                Op::GetUpval { dst, index } => {
                    let c = closure.closure().upvals[index as usize];
                    reg!(dst) = c.cell().value;
                }
                Op::SetUpval { index, src } => {
                    let c = closure.closure().upvals[index as usize];
                    c.cell().value = reg!(src);
                }
                Op::Closure { dst, proto: pi } => {
                    let lp: *const LoadedProto = &*self.protos[pi as usize];
                    let mut ups = Vec::with_capacity(unsafe { &*lp }.upvals.len());
                    for d in unsafe { &*lp }.upvals.iter() {
                        if d.from_parent_local {
                            match reg!(d.index) {
                                Value::Obj(c) if c.kind() == Kind::Cell => ups.push(c),
                                _ => fail!("internal error: a closure captures something that is not a cell"),
                            }
                        } else {
                            ups.push(closure.closure().upvals[d.index as usize]);
                        }
                    }
                    reg!(dst) = self.new_closure(pi, ups).map(Value::Obj).unwrap_or(Value::Nil);
                }

                Op::Add { dst, a, b } => {
                    let (x, y) = (reg!(a), reg!(b));
                    if let (Value::Int(i), Value::Int(j)) = (x, y) {
                        reg!(dst) = Value::Int(i.wrapping_add(j));
                    } else {
                        match self.add(x, y) {
                            Ok(v) => reg!(dst) = v,
                            Err(e) => throw!(e),
                        }
                    }
                }
                Op::Sub { dst, a, b } => {
                    if let (Value::Int(i), Value::Int(j)) = (reg!(a), reg!(b)) {
                        reg!(dst) = Value::Int(i.wrapping_sub(j));
                    } else {
                        arith!(Arith::Sub, dst, a, b)
                    }
                }
                Op::Mul { dst, a, b } => arith!(Arith::Mul, dst, a, b),
                Op::Div { dst, a, b } => arith!(Arith::Div, dst, a, b),
                Op::Rem { dst, a, b } => arith!(Arith::Rem, dst, a, b),
                Op::Shl { dst, a, b } => arith!(Arith::Shl, dst, a, b),
                Op::Shr { dst, a, b } => arith!(Arith::Shr, dst, a, b),
                Op::UShr { dst, a, b } => arith!(Arith::UShr, dst, a, b),
                Op::BitAnd { dst, a, b } => arith!(Arith::BitAnd, dst, a, b),
                Op::BitOr { dst, a, b } => arith!(Arith::BitOr, dst, a, b),
                Op::BitXor { dst, a, b } => arith!(Arith::BitXor, dst, a, b),
                Op::Neg { dst, src } => match reg!(src) {
                    Value::Int(i) => reg!(dst) = Value::Int(i.wrapping_neg()),
                    Value::Float(x) => reg!(dst) = Value::Float(-x),
                    other => fail!("cannot negate {}", ops::type_name(other)),
                },
                Op::Not { dst, src } => {
                    let v = reg!(src);
                    reg!(dst) = Value::Bool(!v.truthy());
                }
                Op::BitNot { dst, src } => match reg!(src) {
                    Value::Int(i) => reg!(dst) = Value::Int(!i),
                    other => fail!("`~` needs an integer, not {}", ops::type_name(other)),
                },
                Op::AddInt { dst, src, value } => match reg!(src) {
                    Value::Int(i) => reg!(dst) = Value::Int(i.wrapping_add(value as i64)),
                    Value::Float(x) => reg!(dst) = Value::Float(x + value as f64),
                    other => fail!("cannot add one to {}", ops::type_name(other)),
                },

                Op::Eq { dst, a, b } => {
                    let r = ops::equal(reg!(a), reg!(b));
                    reg!(dst) = Value::Bool(r);
                }
                Op::NotEq { dst, a, b } => {
                    let r = !ops::equal(reg!(a), reg!(b));
                    reg!(dst) = Value::Bool(r);
                }
                Op::Lt { dst, a, b } => {
                    if let (Value::Int(i), Value::Int(j)) = (reg!(a), reg!(b)) {
                        reg!(dst) = Value::Bool(i < j);
                    } else {
                        compare!(Cmp::Lt, dst, a, b)
                    }
                }
                Op::Le { dst, a, b } => compare!(Cmp::Le, dst, a, b),
                Op::Gt { dst, a, b } => compare!(Cmp::Gt, dst, a, b),
                Op::Ge { dst, a, b } => compare!(Cmp::Ge, dst, a, b),

                Op::Jump { offset } => jump!(offset),
                Op::JumpIf { cond, offset } => {
                    if reg!(cond).truthy() {
                        jump!(offset);
                    }
                }
                Op::JumpIfNot { cond, offset } => {
                    if !reg!(cond).truthy() {
                        jump!(offset);
                    }
                }
                Op::JumpIfNil { src, offset } => {
                    if reg!(src).is_nil() {
                        jump!(offset);
                    }
                }
                Op::JumpIfNotNil { src, offset } => {
                    if !reg!(src).is_nil() {
                        jump!(offset);
                    }
                }

                Op::Call { base: cb, argc } => do_call!(base + cb as usize, argc as usize),
                Op::CallMethod { base: cb, argc, name } => {
                    let slot = base + cb as usize;
                    let sym = p!().syms[name as usize];
                    let recv = f!().stack[slot];
                    if let Value::Obj(o) = recv {
                        if o.kind() == Kind::Object {
                            match o.object().fields.get(sym) {
                                Some(fv) => {
                                    f!().stack[slot] = fv;
                                    do_call!(slot, argc as usize);
                                    continue;
                                }
                                None => {
                                    let n = self.syms.name(sym).to_string();
                                    fail!("the object has no `{n}`")
                                }
                            }
                        }
                    }
                    poll!(1);
                    match self.methods.get(&(ops::kind_tag(recv), sym)).copied() {
                        Some(id) => native_call!(id, slot, argc as usize + 1, slot),
                        None => {
                            let n = self.syms.name(sym).to_string();
                            fail!("{} has no method `{n}`", ops::type_name(recv))
                        }
                    }
                }
                Op::Return { src } => {
                    let v = reg!(src);
                    do_return!(v)
                }
                Op::ReturnNil => do_return!(Value::Nil),

                Op::GetField { dst, obj, name } => {
                    let sym = p!().syms[name as usize];
                    let o = reg!(obj);
                    match self.get_field(o, sym) {
                        Ok(v) => reg!(dst) = v,
                        Err(e) => throw!(e),
                    }
                }
                Op::SetField { obj, name, src } => {
                    let sym = p!().syms[name as usize];
                    let (o, v) = (reg!(obj), reg!(src));
                    if let Err(e) = self.set_field(o, sym, v) {
                        throw!(e)
                    }
                }
                Op::GetIndex { dst, obj, index } => {
                    let (o, i) = (reg!(obj), reg!(index));
                    match self.get_index(o, i) {
                        Ok(v) => reg!(dst) = v,
                        Err(e) => throw!(e),
                    }
                }
                Op::SetIndex { obj, index, src } => {
                    let (o, i, v) = (reg!(obj), reg!(index), reg!(src));
                    if let Err(e) = self.set_index(o, i, v) {
                        throw!(e)
                    }
                }
                Op::NewArray { dst, capacity } => {
                    reg!(dst) = self.new_array(Vec::with_capacity(capacity as usize));
                }
                Op::ArrayPush { array, src } => {
                    let v = reg!(src);
                    if let Value::Obj(a) = reg!(array) {
                        a.array().items.push(v);
                    }
                }
                Op::NewObject { dst } => reg!(dst) = self.new_object(),
                Op::NewMap { dst } => reg!(dst) = self.new_map(),
                Op::NewRange { dst, from, to } => match (reg!(from), reg!(to)) {
                    (Value::Int(a), Value::Int(b)) => reg!(dst) = self.new_range(a, b),
                    (a, b) => fail!("a range needs integers, not {} and {}", ops::type_name(a), ops::type_name(b)),
                },

                Op::Interp { dst, base: ib, count } => {
                    let mut s = String::new();
                    for i in 0..count as usize {
                        let v = reg!(ib as usize + i);
                        s.push_str(&ops::display(self, v, 0));
                    }
                    reg!(dst) = self.new_string(&s);
                }

                Op::IterInit { base: ib, src } => {
                    let v = reg!(src);
                    match v {
                        Value::Obj(o) if matches!(o.kind(), Kind::Array | Kind::Map | Kind::Range | Kind::String) => {
                            reg!(ib) = v;
                            reg!(ib as usize + 1) = Value::Int(0);
                        }
                        other => fail!("cannot loop over {}", ops::type_name(other)),
                    }
                }
                Op::IterNext { base: ib, key, value, offset } => {
                    let Value::Obj(o) = reg!(ib) else { fail!("internal error: a loop lost its iterable") };
                    let Value::Int(state) = reg!(ib as usize + 1) else { fail!("internal error: a loop lost its position") };
                    let next: Option<(Value, Value, i64)> = match o.kind() {
                        Kind::Array => {
                            let items = &o.array().items;
                            ((state as usize) < items.len()).then(|| (Value::Int(state), items[state as usize], state + 1))
                        }
                        Kind::Range => {
                            let r = o.range();
                            (r.from + state < r.to).then(|| (Value::Int(state), Value::Int(r.from + state), state + 1))
                        }
                        Kind::Map => o.map().map.entry_from(state as usize).map(|(next, k, v)| (k, v, next as i64)),
                        Kind::String => {
                            let s = o.str();
                            s[(state as usize).min(s.len())..].chars().next().map(|c| {
                                let n = self.new_string(&c.to_string());
                                (Value::Int(state), n, state + c.len_utf8() as i64)
                            })
                        }
                        _ => None,
                    };
                    match next {
                        Some((k, v, st)) => {
                            reg!(key) = k;
                            reg!(value) = v;
                            reg!(ib as usize + 1) = Value::Int(st);
                        }
                        None => pc = (pc as i64 + offset as i64) as usize,
                    }
                }
                Op::ForRange { base: rb, var, offset } => match (reg!(rb), reg!(rb as usize + 1)) {
                    (Value::Int(a), Value::Int(b)) => {
                        if a >= b {
                            pc = (pc as i64 + offset as i64) as usize;
                        } else {
                            reg!(var) = Value::Int(a);
                            reg!(rb) = Value::Int(a + 1);
                        }
                    }
                    (a, b) => fail!("a range needs integers, not {} and {}", ops::type_name(a), ops::type_name(b)),
                },

                Op::Throw { src } | Op::Rethrow { src } => {
                    let v = reg!(src);
                    throw!(v)
                }
                Op::TryBegin { dst, catch_offset } => {
                    let catch_pc = (pc as i64 + catch_offset as i64) as usize;
                    f!().handlers.push(Handler { frame: frame_idx, catch_pc, dst });
                }
                Op::TryEnd => {
                    f!().handlers.pop();
                }
                Op::IsType { dst, src, name } => {
                    let n = match p!().consts[name as usize] {
                        Value::Obj(o) if o.kind() == Kind::String => o.str(),
                        _ => "",
                    };
                    let r = self.is_type(reg!(src), n);
                    reg!(dst) = Value::Bool(r);
                }

                Op::TestArray { dst, src, len, at_least } => {
                    let r = match reg!(src) {
                        Value::Obj(o) if o.kind() == Kind::Array => {
                            let n = o.array().items.len();
                            if at_least { n >= len as usize } else { n == len as usize }
                        }
                        _ => false,
                    };
                    reg!(dst) = Value::Bool(r);
                }
                Op::TestObject { dst, src } => {
                    let r = matches!(reg!(src), Value::Obj(o) if o.kind() == Kind::Object);
                    reg!(dst) = Value::Bool(r);
                }
                Op::TestField { dst, src, name } => {
                    let sym = p!().syms[name as usize];
                    let r = matches!(reg!(src), Value::Obj(o) if o.kind() == Kind::Object && o.object().fields.has(sym));
                    reg!(dst) = Value::Bool(r);
                }
                Op::Slice { dst, src, from } => {
                    let items = match reg!(src) {
                        Value::Obj(o) if o.kind() == Kind::Array => o.array().items.get(from as usize..).unwrap_or(&[]).to_vec(),
                        _ => Vec::new(),
                    };
                    reg!(dst) = self.new_array(items);
                }

                Op::Import { dst, path } => {
                    let path = match p!().consts[path as usize] {
                        Value::Obj(o) => o.str().to_string(),
                        _ => String::new(),
                    };
                    save_pc!();
                    match self.import_value(&path) {
                        Ok(v) => reg!(dst) = v,
                        Err(e) => throw!(e),
                    }
                }
                Op::ImportAll { path } => {
                    let path = match p!().consts[path as usize] {
                        Value::Obj(o) => o.str().to_string(),
                        _ => String::new(),
                    };
                    save_pc!();
                    match self.import_value(&path) {
                        Ok(Value::Obj(m)) if m.kind() == Kind::Object => {
                            let module = p!().module as usize;
                            let members: Vec<(u32, Value)> = m.object().fields.iter().copied().collect();
                            for (k, v) in members {
                                self.modules[module].globals.insert(k, v);
                            }
                        }
                        Ok(other) => fail!("`{path}` is {}, not a module with members", ops::type_name(other)),
                        Err(e) => throw!(e),
                    }
                }
                Op::BindNative { .. } => fail!("native libraries are not available yet"),

                Op::Await { dst, src } => {
                    let v = reg!(src);
                    let Value::Obj(o) = v else {
                        reg!(dst) = v;
                        continue;
                    };
                    match o.kind() {
                        Kind::Fiber => {
                            if o == fo {
                                fail!("a fiber cannot wait for itself")
                            }
                            let state = o.fiber().fiber.state;
                            match state {
                                FiberState::Done => reg!(dst) = o.fiber().fiber.result,
                                FiberState::Failed => {
                                    let e = o.fiber().fiber.error;
                                    throw!(e)
                                }
                                FiberState::Cancelled => fail!("the fiber it was waiting for was cancelled"),
                                _ => {
                                    o.fiber().fiber.waiters.push(fo);
                                    f!().await_dst = dst;
                                    save_pc!();
                                    return Exit::Wait;
                                }
                            }
                        }
                        Kind::Promise => {
                            let state = o.promise().state;
                            match state {
                                Settled::Resolved(x) => reg!(dst) = x,
                                Settled::Rejected(x) => throw!(x),
                                Settled::Pending => {
                                    o.promise().waiters.push(fo);
                                    f!().await_dst = dst;
                                    save_pc!();
                                    return Exit::Wait;
                                }
                            }
                        }
                        _ => reg!(dst) = v,
                    }
                }
            }
        }
    }
}
