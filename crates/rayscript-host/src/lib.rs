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
//! - `{"t":"hook","name":"file.saved","data":{...}}` is an IDE signal. Each handler the script gave
//!   `onHook("file.saved", handler)` runs with `data`. Hooks nothing listens to are dropped, and the host tells
//!   the editor which are listened to (`subscribe`), so the editor need only send those.
//! - `{"t":"command","id":"preview","args":{...}}` runs the handler the script gave
//!   `registerCommand("preview", handler)`, with `args`. Without one, an `error` of kind `view` says so.
//! - `{"t":"closed","root":3}` says the view the script mounted as root 3 has been closed, as a tab is: the
//!   host unmounts it.
//! - `{"t":"tool","id":"add"}` says a tool of the extension's header was chosen: the handler the script
//!   gave `onTool("add", handler)` runs. Without one, an `error` of kind `view` says so.
//! - `{"t":"debug","op":"breakpoints","file":"/path/main.rays","lines":[3,9]}` puts the breakpoints of a file on
//!   those lines, replacing the ones it had, and is answered with `{"t":"breakpoints","file":..,"lines":[..]}`: the
//!   lines as they stand, each moved down to a line that has code. A file that is not loaded yet takes them when it is.
//!   `{"t":"debug","op":"pause"}` stops the running script at its next instruction. A fiber that stops (at a
//!   breakpoint, a step, or a pause) is announced with `{"t":"paused","reason":"breakpoint|step|pause","frames":[{"function","file","line","col"}]}`,
//!   the innermost frame first, and the script then runs nothing until it is told to go on.
//!   While stopped: `{"t":"debug","op":"continue|stepOver|stepIn|stepOut"}` goes on, answered with `{"t":"resumed"}`;
//!   `{"t":"debug","op":"variables","frame":0}` gives `{"t":"variables","frame":0,"locals":[..],"globals":[..]}`, each variable
//!   `{"name","value","type","ref"}` with a `ref` other than 0 when there is more inside; `{"t":"debug","op":"expand","ref":3}` gives
//!   `{"t":"children","ref":3,"items":[..]}`; `{"t":"debug","op":"evaluate","frame":0,"expr":"todo.items[1].title"}` gives
//!   `{"t":"evaluated","expr":..,"ok":true,"result":{..}}` or `"ok":false` with a `message`. An expression is a name and
//!   members and indexes of it, not arithmetic or calls.
//! - `{"t":"describe"}` asks what the host is doing.
//! - `{"t":"shutdown"}` ends the host. So does the end of its input.
//!
//! # From the host to the editor
//!
//! - `{"t":"ready","protocol":1}` once, first.
//! - `{"t":"started","name":"main.rays"}` when a `run` compiled and has begun. Without it (an `error` of
//!   kind `syntax` or `compile` instead), the script did not run: an editor can keep showing the last one
//!   that did.
//! - `{"t":"ops","ops":[...]}` changes to the element tree, in order. Elements are numbered by the host.
//!   Each operation is an array: `["create",id,tag]`, `["createText",id,text]`, `["setText",id,text]`,
//!   `["set",id,name,value]`, `["unset",id,name]`, `["listen",id,event]`, `["unlisten",id,event]`,
//!   `["insert",parent,child,before|null]` (an element already in the tree is moved) and `["remove",id]`
//!   (the element and everything under it is gone). A view is mounted as an element tagged `#root` with a
//!   `name` property; the editor puts it where the name says.
//! - `{"t":"call","id":3,"fn":"rayde.workspace.say","args":[...]}` calls a function of a remote module.
//!   Answer with `resolve` or `reject`. Operations the script has made before the call are sent before it.
//! - `{"t":"subscribe","hook":"file.saved"}` the first time a script listens for a hook.
//! - `{"t":"registered","id":"preview"}` when a script registers a command.
//! - `{"t":"view","title":"..."}` when the script called `setTitle(text)`: the heading of the extension's view.
//! - `{"t":"log","message":"..."}` is what the script `trace`d.
//! - `{"t":"error","kind":"syntax"|"compile"|"runtime"|"view"|"protocol","message":"...","items":[...]}`
//!   says what went wrong. `message` is text for a person; `items` are the same problems with positions:
//!   `{"message","file","line","col","endLine","endCol","help","function"}`, each field only when known.
//! - `{"t":"describe","mounts":[...],"nodes":n,...}` answers `describe`.
//!
//! A script may `sleep(milliseconds)`, which is a promise too, `onTool(id, handler)` to answer a tool of its
//! header, `onHook(name, handler)` and `registerCommand(id, handler)`, and `setTitle(text)` to change the
//! heading of its view. A view mounted with a name that begins `tab:` is meant for a tab, titled by the rest.

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
    /// Polls (loop edges and calls) one effect, computed value or other call from a built-in may use
    /// before it is stopped; 0 for the default of five million.
    pub call_fuel: u64,
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
    tools: Rc<RefCell<HashMap<String, Value>>>,
    hooks: Rc<RefCell<HashMap<String, Vec<Value>>>>,
    commands: Rc<RefCell<HashMap<String, Value>>>,
    scripts: Vec<(String, Value, bool)>,
    shutdown: bool,
    ops_sent: u64,
    slice: Duration,
    /// Whether the editor has been told about the fiber that is stopped.
    pause_told: bool,
}

impl Host {
    pub fn new(config: Config) -> Host {
        let mut vm = Vm::new();
        vm.set_call_fuel(if config.call_fuel == 0 { 5_000_000 } else { config.call_fuel });
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
        let tools: Rc<RefCell<HashMap<String, Value>>> = Rc::new(RefCell::new(HashMap::new()));
        {
            let tools = tools.clone();
            vm.define_native("onTool", move |vm, args| {
                let (Some(id), Some(handler)) = (args.first().and_then(|v| vm.string_of(*v)).map(str::to_string), args.get(1).copied()) else {
                    return Err(vm.new_string("onTool requires a tool id and a handler"));
                };
                // The handler is kept for the life of the extension.
                vm.root(handler);
                tools.borrow_mut().insert(id, handler);
                Ok(Value::Nil)
            });
        }
        let hooks: Rc<RefCell<HashMap<String, Vec<Value>>>> = Rc::new(RefCell::new(HashMap::new()));
        {
            let (hooks, out) = (hooks.clone(), out.clone());
            vm.define_native("onHook", move |vm, args| {
                let (Some(name), Some(handler)) = (args.first().and_then(|v| vm.string_of(*v)).map(str::to_string), args.get(1).copied()) else {
                    return Err(vm.new_string("onHook requires a hook name and a handler"));
                };
                vm.root(handler);
                let first = !hooks.borrow().contains_key(&name);
                if first {
                    let mut line = String::from("{\"t\":\"subscribe\",\"hook\":");
                    quote(&name, &mut line);
                    line.push('}');
                    out.borrow_mut().push(line);
                }
                hooks.borrow_mut().entry(name).or_default().push(handler);
                Ok(Value::Nil)
            });
        }
        let commands: Rc<RefCell<HashMap<String, Value>>> = Rc::new(RefCell::new(HashMap::new()));
        {
            let (commands, out) = (commands.clone(), out.clone());
            vm.define_native("registerCommand", move |vm, args| {
                let (Some(id), Some(handler)) = (args.first().and_then(|v| vm.string_of(*v)).map(str::to_string), args.get(1).copied()) else {
                    return Err(vm.new_string("registerCommand requires a command id and a handler"));
                };
                vm.root(handler);
                let mut line = String::from("{\"t\":\"registered\",\"id\":");
                quote(&id, &mut line);
                line.push('}');
                out.borrow_mut().push(line);
                commands.borrow_mut().insert(id, handler);
                Ok(Value::Nil)
            });
        }
        {
            let out = out.clone();
            vm.define_native("setTitle", move |vm, args| {
                let text = args.first().map(|v| vm.display(*v)).unwrap_or_default();
                let mut line = String::from("{\"t\":\"view\",\"title\":");
                quote(&text, &mut line);
                line.push('}');
                out.borrow_mut().push(line);
                Ok(Value::Nil)
            });
        }
        let slice = if config.slice.is_zero() { Duration::from_millis(4) } else { config.slice };
        Host { vm, out, modules, timers, tools, hooks, commands, scripts: Vec::new(), shutdown: false, ops_sent: 0, slice, pause_told: false }
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
                self.error("protocol", &format!("Invalid JSON: {text}"), "");
                return;
            }
        };
        let Some(kind) = self.string_field(msg, "t") else {
            self.error("protocol", "Message has no \"t\" field", "");
            return;
        };
        match kind.as_str() {
            "run" => {
                let name = self.string_field(msg, "name").unwrap_or_else(|| "main.rays".into());
                match self.string_field(msg, "source") {
                    Some(source) => self.run(&name, &source),
                    None => self.error("protocol", "`run` requires \"source\"", ""),
                }
            }
            "module" => match (self.string_field(msg, "path"), self.vm.get_member(msg, "functions")) {
                (Some(path), Some(Value::Obj(list))) if list.kind() == rayscript_vm::object::Kind::Array => {
                    let names: Vec<String> = list.array().items.iter().filter_map(|v| self.vm.string_of(*v).map(str::to_string)).collect();
                    self.modules.borrow_mut().insert(path, names);
                }
                _ => self.error("protocol", "`module` requires \"path\" and \"functions\"", ""),
            },
            "event" => {
                let (Some(node), Some(event)) = (self.int_field(msg, "node"), self.string_field(msg, "event")) else {
                    self.error("protocol", "`event` requires \"node\" and \"event\"", "");
                    return;
                };
                let payload = self.vm.get_member(msg, "payload").unwrap_or(Value::Nil);
                // An element that is gone, or no longer listening, is the usual end of a race: not an error.
                self.vm.view_dispatch(node as u32, &event, payload);
            }
            "resolve" | "reject" => {
                let Some(id) = self.int_field(msg, "id") else {
                    self.error("protocol", &format!("`{kind}` requires the call \"id\""), "");
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
                    self.error("protocol", &format!("No pending call with id {id}"), "");
                }
            }
            "tool" => match self.string_field(msg, "id") {
                Some(id) => {
                    let handler = self.tools.borrow().get(&id).copied();
                    match handler {
                        Some(h) => {
                            self.vm.view_spawn_handler(h, &[]);
                        }
                        None => self.error("view", &format!("No handler registered for tool `{id}`; register one with onTool('{id}', handler)"), ""),
                    }
                }
                None => self.error("protocol", "`tool` requires \"id\"", ""),
            },
            "hook" => {
                let Some(name) = self.string_field(msg, "name") else {
                    self.error("protocol", "`hook` requires \"name\"", "");
                    return;
                };
                let data = self.vm.get_member(msg, "data").unwrap_or(Value::Nil);
                let handlers = self.hooks.borrow().get(&name).cloned().unwrap_or_default();
                for h in handlers {
                    self.vm.view_spawn_handler(h, &[data]);
                }
            }
            "command" => {
                let Some(id) = self.string_field(msg, "id") else {
                    self.error("protocol", "`command` requires \"id\"", "");
                    return;
                };
                let args = self.vm.get_member(msg, "args").unwrap_or(Value::Nil);
                let handler = self.commands.borrow().get(&id).copied();
                match handler {
                    Some(h) => {
                        self.vm.view_spawn_handler(h, &[args]);
                    }
                    None => self.error("view", &format!("No command registered with id `{id}`; register one with registerCommand('{id}', handler)"), ""),
                }
            }
            "closed" => match self.int_field(msg, "root") {
                Some(root) => {
                    self.vm.view_unmount(root as u32);
                }
                None => self.error("protocol", "`closed` requires \"root\"", ""),
            },
            "debug" => self.debug(msg),
            "describe" => self.describe(),
            "shutdown" => self.shutdown = true,
            other => self.error("protocol", &format!("Unknown message type `{other}`"), ""),
        }
    }

    fn var_json(v: &rayscript_vm::DebugVar) -> String {
        let mut out = String::from("{\"name\":");
        quote(&v.name, &mut out);
        out.push_str(",\"value\":");
        quote(&v.value, &mut out);
        out.push_str(",\"type\":");
        quote(v.type_name, &mut out);
        out.push_str(&format!(",\"ref\":{}}}", v.reference));
        out
    }

    fn vars_json(vars: &[rayscript_vm::DebugVar]) -> String {
        vars.iter().map(Host::var_json).collect::<Vec<_>>().join(",")
    }

    /// Tells the editor a fiber has stopped, with where, once.
    fn tell_pause(&mut self) {
        let Some(reason) = self.vm.debug_paused() else {
            self.pause_told = false;
            return;
        };
        if self.pause_told {
            return;
        }
        self.pause_told = true;
        let frames: Vec<String> = self
            .vm
            .debug_frames()
            .iter()
            .map(|f| {
                let mut out = String::from("{\"function\":");
                quote(&f.function, &mut out);
                out.push_str(",\"file\":");
                quote(&f.file, &mut out);
                out.push_str(&format!(",\"line\":{},\"col\":{}}}", f.line, f.col));
                out
            })
            .collect();
        let reason = match reason {
            rayscript_vm::PauseReason::Breakpoint => "breakpoint",
            rayscript_vm::PauseReason::Step => "step",
            rayscript_vm::PauseReason::Request => "pause",
        };
        self.send(format!("{{\"t\":\"paused\",\"reason\":\"{reason}\",\"frames\":[{}]}}", frames.join(",")));
    }

    /// The debugging messages: breakpoints and pausing at any time, the rest while a fiber is stopped.
    fn debug(&mut self, msg: Value) {
        let Some(op) = self.string_field(msg, "op") else {
            self.error("protocol", "`debug` requires \"op\"", "");
            return;
        };
        let stopped = self.vm.debug_paused().is_some();
        match op.as_str() {
            "breakpoints" => {
                let (Some(file), Some(Value::Obj(list))) = (self.string_field(msg, "file"), self.vm.get_member(msg, "lines")) else {
                    self.error("protocol", "`debug` `breakpoints` requires \"file\" and \"lines\"", "");
                    return;
                };
                let lines: Vec<u32> = list
                    .array()
                    .items
                    .iter()
                    .filter_map(|v| match v {
                        Value::Int(n) if *n > 0 => Some(*n as u32),
                        _ => None,
                    })
                    .collect();
                let placed = self.vm.debug_set_breakpoints(&file, &lines);
                // A module that is not loaded yet takes the lines as they were asked when it is.
                let shown = if self.vm.debug_module_loaded(&file) { placed } else { lines };
                let mut line = String::from("{\"t\":\"breakpoints\",\"file\":");
                quote(&file, &mut line);
                line.push_str(&format!(",\"lines\":[{}]}}", shown.iter().map(|l| l.to_string()).collect::<Vec<_>>().join(",")));
                self.send(line);
            }
            "pause" => self.vm.debug_pause(),
            "continue" | "stepOver" | "stepIn" | "stepOut" => {
                if !stopped {
                    self.error("protocol", &format!("`debug` `{op}`: no fiber is stopped"), "");
                    return;
                }
                let mode = match op.as_str() {
                    "stepOver" => Some(rayscript_vm::StepMode::Over),
                    "stepIn" => Some(rayscript_vm::StepMode::In),
                    "stepOut" => Some(rayscript_vm::StepMode::Out),
                    _ => None,
                };
                self.vm.debug_resume(mode);
                self.pause_told = false;
                self.send("{\"t\":\"resumed\"}".to_string());
            }
            "variables" | "expand" | "evaluate" if !stopped => self.error("protocol", &format!("`debug` `{op}`: no fiber is stopped"), ""),
            "variables" => {
                let frame = self.int_field(msg, "frame").unwrap_or(0).max(0) as usize;
                let locals = self.vm.debug_locals(frame);
                let globals = self.vm.debug_globals(frame);
                self.send(format!("{{\"t\":\"variables\",\"frame\":{frame},\"locals\":[{}],\"globals\":[{}]}}", Host::vars_json(&locals), Host::vars_json(&globals)));
            }
            "expand" => {
                let reference = self.int_field(msg, "ref").unwrap_or(0).max(0) as u32;
                let items = self.vm.debug_expand(reference);
                self.send(format!("{{\"t\":\"children\",\"ref\":{reference},\"items\":[{}]}}", Host::vars_json(&items)));
            }
            "evaluate" => {
                let frame = self.int_field(msg, "frame").unwrap_or(0).max(0) as usize;
                let expr = self.string_field(msg, "expr").unwrap_or_default();
                let mut line = String::from("{\"t\":\"evaluated\",\"expr\":");
                quote(&expr, &mut line);
                match self.vm.debug_evaluate(frame, &expr) {
                    Ok(v) => line.push_str(&format!(",\"ok\":true,\"result\":{}}}", Host::var_json(&v))),
                    Err(message) => {
                        line.push_str(",\"ok\":false,\"message\":");
                        quote(&message, &mut line);
                        line.push('}');
                    }
                }
                self.send(line);
            }
            other => self.error("protocol", &format!("Unknown `debug` op `{other}`"), ""),
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
        let list = |names: Vec<String>| {
            let mut sorted = names;
            sorted.sort();
            let mut out = String::new();
            for (i, n) in sorted.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                quote(n, &mut out);
            }
            out
        };
        let hooks = list(self.hooks.borrow().keys().cloned().collect());
        let commands = list(self.commands.borrow().keys().cloned().collect());
        let line = format!(
            "{{\"t\":\"describe\",\"protocol\":{PROTOCOL},\"mounts\":[{mounts}],\"nodes\":{},\"fibers\":{},\"pendingCalls\":{},\"timers\":{},\"opsSent\":{},\"remoteModules\":[{names}],\"hooks\":[{hooks}],\"commands\":[{commands}],\"heap\":{{\"objects\":{},\"bytes\":{},\"collections\":{}}}}}",
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
                let mut line = String::from("{\"t\":\"started\",\"name\":");
                quote(name, &mut line);
                line.push('}');
                self.send(line);
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
        self.vm.set_view_deadline(Some(deadline));
        self.fire_timers();
        let mut more;
        loop {
            more = self.vm.view_step(FUEL);
            if !more || Instant::now() >= deadline {
                break;
            }
        }
        self.report_failures();
        self.tell_pause();
        for (failure, text) in self.vm.take_detached_failures() {
            let mut items = Vec::new();
            for t in &failure.trace {
                let (file, line, col) = self.vm.locate(t.module, t.offset);
                items.push(self.item(&failure.message, &file, Some((line, col, line, col)), None, Some(&t.function)));
            }
            self.error("runtime", &text, &items.join(","));
        }
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
