//! Views: a reactive graph and a tree of elements that a script builds with markup, and the stream of
//! operations that tells a host what the tree looks like.
//!
//! Markup does not build elements. It builds templates (`__el`, `__hole`, `__if`, `__for`), plain data
//! that can be kept, passed around and used more than once. Mounting a template builds elements from it,
//! once, and gives each part that can change an effect of its own: a text hole, an attribute, a branch,
//! a list. When a signal changes, only the effects that read it run, and each patches one thing. The
//! patches are [`ViewOp`]s, taken with [`Vm::view_take_ops`]; the host applies them to its own elements.
//! Events come back with [`Vm::view_dispatch`].
//!
//! Effects run when the host calls [`Vm::view_flush`] (or [`Vm::view_pump`], which also runs fibers),
//! not when a signal is written, so a handler that sets three signals causes one round of updates.
//! A computed value is pulled when it is read, so it is never stale.

mod dom;
mod natives;
mod reactive;
mod template;
mod tree;

pub use dom::{Dom, DomError};
pub use reactive::{Item, RState, ReactiveObj, Role, SignalObj};
pub use template::{PropsObj, TemplateObj, Tpl};

use crate::value::{Obj, Value};
use crate::vm::Vm;
use std::collections::HashMap;
use std::fmt;

/// The value of an element's property as the host sees it.
#[derive(Clone, Debug, PartialEq)]
pub enum PropValue {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<PropValue>),
    Map(Vec<(String, PropValue)>),
}

/// One change to the host's element tree. Ids are numbers the runtime hands out and does not reuse.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewOp {
    /// A new element, not yet in the tree.
    Create { id: u32, tag: String },
    /// A new text node, not yet in the tree.
    CreateText { id: u32, text: String },
    SetText { id: u32, text: String },
    SetProp { id: u32, name: String, value: PropValue },
    RemoveProp { id: u32, name: String },
    /// The host starts sending the events called `event` from this element.
    Listen { id: u32, event: String },
    Unlisten { id: u32, event: String },
    /// Puts `child` last in `parent`, or before `before` when given. A child already in the tree moves.
    Insert { parent: u32, child: u32, before: Option<u32> },
    /// Takes the node and everything under it out of the tree and forgets them.
    Remove { id: u32 },
}

impl fmt::Display for ViewOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ViewOp::Create { id, tag } => write!(f, "create {id} <{tag}>"),
            ViewOp::CreateText { id, text } => write!(f, "create {id} text {text:?}"),
            ViewOp::SetText { id, text } => write!(f, "text {id} {text:?}"),
            ViewOp::SetProp { id, name, value } => write!(f, "set {id} {name} = {value:?}"),
            ViewOp::RemoveProp { id, name } => write!(f, "unset {id} {name}"),
            ViewOp::Listen { id, event } => write!(f, "listen {id} {event}"),
            ViewOp::Unlisten { id, event } => write!(f, "unlisten {id} {event}"),
            ViewOp::Insert { parent, child, before: Some(b) } => write!(f, "insert {child} in {parent} before {b}"),
            ViewOp::Insert { parent, child, before: None } => write!(f, "insert {child} in {parent}"),
            ViewOp::Remove { id } => write!(f, "remove {id}"),
        }
    }
}

pub(crate) enum NodeKind {
    Element { tag: String, props: Vec<(Box<str>, PropValue)>, listeners: Vec<(Box<str>, Value)> },
    Text { text: String },
    /// Elements that are not elements: a fragment, or the place a hole, a branch or a list keeps its
    /// content. The host never sees one; its children belong to the nearest element above it.
    Group,
}

pub(crate) struct Node {
    pub parent: u32,
    pub children: Vec<u32>,
    pub kind: NodeKind,
}

/// A view being shown: the element the host puts it in, and the scope that owns its effects.
pub(crate) struct Mount {
    pub root: u32,
    pub name: String,
    pub scope: Obj,
}

#[derive(Default)]
pub(crate) struct ViewState {
    pub ops: Vec<ViewOp>,
    pub nodes: HashMap<u32, Node>,
    pub next_node: u32,
    /// Effects marked dirty and not yet run.
    pub queue: Vec<Obj>,
    /// The computation running now, which reads are recorded for.
    pub observer: Option<Obj>,
    /// The scope that new computations and cleanups belong to.
    pub owner: Option<Obj>,
    pub seq: u64,
    pub depth: u32,
    pub errors: Vec<String>,
    pub mounts: Vec<Mount>,
    /// Fibers running event handlers, until they end.
    pub handlers: Vec<Obj>,
    /// When a flush must stop, leaving the rest of the effects queued.
    pub deadline: Option<std::time::Instant>,
}

impl ViewState {
    /// Calls `f` with every value the view keeps alive.
    pub(crate) fn trace(&self, f: &mut dyn FnMut(Value)) {
        for q in &self.queue {
            f(Value::Obj(*q));
        }
        for o in [self.observer, self.owner].into_iter().flatten() {
            f(Value::Obj(o));
        }
        for m in &self.mounts {
            f(Value::Obj(m.scope));
        }
        for h in &self.handlers {
            f(Value::Obj(*h));
        }
        for n in self.nodes.values() {
            if let NodeKind::Element { listeners, .. } = &n.kind {
                for (_, h) in listeners {
                    f(*h);
                }
            }
        }
    }
}

/// The most effects one flush runs before it gives up: effects that keep triggering each other.
const MAX_RUNS_PER_FLUSH: usize = 100_000;

impl Vm {
    /// The operations since the last call, in the order to apply them.
    pub fn view_take_ops(&mut self) -> Vec<ViewOp> {
        std::mem::take(&mut self.view.ops)
    }

    /// What went wrong in effects and handlers since the last call, as text with positions.
    pub fn view_take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.view.errors)
    }

    pub(crate) fn view_error(&mut self, thrown: Value) {
        let text = self.display(thrown);
        self.view.errors.push(text);
    }

    /// A flush that reaches this time stops and leaves the effects it has not run queued, to be run by
    /// the next one. None for no limit.
    pub fn set_view_deadline(&mut self, deadline: Option<std::time::Instant>) {
        self.view.deadline = deadline;
    }

    /// Runs the effects that signals have marked, and those they mark in turn, until none are left or
    /// the deadline has passed.
    pub fn view_flush(&mut self) {
        let mut runs = 0;
        while !self.view.queue.is_empty() {
            let mut batch = std::mem::take(&mut self.view.queue);
            // Older first: an effect made inside another is younger, and the older may dispose it.
            batch.sort_by_key(|o| o.reactive().seq);
            for (i, o) in batch.iter().enumerate() {
                if i % 32 == 31 && self.view.deadline.is_some_and(|d| std::time::Instant::now() >= d) {
                    // What is left stays marked and queued, ahead of what these runs queued.
                    let rest = batch[i..].to_vec();
                    self.view.queue.splice(0..0, rest);
                    return;
                }
                o.reactive().queued = false;
                if o.reactive().state == RState::Disposed {
                    continue;
                }
                runs += 1;
                if runs > MAX_RUNS_PER_FLUSH {
                    for rest in &batch[i..] {
                        rest.reactive().queued = false;
                    }
                    for rest in std::mem::take(&mut self.view.queue) {
                        rest.reactive().queued = false;
                    }
                    self.view.errors.push("effects keep triggering each other; the update was stopped".to_string());
                    return;
                }
                if let Err(e) = self.update_if_necessary(*o) {
                    self.view_error(e);
                }
            }
        }
    }

    /// Runs ready fibers and effects until nothing is left to do. `fuel` limits each fiber's turn.
    pub fn view_pump(&mut self, fuel: u64) {
        for _ in 0..10_000 {
            if !self.view_step(fuel) {
                break;
            }
        }
    }

    /// One round: every ready fiber gets a turn of at most `fuel` polls, then the effects they marked run.
    /// True if there is more to do, such as a fiber that gave up its turn.
    pub fn view_step(&mut self, fuel: u64) -> bool {
        // A fiber stopped by the debugger holds everything still: nothing runs, and nothing is flushed.
        if self.debug_paused().is_some() {
            return false;
        }
        self.tick(fuel);
        if self.debug_paused().is_some() {
            return false;
        }
        self.view_reap_handlers();
        self.view_flush();
        self.has_ready() || !self.view.queue.is_empty()
    }

    /// The views being shown: each root's id and the name it was mounted with.
    pub fn view_mounts(&self) -> Vec<(u32, String)> {
        self.view.mounts.iter().map(|m| (m.root, m.name.clone())).collect()
    }

    fn view_reap_handlers(&mut self) {
        let handlers = std::mem::take(&mut self.view.handlers);
        for h in handlers {
            let fiber = h.fiber();
            if fiber.fiber.is_finished() {
                if fiber.fiber.state == crate::fiber::FiberState::Failed {
                    let text = self.render_failure(Value::Obj(h));
                    self.view.errors.push(text);
                }
            } else {
                self.view.handlers.push(h);
            }
        }
    }

    /// Shows `view` (a template, or a component to call) in a new root element named `name`, and gives
    /// the root's id. The host puts the root where `name` says.
    pub fn view_mount(&mut self, view: Value, name: &str) -> Result<u32, Value> {
        let scope = self.new_reactive(Role::Root, false)?;
        let root = self.new_element("#root");
        self.view.ops.push(ViewOp::SetProp { id: root, name: "name".into(), value: PropValue::Str(name.to_string()) });
        let (owner, observer) = (self.view.owner.replace(scope), self.view.observer.take());
        let result = self.realize_view(root, view);
        self.view.owner = owner;
        self.view.observer = observer;
        match result {
            Ok(()) => {
                self.view.mounts.push(Mount { root, name: name.to_string(), scope });
                Ok(root)
            }
            Err(e) => {
                self.dispose(scope);
                self.remove_node(root);
                Err(e)
            }
        }
    }

    /// Takes a mounted view down: stops its effects and removes its elements.
    pub fn view_unmount(&mut self, root: u32) -> bool {
        let Some(at) = self.view.mounts.iter().position(|m| m.root == root) else { return false };
        let m = self.view.mounts.remove(at);
        self.dispose(m.scope);
        self.remove_node(m.root);
        true
    }

    /// Sends an event from the host to the handler of element `node`. The handler runs as a fiber; give
    /// it a turn with [`Vm::view_pump`]. False when nothing listens.
    pub fn view_dispatch(&mut self, node: u32, event: &str, payload: Value) -> bool {
        let handler = match self.view.nodes.get(&node).map(|n| &n.kind) {
            Some(NodeKind::Element { listeners, .. }) => listeners.iter().find(|(e, _)| &**e == event).map(|(_, h)| *h),
            _ => None,
        };
        let Some(handler) = handler else { return false };
        self.view_spawn_handler(handler, &[payload])
    }

    /// Runs `handler` as an event handler does: as a fiber, whose failure is reported with the view's errors.
    /// A host function is called here and now; its answer, if it is a promise, is nobody's to wait for.
    pub fn view_spawn_handler(&mut self, handler: Value, args: &[Value]) -> bool {
        if matches!(handler, Value::Native(_)) {
            if let Err(e) = self.call(handler, args) {
                self.view_error(e);
            }
            return true;
        }
        match self.spawn(handler, args) {
            Ok(fiber) => {
                if let Some(o) = fiber.as_obj() {
                    self.view.handlers.push(o);
                }
                true
            }
            Err(e) => {
                self.view_error(e);
                false
            }
        }
    }

    /// How many nodes the runtime holds: for tests that a removed view leaves nothing behind.
    pub fn view_node_count(&self) -> usize {
        self.view.nodes.len()
    }

    /// The runtime's own tree under `id` as markup, groups left out: what the host's tree should be.
    pub fn view_html(&self, id: u32) -> String {
        let mut out = String::new();
        self.write_html(id, &mut out);
        out
    }

    /// The contents of node `id`, as [`Dom::inner`] shows them.
    pub fn view_inner_html(&self, id: u32) -> String {
        let mut out = String::new();
        if let Some(n) = self.view.nodes.get(&id) {
            for c in &n.children {
                self.write_html(*c, &mut out);
            }
        }
        out
    }

    fn write_html(&self, id: u32, out: &mut String) {
        let Some(n) = self.view.nodes.get(&id) else { return };
        match &n.kind {
            NodeKind::Text { text } => out.push_str(text),
            NodeKind::Group => {
                for c in &n.children {
                    self.write_html(*c, out);
                }
            }
            NodeKind::Element { tag, props, .. } => {
                out.push('<');
                out.push_str(tag);
                for (k, v) in props {
                    dom::write_prop(out, k, v);
                }
                let mut inside = String::new();
                for c in &n.children {
                    self.write_html(*c, &mut inside);
                }
                if inside.is_empty() {
                    out.push_str("/>");
                    return;
                }
                out.push('>');
                out.push_str(&inside);
                out.push_str("</");
                out.push_str(tag);
                out.push('>');
            }
        }
    }
}

pub(crate) fn install(vm: &mut Vm) {
    natives::install(vm);
}
