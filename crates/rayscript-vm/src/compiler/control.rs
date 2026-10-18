//! Control flow: branches, loops, comprehensions, `try`, and `switch` with its patterns.

use super::Compiler;
use crate::bytecode::{Op, Reg};
use rayscript_syntax::ast::*;
use rayscript_syntax::span::Span;

/// The names a `catch` can select by, among the types the runtime knows.
const CATCHABLE: &[&str] = &["String", "Int", "Float", "Bool", "Array", "Map", "Object"];

fn strip(e: &Expr) -> &Expr {
    match &e.kind {
        ExprKind::Paren(i) => strip(i),
        _ => e,
    }
}

impl Compiler {
    fn build_for_statement_value(&mut self, dst: Reg) -> Reg {
        // A branch writes `dst` on each path; a variable it reads must not be overwritten early.
        let is_local = self.f().scopes.iter().any(|s| s.locals.iter().any(|(_, l)| l.reg == dst));
        if is_local { self.alloc() } else { dst }
    }

    pub(crate) fn if_expr(&mut self, cond: &Expr, then: &Expr, els: Option<&Expr>, dst: Reg) {
        let t = self.build_for_statement_value(dst);
        let c = self.expr_any(cond);
        let to_else = self.emit(Op::JumpIfNot { cond: c, offset: 0 });
        self.expr_to(then, t);
        let end = self.emit(Op::Jump { offset: 0 });
        self.patch(to_else);
        match els {
            Some(e) => self.expr_to(e, t),
            None => {
                self.emit(Op::LoadNil { dst: t });
            }
        }
        self.patch(end);
        if t != dst {
            self.emit(Op::Move { dst, src: t });
        }
    }

    // Loops

    fn begin_loop(&mut self) {
        let tries = self.f().tries;
        self.f().loops.push(super::LoopCtx { breaks: Vec::new(), continues: Vec::new(), tries });
    }

    /// Ends the loop being compiled: breaks go to here, continues to `continue_at`.
    fn end_loop(&mut self, continue_at: usize) {
        let ctx = self.f().loops.pop().unwrap();
        for b in ctx.breaks {
            self.patch(b);
        }
        for c in ctx.continues {
            self.patch_to(c, continue_at);
        }
    }

    pub(crate) fn break_or_continue(&mut self, is_break: bool, span: Span, dst: Reg) {
        self.at(span);
        let Some(loop_tries) = self.f().loops.last().map(|l| l.tries) else {
            self.error(span, if is_break { "`break` is only for inside a loop" } else { "`continue` is only for inside a loop" });
            return;
        };
        let open = self.f().tries - loop_tries;
        for _ in 0..open {
            self.emit(Op::TryEnd);
        }
        let j = self.emit(Op::Jump { offset: 0 });
        let ctx = self.f().loops.last_mut().unwrap();
        if is_break { ctx.breaks.push(j) } else { ctx.continues.push(j) }
        self.emit(Op::LoadNil { dst });
    }

    fn discard(&mut self, e: &Expr) {
        let m = self.mark();
        let t = self.alloc();
        self.expr_to(e, t);
        self.free_to(m);
    }

    pub(crate) fn while_expr(&mut self, cond: &Expr, body: &Expr, dst: Reg) {
        let top = self.here();
        self.begin_loop();
        let m = self.mark();
        let c = self.expr_any(cond);
        let exit = self.emit(Op::JumpIfNot { cond: c, offset: 0 });
        self.free_to(m);
        self.discard(body);
        self.jump_back(top);
        self.patch(exit);
        self.end_loop(top);
        self.emit(Op::LoadNil { dst });
    }

    pub(crate) fn do_while(&mut self, body: &Expr, cond: &Expr, dst: Reg) {
        let top = self.here();
        self.begin_loop();
        self.discard(body);
        let cont = self.here();
        let m = self.mark();
        let c = self.expr_any(cond);
        let back = self.emit(Op::JumpIf { cond: c, offset: 0 });
        self.patch_to(back, top);
        self.free_to(m);
        self.end_loop(cont);
        self.emit(Op::LoadNil { dst });
    }

    pub(crate) fn for_expr(&mut self, key: Option<&Ident>, var: &Ident, iter: &Expr, body: &Expr, dst: Reg) {
        self.for_loop(key, var, iter, &mut |c| c.discard(body));
        self.emit(Op::LoadNil { dst });
    }

    /// A `for` loop whose body is emitted by `body`: `a...b` counts without making a range, anything
    /// else is walked with an iterator.
    fn for_loop(&mut self, key: Option<&Ident>, var: &Ident, iter: &Expr, body: &mut dyn FnMut(&mut Compiler)) {
        self.push_scope();
        let base = self.alloc_n(2);
        let range = match &strip(iter).kind {
            ExprKind::Binary { op: BinOp::Interval, left, right } => Some((left, right)),
            _ => None,
        };
        let (top, exit) = if let Some((l, r)) = range {
            if let Some(k) = key {
                self.error(k.span, "a range `a...b` has no keys; use `for (x in a...b)`");
            }
            self.expr_to(l, base);
            self.expr_to(r, base + 1);
            let value = self.alloc();
            let top = self.here();
            self.at(var.span);
            let exit = self.emit(Op::ForRange { base, var: value, offset: 0 });
            self.bind_loop_var(var, value);
            (top, exit)
        } else {
            self.expr_to(iter, base);
            self.at(iter.span);
            self.emit(Op::IterInit { base, src: base });
            let key_reg = self.alloc();
            let value = self.alloc();
            let top = self.here();
            self.at(var.span);
            let exit = self.emit(Op::IterNext { base, key: key_reg, value, offset: 0 });
            if let Some(k) = key {
                self.bind_loop_var(k, key_reg);
            }
            self.bind_loop_var(var, value);
            (top, exit)
        };
        self.begin_loop();
        body(self);
        self.jump_back(top);
        self.patch(exit);
        self.end_loop(top);
        self.pop_scope();
    }

    /// The loop variable is a fresh variable each time round: in a cell when a closure captures it.
    fn bind_loop_var(&mut self, id: &Ident, reg: Reg) {
        let local = self.declare(&id.name, reg, id.span.start);
        if local.cell {
            self.emit(Op::NewCell { dst: reg, src: reg });
        }
    }

    // Comprehensions

    pub(crate) fn comprehension(&mut self, map: bool, body: &Expr, dst: Reg) {
        let is_local = self.f().scopes.iter().any(|s| s.locals.iter().any(|(_, l)| l.reg == dst));
        let acc = if is_local { self.alloc() } else { dst };
        if map {
            self.emit(Op::NewMap { dst: acc });
        } else {
            self.emit(Op::NewArray { dst: acc, capacity: 0 });
        }
        self.comp_body(body, acc, map);
        if acc != dst {
            self.emit(Op::Move { dst, src: acc });
        }
    }

    fn comp_body(&mut self, e: &Expr, acc: Reg, map: bool) {
        self.at(e.span);
        match &e.kind {
            ExprKind::For { key, var, iter, body } => {
                self.for_loop(key.as_ref(), var, iter, &mut |c| c.comp_body(body, acc, map));
            }
            ExprKind::While { cond, body } => {
                let top = self.here();
                self.begin_loop();
                let m = self.mark();
                let c = self.expr_any(cond);
                let exit = self.emit(Op::JumpIfNot { cond: c, offset: 0 });
                self.free_to(m);
                self.comp_body(body, acc, map);
                self.jump_back(top);
                self.patch(exit);
                self.end_loop(top);
            }
            ExprKind::If { cond, then, els } => {
                let m = self.mark();
                let c = self.expr_any(cond);
                let to_else = self.emit(Op::JumpIfNot { cond: c, offset: 0 });
                self.free_to(m);
                self.comp_body(then, acc, map);
                match els {
                    Some(e) => {
                        let end = self.emit(Op::Jump { offset: 0 });
                        self.patch(to_else);
                        self.comp_body(e, acc, map);
                        self.patch(end);
                    }
                    None => self.patch(to_else),
                }
            }
            ExprKind::Map(pairs) if map && pairs.len() == 1 => {
                let m = self.mark();
                let k = self.expr_any(&pairs[0].0);
                let v = self.expr_any(&pairs[0].1);
                self.emit(Op::SetIndex { obj: acc, index: k, src: v });
                self.free_to(m);
            }
            _ => {
                let m = self.mark();
                let r = self.expr_any(e);
                self.emit(Op::ArrayPush { array: acc, src: r });
                self.free_to(m);
            }
        }
    }

    // try

    pub(crate) fn try_expr(&mut self, body: &Expr, catches: &[Catch], dst: Reg) {
        let t = self.build_for_statement_value(dst);
        let err = self.alloc();
        let begin = self.emit(Op::TryBegin { dst: err, catch_offset: 0 });
        self.f().tries += 1;
        self.expr_to(body, t);
        self.f().tries -= 1;
        self.emit(Op::TryEnd);
        let mut ends = vec![self.emit(Op::Jump { offset: 0 })];
        self.patch(begin);
        for c in catches {
            let mut next = None;
            if let Some(ty) = &c.ty {
                if let TypeKind::Path { path, .. } = &ty.kind {
                    let name = path.last().map(|p| p.name.as_str()).unwrap_or("");
                    if CATCHABLE.contains(&name) {
                        let m = self.mark();
                        let x = self.alloc();
                        let k = self.const_str(name);
                        self.at(ty.span);
                        self.emit(Op::IsType { dst: x, src: err, name: k });
                        next = Some(self.emit(Op::JumpIfNot { cond: x, offset: 0 }));
                        self.free_to(m);
                    }
                }
            }
            self.push_scope();
            let reg = self.alloc();
            let local = self.declare(&c.name.name, reg, c.name.span.start);
            if local.cell {
                self.emit(Op::NewCell { dst: reg, src: err });
            } else {
                self.emit(Op::Move { dst: reg, src: err });
            }
            self.expr_to(&c.body, t);
            self.pop_scope();
            ends.push(self.emit(Op::Jump { offset: 0 }));
            if let Some(n) = next {
                self.patch(n);
            }
        }
        // No catch took it: it goes on to the next handler out.
        self.emit(Op::Rethrow { src: err });
        for e in ends {
            self.patch(e);
        }
        if t != dst {
            self.emit(Op::Move { dst, src: t });
        }
    }

    // switch

    pub(crate) fn switch_expr(&mut self, subject: &Expr, cases: &[Case], default: Option<&Expr>, dst: Reg) {
        let t = self.build_for_statement_value(dst);
        let s = self.alloc();
        self.expr_to(subject, s);
        let mut ends = Vec::new();
        for case in cases {
            self.push_scope();
            // The names a case binds get their registers first, so the tests can use temporaries freely.
            let mut names: Vec<(String, Vec<u32>)> = Vec::new();
            for p in &case.patterns {
                collect_binds(p, &mut names);
            }
            for (name, keys) in &names {
                let reg = self.alloc();
                let cell = keys.iter().any(|k| self.is_captured(*k));
                self.declare_as(name, reg, cell);
            }
            let mut case_fails: Vec<usize> = Vec::new();
            let mut to_body: Vec<usize> = Vec::new();
            for (i, p) in case.patterns.iter().enumerate() {
                let fails = self.pattern_test(s, p);
                if i + 1 < case.patterns.len() {
                    to_body.push(self.emit(Op::Jump { offset: 0 }));
                    for f in fails {
                        self.patch(f);
                    }
                } else {
                    case_fails.extend(fails);
                }
            }
            for j in to_body {
                self.patch(j);
            }
            if let Some(g) = &case.guard {
                let m = self.mark();
                let gr = self.expr_any(g);
                case_fails.push(self.emit(Op::JumpIfNot { cond: gr, offset: 0 }));
                self.free_to(m);
            }
            self.expr_to(&case.body, t);
            ends.push(self.emit(Op::Jump { offset: 0 }));
            for f in case_fails {
                self.patch(f);
            }
            self.pop_scope();
        }
        match default {
            Some(d) => self.expr_to(d, t),
            None => {
                self.emit(Op::LoadNil { dst: t });
            }
        }
        for e in ends {
            self.patch(e);
        }
        if t != dst {
            self.emit(Op::Move { dst, src: t });
        }
    }

    fn bind_value(&mut self, name: &str, src: Reg) {
        match self.local_of(name) {
            Some(l) if l.cell => {
                self.emit(Op::NewCell { dst: l.reg, src });
            }
            Some(l) => {
                self.emit(Op::Move { dst: l.reg, src });
            }
            None => {}
        }
    }

    /// Code that falls through when `reg` matches the pattern, and otherwise jumps; returns those jumps.
    fn pattern_test(&mut self, reg: Reg, p: &Pattern) -> Vec<usize> {
        self.at(p.span);
        let m = self.mark();
        let mut fails = Vec::new();
        match &p.kind {
            PatternKind::Wildcard => {}
            PatternKind::Bind(name) => self.bind_value(name, reg),
            PatternKind::Lit(e) => {
                let t = self.alloc();
                self.expr_to(e, t);
                let x = self.alloc();
                self.emit(Op::Eq { dst: x, a: reg, b: t });
                fails.push(self.emit(Op::JumpIfNot { cond: x, offset: 0 }));
            }
            PatternKind::Ctor { path, args } => {
                if !args.is_empty() {
                    self.error(p.span, "a pattern with arguments is not supported yet");
                } else {
                    let t = self.alloc();
                    self.path_value(path, t);
                    let x = self.alloc();
                    self.emit(Op::Eq { dst: x, a: reg, b: t });
                    fails.push(self.emit(Op::JumpIfNot { cond: x, offset: 0 }));
                }
            }
            PatternKind::Array { items, rest } => {
                let x = self.alloc();
                self.emit(Op::TestArray { dst: x, src: reg, len: items.len() as u16, at_least: rest.is_some() });
                fails.push(self.emit(Op::JumpIfNot { cond: x, offset: 0 }));
                for (i, item) in items.iter().enumerate() {
                    let idx = self.alloc();
                    self.emit(Op::LoadInt { dst: idx, value: i as i32 });
                    let el = self.alloc();
                    self.emit(Op::GetIndex { dst: el, obj: reg, index: idx });
                    fails.extend(self.pattern_test(el, item));
                }
                if let Some(Some(id)) = rest {
                    let r = self.alloc();
                    self.emit(Op::Slice { dst: r, src: reg, from: items.len() as u16 });
                    self.bind_value(&id.name, r);
                }
            }
            PatternKind::Object(fields) => {
                let x = self.alloc();
                self.emit(Op::TestObject { dst: x, src: reg });
                fails.push(self.emit(Op::JumpIfNot { cond: x, offset: 0 }));
                for (name, sub) in fields {
                    let k = self.const_str(&name.name);
                    let has = self.alloc();
                    self.emit(Op::TestField { dst: has, src: reg, name: k });
                    fails.push(self.emit(Op::JumpIfNot { cond: has, offset: 0 }));
                    let el = self.alloc();
                    self.emit(Op::GetField { dst: el, obj: reg, name: k });
                    fails.extend(self.pattern_test(el, sub));
                }
            }
        }
        self.free_to(m);
        fails
    }
}

fn push_bind(out: &mut Vec<(String, Vec<u32>)>, name: &str, key: u32) {
    match out.iter_mut().find(|(n, _)| n == name) {
        Some((_, keys)) => keys.push(key),
        None => out.push((name.to_string(), vec![key])),
    }
}

/// The names a pattern binds, with the declaration keys the analysis recorded for each.
fn collect_binds(p: &Pattern, out: &mut Vec<(String, Vec<u32>)>) {
    match &p.kind {
        PatternKind::Bind(n) => push_bind(out, n, p.span.start),
        PatternKind::Array { items, rest } => {
            for i in items {
                collect_binds(i, out);
            }
            if let Some(Some(id)) = rest {
                push_bind(out, &id.name, id.span.start);
            }
        }
        PatternKind::Object(fields) => {
            for (_, sub) in fields {
                collect_binds(sub, out);
            }
        }
        PatternKind::Ctor { args, .. } => {
            for a in args {
                collect_binds(a, out);
            }
        }
        PatternKind::Wildcard | PatternKind::Lit(_) => {}
    }
}
