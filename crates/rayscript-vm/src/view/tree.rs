//! Mounting templates: building the nodes, giving what can change an effect, and keeping the host's
//! tree in step with the runtime's by emitting operations.
//!
//! The runtime's tree has two kinds of node besides elements and text: a group, which has no element of
//! its own and puts its children among its siblings. A hole, a branch and a list each keep their
//! content in a group, so that replacing the content never needs a marker node in the host.

use super::reactive::{Item, RState, Role};
use super::template::Tpl;
use super::{Node, NodeKind, PropValue, ViewOp, Vm};
use crate::object::{Kind, MapKey};
use crate::vm::ops;
use crate::value::{Obj, Value};
use std::collections::{HashMap, VecDeque};

const MAX_PROP_DEPTH: usize = 8;

/// `onClick` is the event `click`; the name keeps its capitals after the first letter.
fn event_of(name: &str) -> Option<String> {
    let rest = name.strip_prefix("on")?;
    let mut chars = rest.chars();
    let first = chars.next().filter(|c| c.is_uppercase())?;
    Some(first.to_lowercase().chain(chars).collect())
}

fn is_callable(v: Value) -> bool {
    matches!(v, Value::Native(_)) || matches!(v, Value::Obj(o) if o.kind() == Kind::Closure)
}

impl Vm {
    // Nodes

    fn new_node(&mut self, kind: NodeKind) -> u32 {
        self.view.next_node += 1;
        let id = self.view.next_node;
        self.view.nodes.insert(id, Node { parent: 0, children: Vec::new(), kind });
        id
    }

    pub(crate) fn new_element(&mut self, tag: &str) -> u32 {
        let id = self.new_node(NodeKind::Element { tag: tag.to_string(), props: Vec::new(), listeners: Vec::new() });
        self.view.ops.push(ViewOp::Create { id, tag: tag.to_string() });
        id
    }

    fn new_text(&mut self, text: String) -> u32 {
        let id = self.new_node(NodeKind::Text { text: text.clone() });
        self.view.ops.push(ViewOp::CreateText { id, text });
        id
    }

    fn new_group(&mut self) -> u32 {
        self.new_node(NodeKind::Group)
    }

    /// The element whose children include the children of `n`: `n` itself, or the first element above it.
    fn real_parent(&self, mut n: u32) -> Option<u32> {
        while n != 0 {
            let node = self.view.nodes.get(&n)?;
            if matches!(node.kind, NodeKind::Element { .. }) {
                return Some(n);
            }
            n = node.parent;
        }
        None
    }

    /// The first element or text node at or inside `n`.
    fn first_real(&self, n: u32) -> Option<u32> {
        let node = self.view.nodes.get(&n)?;
        match node.kind {
            NodeKind::Group => node.children.iter().find_map(|c| self.first_real(*c)),
            _ => Some(n),
        }
    }

    /// The elements and text nodes that stand for `n` in the host's tree, in order.
    fn flatten_real(&self, n: u32, out: &mut Vec<u32>) {
        let Some(node) = self.view.nodes.get(&n) else { return };
        match node.kind {
            NodeKind::Group => {
                for c in &node.children {
                    self.flatten_real(*c, out);
                }
            }
            _ => out.push(n),
        }
    }

    /// The first host node from `parent`'s child number `from` on, looking past the end of a group into
    /// what follows it; None for the end of the element.
    fn next_real(&self, mut parent: u32, mut from: usize) -> Option<u32> {
        loop {
            let node = self.view.nodes.get(&parent)?;
            if let Some(found) = node.children.iter().skip(from).find_map(|c| self.first_real(*c)) {
                return Some(found);
            }
            if !matches!(node.kind, NodeKind::Group) || node.parent == 0 {
                return None;
            }
            let up = node.parent;
            from = self.view.nodes.get(&up)?.children.iter().position(|c| *c == parent)? + 1;
            parent = up;
        }
    }

    /// Puts `child` at `index` among the children of `parent`, and in the host's tree when `parent` is in it.
    fn attach_at(&mut self, parent: u32, index: usize, child: u32) {
        if let Some(p) = self.view.nodes.get_mut(&parent) {
            p.children.insert(index.min(p.children.len()), child);
        }
        if let Some(c) = self.view.nodes.get_mut(&child) {
            c.parent = parent;
        }
        let Some(real) = self.real_parent(parent) else { return };
        let before = self.next_real(parent, index + 1);
        let mut nodes = Vec::new();
        self.flatten_real(child, &mut nodes);
        for n in nodes {
            self.view.ops.push(ViewOp::Insert { parent: real, child: n, before });
        }
    }

    fn append(&mut self, parent: u32, child: u32) {
        let at = self.view.nodes.get(&parent).map_or(0, |p| p.children.len());
        self.attach_at(parent, at, child);
    }

    /// Takes a node out of the tree and forgets it and what is under it.
    pub(crate) fn remove_node(&mut self, id: u32) {
        let Some(node) = self.view.nodes.get(&id) else { return };
        let parent = node.parent;
        let mut hosted = Vec::new();
        self.flatten_real(id, &mut hosted);
        for n in hosted {
            self.view.ops.push(ViewOp::Remove { id: n });
        }
        if let Some(p) = self.view.nodes.get_mut(&parent) {
            p.children.retain(|c| *c != id);
        }
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            if let Some(node) = self.view.nodes.remove(&n) {
                stack.extend(node.children);
            }
        }
    }

    fn clear_children(&mut self, group: u32) {
        let children = self.view.nodes.get(&group).map(|g| g.children.clone()).unwrap_or_default();
        for c in children {
            self.remove_node(c);
        }
    }

    // Properties

    fn to_prop(&self, v: Value, depth: usize) -> PropValue {
        match v {
            Value::Nil | Value::Native(_) => PropValue::Nil,
            Value::Bool(b) => PropValue::Bool(b),
            Value::Int(i) => PropValue::Int(i),
            Value::Float(f) => PropValue::Float(f),
            Value::Obj(o) => match o.kind() {
                Kind::String => PropValue::Str(o.str().to_string()),
                Kind::Array if depth < MAX_PROP_DEPTH => PropValue::List(o.array().items.iter().map(|x| self.to_prop(*x, depth + 1)).collect()),
                Kind::Object if depth < MAX_PROP_DEPTH => PropValue::Map(o.object().fields.iter().map(|(s, x)| (self.syms.name(*s).to_string(), self.to_prop(*x, depth + 1))).collect()),
                Kind::Map if depth < MAX_PROP_DEPTH => PropValue::Map(o.map().map.iter().map(|(k, x)| (self.display(k), self.to_prop(x, depth + 1))).collect()),
                _ => PropValue::Nil,
            },
        }
    }

    /// Sets a property of an element, or its handler for an event, and tells the host if that changes anything.
    pub(crate) fn apply_prop(&mut self, id: u32, name: &str, v: Value) {
        if let Some(event) = event_of(name) {
            return self.set_listener(id, event, v);
        }
        let value = self.to_prop(v, 0);
        let Some(Node { kind: NodeKind::Element { props, .. }, .. }) = self.view.nodes.get_mut(&id) else { return };
        let at = props.iter().position(|(n, _)| &**n == name);
        match (at, value) {
            (None, PropValue::Nil) => {}
            (Some(i), PropValue::Nil) => {
                props.remove(i);
                self.view.ops.push(ViewOp::RemoveProp { id, name: name.to_string() });
            }
            (Some(i), value) => {
                if props[i].1 != value {
                    props[i].1 = value.clone();
                    self.view.ops.push(ViewOp::SetProp { id, name: name.to_string(), value });
                }
            }
            (None, value) => {
                props.push((name.into(), value.clone()));
                self.view.ops.push(ViewOp::SetProp { id, name: name.to_string(), value });
            }
        }
    }

    fn set_listener(&mut self, id: u32, event: String, handler: Value) {
        let Some(Node { kind: NodeKind::Element { listeners, .. }, .. }) = self.view.nodes.get_mut(&id) else { return };
        let at = listeners.iter().position(|(e, _)| **e == *event);
        match (at, is_callable(handler)) {
            (Some(i), true) => listeners[i].1 = handler,
            (None, true) => {
                listeners.push((event.as_str().into(), handler));
                self.view.ops.push(ViewOp::Listen { id, event });
            }
            (Some(i), false) => {
                listeners.remove(i);
                self.view.ops.push(ViewOp::Unlisten { id, event });
            }
            (None, false) => {}
        }
    }

    /// The names and values of an object's fields, or of a component's attributes (running the thunks of
    /// those written with expressions, so they are read by the computation that is running).
    fn fields_of(&mut self, v: Value) -> Result<Vec<(String, Value)>, Value> {
        let Value::Obj(o) = v else { return Ok(Vec::new()) };
        match o.kind() {
            Kind::Object => Ok(o.object().fields.iter().map(|(s, x)| (self.syms.name(*s).to_string(), *x)).collect()),
            Kind::Props => {
                let (statics, dynamics) = (o.props().statics, o.props().dynamics);
                let mut out = if statics.is_nil() { Vec::new() } else { self.fields_of(statics)? };
                if let Value::Obj(d) = dynamics {
                    let thunks: Vec<(String, Value)> = d.object().fields.iter().map(|(s, x)| (self.syms.name(*s).to_string(), *x)).collect();
                    for (name, thunk) in thunks {
                        let value = self.call(thunk, &[])?;
                        if name.starts_with("..") {
                            out.extend(self.fields_of(value)?);
                        } else {
                            out.push((name, value));
                        }
                    }
                }
                Ok(out)
            }
            _ => Ok(Vec::new()),
        }
    }

    // Mounting

    /// Mounts `v` as children of `parent`: an array mounts each of its items, nothing mounts nothing.
    pub(crate) fn realize_into(&mut self, parent: u32, v: Value) -> Result<(), Value> {
        if let Value::Obj(o) = v {
            if o.kind() == Kind::Array {
                for item in o.array().items.clone() {
                    self.realize_into(parent, item)?;
                }
                return Ok(());
            }
        }
        if let Some(n) = self.realize_one(v)? {
            self.append(parent, n);
        }
        Ok(())
    }

    pub(crate) fn realize_view(&mut self, root: u32, view: Value) -> Result<(), Value> {
        self.realize_into(root, view)
    }

    fn primitive_text(&self, v: Value) -> Option<String> {
        match v {
            Value::Int(_) | Value::Float(_) => Some(self.display(v)),
            Value::Obj(o) if o.kind() == Kind::String => Some(o.str().to_string()),
            _ => None,
        }
    }

    /// Builds the node for a value, not yet in the tree. None for a value that shows nothing.
    fn realize_one(&mut self, v: Value) -> Result<Option<u32>, Value> {
        if let Some(text) = self.primitive_text(v) {
            return Ok(Some(self.new_text(text)));
        }
        match v {
            Value::Nil | Value::Bool(_) => Ok(None),
            Value::Native(_) => self.component(v, Value::Nil, Value::Nil, Value::Nil),
            Value::Obj(o) => match o.kind() {
                Kind::Closure => self.component(v, Value::Nil, Value::Nil, Value::Nil),
                Kind::Array => {
                    let group = self.new_group();
                    if let Err(e) = self.realize_into(group, v) {
                        self.remove_node(group);
                        return Err(e);
                    }
                    Ok(Some(group))
                }
                Kind::Template => self.realize_template(o),
                _ => {
                    let text = self.display(v);
                    Ok(Some(self.new_text(text)))
                }
            },
            _ => Ok(None),
        }
    }

    fn realize_template(&mut self, o: Obj) -> Result<Option<u32>, Value> {
        match &o.template().tpl {
            Tpl::Element { tag, statics, dynamics, children } => {
                let (tag, statics, dynamics, children) = (*tag, *statics, *dynamics, *children);
                self.element(tag, statics, dynamics, children)
            }
            Tpl::Fragment { children } => {
                let children = *children;
                let group = self.new_group();
                if let Err(e) = self.realize_into(group, children) {
                    self.remove_node(group);
                    return Err(e);
                }
                Ok(Some(group))
            }
            Tpl::Hole { func } => {
                let func = *func;
                let group = self.new_group();
                self.region(group, Role::Hole { node: group, func }).map(Some)
            }
            Tpl::If { cond, then, els } => {
                let (cond, then, els) = (*cond, *then, *els);
                let group = self.new_group();
                let memo = self.new_reactive(Role::Cond { func: cond }, true)?;
                self.region(group, Role::If { node: group, cond: memo, then, els }).map(Some)
            }
            Tpl::For { iter, body } => {
                let (iter, body) = (*iter, *body);
                let group = self.new_group();
                self.region(group, Role::For { node: group, iter, body, items: Vec::new() }).map(Some)
            }
        }
    }

    /// Starts the effect that fills `group`, which runs once now.
    fn region(&mut self, group: u32, role: Role) -> Result<u32, Value> {
        let r = self.new_reactive(role, true)?;
        if let Err(e) = self.update_if_necessary(r) {
            self.dispose(r);
            self.remove_node(group);
            return Err(e);
        }
        Ok(group)
    }

    fn element(&mut self, tag: Value, statics: Value, dynamics: Value, children: Value) -> Result<Option<u32>, Value> {
        let name = match tag {
            Value::Obj(o) if o.kind() == Kind::String => o.str().to_string(),
            f if is_callable(f) => return self.component(f, statics, dynamics, children),
            other => {
                let m = format!("{} cannot be used as a tag", self.display(other));
                return Err(self.err(m));
            }
        };
        let id = self.new_element(&name);
        match self.fill_element(id, statics, dynamics, children) {
            Ok(()) => Ok(Some(id)),
            Err(e) => {
                self.remove_node(id);
                Err(e)
            }
        }
    }

    fn fill_element(&mut self, id: u32, statics: Value, dynamics: Value, children: Value) -> Result<(), Value> {
        for (name, value) in self.fields_of(statics)? {
            self.apply_prop(id, &name, value);
        }
        if let Value::Obj(d) = dynamics {
            let thunks: Vec<(String, Value)> = d.object().fields.iter().map(|(s, x)| (self.syms.name(*s).to_string(), *x)).collect();
            for (name, func) in thunks {
                let role = if name.starts_with("..") { Role::Spread { node: id, func, applied: Vec::new() } } else { Role::Prop { node: id, name: name.into(), func } };
                let r = self.new_reactive(role, true)?;
                self.update_if_necessary(r)?;
            }
        }
        self.realize_into(id, children)
    }

    /// Calls a component with its attributes in its own scope, and mounts what it gives back.
    fn component(&mut self, f: Value, statics: Value, dynamics: Value, children: Value) -> Result<Option<u32>, Value> {
        let scope = self.new_reactive(Role::Root, true)?;
        scope.reactive().state = RState::Clean;
        let props = self.new_props(statics, dynamics, children)?;
        let (owner, observer) = (self.view.owner.replace(scope), self.view.observer.take());
        let out = self.call(f, &[props]).and_then(|v| self.realize_one(v));
        self.view.owner = owner;
        self.view.observer = observer;
        out
    }

    // Updating

    fn fill_region(&mut self, group: u32, v: Value) -> Result<(), Value> {
        if let Some(text) = self.primitive_text(v) {
            let only = self.view.nodes.get(&group).filter(|g| g.children.len() == 1).map(|g| g.children[0]);
            if let Some(Node { kind: NodeKind::Text { text: old }, .. }) = only.and_then(|t| self.view.nodes.get_mut(&t)) {
                if *old != text {
                    *old = text.clone();
                    self.view.ops.push(ViewOp::SetText { id: only.unwrap(), text });
                }
                return Ok(());
            }
        }
        self.clear_children(group);
        self.realize_into(group, v)
    }

    /// What a list walks over: each item with its key, which is the position for a list and the key for a map.
    fn entries_of(&mut self, list: Value) -> Result<Vec<(Value, Value)>, Value> {
        match list {
            Value::Nil => Ok(Vec::new()),
            Value::Obj(o) => match o.kind() {
                Kind::Array => Ok(o.array().items.iter().enumerate().map(|(i, v)| (*v, Value::Int(i as i64))).collect()),
                Kind::Range => Ok((o.range().from..o.range().to).enumerate().map(|(i, v)| (Value::Int(v), Value::Int(i as i64))).collect()),
                Kind::Map => Ok(o.map().map.iter().map(|(k, v)| (v, k)).collect()),
                _ => {
                    let items = crate::stdlib::elements(self, list)?;
                    Ok(items.into_iter().enumerate().map(|(i, v)| (v, Value::Int(i as i64))).collect())
                }
            },
            other => {
                let m = format!("{} is not something to loop over", crate::vm::type_name(other));
                Err(self.err(m))
            }
        }
    }

    fn body_wants_key(&self, body: Value) -> bool {
        matches!(body, Value::Obj(c) if c.kind() == Kind::Closure && self.protos[c.closure().proto as usize].params >= 2)
    }

    fn make_item(&mut self, body: Value, item: Value, key: Value, wants_key: bool) -> Result<Item, Value> {
        let scope = self.new_reactive(Role::Root, false)?;
        scope.reactive().state = RState::Clean;
        let group = self.new_group();
        let (owner, observer) = (self.view.owner.replace(scope), self.view.observer.take());
        let args = [item, key];
        let out = self.call(body, &args[..if wants_key { 2 } else { 1 }]).and_then(|children| self.realize_into(group, children));
        self.view.owner = owner;
        self.view.observer = observer;
        match out {
            Ok(()) => Ok(Item { item, key, scope, group }),
            Err(e) => {
                self.dispose(scope);
                self.remove_node(group);
                Err(e)
            }
        }
    }

    /// Makes the items of a list match `list`: an item whose value is still there keeps its elements
    /// and effects and is moved if need be; the rest are made or removed.
    fn reconcile(&mut self, r: Obj, group: u32, body: Value, list: Value) -> Result<(), Value> {
        let entries = self.entries_of(list)?;
        let wants_key = self.body_wants_key(body);
        let old = match &mut r.reactive().role {
            Role::For { items, .. } => std::mem::take(items),
            _ => Vec::new(),
        };
        let same = |it: &Item, (item, key): &(Value, Value)| ops::equal(it.item, *item) && (!wants_key || ops::equal(it.key, *key));
        let (n_old, n_new) = (old.len(), entries.len());
        // What is the same at the start and at the end stays as it is; only the middle needs looking up.
        let mut start = 0;
        while start < n_old.min(n_new) && same(&old[start], &entries[start]) {
            start += 1;
        }
        let mut end = 0;
        while end < n_old.min(n_new) - start && same(&old[n_old - 1 - end], &entries[n_new - 1 - end]) {
            end += 1;
        }
        let mut matched: Vec<Option<usize>> = vec![None; n_new];
        for (i, slot) in matched.iter_mut().enumerate().take(start) {
            *slot = Some(i);
        }
        for j in 0..end {
            matched[n_new - 1 - j] = Some(n_old - 1 - j);
        }
        if start + end < n_new && start + end < n_old {
            let identity = |item: Value, key: Value| (MapKey::of(item), if wants_key { MapKey::of(key) } else { MapKey::Nil });
            let mut waiting: HashMap<(MapKey, MapKey), VecDeque<usize>> = HashMap::new();
            for (i, it) in old.iter().enumerate().take(n_old - end).skip(start) {
                waiting.entry(identity(it.item, it.key)).or_default().push_back(i);
            }
            for ni in start..n_new - end {
                let (item, key) = entries[ni];
                matched[ni] = waiting.get_mut(&identity(item, key)).and_then(|q| q.pop_front());
            }
        }
        let mut old: Vec<Option<Item>> = old.into_iter().map(Some).collect();
        let mut kept = vec![false; old.len()];
        for m in matched.iter().flatten() {
            kept[*m] = true;
        }
        for (i, slot) in old.iter_mut().enumerate() {
            if !kept[i] {
                if let Some(gone) = slot.take() {
                    self.dispose(gone.scope);
                    self.remove_node(gone.group);
                }
            }
        }
        let in_place = stable_positions(&matched);
        let mut items: Vec<Item> = Vec::with_capacity(entries.len());
        let mut failure = None;
        for (ni, (item, key)) in entries.iter().enumerate() {
            match matched[ni] {
                Some(oi) => items.push(old[oi].take().expect("an item is matched once")),
                None => match self.make_item(body, *item, *key, wants_key) {
                    Ok(it) => items.push(it),
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                },
            }
        }
        for rest in old.into_iter().flatten() {
            self.dispose(rest.scope);
            self.remove_node(rest.group);
        }
        let order: Vec<u32> = items.iter().map(|it| it.group).collect();
        if let Some(g) = self.view.nodes.get_mut(&group) {
            g.children = order.clone();
        }
        for id in &order {
            if let Some(n) = self.view.nodes.get_mut(id) {
                n.parent = group;
            }
        }
        if let Some(real) = self.real_parent(group) {
            for i in (0..order.len()).rev() {
                if matched[i].is_some() && in_place[i] {
                    continue;
                }
                let before = self.next_real(group, i + 1);
                let mut nodes = Vec::new();
                self.flatten_real(order[i], &mut nodes);
                for n in nodes {
                    self.view.ops.push(ViewOp::Insert { parent: real, child: n, before });
                }
            }
        }
        if let Role::For { items: slot, .. } = &mut r.reactive().role {
            *slot = items;
        }
        match failure {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// For each new position, whether its item can stay where it is: the matched items whose old positions
/// are in the longest increasing run need no move.
fn stable_positions(matched: &[Option<usize>]) -> Vec<bool> {
    let mut stable = vec![false; matched.len()];
    // tails[k] is the new position ending the best run of length k + 1; prev links the run back.
    let mut tails: Vec<usize> = Vec::new();
    let mut prev: Vec<Option<usize>> = vec![None; matched.len()];
    for (ni, m) in matched.iter().enumerate() {
        let Some(oi) = m else { continue };
        let at = tails.partition_point(|t| matched[*t].unwrap() < *oi);
        prev[ni] = if at > 0 { Some(tails[at - 1]) } else { None };
        if at == tails.len() {
            tails.push(ni);
        } else {
            tails[at] = ni;
        }
    }
    let mut cur = tails.last().copied();
    while let Some(ni) = cur {
        stable[ni] = true;
        cur = prev[ni];
    }
    stable
}

// The roles that mount things. Each runs with the computation that owns it as the observer, so what it
// calls to find out what to show is recorded; mounting what it finds is not.

pub(super) fn run_prop(vm: &mut Vm, r: Obj) -> Result<(), Value> {
    let Role::Prop { node, name, func } = &r.reactive().role else { unreachable!() };
    let (node, name, func) = (*node, name.clone(), *func);
    let v = vm.call(func, &[])?;
    let v = vm.unwrap_reactive(v)?;
    vm.untracked(|vm| vm.apply_prop(node, &name, v));
    Ok(())
}

pub(super) fn run_spread(vm: &mut Vm, r: Obj) -> Result<(), Value> {
    let Role::Spread { node, func, applied } = &r.reactive().role else { unreachable!() };
    let (node, func, applied) = (*node, *func, applied.clone());
    let v = vm.call(func, &[])?;
    let fields = vm.fields_of(v)?;
    vm.untracked(|vm| {
        for (name, value) in &fields {
            vm.apply_prop(node, name, *value);
        }
        for gone in &applied {
            if !fields.iter().any(|(n, _)| **n == **gone) {
                vm.apply_prop(node, gone, Value::Nil);
            }
        }
    });
    if let Role::Spread { applied, .. } = &mut r.reactive().role {
        *applied = fields.into_iter().map(|(n, _)| n.into()).collect();
    }
    Ok(())
}

pub(super) fn run_hole(vm: &mut Vm, r: Obj) -> Result<(), Value> {
    let Role::Hole { node, func } = &r.reactive().role else { unreachable!() };
    let (node, func) = (*node, *func);
    let v = vm.call(func, &[])?;
    let v = vm.unwrap_reactive(v)?;
    vm.untracked(|vm| vm.fill_region(node, v))
}

pub(super) fn run_if(vm: &mut Vm, r: Obj) -> Result<(), Value> {
    let Role::If { node, cond, then, els } = &r.reactive().role else { unreachable!() };
    let (node, cond, then, els) = (*node, *cond, *then, *els);
    let on = vm.reactive_get(cond, true)?.truthy();
    let branch = if on { then } else { els };
    vm.untracked(|vm| {
        vm.clear_children(node);
        if branch.is_nil() {
            return Ok(());
        }
        let children = vm.call(branch, &[])?;
        vm.realize_into(node, children)
    })
}

pub(super) fn run_for(vm: &mut Vm, r: Obj) -> Result<(), Value> {
    let Role::For { node, iter, body, .. } = &r.reactive().role else { unreachable!() };
    let (node, iter, body) = (*node, *iter, *body);
    let list = vm.call(iter, &[])?;
    let list = vm.unwrap_reactive(list)?;
    vm.untracked(|vm| vm.reconcile(r, node, body, list))
}
