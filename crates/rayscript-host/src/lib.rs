//! Runs one RayScript extension in a process of its own and talks to the editor in JSON lines: one JSON
//! object to a line, on standard input and standard output. Standard error is for the host's own
//! diagnostics and is free text. One extension to a process means a runaway script or a fault in native
//! code takes down only that extension, and the editor can kill it.
//!
//! Every message has a `"t"` that says what it is.
//!
//! # From the editor to the host
//!
//! - `{"t":"module","path":"rayde.workspace","functions":["say","activeBuffer"]}` makes a module that
//!   scripts can `import`. Each function, when called, sends a `call` and gives the script a promise to
//!   `await`. Send these before `run`.
//! - `{"t":"run","name":"main.rays","source":"..."}` runs a script. The host imports other script
//!   modules from files under its `--root`, and the standard library and remote modules from itself.
//! - `{"t":"event","node":7,"event":"click","payload":...}` sends an event to the handler of an element.
//!   The payload is any JSON value, and is what the handler is called with.
//! - `{"t":"resolve","id":3,"value":...}` and `{"t":"reject","id":3,"message":"..."}` answer a `call`.
//! - `{"t":"describe"}` asks what the host is doing.
//! - `{"t":"shutdown"}` ends the host. So does the end of its input.
//!
//! # From the host to the editor
//!
//! - `{"t":"ready","protocol":1}` once, first.
//! - `{"t":"ops","ops":[...]}` changes to the element tree, in order. Elements are numbered by the host.
//!   Each operation is an array: `["create",id,tag]`, `["createText",id,text]`, `["setText",id,text]`,
//!   `["set",id,name,value]`, `["unset",id,name]`, `["listen",id,event]`, `["unlisten",id,event]`,
//!   `["insert",parent,child,before|null]` (an element already in the tree is moved) and `["remove",id]`
//!   (the element and everything under it is gone). A view is mounted as an element tagged `#root` with a
//!   `name` property; the editor puts it where the name says.
//! - `{"t":"call","id":3,"fn":"rayde.workspace.say","args":[...]}` calls a function of a remote module.
//!   Answer with `resolve` or `reject`. Operations the script has made before the call are sent before it.
//! - `{"t":"log","message":"..."}` is what the script `trace`d.
//! - `{"t":"error","kind":"syntax"|"compile"|"runtime"|"view"|"protocol","message":"...","items":[...]}`
//!   says what went wrong. `message` is text for a person; `items` are the same problems with positions:
//!   `{"message","file","line","col","endLine","endCol","help","function"}`, each field only when known.
//! - `{"t":"describe","mounts":[...],"nodes":n,...}` answers `describe`.
//!
//! A script may `sleep(milliseconds)`, which is a promise too.

pub mod wire;

use rayscript_syntax::{Diagnostic, LineIndex, Severity};
use rayscript_vm::stdlib::json::{parse, quote, stringify};
use rayscript_vm::{ModuleResolver, Resolved, Value, Vm};
use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The version of this protocol.
pub const PROTOCOL: u32 = 1;

/// Polls a fiber may use in one turn.
const FUEL: u64 = 20_000;

type Outbox = Rc<RefCell<Vec<String>>>;

#[derive(Default)]
pub struct Config {
    /// Where `import a.b.C` looks for `a/b/C.rays`; without it, only remote modules and the standard library.
    pub root: Option<PathBuf>,
    /// How long one `pump` may run before it gives the thread back.
    pub slice: Duration,
}

struct Resolver {
    modules: Rc<RefCell<HashMap<String, Vec<String>>>>,
    out: Outbox,
    root: Option<PathBuf>,
}

impl ModuleResolver for Resolver {
    fn resolve(&mut self, vm: &mut Vm, path: &str) -> Option<Resolved> {
        let functions = self.modules.borrow().get(path).cloned();
        if let Some(functions) = functions {
            let module = vm.new_object();
            for f in functions {
                let full = format!("{path}.{f}");
                let out = self.out.clone();
                let name = full.clone();
                let call = vm.native(&full, move |vm, args| remote_call(vm, &out, &name, args));
                vm.set_member(module, &f, call);
            }
            return Some(Resolved::Value(module));
        }
        let root = self.root.as_ref()?;
        let mut file = root.clone();
        for part in path.split('.') {
            if part.is_empty() || !part.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
                return None;
            }
            file.push(part);
        }
        file.set_extension("rays");
        let source = std::fs::read_to_string(&file).ok()?;
        Some(Resolved::Script { name: file.display().to_string(), source })
    }
}

/// Sends a call to the editor and gives back the promise its answer settles.
fn remote_call(vm: &mut Vm, out: &Outbox, name: &str, args: &[Value]) -> Result<Value, Value> {
    let list = vm.new_array(args.to_vec());
    let json = stringify(vm, list)?;
    let (promise, id) = vm.new_promise();
    // What the script has drawn so far goes first: the editor sees the call after what led up to it.
    let ops = vm.view_take_ops();
    let mut out = out.borrow_mut();
    if !ops.is_empty() {
        out.push(wire::ops_line(&ops));
    }
    let mut line = format!("{{\"t\":\"call\",\"id\":{id},\"fn\":");
    quote(name, &mut line);
    line.push_str(&format!(",\"args\":{json}}}"));
    out.push(line);
    Ok(promise)
}

/// One extension: a VM, the modules the editor has described, and the lines waiting to be sent.
pub struct Host {
    vm: Vm,
    out: Outbox,
    modules: Rc<RefCell<HashMap<String, Vec<String>>>>,
    timers: Rc<RefCell<BinaryHeap<Reverse<(Instant, u64)>>>>,
    scripts: Vec<(String, Value, bool)>,
    shutdown: bool,
    ops_sent: u64,
    slice: Duration,
}

impl Host {
    pub fn new(config: Config) -> Host {
        let mut vm = Vm::new();
        let out: Outbox = Rc::new(RefCell::new(Vec::new()));
        let modules = Rc::new(RefCell::new(HashMap::new()));
        let timers = Rc::new(RefCell::new(BinaryHeap::new()));
        vm.set_resolver(Box::new(Resolver { modules: modules.clone(), out: out.clone(), root: config.root }));
        {
            let out = out.clone();
            vm.set_trace_sink(Rc::new(move |text| {
                let mut line = String::from("{\"t\":\"log\",\"message\":");
                quote(text, &mut line);
                line.push('}');
                out.borrow_mut().push(line);
            }));
        }
        {
            let timers = timers.clone();
            vm.define_native("sleep", move |vm, args| {
                let ms = match args.first() {
                    Some(Value::Int(n)) => (*n).max(0) as u64,
                    Some(Value::Float(f)) => f.max(0.0) as u64,
                    _ => 0,
                };
                let (promise, id) = vm.new_promise();
                timers.borrow_mut().push(Reverse((Instant::now() + Duration::from_millis(ms), id)));
                Ok(promise)
            });
        }
        let slice = if config.slice.is_zero() { Duration::from_millis(4) } else { config.slice };
        Host { vm, out, modules, timers, scripts: Vec::new(), shutdown: false, ops_sent: 0, slice }
    }

    pub fn vm(&mut self) -> &mut Vm {
        &mut self.vm
    }

    /// Whether the editor has asked the host to end.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown
    }

    /// The lines to send, in order; they are gone from the host afterwards.
    pub fn take_output(&mut self) -> Vec<String> {
        std::mem::take(&mut *self.out.borrow_mut())
    }

    /// When the soonest timer is due.
    pub fn next_wake(&self) -> Option<Instant> {
        self.timers.borrow().peek().map(|Reverse((at, _))| *at)
    }

    fn send(&mut self, line: String) {
        self.out.borrow_mut().push(line);
    }

    fn error(&mut self, kind: &str, message: &str, items: &str) {
        let mut line = format!("{{\"t\":\"error\",\"kind\":\"{kind}\",\"message\":");
        quote(message, &mut line);
        if !items.is_empty() {
            line.push_str(",\"items\":[");
            line.push_str(items);
            line.push(']');
        }
        line.push('}');
        self.send(line);
    }

    fn string_field(&self, msg: Value, name: &str) -> Option<String> {
        self.vm.get_member(msg, name).and_then(|v| self.vm.string_of(v)).map(str::to_string)
    }

    fn int_field(&self, msg: Value, name: &str) -> Option<i64> {
        match self.vm.get_member(msg, name) {
            Some(Value::Int(n)) => Some(n),
            Some(Value::Float(f)) if f.fract() == 0.0 => Some(f as i64),
            _ => None,
        }
    }

    /// Handles one line from the editor.
    pub fn handle_line(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let msg = match parse(&mut self.vm, line) {
            Ok(m) => m,
            Err(e) => {
                let text = self.vm.display(e);
                self.error("protocol", &format!("that line is not JSON: {text}"), "");
                return;
            }
        };
        let Some(kind) = self.string_field(msg, "t") else {
            self.error("protocol", "a message needs a \"t\" that says what it is", "");
            return;
        };
        match kind.as_str() {
            "run" => {
                let name = self.string_field(msg, "name").unwrap_or_else(|| "main.rays".into());
                match self.string_field(msg, "source") {
                    Some(source) => self.run(&name, &source),
                    None => self.error("protocol", "`run` needs a \"source\"", ""),
                }
            }
            "module" => match (self.string_field(msg, "path"), self.vm.get_member(msg, "functions")) {
                (Some(path), Some(Value::Obj(list))) if list.kind() == rayscript_vm::object::Kind::Array => {
                    let names: Vec<String> = list.array().items.iter().filter_map(|v| self.vm.string_of(*v).map(str::to_string)).collect();
                    self.modules.borrow_mut().insert(path, names);
                }
                _ => self.error("protocol", "`module` needs a \"path\" and a list of \"functions\"", ""),
            },
            "event" => {
                let (Some(node), Some(event)) = (self.int_field(msg, "node"), self.string_field(msg, "event")) else {
                    self.error("protocol", "`event` needs a \"node\" and an \"event\"", "");
                    return;
                };
                let payload = self.vm.get_member(msg, "payload").unwrap_or(Value::Nil);
                // An element that is gone, or no longer listening, is the usual end of a race: not an error.
                self.vm.view_dispatch(node as u32, &event, payload);
            }
            "resolve" | "reject" => {
                let Some(id) = self.int_field(msg, "id") else {
                    self.error("protocol", &format!("`{kind}` needs the \"id\" of a call"), "");
                    return;
                };
                let settled = if kind == "resolve" {
                    let value = self.vm.get_member(msg, "value").unwrap_or(Value::Nil);
                    self.vm.resolve(id as u64, value)
                } else {
                    let text = self.string_field(msg, "message").unwrap_or_else(|| "the call failed".into());
                    let thrown = self.vm.new_string(&text);
                    self.vm.reject(id as u64, thrown)
                };
                if !settled {
                    self.error("protocol", &format!("no call is waiting for id {id}"), "");
                }
            }
            "describe" => self.describe(),
            "shutdown" => self.shutdown = true,
            other => self.error("protocol", &format!("unknown message `{other}`"), ""),
        }
    }

    fn describe(&mut self) {
        let stats = self.vm.heap_stats();
        let mut mounts = String::new();
        for (i, (root, name)) in self.vm.view_mounts().iter().enumerate() {
            if i > 0 {
                mounts.push(',');
            }
            mounts.push_str(&format!("{{\"root\":{root},\"name\":"));
            quote(name, &mut mounts);
            mounts.push('}');
        }
        let mut modules: Vec<String> = self.modules.borrow().keys().cloned().collect();
        modules.sort();
        let mut names = String::new();
        for (i, m) in modules.iter().enumerate() {
            if i > 0 {
                names.push(',');
            }
            quote(m, &mut names);
        }
        let line = format!(
            "{{\"t\":\"describe\",\"protocol\":{PROTOCOL},\"mounts\":[{mounts}],\"nodes\":{},\"fibers\":{},\"pendingCalls\":{},\"timers\":{},\"opsSent\":{},\"remoteModules\":[{names}],\"heap\":{{\"objects\":{},\"bytes\":{},\"collections\":{}}}}}",
            self.vm.view_node_count(),
            self.vm.fiber_count(),
            self.vm.pending_count(),
            self.timers.borrow().len(),
            self.ops_sent,
            stats.objects,
            stats.bytes,
            self.vm.collections(),
        );
        self.send(line);
    }

    fn item(&self, message: &str, file: &str, at: Option<(u32, u32, u32, u32)>, help: Option<&str>, function: Option<&str>) -> String {
        let mut out = String::from("{\"message\":");
        quote(message, &mut out);
        out.push_str(",\"file\":");
        quote(file, &mut out);
        if let Some((line, col, end_line, end_col)) = at {
            out.push_str(&format!(",\"line\":{line},\"col\":{col},\"endLine\":{end_line},\"endCol\":{end_col}"));
        }
        if let Some(h) = help {
            out.push_str(",\"help\":");
            quote(h, &mut out);
        }
        if let Some(f) = function {
            out.push_str(",\"function\":");
            quote(f, &mut out);
        }
        out.push('}');
        out
    }

    fn diagnostics(&mut self, kind: &str, name: &str, source: &str, diags: &[Diagnostic]) {
        let index = LineIndex::new(source);
        let items: Vec<String> = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| {
                let (l, c) = index.locate(source, d.span.start);
                let (el, ec) = index.locate(source, d.span.end);
                self.item(&d.message, name, Some((l + 1, c + 1, el + 1, ec + 1)), d.help.as_deref(), None)
            })
            .collect();
        let text = rayscript_syntax::render_all(diags, name, source, false);
        self.error(kind, &text, &items.join(","));
    }

    fn run(&mut self, name: &str, source: &str) {
        let parsed = rayscript_syntax::parse(source);
        if parsed.has_errors() {
            self.diagnostics("syntax", name, source, &parsed.diagnostics);
            return;
        }
        let module = match rayscript_vm::compile(name, source, &parsed.module) {
            Ok(m) => m,
            Err(diags) => {
                self.diagnostics("compile", name, source, &diags);
                return;
            }
        };
        let id = self.vm.load(&module);
        match self.vm.spawn_main(id) {
            Ok(fiber) => {
                self.vm.root(fiber);
                self.scripts.push((name.to_string(), fiber, false));
            }
            Err(e) => {
                let text = self.vm.display(e);
                self.error("runtime", &text, "");
            }
        }
    }

    /// Reports each script that has failed, once.
    fn report_failures(&mut self) {
        for i in 0..self.scripts.len() {
            let (_, fiber, reported) = self.scripts[i].clone();
            if reported || self.vm.fiber_state(fiber) != Some(rayscript_vm::fiber::FiberState::Failed) {
                continue;
            }
            self.scripts[i].2 = true;
            let text = self.vm.render_failure(fiber);
            let mut items = Vec::new();
            if let Some(f) = self.vm.fiber_failure(fiber) {
                for t in &f.trace {
                    let (file, line, col) = self.vm.locate(t.module, t.offset);
                    items.push(self.item(&f.message, &file, Some((line, col, line, col)), None, Some(&t.function)));
                }
            }
            self.error("runtime", &text, &items.join(","));
        }
    }

    fn fire_timers(&mut self) {
        let now = Instant::now();
        loop {
            let due = match self.timers.borrow().peek() {
                Some(Reverse((at, id))) if *at <= now => Some(*id),
                _ => None,
            };
            let Some(id) = due else { break };
            self.timers.borrow_mut().pop();
            self.vm.resolve(id, Value::Nil);
        }
    }

    fn emit_ops(&mut self) {
        let ops = self.vm.view_take_ops();
        if !ops.is_empty() {
            self.ops_sent += ops.len() as u64;
            self.send(wire::ops_line(&ops));
        }
    }

    /// Runs what is ready for up to a slice of time, and queues what it made for sending. True if there is
    /// more to do right away.
    pub fn pump(&mut self) -> bool {
        let deadline = Instant::now() + self.slice;
        self.fire_timers();
        let mut more;
        loop {
            more = self.vm.view_step(FUEL);
            if !more || Instant::now() >= deadline {
                break;
            }
        }
        self.report_failures();
        for e in self.vm.view_take_errors() {
            self.error("view", &e, "");
        }
        self.emit_ops();
        more
    }
}

enum Input {
    Line(String),
    End,
}

/// Serves the protocol on `input` and `output` until the editor shuts the host down or closes the input.
/// Run it on a thread with a stack of 16 MiB or more.
pub fn serve<R: BufRead + Send + 'static, W: Write>(input: R, mut output: W, config: Config) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in input.lines() {
            match line {
                Ok(l) => {
                    if tx.send(Input::Line(l)).is_err() {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(Input::End);
    });
    let mut host = Host::new(config);
    writeln!(output, "{{\"t\":\"ready\",\"protocol\":{PROTOCOL}}}")?;
    output.flush()?;
    let mut ended = false;
    while !host.is_shutdown() && !ended {
        loop {
            match rx.try_recv() {
                Ok(Input::Line(l)) => host.handle_line(&l),
                Ok(Input::End) | Err(mpsc::TryRecvError::Disconnected) => {
                    ended = true;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        let more = host.pump();
        for line in host.take_output() {
            writeln!(output, "{line}")?;
        }
        output.flush()?;
        if more || host.is_shutdown() || ended {
            continue;
        }
        let received = match host.next_wake() {
            Some(at) => rx.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        match received {
            Ok(Input::Line(l)) => host.handle_line(&l),
            Ok(Input::End) | Err(mpsc::RecvTimeoutError::Disconnected) => ended = true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    Ok(())
}
