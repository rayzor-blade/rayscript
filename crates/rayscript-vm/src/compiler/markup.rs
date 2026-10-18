//! Markup becomes calls to the runtime's view functions, with a thunk (a function of no arguments)
//! for everything that can change, so the reactive runtime can run it again when what it reads changes:
//!
//! - an element: `__el(tag, statics, dynamics, children)`, `tag` a string, or the value of a component's name;
//!   `statics` an object of the attributes that are constants and `dynamics` one of thunks; a spread is a
//!   thunk under a name that begins with `..`;
//! - a fragment: `__frag(children)`;
//! - `{expr}`: `__hole(thunk)`; text is a string;
//! - `<if>`: `__if(cond, then, else)`, each a thunk, the branches' thunks returning a children array;
//! - `<for>`: `__for(iter, body)`, the body a function of the item (and the key, when there is one) that returns a children array.

use super::Compiler;
use crate::bytecode::{Op, Reg};
use rayscript_syntax::ast::*;

impl Compiler {
    pub(crate) fn markup_value(&mut self, node: &MarkupNode, dst: Reg) {
        self.markup_child(node, dst);
    }

    /// Calls the runtime function `name` with `args` already in place above `base`.
    fn call_runtime(&mut self, base: Reg, argc: u16, dst: Reg) {
        self.emit(Op::Call { base, argc });
        if base != dst {
            self.emit(Op::Move { dst, src: base });
        }
    }

    fn runtime_fn(&mut self, name: &str, base: Reg) {
        let k = self.const_str(name);
        self.emit(Op::GetGlobal { dst: base, name: k });
    }

    /// A function of no arguments whose body `body` emits into a register that it returns.
    fn thunk(&mut self, name: &str, dst: Reg, body: &mut dyn FnMut(&mut Compiler, Reg)) {
        self.begin_function(name, 0, false);
        let t = self.alloc();
        body(self, t);
        self.emit(Op::Return { src: t });
        let proto = self.end_function();
        self.emit(Op::Closure { dst, proto });
    }

    fn expr_thunk(&mut self, e: &Expr, dst: Reg) {
        self.at(e.span);
        self.thunk("thunk", dst, &mut |c, t| c.expr_to(e, t));
    }

    fn children_thunk(&mut self, nodes: &[MarkupNode], dst: Reg) {
        self.thunk("children", dst, &mut |c, t| c.children_array(nodes, t));
    }

    fn children_array(&mut self, nodes: &[MarkupNode], dst: Reg) {
        let is_local = self.f().scopes.iter().any(|s| s.locals.iter().any(|(_, l)| l.reg == dst));
        let arr = if is_local { self.alloc() } else { dst };
        self.emit(Op::NewArray { dst: arr, capacity: nodes.len().min(u16::MAX as usize) as u16 });
        for n in nodes {
            let m = self.mark();
            let r = self.alloc();
            self.markup_child(n, r);
            self.emit(Op::ArrayPush { array: arr, src: r });
            self.free_to(m);
        }
        if arr != dst {
            self.emit(Op::Move { dst, src: arr });
        }
    }

    fn markup_child(&mut self, node: &MarkupNode, dst: Reg) {
        self.at(node.span());
        let m = self.mark();
        match node {
            MarkupNode::Text { text, .. } => {
                let index = self.const_str(text);
                self.emit(Op::LoadConst { dst, index });
            }
            MarkupNode::Hole { expr, .. } => {
                let base = self.alloc();
                self.runtime_fn("__hole", base);
                let a = self.alloc();
                self.expr_thunk(expr, a);
                self.call_runtime(base, 1, dst);
            }
            MarkupNode::Fragment { children, .. } => {
                let base = self.alloc();
                self.runtime_fn("__frag", base);
                let a = self.alloc();
                self.children_array(children, a);
                self.call_runtime(base, 1, dst);
            }
            MarkupNode::If { cond, then, els, .. } => {
                let base = self.alloc();
                self.runtime_fn("__if", base);
                let c = self.alloc();
                self.expr_thunk(cond, c);
                let t = self.alloc();
                self.children_thunk(then, t);
                let e = self.alloc();
                match els {
                    Some(els) => self.children_thunk(els, e),
                    None => {
                        self.emit(Op::LoadNil { dst: e });
                    }
                }
                self.call_runtime(base, 3, dst);
            }
            MarkupNode::For { key, var, iter, body, .. } => {
                let base = self.alloc();
                self.runtime_fn("__for", base);
                let i = self.alloc();
                self.expr_thunk(iter, i);
                let f = self.alloc();
                let mut names = vec![var];
                if let Some(k) = key {
                    names.push(k);
                }
                self.begin_function("loop body", names.len() as u16, false);
                for (n, id) in names.iter().enumerate() {
                    let local = self.declare(&id.name, n as Reg, id.span.start);
                    if local.cell {
                        self.emit(Op::NewCell { dst: n as Reg, src: n as Reg });
                    }
                }
                let t = self.alloc();
                self.children_array(body, t);
                self.emit(Op::Return { src: t });
                let proto = self.end_function();
                self.emit(Op::Closure { dst: f, proto });
                self.call_runtime(base, 2, dst);
            }
            MarkupNode::Element(e) => self.element(e, dst),
        }
        self.free_to(m);
    }

    fn element(&mut self, e: &Element, dst: Reg) {
        let base = self.alloc();
        self.runtime_fn("__el", base);
        // The tag: a string for an element, the component's value for a name that starts upper case.
        let tag = self.alloc();
        let first = e.tag.name.split('.').next().unwrap_or("");
        if first.chars().next().is_some_and(|c| c.is_uppercase()) {
            let path: Vec<Ident> = e.tag.name.split('.').map(|s| Ident { name: s.to_string(), span: e.tag.span }).collect();
            self.path_value(&path, tag);
        } else {
            let index = self.const_str(&e.tag.name);
            self.emit(Op::LoadConst { dst: tag, index });
        }
        let statics = self.alloc();
        self.emit(Op::NewObject { dst: statics });
        let dynamics = self.alloc();
        self.emit(Op::NewObject { dst: dynamics });
        let mut spreads = 0;
        for a in &e.attrs {
            let m = self.mark();
            match a {
                Attr::Named { name, value, .. } => {
                    let k = self.const_str(&name.name);
                    match value {
                        AttrValue::Flag => {
                            let t = self.alloc();
                            self.emit(Op::LoadBool { dst: t, value: true });
                            self.emit(Op::SetField { obj: statics, name: k, src: t });
                        }
                        AttrValue::Str(s) => {
                            let t = self.alloc();
                            let index = self.const_str(s);
                            self.emit(Op::LoadConst { dst: t, index });
                            self.emit(Op::SetField { obj: statics, name: k, src: t });
                        }
                        AttrValue::Interp(parts) => {
                            let t = self.alloc();
                            self.thunk("thunk", t, &mut |c, r| {
                                let base = c.alloc_n(parts.len() as u16);
                                for (i, p) in parts.iter().enumerate() {
                                    let reg = base + i as Reg;
                                    match p {
                                        InterpPart::Lit(s) => {
                                            let index = c.const_str(s);
                                            c.emit(Op::LoadConst { dst: reg, index });
                                        }
                                        InterpPart::Expr(x) => c.expr_to(x, reg),
                                    }
                                }
                                c.emit(Op::Interp { dst: r, base, count: parts.len() as u16 });
                            });
                            self.emit(Op::SetField { obj: dynamics, name: k, src: t });
                        }
                        AttrValue::Expr(x) => {
                            let t = self.alloc();
                            self.expr_thunk(x, t);
                            self.emit(Op::SetField { obj: dynamics, name: k, src: t });
                        }
                    }
                }
                Attr::Spread { expr, .. } => {
                    let t = self.alloc();
                    self.expr_thunk(expr, t);
                    let k = self.const_str(&format!("..{spreads}"));
                    spreads += 1;
                    self.emit(Op::SetField { obj: dynamics, name: k, src: t });
                }
            }
            self.free_to(m);
        }
        let children = self.alloc();
        self.children_array(&e.children, children);
        self.at(e.span);
        self.call_runtime(base, 4, dst);
    }
}
