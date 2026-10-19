//! VM-side ownership, completion decoding and callback dispatch for native lanes.

use super::lane::{self, Event, Waker};
use super::*;
use crate::Obj;
use std::sync::{Arc, Mutex, mpsc};

pub(super) struct Workers {
    bus: Arc<lane::Bus>,
    events: mpsc::Receiver<Event>,
    lanes: HashMap<usize, lane::Lane>,
    jobs: HashMap<u64, Pending>,
    callback: Option<(u64, bool)>,
    next: u64,
    closing: bool,
    callbacks: VecDeque<lane::Request>,
}
impl Default for Workers {
    fn default() -> Self {
        let (tx, events) = mpsc::channel();
        Self {
            bus: Arc::new(lane::Bus {
                tx,
                wake: Mutex::new(None),
            }),
            events,
            lanes: HashMap::new(),
            jobs: HashMap::new(),
            callback: None,
            next: 1,
            closing: false,
            callbacks: VecDeque::new(),
        }
    }
}
struct Pending {
    bound: Rc<Bound>,
    args: Vec<Value>,
    owner: Option<(Value, Rc<Resource>)>,
    frame: Option<callback::Frame>,
    locks: Vec<Lock>,
    lock_cursor: usize,
    promise: u64,
    lane: usize,
}

pub(super) enum Lock {
    Resource(Rc<Resource>, Option<u64>),
    Backing(Rc<structure::Backing>, Option<u64>),
}
impl Drop for Lock {
    fn drop(&mut self) {
        match self {
            Self::Resource(resource, before) => resource.busy.set(*before),
            Self::Backing(backing, before) => backing.busy.set(*before),
        }
    }
}
pub(super) fn check(busy: Option<u64>) -> Result<(), String> {
    if busy.is_some_and(|job| !lane::allowed(job)) {
        Err("native storage is busy on a worker".into())
    } else {
        Ok(())
    }
}
fn lock_value(value: Value, id: u64, locks: &mut Vec<Lock>) -> Result<(), String> {
    fn resource(r: &Rc<Resource>, id: u64, locks: &mut Vec<Lock>) -> Result<(), String> {
        let old = r.busy.get();
        if old == Some(id) {
            return Ok(());
        }
        check(old)?;
        r.busy.set(Some(id));
        locks.push(Lock::Resource(r.clone(), old));
        Ok(())
    }
    match foreign(value) {
        Some(Foreign::Handle(r)) => resource(r, id, locks)?,
        Some(Foreign::Pointer { owner: Some(r), .. }) => resource(r, id, locks)?,
        _ => {}
    }
    structure::lock(value, id, locks)
}

impl Workers {
    pub(super) fn busy(&self) -> bool {
        !self.jobs.is_empty()
    }
    pub(super) fn has_lane(&self, library: &Rc<Library>) -> bool {
        self.lanes.contains_key(&(Rc::as_ptr(library) as usize))
    }
    pub(super) fn hub(&self, library: &Rc<Library>) -> Option<Arc<lane::Hub>> {
        self.lanes
            .get(&(Rc::as_ptr(library) as usize))
            .map(|lane| lane.hub.clone())
    }
    pub(super) fn trace(&self, f: &mut dyn FnMut(Value)) {
        for pending in self.jobs.values() {
            for arg in &pending.args {
                f(*arg);
            }
            if let Some(frame) = &pending.frame {
                frame.trace(f);
            }
        }
    }
    pub(super) fn output_busy(&self, value: Value) -> bool {
        self.jobs.values().any(|job| {
            job.bound
                .args
                .iter()
                .zip(&job.args)
                .any(|(ty, arg)| matches!(ty, NativeType::Out(_)) && arg.as_obj() == value.as_obj())
        })
    }
}

pub(super) fn submit(vm: &mut Vm, bound: Rc<Bound>, args: &[Value]) -> NativeResult {
    let result = prepare(vm, bound.clone(), args);
    result.map_err(|e| vm.err(format!("{}: {e}", bound.name)))
}

fn prepare(vm: &mut Vm, bound: Rc<Bound>, args: &[Value]) -> Result<Value, String> {
    if vm.foreign.workers.closing {
        return Err("native workers are shutting down".into());
    }
    if args.len() != bound.args.len() {
        return Err(format!(
            "expected {} arguments, got {}",
            bound.args.len(),
            args.len()
        ));
    }
    let key = Rc::as_ptr(&bound.library) as usize;
    let parent = match vm.foreign.workers.callback {
        Some((parent, reentrant))
            if vm
                .foreign
                .workers
                .jobs
                .get(&parent)
                .is_some_and(|p| p.lane == key) =>
        {
            if !reentrant {
                return Err(
                    "same-library reentry from a foreign callback thread is unavailable".into(),
                );
            }
            Some(parent)
        }
        _ => None,
    };
    let owner = bound
        .owner
        .map(|index| match foreign(args[index]) {
            Some(Foreign::Handle(r)) if !r.ptr.get().is_null() => Ok((args[index], r.clone())),
            _ => Err("@:owner argument must be an open handle".to_string()),
        })
        .transpose()?;
    let mut arguments = Arguments::new(args.len());
    for (index, (ty, value)) in bound.args.iter().zip(args).enumerate() {
        if matches!(ty, NativeType::Out(_))
            && (args[..index].iter().any(|v| v.as_obj() == value.as_obj())
                || vm.foreign.workers.output_busy(*value)
                || vm
                    .foreign
                    .frames
                    .iter()
                    .any(|f| f.outputs.iter().any(|v| v.as_obj() == value.as_obj())))
        {
            return Err("an Out slot is already in use by an active native call".into());
        }
        bound
            .encode(ty, *value, &mut arguments, index)
            .map_err(|e| format!("argument {}: {e}", index + 1))?;
    }
    let id = vm.foreign.workers.next;
    vm.foreign.workers.next += 1;
    let mut frame = callback::Frame::new(args, &bound.args);
    let mut locks = Vec::new();
    for value in &frame.roots {
        lock_value(*value, id, &mut locks)?;
    }
    frame.pin_all(vm);
    let lock_cursor = frame.exposed.len();
    if !vm.foreign.workers.lanes.contains_key(&key) {
        let lane = lane::Lane::new(vm.foreign.workers.bus.clone())?;
        vm.foreign.workers.lanes.insert(key, lane);
    }
    let hub = vm.foreign.workers.lanes[&key].hub.clone();
    callback::attach(vm, &bound.library, hub);
    for (ty, value) in bound.args.iter().zip(args) {
        if matches!(ty, NativeType::Out(_)) {
            value.as_obj().unwrap().host().references.clear();
        }
    }
    let (promise, promise_id) = vm.new_promise();
    if promise.is_nil() {
        return Err("cannot allocate native promise".into());
    }
    let work = lane::Work {
        id,
        parent,
        cif: bound.cif.as_raw_ptr() as usize,
        code: bound.code.as_mut_ptr() as usize,
        arguments,
        result: Slot { u64_: 0 },
    };
    vm.foreign.workers.jobs.insert(
        id,
        Pending {
            bound,
            args: args.to_vec(),
            owner,
            frame: Some(frame),
            locks,
            lock_cursor,
            promise: promise_id,
            lane: key,
        },
    );
    if let Err(error) = vm.foreign.workers.lanes[&key].submit(work) {
        vm.foreign.workers.jobs.remove(&id);
        vm.reject(promise_id, Value::Nil);
        return Err(error);
    }
    vm.native_pending = Some(promise);
    Ok(Value::Nil)
}

impl Vm {
    /// Wake the host loop after a worker posts a callback or completion.
    pub fn set_native_waker(&mut self, wake: Waker) {
        *self.foreign.workers.bus.wake.lock().unwrap() = Some(wake);
    }
    pub fn has_native_work(&self) -> bool {
        self.foreign.workers.busy()
    }
    pub(crate) fn native_waits_for(&self, root: Value) -> bool {
        fn reaches(value: Obj, root: Value, seen: &mut std::collections::HashSet<Obj>) -> bool {
            if Some(value) == root.as_obj() {
                return true;
            }
            seen.insert(value)
                && value
                    .fiber()
                    .fiber
                    .waiters
                    .iter()
                    .any(|waiter| reaches(*waiter, root, seen))
        }
        self.foreign
            .workers
            .jobs
            .values()
            .filter_map(|job| self.pending.get(&job.promise))
            .any(|promise| {
                promise
                    .promise()
                    .waiters
                    .iter()
                    .any(|waiter| reaches(*waiter, root, &mut std::collections::HashSet::new()))
            })
    }
    pub fn poll_native(&mut self) -> usize {
        if self.debug.paused.is_some() {
            return 0;
        }
        let mut count = 0;
        while let Some(event) = self.next_native_event(false) {
            self.native_event(event);
            count += 1;
        }
        count
    }
    pub(crate) fn wait_native(&mut self) {
        if let Some(event) = self.next_native_event(true) {
            self.native_event(event);
        }
    }
    pub(crate) fn finish_native(&mut self, promise: Value) -> NativeResult {
        let p = promise.as_obj().unwrap();
        loop {
            match p.promise().state {
                crate::object::Settled::Resolved(value) => return Ok(value),
                crate::object::Settled::Rejected(error) => return Err(error),
                crate::object::Settled::Pending => self.wait_native(),
            }
        }
    }
    pub(crate) fn shutdown_native(&mut self) {
        self.foreign.workers.closing = true;
        while self.has_native_work() {
            self.wait_native();
        }
        self.foreign.workers.lanes.clear();
    }
    fn next_native_event(&mut self, wait: bool) -> Option<Event> {
        let workers = &mut self.foreign.workers;
        let ready = workers.callbacks.iter().position(|request| {
            workers.closing
                || workers
                    .jobs
                    .get(&request.job)
                    .is_none_or(|job| job.frame.is_some())
        });
        if let Some(index) = ready {
            return workers.callbacks.remove(index).map(Event::Callback);
        }
        if wait {
            workers.events.recv().ok()
        } else {
            workers.events.try_recv().ok()
        }
    }
    fn native_event(&mut self, event: Event) {
        match event {
            Event::Complete(work) => self.native_complete(work),
            Event::Callback(request) => {
                if !self.foreign.workers.closing
                    && self
                        .foreign
                        .workers
                        .jobs
                        .get(&request.job)
                        .is_some_and(|job| job.frame.is_none())
                {
                    self.foreign.workers.callbacks.push_back(request);
                    return;
                }
                if !self.foreign.workers.closing {
                    if let Some(pending) = self.foreign.workers.jobs.get_mut(&request.job) {
                        let frame = pending
                            .frame
                            .take()
                            .expect("native callback frame already active");
                        self.foreign.frames.push(frame);
                        let before = self
                            .foreign
                            .workers
                            .callback
                            .replace((request.job, request.reentrant));
                        let access = lane::Access::enter(request.job);
                        unsafe {
                            callback::dispatch(self, request.gate, request.args, request.result);
                        }
                        let args = self.foreign.workers.jobs[&request.job].args.clone();
                        self.foreign.frames.last_mut().unwrap().retain(&args);
                        let mut frame = self.foreign.frames.pop().unwrap();
                        let pending = self.foreign.workers.jobs.get_mut(&request.job).unwrap();
                        let locked =
                            frame.exposed[pending.lock_cursor..]
                                .iter()
                                .try_for_each(|value| {
                                    lock_value(*value, request.job, &mut pending.locks)
                                });
                        pending.lock_cursor = frame.exposed.len();
                        if let Err(error) = locked {
                            if frame.error.is_none() {
                                frame.error = Some(self.err(error));
                            }
                            unsafe {
                                callback::failure(request.gate, request.result);
                            }
                        }
                        self.foreign
                            .workers
                            .jobs
                            .get_mut(&request.job)
                            .unwrap()
                            .frame = Some(frame);
                        drop(access);
                        self.foreign.workers.callback = before;
                    }
                }
                request.reply.send();
            }
        }
    }
    fn native_complete(&mut self, work: lane::Work) {
        let Some(mut pending) = self.foreign.workers.jobs.remove(&work.id) else {
            return;
        };
        let access = lane::Access::enter(work.id);
        let mut result = unsafe {
            pending.bound.decode(
                &pending.bound.ret,
                work.result,
                self,
                pending.owner.as_ref(),
                true,
            )
        };
        for (ty, value) in pending.bound.args.iter().zip(&pending.args) {
            if let NativeType::Out(inner) = ty {
                let slot = match foreign(*value) {
                    Some(Foreign::Out { slot, .. }) => unsafe { *slot.get() },
                    _ => unreachable!(),
                };
                match unsafe {
                    pending
                        .bound
                        .decode(inner, slot, self, pending.owner.as_ref(), false)
                } {
                    Ok(value_out) => value.as_obj().unwrap().host().references = vec![value_out],
                    Err(error) if result.is_ok() => result = Err(error),
                    _ => {}
                }
            }
        }
        if let Err(error) = structure::refresh(self, &pending.args, result.as_ref().ok().copied()) {
            if result.is_ok() {
                result = Err(error);
            }
        }
        let error = pending.frame.as_mut().unwrap().error.take();
        match error
            .map(Err)
            .unwrap_or_else(|| result.map_err(|e| self.err(format!("{}: {e}", pending.bound.name))))
        {
            Ok(value) => {
                self.resolve(pending.promise, value);
            }
            Err(error) => {
                self.reject(pending.promise, error);
            }
        }
        pending.locks.clear();
        drop(access);
        self.foreign.workers.lanes[&pending.lane]
            .hub
            .decoded(work.id);
        drop(pending);
        drop(work);
        self.foreign.drain();
    }
}
