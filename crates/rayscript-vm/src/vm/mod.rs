//! The VM: a heap, the loaded code, the globals, and the fibers it runs. All of a fiber's state is in
//! heap objects, so a collection needs no help from the Rust stack: it marks from the globals, the
//! constants, the fibers and what the host holds, and nothing else.

mod debug;
mod exec;
mod ffi;
mod gc;
pub(crate) mod ops;
mod sched;

pub use debug::{DebugFrame, DebugVar, Debugger, PauseReason, StepMode};
pub use exec::Exit;
pub use ops::type_name;
pub use sched::{Budget, Slice};

use crate::bytecode::{Const, Module, Op, UpvalDesc};
use crate::fiber::{Fiber, FiberState, Frame};
use crate::heap::{Heap, HeapConfig, HEADER_SIZE};
use crate::object::*;
use crate::value::{Obj, Value};
use rayscript_syntax::ariadne::{Cache, Label, Report, Source};
use rayscript_syntax::span::{self, render_all, LineIndex, Severity, Span};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

/// What a native function gives back: a value, or one to throw in the script.
pub type NativeResult = Result<Value, Value>;

/// A function the host provides. The arguments are copied out of the fiber's registers first, so it may
/// allocate and call back into the VM. It may hold state of its own: a module that forwards to the Haxe
/// side is natives that share a handle to it.
pub type NativeFn = Rc<dyn Fn(&mut Vm, &[Value]) -> NativeResult>;

/// A plain function as a native.
pub type NativePtr = fn(&mut Vm, &[Value]) -> NativeResult;

pub(crate) struct NativeEntry {
    pub name: String,
    pub f: NativeFn,
}

/// What a host answers to `import path`.
pub enum Resolved {
    /// A value: a module's object, a function, a class.
    Value(Value),
    /// The text of a script module, to be compiled and run, and its members imported.
    Script { name: String, source: String },
}

/// How `import` finds what is not registered: the host decides, so the Haxe standard library, ashui and
/// the editor's own APIs can each be a module it answers for, on demand.
pub trait ModuleResolver {
    fn resolve(&mut self, vm: &mut Vm, path: &str) -> Option<Resolved>;
}

/// A function ready to run: its code, and its constants made into values.
pub(crate) struct LoadedProto {
    pub name: Rc<str>,
    pub params: u16,
    pub has_rest: bool,
    pub registers: u16,
    pub code: Vec<Op>,
    pub offsets: Vec<u32>,
    pub consts: Vec<Value>,
    /// For a constant that is a string, its symbol; the names of fields, methods and globals.
    pub syms: Vec<u32>,
    pub upvals: Vec<UpvalDesc>,
    pub module: u32,
    /// Named variables, for a debugger.
    pub locals: Vec<crate::bytecode::LocalInfo>,
}

pub(crate) struct LoadedModule {
    pub name: String,
    pub source: Rc<str>,
    pub lines: LineIndex,
    pub main: u32,
    /// The module's own `var`s and `function`s, which are not seen by other modules except by import.
    pub globals: HashMap<u32, Value>,
}

pub struct Vm {
    foreign: ffi::Libraries,
    pub(crate) heap: Heap,
    pub(crate) syms: Symbols,
    /// Boxed so a pointer to one stays good while more are loaded.
    pub(crate) protos: Vec<Box<LoadedProto>>,
    pub(crate) modules: Vec<LoadedModule>,
    /// What every module sees: the built-ins the host defines.
    pub(crate) globals: HashMap<u32, Value>,
    pub(crate) natives: Vec<NativeEntry>,
    /// Methods of built-in kinds, by kind and name: a native whose first argument is the receiver.
    pub(crate) methods: HashMap<(u8, u32), u32>,
    /// Modules by their import path, from the host.
    pub(crate) host_modules: HashMap<String, Value>,
    /// Script modules brought in by `import`: none while one is being initialised.
    pub(crate) script_modules: HashMap<String, Option<Value>>,
    fancy_errors: bool,
    pub(crate) resolver: Option<Box<dyn ModuleResolver>>,
    pub(crate) ready: VecDeque<Obj>,
    pub(crate) current: Option<Obj>,
    /// Every fiber started and not yet finished.
    pub(crate) fibers: Vec<Obj>,
    /// Promises the host has yet to settle.
    pub(crate) pending: HashMap<u64, Obj>,
    next_id: u64,
    pub(crate) oom: bool,
    /// Set by a native that wants the fiber that called it to give up its turn.
    pub(crate) yield_flag: bool,
    /// Values the host keeps alive.
    pub(crate) roots: Vec<Value>,
    /// How deep a native's call back into script is; a fiber cannot suspend inside one.
    pub(crate) nested: u32,
    /// The stack position of the outermost call a native made back into script.
    pub(crate) nested_base: usize,
    pub(crate) collections: usize,
    pub(crate) view: crate::view::ViewState,
    pub(crate) trace_sink: Option<Rc<dyn Fn(&str)>>,
    /// Polls a call a native makes back into script may use before it is stopped.
    pub(crate) call_fuel: u64,
    /// Fibers a script started with `spawn` that failed with nobody waiting.
    pub(crate) detached_failures: Vec<(crate::fiber::Failure, String)>,
    pub(crate) debug: Debugger,
}

impl Default for Vm {
    fn default() -> Self {
        Vm::new()
    }
}

impl Vm {
    pub fn new() -> Vm {
        Vm::with_config(HeapConfig::default())
    }

    pub fn with_config(config: HeapConfig) -> Vm {
        let mut vm = Vm {
            foreign: ffi::Libraries::default(),
            heap: Heap::new(config),
            syms: Symbols::default(),
            protos: Vec::new(),
            modules: Vec::new(),
            globals: HashMap::new(),
            natives: Vec::new(),
            methods: HashMap::new(),
            host_modules: HashMap::new(),
            script_modules: HashMap::new(),
            fancy_errors: false,
            resolver: None,
            ready: VecDeque::new(),
            current: None,
            fibers: Vec::new(),
            pending: HashMap::new(),
            next_id: 1,
            oom: false,
            yield_flag: false,
            roots: Vec::new(),
            nested: 0,
            nested_base: 0,
            collections: 0,
            view: Default::default(),
            trace_sink: None,
            call_fuel: u64::MAX,
            detached_failures: Vec::new(),
            debug: Debugger::default(),
        };
        crate::stdlib::install(&mut vm);
        crate::view::install(&mut vm);
        vm
    }

    // Allocation. An allocation the heap refuses sets `oom`, which the running fiber is stopped for at its next poll.

    /// A new object of type `T`, whose `Header` field `make` receives with the kind filled in.
    pub(crate) fn alloc<T>(&mut self, kind: Kind, make: impl FnOnce(crate::heap::Header) -> T) -> Option<Obj> {
        let size = std::mem::size_of::<T>().max(HEADER_SIZE);
        let p = self.heap.alloc(size, kind as u8, kind.droppable());
        if p.is_null() {
            self.oom = true;
            return None;
        }
        unsafe {
            let header = std::ptr::read(p);
            std::ptr::write(p as *mut T, make(header));
            Some(Obj(std::ptr::NonNull::new_unchecked(p)))
        }
    }

    pub fn new_string(&mut self, s: &str) -> Value {
        self.heap.track_external(s.len() as isize);
        match self.alloc(Kind::String, |header| StringObj { header, text: s.into() }) {
            Some(o) => Value::Obj(o),
            None => Value::Nil,
        }
    }

    pub fn new_array(&mut self, items: Vec<Value>) -> Value {
        self.heap.track_external((items.capacity() * std::mem::size_of::<Value>()) as isize);
        match self.alloc(Kind::Array, |header| ArrayObj { header, items }) {
            Some(o) => Value::Obj(o),
            None => Value::Nil,
        }
    }

    pub fn new_object(&mut self) -> Value {
        match self.alloc(Kind::Object, |header| ObjectObj { header, fields: Fields::new() }) {
            Some(o) => Value::Obj(o),
            None => Value::Nil,
        }
    }

    pub fn new_map(&mut self) -> Value {
        match self.alloc(Kind::Map, |header| MapObj { header, map: OrderedMap::new() }) {
            Some(o) => Value::Obj(o),
            None => Value::Nil,
        }
    }

    pub(crate) fn new_cell(&mut self, value: Value) -> Option<Obj> {
        self.alloc(Kind::Cell, |header| CellObj { header, value })
    }

    pub(crate) fn new_range(&mut self, from: i64, to: i64) -> Value {
        match self.alloc(Kind::Range, |header| RangeObj { header, from, to }) {
            Some(o) => Value::Obj(o),
            None => Value::Nil,
        }
    }

    pub(crate) fn new_closure(&mut self, proto: u32, upvals: Vec<Obj>) -> Option<Obj> {
        self.alloc(Kind::Closure, |header| ClosureObj { header, proto, upvals })
    }

    /// A host object of type `type_id` holding `data`.
    pub fn new_host(&mut self, type_id: u32, data: Box<dyn std::any::Any>) -> Value {
        match self.alloc(Kind::Host, |header| HostObj { header, type_id, data }) {
            Some(o) => Value::Obj(o),
            None => Value::Nil,
        }
    }

    /// A promise a script can await and the host will settle with `resolve` or `reject`; its number says which.
    pub fn new_promise(&mut self) -> (Value, u64) {
        let id = self.next_id;
        self.next_id += 1;
        match self.alloc(Kind::Promise, |header| PromiseObj { header, state: Settled::Pending, waiters: Vec::new() }) {
            Some(o) => {
                self.pending.insert(id, o);
                (Value::Obj(o), id)
            }
            None => (Value::Nil, id),
        }
    }

    // Names

    /// Limits each call a native makes back into script, such as an effect or a callback, to this many
    /// polls (loop edges and calls). A call over the limit is stopped with an error that says where.
    pub fn set_call_fuel(&mut self, polls: u64) {
        self.call_fuel = polls;
    }

    /// The failures of fibers that `spawn` started and nothing waited for, since the last call.
    pub fn take_detached_failures(&mut self) -> Vec<(crate::fiber::Failure, String)> {
        std::mem::take(&mut self.detached_failures)
    }

    /// Where `trace` writes, in place of standard output.
    pub fn set_trace_sink(&mut self, sink: Rc<dyn Fn(&str)>) {
        self.trace_sink = Some(sink);
    }

    /// A field of an object by name; None if `object` is not an object or has no such field.
    pub fn get_member(&self, object: Value, name: &str) -> Option<Value> {
        let sym = self.syms.lookup(name)?;
        match object {
            Value::Obj(o) if o.kind() == Kind::Object => o.object().fields.get(sym),
            _ => None,
        }
    }

    /// The names and values of an object's fields, in the order they were added; empty for anything else.
    pub fn object_fields(&self, object: Value) -> Vec<(String, Value)> {
        match object {
            Value::Obj(o) if o.kind() == Kind::Object => o.object().fields.iter().map(|(s, v)| (self.syms.name(*s).to_string(), *v)).collect(),
            _ => Vec::new(),
        }
    }

    /// How many fibers have started and not ended.
    pub fn fiber_count(&self) -> usize {
        self.fibers.len()
    }

    /// How many promises wait for the host to settle them.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn intern(&mut self, name: &str) -> u32 {
        self.syms.intern(name)
    }

    pub fn define_global(&mut self, name: &str, v: Value) {
        let sym = self.syms.intern(name);
        self.globals.insert(sym, v);
    }

    pub fn global(&self, name: &str) -> Option<Value> {
        self.syms.lookup(name).and_then(|s| self.globals.get(&s).copied())
    }

    /// Forgets what a module defined, so the collector can free it: for an extension being turned off or reloaded.
    pub fn unload_module(&mut self, module: u32) {
        self.modules[module as usize].globals.clear();
    }

    /// A name a loaded module defined at its top level.
    pub fn module_global(&self, module: u32, name: &str) -> Option<Value> {
        self.syms.lookup(name).and_then(|s| self.modules[module as usize].globals.get(&s).copied())
    }

    /// Makes a native available to scripts as a value.
    pub fn native(&mut self, name: &str, f: impl Fn(&mut Vm, &[Value]) -> NativeResult + 'static) -> Value {
        self.natives.push(NativeEntry { name: name.to_string(), f: Rc::new(f) });
        Value::Native((self.natives.len() - 1) as u32)
    }

    pub fn define_native(&mut self, name: &str, f: impl Fn(&mut Vm, &[Value]) -> NativeResult + 'static) {
        let v = self.native(name, f);
        self.define_global(name, v);
    }

    /// A method of a built-in kind: `receiver.name(args)` calls `f` with the receiver first.
    pub(crate) fn define_method(&mut self, kind: Kind, name: &str, f: NativePtr) {
        self.define_method_tag(kind as u8, &format!("{}.{}", kind.name(), name), name, f);
    }

    pub(crate) fn define_method_tag(&mut self, tag: u8, full_name: &str, name: &str, f: NativePtr) {
        let id = match self.native(full_name, f) {
            Value::Native(id) => id,
            _ => unreachable!(),
        };
        let sym = self.syms.intern(name);
        self.methods.insert((tag, sym), id);
    }

    /// An object of natives under `path`, found by `import path`; one with no dots in its path is a global too, as `Math` is.
    pub fn define_module(&mut self, path: &str, members: &[(&str, NativePtr)]) {
        let obj = self.new_object();
        for (name, f) in members {
            let v = self.native(&format!("{path}.{name}"), *f);
            let sym = self.syms.intern(name);
            obj.as_obj().unwrap().object().fields.set(sym, v);
        }
        self.register_module(path, obj);
    }

    /// Registers `value` as what `import path` gives.
    pub fn register_module(&mut self, path: &str, value: Value) {
        self.host_modules.insert(path.to_string(), value);
        if !path.contains('.') {
            self.define_global(path, value);
            return;
        }
        // A dotted path is also reachable by its full name, as Haxe's packages are: `haxe.Json`.
        let parts: Vec<&str> = path.split('.').collect();
        let mut current = match self.global(parts[0]) {
            Some(v) if matches!(v, Value::Obj(o) if o.kind() == Kind::Object) => v,
            _ => {
                let o = self.new_object();
                self.define_global(parts[0], o);
                o
            }
        };
        for seg in &parts[1..parts.len() - 1] {
            let sym = self.syms.intern(seg);
            let existing = current.as_obj().and_then(|o| o.object().fields.get(sym));
            current = match existing {
                Some(v) if matches!(v, Value::Obj(o) if o.kind() == Kind::Object) => v,
                _ => {
                    let o = self.new_object();
                    self.set_member(current, seg, o);
                    o
                }
            };
        }
        self.set_member(current, parts[parts.len() - 1], value);
    }

    /// Sets a member of an object value: for building a module a piece at a time.
    pub fn set_member(&mut self, object: Value, name: &str, v: Value) {
        if let Some(o) = object.as_obj().filter(|o| o.kind() == Kind::Object) {
            let sym = self.syms.intern(name);
            o.object().fields.set(sym, v);
        }
    }

    /// Sets the host's answer to an `import` that nothing registered.
    pub fn set_resolver(&mut self, r: Box<dyn ModuleResolver>) {
        self.resolver = Some(r);
    }

    /// What `import path` gives: a module the host registered, a script module (loaded once), what the
    /// resolver answers, or a member of a module a shorter path names.
    pub fn import_value(&mut self, path: &str) -> Result<Value, Value> {
        if let Some(v) = self.host_modules.get(path) {
            return Ok(*v);
        }
        match self.script_modules.get(path) {
            Some(Some(v)) => return Ok(*v),
            Some(None) => {
                let m = format!("`{path}` imports itself, directly or through other modules");
                return Err(self.new_string(&m));
            }
            None => {}
        }
        if let Some(mut r) = self.resolver.take() {
            let answer = r.resolve(self, path);
            self.resolver = Some(r);
            match answer {
                Some(Resolved::Value(v)) => {
                    self.host_modules.insert(path.to_string(), v);
                    return Ok(v);
                }
                Some(Resolved::Script { name, source }) => return self.load_script_module(path, &name, &source),
                None => {}
            }
        }
        if let Some((prefix, last)) = path.rsplit_once('.') {
            if let Ok(Value::Obj(m)) = self.import_value(prefix) {
                if m.kind() == Kind::Object {
                    let sym = self.syms.intern(last);
                    if let Some(v) = m.object().fields.get(sym) {
                        return Ok(v);
                    }
                }
            }
        }
        let m = format!("cannot find the module `{path}`");
        Err(self.new_string(&m))
    }

    /// Compiles and runs a script module once, and gives back an object of what it defines.
    fn load_script_module(&mut self, path: &str, name: &str, source: &str) -> Result<Value, Value> {
        let parsed = rayscript_syntax::parse(source);
        if parsed.has_errors() {
            let text = render_all(&parsed.diagnostics, name, source, false);
            return Err(self.new_string(&text));
        }
        let module = match crate::compiler::compile(name, source, &parsed.module) {
            Ok(m) => m,
            Err(diags) => {
                let text = render_all(&diags, name, source, false);
                return Err(self.new_string(&text));
            }
        };
        self.script_modules.insert(path.to_string(), None);
        let id = self.load(&module);
        let main = self.modules[id as usize].main;
        let result = match self.new_closure(main, Vec::new()) {
            Some(c) => self.call(Value::Obj(c), &[]),
            None => Err(self.new_string("out of memory")),
        };
        if let Err(e) = result {
            self.script_modules.remove(path);
            return Err(e);
        }
        let ns = self.new_object();
        let members: Vec<(u32, Value)> = self.modules[id as usize].globals.iter().map(|(k, v)| (*k, *v)).collect();
        for (k, v) in members {
            ns.as_obj().unwrap().object().fields.set(k, v);
        }
        self.script_modules.insert(path.to_string(), Some(ns));
        Ok(ns)
    }

    /// Parses, compiles and loads a script and makes a fiber of its statements, which runs when the VM next does.
    pub fn start_source(&mut self, name: &str, source: &str) -> Result<Value, String> {
        let parsed = rayscript_syntax::parse(source);
        if parsed.has_errors() {
            return Err(render_all(&parsed.diagnostics, name, source, self.fancy_errors));
        }
        let module = crate::compiler::compile(name, source, &parsed.module).map_err(|diags| render_all(&diags, name, source, self.fancy_errors))?;
        let id = self.load(&module);
        self.spawn_main(id).map_err(|e| self.display(e))
    }

    /// Parses, compiles and runs a script to its end, and gives its top-level `return`, or `null`. The text
    /// of an error is the diagnostics as they would be shown, or the error the script ended with and where.
    pub fn run_source(&mut self, name: &str, source: &str) -> Result<Value, String> {
        let fiber = self.start_source(name, source)?;
        self.run_until_idle(10_000);
        match self.fiber_state(fiber) {
            Some(FiberState::Done) => Ok(self.fiber_result(fiber).unwrap_or(Value::Nil)),
            Some(FiberState::Failed) => Err(self.render_failure(fiber)),
            Some(FiberState::Waiting) => Err("the script is waiting for something that never came".to_string()),
            other => Err(format!("the script ended {other:?}")),
        }
    }

    /// Whether error text from `run_source` and `render_failure` has colour and Unicode line drawing, for a
    /// terminal. Off, it is plain ASCII.
    pub fn set_fancy_errors(&mut self, fancy: bool) {
        self.fancy_errors = fancy;
    }

    /// A failed fiber's error as a report: the message, then the line where it was raised and the calls that
    /// led there, each with its line.
    pub fn render_failure(&self, fiber: Value) -> String {
        let Some(failure) = self.fiber_failure(fiber) else { return "the fiber did not fail".to_string() };
        let color = Severity::Error.color();
        let at = |t: &crate::fiber::TraceEntry| {
            let source = &self.modules[t.module as usize].source;
            (t.module as usize, token_range(source, t.offset))
        };
        let mut out = Vec::new();
        let Some(first) = failure.trace.first() else { return format!("Error: {}\n", failure.message) };
        let mut report = Report::build(Severity::Error.kind(), at(first)).with_config(span::config(self.fancy_errors)).with_message(&failure.message);
        report = report.with_label(Label::new(at(first)).with_message(format!("in `{}`", first.function)).with_color(color));
        for (callee, t) in failure.trace.iter().zip(failure.trace.iter().skip(1)).take(MAX_TRACE_LABELS) {
            report = report.with_label(Label::new(at(t)).with_message(format!("`{}` is called here", callee.function)).with_color(span::NOTE_COLOR));
        }
        if failure.trace.len() > MAX_TRACE_LABELS + 1 {
            report = report.with_note(format!("and {} more calls", failure.trace.len() - MAX_TRACE_LABELS - 1));
        }
        let cache = ModuleSources { vm: self, sources: std::cell::RefCell::new(HashMap::new()) };
        let _ = report.finish().write(cache, &mut out);
        String::from_utf8_lossy(&out).into_owned()
    }

    /// A value the host keeps alive until `unroot`.
    pub fn root(&mut self, v: Value) {
        self.roots.push(v);
    }

    // Loading

    /// Makes a compiled module ready to run, and returns its number.
    pub fn load(&mut self, module: &Module) -> u32 {
        let module_id = self.modules.len() as u32;
        let base = self.protos.len() as u32;
        for p in &module.protos {
            let mut consts = Vec::with_capacity(p.consts.len());
            let mut syms = Vec::with_capacity(p.consts.len());
            for c in &p.consts {
                match c {
                    Const::Int(i) => {
                        consts.push(Value::Int(*i));
                        syms.push(u32::MAX);
                    }
                    Const::Float(f) => {
                        consts.push(Value::Float(*f));
                        syms.push(u32::MAX);
                    }
                    Const::Str(s) => {
                        consts.push(self.new_string(s));
                        syms.push(self.syms.intern(s));
                    }
                }
            }
            // Closures refer to functions of their own module, so they are renumbered into the VM's table.
            let code = p
                .code
                .iter()
                .map(|op| match *op {
                    Op::Closure { dst, proto } => Op::Closure { dst, proto: proto + base },
                    other => other,
                })
                .collect();
            self.protos.push(Box::new(LoadedProto {
                name: p.name.clone(),
                params: p.params,
                has_rest: p.has_rest,
                registers: p.registers,
                code,
                offsets: p.offsets.clone(),
                consts,
                syms,
                upvals: p.upvals.clone(),
                module: module_id,
                locals: p.locals.clone(),
            }));
        }
        self.modules.push(LoadedModule { name: module.name.clone(), source: module.source.clone(), lines: LineIndex::new(&module.source), main: module.main + base, globals: HashMap::new() });
        if !self.debug.wanted_is_empty() {
            self.debug_resolve();
        }
        module_id
    }

    // Fibers

    /// A fiber that will call `callee` with `args` when it first runs; it is queued to run.
    pub fn spawn(&mut self, callee: Value, args: &[Value]) -> Result<Value, Value> {
        let id = self.next_id;
        self.next_id += 1;
        let Some(obj) = self.alloc(Kind::Fiber, |header| FiberObj { header, fiber: Fiber::new(id) }) else {
            return Err(self.new_string("out of memory"));
        };
        {
            let f = &mut obj.fiber().fiber;
            f.stack.push(callee);
            f.stack.extend_from_slice(args);
        }
        match callee {
            Value::Obj(c) if c.kind() == Kind::Closure => {
                let f = &mut obj.fiber().fiber;
                if let Err(e) = self.push_frame(f, c, 0, args.len()) {
                    return Err(e);
                }
            }
            Value::Native(_) => {
                // A native is run by wrapping the call: the first instruction a fiber of one executes.
                return Err(self.new_string("a fiber starts with a script function"));
            }
            _ => return Err(self.new_string("a fiber starts with a function")),
        }
        self.fibers.push(obj);
        self.ready.push_back(obj);
        Ok(Value::Obj(obj))
    }

    /// Runs a loaded module's own statements as a fiber.
    pub fn spawn_main(&mut self, module: u32) -> Result<Value, Value> {
        let main = self.modules[module as usize].main;
        let Some(closure) = self.new_closure(main, Vec::new()) else {
            return Err(self.new_string("out of memory"));
        };
        self.spawn(Value::Obj(closure), &[])
    }

    /// Lays a call frame on the fiber for `closure`, whose callee slot is `slot` and arguments follow.
    pub(crate) fn push_frame(&mut self, f: &mut Fiber, closure: Obj, slot: usize, argc: usize) -> Result<(), Value> {
        if f.frames.len() >= 10_000 {
            return Err(self.new_string("stack overflow: too many nested calls"));
        }
        let (params, has_rest, registers) = {
            let p = &self.protos[closure.closure().proto as usize];
            (p.params as usize, p.has_rest, p.registers as usize)
        };
        let base = slot + 1;
        let need = base + registers + 1;
        if f.stack.len() < need {
            f.stack.resize(need, Value::Nil);
        }
        if has_rest {
            let fixed = params - 1;
            let extra: Vec<Value> = if argc > fixed { f.stack[base + fixed..base + argc].to_vec() } else { Vec::new() };
            for i in argc.min(fixed)..fixed {
                f.stack[base + i] = Value::Nil;
            }
            f.stack[base + fixed] = Value::Nil;
            let rest = self.new_array(extra);
            // The stack may have been re-read by the allocation; write the packed array last.
            f.stack[base + fixed] = rest;
        } else {
            for i in argc.min(params)..params {
                f.stack[base + i] = Value::Nil;
            }
        }
        f.top = base + registers;
        f.frames.push(Frame { closure, pc: 0, base });
        Ok(())
    }

    /// The fiber's result, once it is done.
    pub fn fiber_state(&self, fiber: Value) -> Option<FiberState> {
        fiber.as_obj().filter(|o| o.kind() == Kind::Fiber).map(|o| o.fiber().fiber.state)
    }

    pub fn fiber_result(&self, fiber: Value) -> Option<Value> {
        fiber.as_obj().filter(|o| o.kind() == Kind::Fiber).map(|o| o.fiber().fiber.result)
    }

    pub fn fiber_failure(&self, fiber: Value) -> Option<crate::fiber::Failure> {
        fiber.as_obj().filter(|o| o.kind() == Kind::Fiber).and_then(|o| o.fiber().fiber.failure.clone())
    }

    /// Asks a fiber to stop at its next poll.
    pub fn cancel(&mut self, fiber: Value) {
        if let Some(o) = fiber.as_obj().filter(|o| o.kind() == Kind::Fiber) {
            o.fiber().fiber.cancelled = true;
            if o.fiber().fiber.state == FiberState::Waiting {
                o.fiber().fiber.state = FiberState::Ready;
                self.ready.push_back(o);
            }
        }
    }

    /// The text of a value as a script would print it.
    pub fn display(&self, v: Value) -> String {
        ops::display(self, v, 0)
    }

    pub fn string_of(&self, v: Value) -> Option<&str> {
        match v {
            Value::Obj(o) if o.kind() == Kind::String => Some(o.str()),
            _ => None,
        }
    }

    pub fn module_name(&self, id: u32) -> &str {
        &self.modules[id as usize].name
    }

    /// `name:line:column` for a source offset of a module.
    pub fn locate(&self, module: u32, offset: u32) -> (String, u32, u32) {
        let m = &self.modules[module as usize];
        let (line, col) = m.lines.locate(&m.source, offset);
        (m.name.clone(), line + 1, col + 1)
    }
}

/// The most calls a failure report points at; the rest are counted.
const MAX_TRACE_LABELS: usize = 8;

/// The source range of the word or symbol at a byte offset.
fn token_range(source: &str, offset: u32) -> std::ops::Range<usize> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let start = (offset as usize).min(source.len());
    let end = match source[start..].chars().next() {
        Some(c) if word(c) => start + source[start..].find(|c: char| !word(c)).unwrap_or(source.len() - start),
        _ => start,
    };
    span::byte_range(Span::new(start as u32, end as u32), source)
}

/// Gives a report the source of each module that has code on the failed fiber's stack.
struct ModuleSources<'a> {
    vm: &'a Vm,
    sources: std::cell::RefCell<HashMap<usize, Source<Rc<str>>>>,
}

impl Cache<usize> for ModuleSources<'_> {
    type Storage = Rc<str>;

    fn fetch(&mut self, id: &usize) -> Result<&Source<Rc<str>>, impl std::fmt::Debug> {
        let map = self.sources.get_mut();
        if !map.contains_key(id) {
            let module = self.vm.modules.get(*id).ok_or("no such module")?;
            map.insert(*id, Source::from(module.source.clone()));
        }
        Ok::<_, &str>(&map[id])
    }

    fn display<'a>(&self, id: &'a usize) -> Option<impl std::fmt::Display + 'a> {
        self.vm.modules.get(*id).map(|m| m.name.clone())
    }
}
