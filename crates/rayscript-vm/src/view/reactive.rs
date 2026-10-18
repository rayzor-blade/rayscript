//! Signals, computed values and effects.
//!
//! A signal holds a value. A computed value or an effect is a function that remembers which signals and
//! computed values it read the last time it ran, and runs again when one of them has changed. Writing a
//! signal marks its readers dirty and everything further down possibly stale; a stale computed value
//! checks its sources before it runs, so a change that does not alter a value stops there. Effects wait
//! in a queue for `view_flush`; computed values are pulled when read.
//!
//! Everything created while a computation runs belongs to it: it is disposed, with its cleanups run,
//! before the computation runs again or when its owner is disposed.

use super::{Vm, tree};
use crate::heap::Header;
use crate::object::Kind;
use crate::value::{Obj, Value};
use crate::vm::ops;

#[repr(C)]
pub struct SignalObj {
    pub header: Header,
    pub value: Value,
    pub subs: Vec<Obj>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum RState {
    Clean,
    /// A source further up may have changed.
    Check,
    /// A source it reads has changed.
    Dirty,
    Disposed,
}

/// One entry of a `<for>`: what it was made from, the scope that owns its effects, and the node that
/// holds its elements.
pub struct Item {
    pub item: Value,
    pub key: Value,
    pub scope: Obj,
    pub group: u32,
}

/// What a computation does when it runs.
pub enum Role {
    /// A scope that owns things and computes nothing.
    Root,
    Computed { func: Value },
    Effect { func: Value },
    /// Whether the function's result is truthy: what an `<if>` watches, so that it runs only when the answer changes.
    Cond { func: Value },
    /// One property of an element.
    Prop { node: u32, name: Box<str>, func: Value },
    /// Properties from an object, `{...props}`, and the names set the last time.
    Spread { node: u32, func: Value, applied: Vec<Box<str>> },
    /// A `{expression}` among the children.
    Hole { node: u32, func: Value },
    If { node: u32, cond: Obj, then: Value, els: Value },
    For { node: u32, iter: Value, body: Value, items: Vec<Item> },
}

#[repr(C)]
pub struct ReactiveObj {
    pub header: Header,
    pub role: Role,
    pub state: RState,
    pub value: Value,
    /// What reads this, when it is a value others can read.
    pub subs: Vec<Obj>,
    /// What this read the last time it ran.
    pub sources: Vec<Obj>,
    pub owner: Option<Obj>,
    pub owned: Vec<Obj>,
    pub cleanups: Vec<Value>,
    pub seq: u64,
    pub queued: bool,
}

impl ReactiveObj {
    pub(crate) fn trace(&self, f: &mut dyn FnMut(Value)) {
        f(self.value);
        for o in self.subs.iter().chain(&self.sources).chain(&self.owned).chain(self.owner.iter()) {
            f(Value::Obj(*o));
        }
        for c in &self.cleanups {
            f(*c);
        }
        match &self.role {
            Role::Root => {}
            Role::Computed { func } | Role::Effect { func } | Role::Cond { func } | Role::Prop { func, .. } | Role::Spread { func, .. } | Role::Hole { func, .. } => f(*func),
            Role::If { cond, then, els, .. } => {
                f(Value::Obj(*cond));
                f(*then);
                f(*els);
            }
            Role::For { iter, body, items, .. } => {
                f(*iter);
                f(*body);
                for it in items {
                    f(it.item);
                    f(it.key);
                    f(Value::Obj(it.scope));
                }
            }
        }
    }

    /// Whether the host's flush runs this itself, rather than something reading it.
    fn is_effect(&self) -> bool {
        !matches!(self.role, Role::Root | Role::Computed { .. } | Role::Cond { .. })
    }
}

/// Most computations running inside one another; each is a Rust frame deep.
const MAX_DEPTH: u32 = 40;

fn subs_of<'a>(source: Obj) -> &'a mut Vec<Obj> {
    match source.kind() {
        Kind::Signal => &mut source.signal().subs,
        _ => &mut source.reactive().subs,
    }
}

impl Vm {
    pub(crate) fn new_signal(&mut self, value: Value) -> Result<Value, Value> {
        match self.alloc(Kind::Signal, |header| SignalObj { header, value, subs: Vec::new() }) {
            Some(o) => Ok(Value::Obj(o)),
            None => Err(self.err("out of memory")),
        }
    }

    /// A computation that has not run yet. `adopt` makes the current owner dispose it.
    pub(crate) fn new_reactive(&mut self, role: Role, adopt: bool) -> Result<Obj, Value> {
        self.view.seq += 1;
        let seq = self.view.seq;
        let owner = if adopt { self.view.owner } else { None };
        let made = self.alloc(Kind::Reactive, |header| ReactiveObj {
            header,
            role,
            state: RState::Dirty,
            value: Value::Nil,
            subs: Vec::new(),
            sources: Vec::new(),
            owner,
            owned: Vec::new(),
            cleanups: Vec::new(),
            seq,
            queued: false,
        });
        let Some(r) = made else { return Err(self.err("out of memory")) };
        if let Some(o) = owner {
            o.reactive().owned.push(r);
        }
        Ok(r)
    }

    /// A signal or computed value where a value is shown stands for its value, and is read by whatever is
    /// running. Anything else is itself.
    pub(crate) fn unwrap_reactive(&mut self, v: Value) -> Result<Value, Value> {
        match v {
            Value::Obj(o) if o.kind() == Kind::Signal => Ok(self.signal_get(o)),
            Value::Obj(o) if o.kind() == Kind::Reactive => self.reactive_get(o, true),
            other => Ok(other),
        }
    }

    /// Records that the running computation read `source`.
    fn track(&mut self, source: Obj) {
        let Some(obs) = self.view.observer else { return };
        let o = obs.reactive();
        if source == obs || o.state == RState::Disposed || o.sources.contains(&source) {
            return;
        }
        o.sources.push(source);
        subs_of(source).push(obs);
    }

    pub(crate) fn signal_get(&mut self, s: Obj) -> Value {
        self.track(s);
        s.signal().value
    }

    pub(crate) fn signal_set(&mut self, s: Obj, v: Value) {
        if ops::equal(s.signal().value, v) {
            return;
        }
        s.signal().value = v;
        self.signal_notify(s);
    }

    /// Marks the readers of a signal as having to run, whether or not its value is different.
    pub(crate) fn signal_notify(&mut self, s: Obj) {
        for sub in s.signal().subs.clone() {
            self.mark(sub, RState::Dirty);
        }
    }

    /// The value of a computed value, brought up to date; recorded as read by the running computation.
    pub(crate) fn reactive_get(&mut self, r: Obj, track: bool) -> Result<Value, Value> {
        self.update_if_necessary(r)?;
        if track {
            self.track(r);
        }
        Ok(r.reactive().value)
    }

    /// Marks `start` as at least `state`, and what reads it as possibly stale; effects are queued.
    fn mark(&mut self, start: Obj, state: RState) {
        let mut work = vec![(start, state)];
        while let Some((n, st)) = work.pop() {
            let rn = n.reactive();
            if rn.state >= st {
                continue;
            }
            rn.state = st;
            if rn.is_effect() && !rn.queued {
                rn.queued = true;
                self.view.queue.push(n);
            }
            for sub in &rn.subs {
                work.push((*sub, RState::Check));
            }
        }
    }

    /// Brings a computation up to date: runs it if a source has changed, and settles it as clean.
    pub(crate) fn update_if_necessary(&mut self, r: Obj) -> Result<(), Value> {
        if r.reactive().state == RState::Check {
            for s in r.reactive().sources.clone() {
                if s.kind() == Kind::Reactive {
                    self.update_if_necessary(s)?;
                    if r.reactive().state == RState::Dirty {
                        break;
                    }
                }
            }
        }
        if r.reactive().state == RState::Dirty {
            self.run_reactive(r)?;
        }
        if r.reactive().state == RState::Check {
            r.reactive().state = RState::Clean;
        }
        Ok(())
    }

    fn run_reactive(&mut self, r: Obj) -> Result<(), Value> {
        if self.view.depth >= MAX_DEPTH {
            return Err(self.err("reactive values depend on each other too deeply"));
        }
        // Clean before the run, so that a write the run makes to what it read marks it again.
        r.reactive().state = RState::Clean;
        self.clean(r);
        let (owner, observer) = (self.view.owner.replace(r), self.view.observer.replace(r));
        self.view.depth += 1;
        let result = self.execute(r);
        self.view.depth -= 1;
        self.view.owner = owner;
        self.view.observer = observer;
        result
    }

    /// The part of a run that depends on what the computation is for.
    fn execute(&mut self, r: Obj) -> Result<(), Value> {
        match &r.reactive().role {
            Role::Root => Ok(()),
            Role::Computed { func } => {
                let func = *func;
                let v = self.call(func, &[])?;
                self.set_computed(r, v);
                Ok(())
            }
            Role::Cond { func } => {
                let func = *func;
                let v = self.call(func, &[])?;
                let v = self.unwrap_reactive(v)?;
                self.set_computed(r, Value::Bool(v.truthy()));
                Ok(())
            }
            Role::Effect { func } => {
                let func = *func;
                let v = self.call(func, &[])?;
                // A function returned is run before the next run, and when the effect is disposed.
                if matches!(v, Value::Native(_)) || matches!(v, Value::Obj(o) if o.kind() == Kind::Closure) {
                    r.reactive().cleanups.push(v);
                }
                Ok(())
            }
            Role::Prop { .. } => tree::run_prop(self, r),
            Role::Spread { .. } => tree::run_spread(self, r),
            Role::Hole { .. } => tree::run_hole(self, r),
            Role::If { .. } => tree::run_if(self, r),
            Role::For { .. } => tree::run_for(self, r),
        }
    }

    fn set_computed(&mut self, r: Obj, v: Value) {
        let rn = r.reactive();
        if ops::equal(rn.value, v) {
            return;
        }
        rn.value = v;
        for sub in rn.subs.clone() {
            self.mark(sub, RState::Dirty);
        }
    }

    /// Disposes what the last run created, runs its cleanups, and forgets what it read.
    fn clean(&mut self, r: Obj) {
        for child in std::mem::take(&mut r.reactive().owned) {
            self.dispose(child);
        }
        self.run_cleanups(r);
        for s in std::mem::take(&mut r.reactive().sources) {
            let subs = subs_of(s);
            if let Some(at) = subs.iter().position(|x| *x == r) {
                subs.swap_remove(at);
            }
        }
    }

    fn run_cleanups(&mut self, r: Obj) {
        let cleanups = std::mem::take(&mut r.reactive().cleanups);
        if cleanups.is_empty() {
            return;
        }
        let (owner, observer) = (self.view.owner.take(), self.view.observer.take());
        for c in cleanups.into_iter().rev() {
            if let Err(e) = self.call(c, &[]) {
                self.view_error(e);
            }
        }
        self.view.owner = owner;
        self.view.observer = observer;
    }

    /// Stops a computation for good: what it owns is disposed, its cleanups run, and nothing wakes it again.
    pub(crate) fn dispose(&mut self, r: Obj) {
        if r.reactive().state == RState::Disposed {
            return;
        }
        r.reactive().state = RState::Disposed;
        let items = match &mut r.reactive().role {
            Role::For { items, .. } => std::mem::take(items),
            _ => Vec::new(),
        };
        for it in items {
            self.dispose(it.scope);
        }
        self.clean(r);
        let rn = r.reactive();
        rn.subs.clear();
        rn.value = Value::Nil;
        rn.role = Role::Root;
        if let Some(owner) = rn.owner.take() {
            let owned = &mut owner.reactive().owned;
            if let Some(at) = owned.iter().position(|x| *x == r) {
                owned.remove(at);
            }
        }
    }

    /// Runs `f` with no computation recording what it reads.
    pub(crate) fn untracked<T>(&mut self, f: impl FnOnce(&mut Vm) -> T) -> T {
        let observer = self.view.observer.take();
        let out = f(self);
        self.view.observer = observer;
        out
    }
}
