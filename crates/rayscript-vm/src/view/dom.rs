//! A host for tests and tools: applies [`ViewOp`]s to a plain tree and checks that each one makes
//! sense, so a runtime that sends the host something it cannot do is caught.

use super::{PropValue, ViewOp};
use std::collections::HashMap;
use std::fmt::{self, Write};

#[derive(Debug, PartialEq)]
pub struct DomError(pub String);

impl fmt::Display for DomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

struct DomNode {
    /// None for a text node.
    tag: Option<String>,
    text: String,
    props: Vec<(String, PropValue)>,
    listens: Vec<String>,
    children: Vec<u32>,
    parent: Option<u32>,
}

#[derive(Default)]
pub struct Dom {
    nodes: HashMap<u32, DomNode>,
}

impl Dom {
    pub fn new() -> Dom {
        Dom::default()
    }

    /// How many nodes exist, in the tree or not.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    fn node(&mut self, id: u32) -> Result<&mut DomNode, DomError> {
        self.nodes.get_mut(&id).ok_or_else(|| DomError(format!("no node {id}")))
    }

    fn create(&mut self, id: u32, node: DomNode) -> Result<(), DomError> {
        if self.nodes.insert(id, node).is_some() {
            return Err(DomError(format!("node {id} created twice")));
        }
        Ok(())
    }

    pub fn apply(&mut self, ops: &[ViewOp]) -> Result<(), DomError> {
        for op in ops {
            self.apply_one(op).map_err(|e| DomError(format!("{e} (at `{op}`)")))?;
        }
        Ok(())
    }

    fn apply_one(&mut self, op: &ViewOp) -> Result<(), DomError> {
        match op {
            ViewOp::Create { id, tag } => self.create(*id, DomNode { tag: Some(tag.clone()), text: String::new(), props: Vec::new(), listens: Vec::new(), children: Vec::new(), parent: None }),
            ViewOp::CreateText { id, text } => self.create(*id, DomNode { tag: None, text: text.clone(), props: Vec::new(), listens: Vec::new(), children: Vec::new(), parent: None }),
            ViewOp::SetText { id, text } => {
                let n = self.node(*id)?;
                if n.tag.is_some() {
                    return Err(DomError(format!("node {id} is an element, not text")));
                }
                n.text = text.clone();
                Ok(())
            }
            ViewOp::SetProp { id, name, value } => {
                let n = self.node(*id)?;
                match n.props.iter_mut().find(|(k, _)| k == name) {
                    Some(p) => p.1 = value.clone(),
                    None => n.props.push((name.clone(), value.clone())),
                }
                Ok(())
            }
            ViewOp::RemoveProp { id, name } => {
                let n = self.node(*id)?;
                match n.props.iter().position(|(k, _)| k == name) {
                    Some(i) => {
                        n.props.remove(i);
                        Ok(())
                    }
                    None => Err(DomError(format!("node {id} has no property {name}"))),
                }
            }
            ViewOp::Listen { id, event } => {
                let n = self.node(*id)?;
                if n.listens.contains(event) {
                    return Err(DomError(format!("node {id} already listens for {event}")));
                }
                n.listens.push(event.clone());
                Ok(())
            }
            ViewOp::Unlisten { id, event } => {
                let n = self.node(*id)?;
                match n.listens.iter().position(|e| e == event) {
                    Some(i) => {
                        n.listens.remove(i);
                        Ok(())
                    }
                    None => Err(DomError(format!("node {id} does not listen for {event}"))),
                }
            }
            ViewOp::Insert { parent, child, before } => {
                if self.node(*parent)?.tag.is_none() {
                    return Err(DomError(format!("node {parent} is text and has no children")));
                }
                self.node(*child)?;
                let mut up = Some(*parent);
                while let Some(u) = up {
                    if u == *child {
                        return Err(DomError(format!("node {child} would be inside itself")));
                    }
                    up = self.nodes[&u].parent;
                }
                if let Some(b) = before {
                    if self.node(*b)?.parent != Some(*parent) {
                        return Err(DomError(format!("node {b} is not a child of {parent}")));
                    }
                    if b == child {
                        return Err(DomError(format!("node {child} cannot go before itself")));
                    }
                }
                self.detach(*child);
                let p = self.node(*parent)?;
                let at = match before {
                    Some(b) => p.children.iter().position(|c| c == b).expect("checked above"),
                    None => p.children.len(),
                };
                p.children.insert(at, *child);
                self.node(*child)?.parent = Some(*parent);
                Ok(())
            }
            ViewOp::Remove { id } => {
                self.node(*id)?;
                self.detach(*id);
                let mut stack = vec![*id];
                while let Some(n) = stack.pop() {
                    if let Some(node) = self.nodes.remove(&n) {
                        stack.extend(node.children);
                    }
                }
                Ok(())
            }
        }
    }

    fn detach(&mut self, id: u32) {
        let Some(parent) = self.nodes.get(&id).and_then(|n| n.parent) else { return };
        if let Some(p) = self.nodes.get_mut(&parent) {
            p.children.retain(|c| *c != id);
        }
        if let Some(n) = self.nodes.get_mut(&id) {
            n.parent = None;
        }
    }

    /// The node as markup: `<div class="a">text<b/></div>`.
    pub fn html(&self, id: u32) -> String {
        let mut out = String::new();
        self.write_html(id, &mut out);
        out
    }

    /// The node as indented markup, one element or run of text to a line.
    pub fn pretty(&self, id: u32) -> String {
        let mut out = String::new();
        self.write_pretty(id, 0, &mut out);
        out
    }

    fn write_pretty(&self, id: u32, depth: usize, out: &mut String) {
        let Some(n) = self.nodes.get(&id) else { return };
        let pad = "  ".repeat(depth);
        if n.tag.is_none() {
            let _ = writeln!(out, "{pad}{}", n.text);
            return;
        }
        let whole = self.html(id);
        let only_text = n.children.iter().all(|c| self.nodes.get(c).is_some_and(|c| c.tag.is_none()));
        if only_text {
            let _ = writeln!(out, "{pad}{whole}");
            return;
        }
        let open = whole.split('>').next().unwrap_or("");
        let tag = n.tag.as_deref().unwrap_or("");
        let _ = writeln!(out, "{pad}{open}>");
        for c in &n.children {
            self.write_pretty(*c, depth + 1, out);
        }
        let _ = writeln!(out, "{pad}</{tag}>");
    }

    /// What is inside the node, as markup.
    pub fn inner(&self, id: u32) -> String {
        let mut out = String::new();
        if let Some(n) = self.nodes.get(&id) {
            for c in &n.children {
                self.write_html(*c, &mut out);
            }
        }
        out
    }

    fn write_html(&self, id: u32, out: &mut String) {
        let Some(n) = self.nodes.get(&id) else { return };
        let Some(tag) = &n.tag else {
            out.push_str(&n.text);
            return;
        };
        let _ = write!(out, "<{tag}");
        for (k, v) in &n.props {
            write_prop(out, k, v);
        }
        if n.children.is_empty() {
            out.push_str("/>");
            return;
        }
        out.push('>');
        for c in &n.children {
            self.write_html(*c, out);
        }
        let _ = write!(out, "</{tag}>");
    }

    /// The elements under `root` with this tag, in document order.
    pub fn find(&self, root: u32, tag: &str) -> Vec<u32> {
        let mut out = Vec::new();
        self.collect(root, tag, &mut out);
        out
    }

    fn collect(&self, id: u32, tag: &str, out: &mut Vec<u32>) {
        let Some(n) = self.nodes.get(&id) else { return };
        if n.tag.as_deref() == Some(tag) {
            out.push(id);
        }
        for c in &n.children {
            self.collect(*c, tag, out);
        }
    }

    /// The events an element has asked to be sent.
    pub fn listens(&self, id: u32) -> Vec<String> {
        self.nodes.get(&id).map(|n| n.listens.clone()).unwrap_or_default()
    }

    pub fn prop(&self, id: u32, name: &str) -> Option<&PropValue> {
        self.nodes.get(&id)?.props.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    /// The nodes in the tree under `root`, itself included.
    pub fn attached(&self, root: u32) -> usize {
        let Some(n) = self.nodes.get(&root) else { return 0 };
        1 + n.children.iter().map(|c| self.attached(*c)).sum::<usize>()
    }
}

/// ` name="text"`, ` flag`, or ` name={value}`.
pub(super) fn write_prop(out: &mut String, name: &str, v: &PropValue) {
    match v {
        PropValue::Str(s) => {
            let _ = write!(out, " {name}=\"{s}\"");
        }
        PropValue::Bool(true) => {
            let _ = write!(out, " {name}");
        }
        other => {
            let _ = write!(out, " {name}={{{}}}", show_prop(other));
        }
    }
}

fn show_prop(v: &PropValue) -> String {
    match v {
        PropValue::Nil => "null".into(),
        PropValue::Bool(b) => b.to_string(),
        PropValue::Int(i) => i.to_string(),
        PropValue::Float(f) => f.to_string(),
        PropValue::Str(s) => format!("{s:?}"),
        PropValue::List(items) => format!("[{}]", items.iter().map(show_prop).collect::<Vec<_>>().join(",")),
        PropValue::Map(fields) => format!("{{{}}}", fields.iter().map(|(k, v)| format!("{k}:{}", show_prop(v))).collect::<Vec<_>>().join(",")),
    }
}
