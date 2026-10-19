//! One thread-affine Krio lane per library. Only raw C storage crosses the channel.

use super::*;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

pub(super) type Waker = Arc<dyn Fn() + Send + Sync>;
pub(super) struct Bus {
    pub tx: mpsc::Sender<Event>,
    pub wake: Mutex<Option<Waker>>,
}
impl Bus {
    pub fn send(&self, event: Event) {
        let _ = self.tx.send(event);
        let wake = self.wake.lock().unwrap().clone();
        if let Some(wake) = wake {
            // A host wake function may run on a C callback's stack.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wake()));
        }
    }
}

pub(super) struct Work {
    pub id: u64,
    pub parent: Option<u64>,
    pub cif: usize,
    pub code: usize,
    pub arguments: Arguments,
    pub result: Slot,
}
// Arguments own their slot/string arrays. The VM pins pointed-to storage and the immutable CIF
// until completion, and admits no heap access on this thread.
unsafe impl Send for Work {}

pub(super) enum Event {
    Complete(Work),
    Callback(Request),
}
pub(super) struct Request {
    pub job: u64,
    pub gate: usize,
    pub args: usize,
    pub result: usize,
    pub reply: Reply,
    pub reentrant: bool,
}
pub(super) enum Reply {
    Lane(Arc<Hub>, u64),
    Thread(mpsc::SyncSender<()>),
}
impl Reply {
    pub fn send(self) {
        match self {
            Self::Lane(hub, id) => {
                let _ = hub.commands.send(Command::Reply(id));
            }
            Self::Thread(tx) => {
                let _ = tx.send(());
            }
        }
    }
}

enum Command {
    Run(Work),
    Reply(u64),
    Decoded(u64),
    Stop,
}
pub(super) struct Hub {
    pub bus: Arc<Bus>,
    commands: mpsc::Sender<Command>,
    pub active: AtomicU64,
    requests: AtomicU64,
}
pub(super) struct Lane {
    pub hub: Arc<Hub>,
    thread: Option<JoinHandle<()>>,
}

thread_local! { static WORKER_JOB: Cell<u64> = const { Cell::new(0) }; static CALLBACK_JOB: Cell<u64> = const { Cell::new(0) }; }
pub(super) fn allowed(job: u64) -> bool {
    CALLBACK_JOB.with(|current| current.get() == job)
}
pub(super) struct Access(u64);
impl Access {
    pub fn enter(job: u64) -> Self {
        Self(CALLBACK_JOB.with(|current| current.replace(job)))
    }
}
impl Drop for Access {
    fn drop(&mut self) {
        CALLBACK_JOB.with(|current| current.set(self.0));
    }
}

impl Hub {
    pub fn callback(self: &Arc<Self>, gate: usize, args: usize, result: usize) -> bool {
        let local = WORKER_JOB.with(Cell::get);
        let job = if local != 0 {
            local
        } else {
            self.active.load(Ordering::Acquire)
        };
        if job == 0 {
            return false;
        }
        if local != 0 {
            let request = self.requests.fetch_add(1, Ordering::Relaxed);
            self.bus.send(Event::Callback(Request {
                job,
                gate,
                args,
                result,
                reply: Reply::Lane(self.clone(), request),
                reentrant: true,
            }));
            krio_fiber::yield_u64(request);
        } else {
            let (tx, rx) = mpsc::sync_channel(0);
            self.bus.send(Event::Callback(Request {
                job,
                gate,
                args,
                result,
                reply: Reply::Thread(tx),
                reentrant: false,
            }));
            let _ = rx.recv();
        }
        true
    }
    pub fn decoded(&self, id: u64) {
        let _ = self.commands.send(Command::Decoded(id));
    }
}

impl Lane {
    pub fn new(bus: Arc<Bus>) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let hub = Arc::new(Hub {
            bus,
            commands: tx,
            active: AtomicU64::new(0),
            requests: AtomicU64::new(1),
        });
        let worker = hub.clone();
        let thread = thread::Builder::new()
            .name("rayscript-native".into())
            .stack_size(2 << 20)
            .spawn(move || {
                let mut queued = VecDeque::new();
                loop {
                    let command = queued
                        .pop_front()
                        .map(Command::Run)
                        .or_else(|| rx.recv().ok());
                    match command {
                        Some(Command::Run(work)) => execute(work, &worker, &rx, &mut queued),
                        Some(Command::Stop) | None => break,
                        _ => {}
                    }
                }
            })
            .map_err(|e| format!("cannot start native lane: {e}"))?;
        Ok(Self {
            hub,
            thread: Some(thread),
        })
    }
    pub fn submit(&self, work: Work) -> Result<(), String> {
        self.hub
            .commands
            .send(Command::Run(work))
            .map_err(|_| "native lane stopped".into())
    }
}
impl Drop for Lane {
    fn drop(&mut self) {
        let _ = self.hub.commands.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn execute(
    mut work: Work,
    hub: &Arc<Hub>,
    rx: &mpsc::Receiver<Command>,
    queued: &mut VecDeque<Work>,
) {
    let id = work.id;
    let previous = hub.active.load(Ordering::Acquire);
    let events = hub.bus.clone();
    let mut fiber = Box::new(krio_fiber::Fiber::with_stack_size(8 << 20, move || {
        let base = work.arguments.slots.as_mut_ptr();
        for (index, pointer) in work.arguments.pointers.iter_mut().enumerate() {
            *pointer = unsafe { base.add(index).cast() };
        }
        unsafe {
            let code: unsafe extern "C" fn() = std::mem::transmute(work.code);
            libffi::raw::ffi_call(
                work.cif as *mut _,
                Some(code),
                (&mut work.result as *mut Slot).cast(),
                work.arguments.pointers.as_mut_ptr(),
            );
        }
        events.send(Event::Complete(work));
    }));
    loop {
        hub.active.store(id, Ordering::Release);
        WORKER_JOB.with(|job| job.set(id));
        let step = fiber.resume_with_u64(0);
        WORKER_JOB.with(|job| job.set(0));
        if step == krio_fiber::FiberStep::Done {
            break;
        }
        assert_eq!(
            step,
            krio_fiber::FiberStep::Yielded,
            "native lane fiber failed"
        );
        let waiting = fiber
            .take_yield_u64()
            .expect("native callback did not identify its request");
        loop {
            match rx.recv() {
                Ok(Command::Reply(request)) if request == waiting => break,
                Ok(Command::Run(child)) if child.parent == Some(id) => {
                    execute(child, hub, rx, queued)
                }
                Ok(Command::Run(other)) => queued.push_back(other),
                _ => panic!("native callback lane disconnected"),
            }
        }
    }
    // A returned CString or borrowed view can refer to library scratch memory. Decode before the
    // next call is allowed to mutate it.
    loop {
        match rx.recv() {
            Ok(Command::Decoded(done)) if done == id => break,
            Ok(Command::Run(other)) => queued.push_back(other),
            _ => panic!("native completion lane disconnected"),
        }
    }
    hub.active.store(previous, Ordering::Release);
}
