//! Running fibers: a slice at a time under a budget, a round of the ready ones, and the settling of
//! what fibers and the host make others wait for.

use super::exec::Exit;
use super::Vm;
use crate::fiber::{Failure, FiberState, Wake};
use crate::object::{Kind, Settled};
use crate::value::{Obj, Value};
use std::time::Instant;

/// How much of the Rust stack calls from a built-in back into script may use between them, from the
/// outermost of them: each level is several frames of the interpreter. The thread that runs a VM needs
/// at least this much to spare and some more for what is below the outermost: run a VM on a thread with
/// a stack of 16 MiB or more.
const MAX_NESTED_STACK: usize = 4 * 1024 * 1024;

/// Where the stack is now, near enough to measure how far it has grown.
fn stack_mark() -> usize {
    let probe = 0u8;
    std::hint::black_box(&probe) as *const u8 as usize
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
        let exit = self.exec(o, budget);
        self.current = before;
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
            f.wake = wake;
            f.state = FiberState::Ready;
            self.ready.push_back(w);
        }
    }

    /// Runs each fiber that is ready once, for at most `fuel` polls; returns how many ran. Those that
    /// yield go to the back of the queue.
    pub fn tick(&mut self, fuel: u64) -> usize {
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

    /// Calls `callee` and runs it to its end, for a native that takes a function: the call may not wait
    /// for anything. An error is the value thrown.
    pub fn call(&mut self, callee: Value, args: &[Value]) -> Result<Value, Value> {
        match callee {
            Value::Native(id) => {
                let f = self.natives[id as usize].f.clone();
                f(self, args)
            }
            Value::Obj(c) if c.kind() == Kind::Closure => {
                let here = stack_mark();
                if self.nested == 0 {
                    self.nested_base = here;
                } else if here.abs_diff(self.nested_base) > MAX_NESTED_STACK {
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
                        Slice::Waiting => {
                            self.cancel(fiber);
                            break Err(self.new_string("a function called from a built-in cannot wait for anything"));
                        }
                    }
                };
                self.nested -= 1;
                result
            }
            other => {
                let m = format!("{} is not a function", super::ops::type_name(other));
                Err(self.new_string(&m))
            }
        }
    }
}
