//! Running fibers: a slice at a time under a budget, a round of the ready ones, and the settling of
//! what fibers and the host make others wait for.

use super::exec::Exit;
use super::Vm;
use crate::fiber::{Failure, FiberState, Wake};
use crate::object::{Kind, Settled};
use crate::value::{Obj, Value};
use std::time::Instant;

// Native-to-script calls use separate Krio stacks; bound their count instead of comparing addresses.
const MAX_NESTED_CALLS: u32 = 64;
const SCRIPT_STACK_SIZE: usize = 1024 * 1024;

struct Turn {
    vm: *mut Vm,
    fiber: Obj,
    budget: *mut Budget,
    exit: Option<Exit>,
}

fn script_driver() -> Box<krio_fiber::Fiber> {
    Box::new(krio_fiber::Fiber::with_stack_size(SCRIPT_STACK_SIZE, || {
        let mut input = krio_fiber::take_input_u64();
        while let Some(address) = input {
            if address == 0 { return; }
            // A turn's pointers live only for resume(), and no VM borrow survives the yield.
            let finished = unsafe {
                let turn = &mut *(address as *mut Turn);
                turn.exit = Some((&mut *turn.vm).exec(turn.fiber, &mut *turn.budget));
                matches!(turn.exit, Some(Exit::Done(_) | Exit::Failed(_, _) | Exit::Cancelled))
            };
            if finished { return; }
            input = krio_fiber::yield_u64(0);
        }
    }))
}

/// How long a fiber may run before it must give way: a number of polls (loop edges and calls), and
/// optionally a time.
pub struct Budget {
    pub fuel: u64,
    pub deadline: Option<Instant>,
    pub(crate) polls: u32,
}

impl Budget {
    pub fn fuel(fuel: u64) -> Budget {
        Budget { fuel, deadline: None, polls: 0 }
    }

    pub fn unlimited() -> Budget {
        Budget { fuel: u64::MAX, deadline: None, polls: 0 }
    }

    pub fn until(deadline: Instant) -> Budget {
        Budget { fuel: u64::MAX, deadline: Some(deadline), polls: 0 }
    }
}

/// How a slice of a fiber's run ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slice {
    /// Out of budget or asked to yield; it can run again.
    Yielded,
    /// Waiting for a fiber or the host.
    Waiting,
    Done,
    Failed,
    Cancelled,
    /// Stopped by the debugger; `debug_resume` lets it go on.
    Paused,
}

impl Vm {
    /// Runs the fiber until it yields, waits, ends, or the budget is spent.
    pub fn run(&mut self, fiber: Value, budget: &mut Budget) -> Slice {
        match fiber.as_obj().filter(|o| o.kind() == Kind::Fiber) {
            Some(o) => self.run_obj(o, budget),
            None => Slice::Failed,
        }
    }

    pub(crate) fn run_obj(&mut self, o: Obj, budget: &mut Budget) -> Slice {
        let state = o.fiber().fiber.state;
        match state {
            FiberState::Done => return Slice::Done,
            FiberState::Failed => return Slice::Failed,
            FiberState::Cancelled => return Slice::Cancelled,
            FiberState::Waiting => return Slice::Waiting,
            FiberState::Ready | FiberState::Running => {}
        }
        o.fiber().fiber.state = FiberState::Running;
        let before = self.current.replace(o);
        let mut driver = o.fiber().fiber.driver.take().unwrap_or_else(script_driver);
        let mut turn = Turn { vm: self, fiber: o, budget, exit: None };
        let step = driver.resume_with_u64((&mut turn as *mut Turn) as u64);
        self.current = before;
        if step == krio_fiber::FiberStep::Errored {
            let _ = driver.take_error();
            let error = self.err("panic in script fiber");
            self.finish(o, FiberState::Failed, Value::Nil, Some(Failure { message: "panic in script fiber".into(), trace: Vec::new() }), error);
            return Slice::Failed;
        }
        let exit = turn.exit.expect("Krio script fiber did not return its turn");
        if matches!(exit, Exit::Yield | Exit::Wait | Exit::Paused(_)) {
            o.fiber().fiber.driver = Some(driver);
        }
        match exit {
            Exit::Yield => {
                o.fiber().fiber.state = FiberState::Ready;
                Slice::Yielded
            }
            Exit::Wait => {
                o.fiber().fiber.state = FiberState::Waiting;
                Slice::Waiting
            }
            Exit::Done(v) => {
                self.finish(o, FiberState::Done, v, None, Value::Nil);
                Slice::Done
            }
            Exit::Failed(failure, error) => {
                self.finish(o, FiberState::Failed, Value::Nil, Some(failure), error);
                Slice::Failed
            }
            Exit::Cancelled => {
                self.finish(o, FiberState::Cancelled, Value::Nil, None, Value::Nil);
                Slice::Cancelled
            }
            Exit::Paused(reason) => {
                o.fiber().fiber.state = FiberState::Ready;
                self.debug_stopped(o, reason);
                Slice::Paused
            }
        }
    }

    /// A fiber has ended: keeps its result, frees its stack, and wakes those waiting for it.
    fn finish(&mut self, o: Obj, state: FiberState, result: Value, failure: Option<Failure>, error: Value) {
        let waiters = {
            let f = &mut o.fiber().fiber;
            f.state = state;
            f.result = result;
            f.failure = failure;
            f.error = error;
            f.stack = Vec::new();
            f.frames = Vec::new();
            f.handlers = Vec::new();
            f.top = 0;
            std::mem::take(&mut f.waiters)
        };
        self.fibers.retain(|x| *x != o);
        self.view.fiber_roots.remove(&o);
        if state == FiberState::Failed && o.fiber().fiber.detached && waiters.is_empty() {
            let text = self.render_failure(Value::Obj(o));
            if let Some(f) = o.fiber().fiber.failure.clone() {
                self.detached_failures.push((f, text));
            }
        }
        let wake = match state {
            FiberState::Done => Wake::Value(result),
            FiberState::Failed => Wake::Throw(error),
            _ => Wake::Throw(self.new_string("the fiber it was waiting for was cancelled")),
        };
        for w in waiters {
            self.wake(w, wake);
        }
        if self.oom {
            self.oom = false;
            self.collect();
        }
    }

    pub(crate) fn wake(&mut self, w: Obj, wake: Wake) {
        let f = &mut w.fiber().fiber;
        if f.state == FiberState::Waiting {
            f.native_wait = false;
            f.wake = wake;
            f.state = FiberState::Ready;
            self.ready.push_back(w);
        }
    }

    /// Runs each fiber that is ready once, for at most `fuel` polls; returns how many ran. Those that
    /// yield go to the back of the queue.
    pub fn tick(&mut self, fuel: u64) -> usize {
        self.poll_native();
        if self.debug.paused.is_some() {
            return 0;
        }
        let n = self.ready.len();
        let mut ran = 0;
        for _ in 0..n {
            let Some(o) = self.ready.pop_front() else { break };
            if o.fiber().fiber.state != FiberState::Ready {
                continue;
            }
            let mut budget = Budget::fuel(fuel);
            match self.run_obj(o, &mut budget) {
                Slice::Yielded => self.ready.push_back(o),
                // Stopped: the rest of the round waits, and the fiber is put first in the queue when it goes on.
                Slice::Paused => return ran + 1,
                _ => {}
            }
            ran += 1;
        }
        ran
    }

    /// Ticks until nothing is ready: every fiber has ended or is waiting for the host.
    pub fn run_until_idle(&mut self, fuel: u64) {
        while self.tick(fuel) > 0 {}
    }

    /// Whether any fiber is ready to run.
    pub fn has_ready(&self) -> bool {
        !self.ready.is_empty() && self.debug.paused.is_none()
    }

    fn settle(&mut self, id: u64, state: Settled) -> bool {
        let Some(p) = self.pending.remove(&id) else { return false };
        let waiters = {
            let po = p.promise();
            po.state = state;
            std::mem::take(&mut po.waiters)
        };
        let wake = match state {
            Settled::Resolved(v) => Wake::Value(v),
            Settled::Rejected(v) => Wake::Throw(v),
            Settled::Pending => return true,
        };
        for w in waiters {
            self.wake(w, wake);
        }
        true
    }

    /// Says that a call ran out of polls, and where it was.
    fn too_long(&self, fiber: Obj) -> String {
        let limit = self.call_fuel;
        let top = fiber.fiber().fiber.frames.last().copied();
        match top {
            Some(fr) => {
                let proto = &self.protos[fr.closure.closure().proto as usize];
                let offset = proto.offsets.get(fr.pc.saturating_sub(1)).copied().unwrap_or(0);
                let (file, line, col) = self.locate(proto.module, offset);
                format!("stopped: it ran for more than {limit} steps, in `{}` ({file}:{line}:{col})", proto.name)
            }
            None => format!("stopped: it ran for more than {limit} steps"),
        }
    }

    /// Settles the promise with `new_promise`'s number: those waiting for it go on with the value.
    pub fn resolve(&mut self, id: u64, v: Value) -> bool {
        self.settle(id, Settled::Resolved(v))
    }

    /// Settles the promise as failed: those waiting for it have the value thrown at them.
    pub fn reject(&mut self, id: u64, v: Value) -> bool {
        self.settle(id, Settled::Rejected(v))
    }

    /// Calls `callee` to completion, driving native lane events when needed. Script callbacks may not
    /// wait on host promises. An error is the value thrown.
    pub fn call(&mut self, callee: Value, args: &[Value]) -> Result<Value, Value> {
        match callee {
            Value::Native(id) => {
                let f = self.natives[id as usize].f.clone();
                let result = f(self, args);
                if let Some(promise) = self.native_pending.take() { self.finish_native(promise) } else { result }
            }
            Value::Obj(c) if c.kind() == Kind::Closure => {
                if self.nested >= MAX_NESTED_CALLS {
                    return Err(self.new_string("calls from built-in functions are nested too deeply"));
                }
                let fiber = self.spawn(callee, args)?;
                let o = fiber.as_obj().unwrap();
                // It runs here and now, not from the queue.
                self.ready.retain(|x| *x != o);
                self.nested += 1;
                let mut budget = Budget::fuel(self.call_fuel);
                let result = loop {
                    match self.run_obj(o, &mut budget) {
                        Slice::Yielded if budget.fuel == 0 => {
                            let message = self.too_long(o);
                            self.cancel(fiber);
                            self.run_obj(o, &mut Budget::unlimited());
                            break Err(self.new_string(&message));
                        }
                        Slice::Yielded | Slice::Paused => continue,
                        Slice::Done => break Ok(o.fiber().fiber.result),
                        Slice::Failed => break Err(o.fiber().fiber.error),
                        Slice::Cancelled => break Err(self.new_string("cancelled")),
                        Slice::Waiting if o.fiber().fiber.native_wait => { self.wait_native(); }
                        Slice::Waiting => {
                            self.cancel(fiber);
                            break Err(self.new_string("a function called from a built-in cannot wait for anything"));
                        }
                    }
                };
                self.nested -= 1;
                if matches!(o.fiber().fiber.state, FiberState::Done | FiberState::Failed | FiberState::Cancelled) {
                    self.ready.retain(|fiber| *fiber != o);
                }
                result
            }
            other => {
                let m = format!("{} is not a function", super::ops::type_name(other));
                Err(self.new_string(&m))
            }
        }
    }
}
