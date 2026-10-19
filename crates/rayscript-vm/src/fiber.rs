//! A script fiber driven by a thread-affine Krio stack. Its registers, bytecode call frames and its
//! `try` handlers are all here, so the VM can stop it at any poll, run another, and come back, with
//! nothing of it on the Rust stack.

use crate::bytecode::Reg;
use crate::value::{Obj, Value};

/// One call in progress.
#[derive(Clone, Copy)]
pub struct Frame {
    pub closure: Obj,
    /// The next instruction.
    pub pc: usize,
    /// Where this frame's registers begin in the stack. The callee sat just below, and gets the result.
    pub base: usize,
}

/// A `try` that is open: where to go, in which frame, and into which register the error goes.
#[derive(Clone, Copy)]
pub struct Handler {
    pub frame: usize,
    pub catch_pc: usize,
    pub dst: Reg,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FiberState {
    /// Runnable, in the VM's queue or waiting to be put there.
    Ready,
    Running,
    /// Waiting for a fiber or a host answer; the one it waits for puts it back in the queue.
    Waiting,
    Done,
    Failed,
    Cancelled,
}

/// What a waiting fiber is woken with.
#[derive(Clone, Copy)]
pub enum Wake {
    None,
    Value(Value),
    Throw(Value),
}

/// One line of a trace: the function and where in its source.
#[derive(Clone, Debug)]
pub struct TraceEntry {
    pub function: String,
    pub module: u32,
    /// The source byte offset of the instruction.
    pub offset: u32,
}

/// Why a fiber ended with an error, as text, with where.
#[derive(Clone, Debug)]
pub struct Failure {
    pub message: String,
    pub trace: Vec<TraceEntry>,
}

pub struct Fiber {
    // Yield/resume slots keep a stable address across scheduler turns.
    pub(crate) driver: Option<Box<krio_fiber::Fiber>>,
    pub stack: Vec<Value>,
    /// Registers below this are live; the collector traces only those.
    pub top: usize,
    pub frames: Vec<Frame>,
    pub handlers: Vec<Handler>,
    pub state: FiberState,
    pub wake: Wake,
    /// The result register of the await or native call the fiber is parked on.
    pub await_dst: Reg,
    pub(crate) native_wait: bool,
    pub result: Value,
    pub failure: Option<Failure>,
    /// The value that was thrown, when the fiber failed; a fiber that awaits this one throws it again.
    pub error: Value,
    /// Fibers parked on this one finishing.
    pub waiters: Vec<Obj>,
    pub cancelled: bool,
    /// Started by a script's `spawn`: a failure nobody is waiting for is kept for the host.
    pub detached: bool,
    pub id: u64,
}

impl Fiber {
    pub fn new(id: u64) -> Fiber {
        Fiber {
            driver: None,
            stack: Vec::new(),
            top: 0,
            frames: Vec::new(),
            handlers: Vec::new(),
            state: FiberState::Ready,
            wake: Wake::None,
            await_dst: 0,
            native_wait: false,
            result: Value::Nil,
            failure: None,
            error: Value::Nil,
            waiters: Vec::new(),
            cancelled: false,
            detached: false,
            id,
        }
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.state, FiberState::Done | FiberState::Failed | FiberState::Cancelled)
    }

    /// Calls `f` with each value the fiber keeps alive.
    pub fn trace(&self, f: &mut dyn FnMut(Value)) {
        let top = self.top.min(self.stack.len());
        for v in &self.stack[..top] {
            f(*v);
        }
        for fr in &self.frames {
            f(Value::Obj(fr.closure));
        }
        f(self.result);
        f(self.error);
        match self.wake {
            Wake::Value(v) | Wake::Throw(v) => f(v),
            Wake::None => {}
        }
        for w in &self.waiters {
            f(Value::Obj(*w));
        }
    }
}

impl Drop for Fiber {
    fn drop(&mut self) {
        if let Some(mut driver) = self.driver.take() {
            if driver.state() == krio_fiber::FiberState::Suspended {
                // A zero turn ends the driver without accessing the VM during a sweep or teardown.
                driver.resume_with_u64(0);
            }
        }
    }
}
