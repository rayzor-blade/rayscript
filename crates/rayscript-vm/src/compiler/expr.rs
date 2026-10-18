//! Expressions and statements. The rule every function here keeps: it leaves the register
//! allocation as it found it, so a caller can lay out call arguments one after another.

use super::{Compiler, Local, Var};
use crate::bytecode::{Op, Reg};
use rayscript_syntax::ast::*;
use rayscript_syntax::span::Span;

/// A place that can be assigned to, with what it needs evaluated once.
enum Place {
    Local(Reg),
    Cell(Reg),
    Upval(u16),
    Global(u32),
    Field { obj: Reg, name: u32 },
    Index { obj: Reg, index: Reg },
    /// An error was reported already.
    Nowhere,
}

fn strip(e: &Expr) -> &Expr {
    match &e.kind {
        ExprKind::Paren(i) => strip(i),
        _ => e,
    }
}

impl Compiler {
    /// An expression whose value is not wanted; `var` and `function` declare in the current scope.
    pub(crate) fn statement(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Var(v) => self.var_decl(v, e.span),
            ExprKind::Function(f) if f.name.is_some() => self.function_decl(f, e.span),
            _ => {
                let m = self.mark();
                let t = self.alloc();
                self.expr_to(e, t);
                self.free_to(m);
            }
        }
    }

    fn var_decl(&mut self, v: &VarDecls, span: Span) {
        self.at(span);
        for d in &v.decls {
            if self.at_module_level() {
                let m = self.mark();
                let t = self.alloc();
                match &d.init {
                    Some(i) => self.expr_to(i, t),
                    None => {
                        self.emit(Op::LoadNil { dst: t });
                    }
                }
                self.at(d.span);
                let name = self.const_str(&d.name.name);
                self.emit(Op::DefGlobal { src: t, name });
                self.free_to(m);
                continue;
            }
            let reg = self.alloc();
            let cell = self.is_captured(d.name.span.start);
            if cell {
                let m = self.mark();
                let t = self.alloc();
                match &d.init {
                    Some(i) => self.expr_to(i, t),
                    None => {
                        self.emit(Op::LoadNil { dst: t });
                    }
                }
                self.emit(Op::NewCell { dst: reg, src: t });
                self.free_to(m);
            } else {
                match &d.init {
                    Some(i) => self.expr_to(i, reg),
                    None => {
                        self.emit(Op::LoadNil { dst: reg });
                    }
                }
            }
            self.declare(&d.name.name, reg, d.name.span.start);
        }
    }

    /// A named function as a statement: a local, or a global at the module's top.
    fn function_decl(&mut self, f: &FnDecl, span: Span) {
        self.at(span);
        let name = f.name.as_ref().unwrap();
        if self.at_module_level() {
            let m = self.mark();
            let t = self.alloc();
            self.function_value(f, t);
            let k = self.const_str(&name.name);
            self.emit(Op::DefGlobal { src: t, name: k });
            self.free_to(m);
            return;
        }
        let reg = self.alloc();
        let local = self.declare(&name.name, reg, name.span.start);
        let m = self.mark();
        if local.cell {
            let nil = self.alloc();
            self.emit(Op::LoadNil { dst: nil });
            self.emit(Op::NewCell { dst: reg, src: nil });
            let t = self.alloc();
            self.function_value(f, t);
            self.emit(Op::SetCell { cell: reg, src: t });
        } else {
            self.function_value(f, reg);
        }
        self.free_to(m);
    }

    // Functions

    pub(crate) fn function_value(&mut self, f: &FnDecl, dst: Reg) {
        let name = f.name.as_ref().map(|n| n.name.as_str()).unwrap_or("function");
        self.compile_function(name, &f.params, &f.body, f.span, dst);
    }

    pub(crate) fn lambda_value(&mut self, l: &Lambda, dst: Reg) {
        self.compile_function("lambda", &l.params, &l.body, l.span, dst);
    }

    /// Compiles a function and emits the closure that makes it into `dst`. The value of the body is returned.
    pub(crate) fn compile_function(&mut self, name: &str, params: &[Param], body: &Expr, span: Span, dst: Reg) {
        let has_rest = params.last().is_some_and(|p| p.rest);
        self.begin_function(name, params.len() as u16, has_rest);
        self.at(span);
        for (i, p) in params.iter().enumerate() {
            let reg = i as Reg;
            if let Some(default) = &p.default {
                // A missing argument, or null, takes the default.
                let skip = self.emit(Op::JumpIfNotNil { src: reg, offset: 0 });
                self.expr_to(default, reg);
                self.patch(skip);
            }
            let local = self.declare(&p.name.name, reg, p.name.span.start);
            if local.cell {
                self.emit(Op::NewCell { dst: reg, src: reg });
            }
        }
        let t = self.alloc();
        self.expr_to(body, t);
        self.emit(Op::Return { src: t });
        let proto = self.end_function();
        self.emit(Op::Closure { dst, proto });
    }

    // Names

    fn load_name(&mut self, name: &str, dst: Reg) {
        match self.resolve(name) {
            Var::Local(Local { reg, cell: false }) => {
                if reg != dst {
                    self.emit(Op::Move { dst, src: reg });
                }
            }
            Var::Local(Local { reg, cell: true }) => {
                self.emit(Op::GetCell { dst, cell: reg });
            }
            Var::Upval(index) => {
                self.emit(Op::GetUpval { dst, index });
            }
            Var::Global => {
                let k = self.const_str(name);
                self.emit(Op::GetGlobal { dst, name: k });
            }
        }
    }

    // Registers

    fn is_local_reg(&mut self, r: Reg) -> bool {
        self.f().scopes.iter().any(|s| s.locals.iter().any(|(_, l)| l.reg == r))
    }

    /// Where to build a value that is written before it is finished: `dst` unless that is a variable
    /// the expression may still read.
    fn build_target(&mut self, dst: Reg) -> Reg {
        if self.is_local_reg(dst) { self.alloc() } else { dst }
    }

    fn finish_target(&mut self, built: Reg, dst: Reg) {
        if built != dst {
            self.emit(Op::Move { dst, src: built });
        }
    }

    /// Whether the expression writes `dst` only with its last instruction, so `dst` may be a variable it reads.
    fn writes_last(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Null | ExprKind::Bool(_) | ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Ident(_) | ExprKind::Interp(_) => true,
            ExprKind::Paren(i) => self.writes_last(i),
            ExprKind::Field { safe, .. } => !safe,
            ExprKind::Index { .. } | ExprKind::Call { .. } | ExprKind::New { .. } | ExprKind::Unary { .. } | ExprKind::Lambda(_) | ExprKind::Function(_) | ExprKind::Await(_) => true,
            ExprKind::Binary { op, .. } => !matches!(op, BinOp::And | BinOp::Or | BinOp::Coalesce),
            _ => false,
        }
    }

    /// The value of `e` in some register: a variable's own, or a temporary.
    pub(crate) fn expr_any(&mut self, e: &Expr) -> Reg {
        match &e.kind {
            ExprKind::Paren(i) => return self.expr_any(i),
            ExprKind::Ident(n) => {
                if let Var::Local(Local { reg, cell: false }) = self.resolve(n) {
                    return reg;
                }
            }
            _ => {}
        }
        let t = self.alloc();
        self.expr_to(e, t);
        t
    }

    // Expressions

    pub(crate) fn expr_to(&mut self, e: &Expr, dst: Reg) {
        self.at(e.span);
        let m = self.mark();
        match &e.kind {
            ExprKind::Null => {
                self.emit(Op::LoadNil { dst });
            }
            ExprKind::Bool(value) => {
                self.emit(Op::LoadBool { dst, value: *value });
            }
            ExprKind::Int(v) => match i32::try_from(*v) {
                Ok(value) => {
                    self.emit(Op::LoadInt { dst, value });
                }
                Err(_) => {
                    let index = self.const_int(*v);
                    self.emit(Op::LoadConst { dst, index });
                }
            },
            ExprKind::Float(v) => {
                let index = self.const_float(*v);
                self.emit(Op::LoadConst { dst, index });
            }
            ExprKind::Str(s) => {
                let index = self.const_str(s);
                self.emit(Op::LoadConst { dst, index });
            }
            ExprKind::Interp(parts) => self.interp(parts, dst),
            ExprKind::Ident(n) => self.load_name(n, dst),
            ExprKind::Paren(i) => self.expr_to(i, dst),
            ExprKind::Array(items) => {
                let t = self.build_target(dst);
                self.emit(Op::NewArray { dst: t, capacity: items.len().min(u16::MAX as usize) as u16 });
                for item in items {
                    let m2 = self.mark();
                    let r = self.expr_any(item);
                    self.emit(Op::ArrayPush { array: t, src: r });
                    self.free_to(m2);
                }
                self.finish_target(t, dst);
            }
            ExprKind::Map(pairs) => {
                let t = self.build_target(dst);
                self.emit(Op::NewMap { dst: t });
                for (k, v) in pairs {
                    let m2 = self.mark();
                    let kr = self.expr_any(k);
                    let vr = self.expr_any(v);
                    self.emit(Op::SetIndex { obj: t, index: kr, src: vr });
                    self.free_to(m2);
                }
                self.finish_target(t, dst);
            }
            ExprKind::Object(fields) => {
                let t = self.build_target(dst);
                self.emit(Op::NewObject { dst: t });
                for f in fields {
                    let m2 = self.mark();
                    let vr = self.expr_any(&f.value);
                    let name = self.const_str(&f.name);
                    self.emit(Op::SetField { obj: t, name, src: vr });
                    self.free_to(m2);
                }
                self.finish_target(t, dst);
            }
            ExprKind::Comprehension { map, body } => self.comprehension(*map, body, dst),
            ExprKind::Block(stmts) => self.block(stmts, dst),
            ExprKind::Var(v) => {
                // The variable keeps its register for the rest of the scope.
                self.var_decl(v, e.span);
                self.emit(Op::LoadNil { dst });
                return;
            }
            ExprKind::Function(f) => {
                if f.name.is_some() {
                    self.function_decl(f, e.span);
                    self.emit(Op::LoadNil { dst });
                    return;
                }
                self.function_value(f, dst);
            }
            ExprKind::Lambda(l) => self.lambda_value(l, dst),
            ExprKind::Field { obj, name, safe } => self.field(obj, name, *safe, dst),
            ExprKind::Index { obj, index } => {
                let o = self.expr_any(obj);
                let i = self.expr_any(index);
                self.at(e.span);
                self.emit(Op::GetIndex { dst, obj: o, index: i });
            }
            ExprKind::Call { callee, args } => self.call(callee, args, e.span, dst),
            ExprKind::New { path, args } => {
                let base = self.alloc();
                self.path_value(path, base);
                self.args_and_call(base, args, e.span, dst);
            }
            ExprKind::Unary { op, expr } => self.unary(*op, expr, dst, e.span),
            ExprKind::Binary { op, left, right } => self.binary(*op, left, right, dst, e.span),
            ExprKind::Assign { op, target, value } => self.assign(*op, target, value, dst, e.span),
            ExprKind::Ternary { cond, then, els } => self.ternary(cond, then, els, dst),
            ExprKind::If { cond, then, els } => self.if_expr(cond, then, els.as_deref(), dst),
            ExprKind::While { cond, body } => self.while_expr(cond, body, dst),
            ExprKind::DoWhile { body, cond } => self.do_while(body, cond, dst),
            ExprKind::For { key, var, iter, body } => self.for_expr(key.as_ref(), var, iter, body, dst),
            ExprKind::Return(v) => {
                match v {
                    Some(v) => {
                        let r = self.expr_any(v);
                        self.at(e.span);
                        self.emit(Op::Return { src: r });
                    }
                    None => {
                        self.emit(Op::ReturnNil);
                    }
                }
                self.emit(Op::LoadNil { dst });
            }
            ExprKind::Break => self.break_or_continue(true, e.span, dst),
            ExprKind::Continue => self.break_or_continue(false, e.span, dst),
            ExprKind::Throw(v) => {
                let r = self.expr_any(v);
                self.at(e.span);
                self.emit(Op::Throw { src: r });
                self.emit(Op::LoadNil { dst });
            }
            ExprKind::Try { body, catches } => self.try_expr(body, catches, dst),
            ExprKind::Switch { subject, cases, default } => self.switch_expr(subject, cases, default.as_deref(), dst),
            ExprKind::Await(v) => {
                let r = self.expr_any(v);
                self.at(e.span);
                self.emit(Op::Await { dst, src: r });
            }
            ExprKind::Markup(n) => self.markup_value(n, dst),
            ExprKind::Error => {
                self.emit(Op::LoadNil { dst });
            }
        }
        self.free_to(m);
    }

    fn block(&mut self, stmts: &[Expr], dst: Reg) {
        self.push_scope();
        match stmts.split_last() {
            None => {
                self.emit(Op::LoadNil { dst });
            }
            Some((last, rest)) => {
                for s in rest {
                    self.statement(s);
                }
                match &last.kind {
                    ExprKind::Var(_) => {
                        self.statement(last);
                        self.emit(Op::LoadNil { dst });
                    }
                    ExprKind::Function(f) if f.name.is_some() => {
                        self.statement(last);
                        self.emit(Op::LoadNil { dst });
                    }
                    _ => self.expr_to(last, dst),
                }
            }
        }
        self.pop_scope();
    }

    fn interp(&mut self, parts: &[InterpPart], dst: Reg) {
        let base = self.alloc_n(parts.len() as u16);
        for (i, p) in parts.iter().enumerate() {
            let r = base + i as Reg;
            match p {
                InterpPart::Lit(s) => {
                    let index = self.const_str(s);
                    self.emit(Op::LoadConst { dst: r, index });
                }
                InterpPart::Expr(x) => self.expr_to(x, r),
            }
        }
        self.emit(Op::Interp { dst, base, count: parts.len() as u16 });
    }

    /// `a.b.c` as a value: the first name as a variable, the rest as fields.
    pub(crate) fn path_value(&mut self, path: &[Ident], dst: Reg) {
        let Some(first) = path.first() else { return };
        self.at(first.span);
        self.load_name(&first.name, dst);
        for seg in &path[1..] {
            self.at(seg.span);
            let name = self.const_str(&seg.name);
            self.emit(Op::GetField { dst, obj: dst, name });
        }
    }

    fn field(&mut self, obj: &Expr, name: &Ident, safe: bool, dst: Reg) {
        let k = self.const_str(&name.name);
        if !safe {
            let o = self.expr_any(obj);
            self.at(name.span);
            self.emit(Op::GetField { dst, obj: o, name: k });
            return;
        }
        let t = self.alloc();
        self.expr_to(obj, t);
        let is_nil = self.emit(Op::JumpIfNil { src: t, offset: 0 });
        self.at(name.span);
        self.emit(Op::GetField { dst, obj: t, name: k });
        let end = self.emit(Op::Jump { offset: 0 });
        self.patch(is_nil);
        self.emit(Op::LoadNil { dst });
        self.patch(end);
    }

    // Calls

    fn call(&mut self, callee: &Expr, args: &[Expr], span: Span, dst: Reg) {
        if let ExprKind::Field { obj, name, safe } = &strip(callee).kind {
            let base = self.alloc();
            self.expr_to(obj, base);
            let skip = if *safe { Some(self.emit(Op::JumpIfNil { src: base, offset: 0 })) } else { None };
            let argc = self.push_args(args);
            self.at(span);
            let k = self.const_str(&name.name);
            self.emit(Op::CallMethod { base, argc, name: k });
            self.move_result(base, dst);
            if let Some(skip) = skip {
                let end = self.emit(Op::Jump { offset: 0 });
                self.patch(skip);
                self.emit(Op::LoadNil { dst });
                self.patch(end);
            }
            return;
        }
        let base = self.alloc();
        self.expr_to(callee, base);
        self.args_and_call(base, args, span, dst);
    }

    fn push_args(&mut self, args: &[Expr]) -> u16 {
        for a in args {
            let r = self.alloc();
            self.expr_to(a, r);
        }
        args.len().min(u16::MAX as usize) as u16
    }

    fn args_and_call(&mut self, base: Reg, args: &[Expr], span: Span, dst: Reg) {
        let argc = self.push_args(args);
        self.at(span);
        self.emit(Op::Call { base, argc });
        self.move_result(base, dst);
    }

    fn move_result(&mut self, base: Reg, dst: Reg) {
        if base != dst {
            self.emit(Op::Move { dst, src: base });
        }
    }

    // Operators

    fn unary(&mut self, op: UnOp, e: &Expr, dst: Reg, span: Span) {
        match op {
            UnOp::Neg | UnOp::Not | UnOp::BitNot => {
                let r = self.expr_any(e);
                self.at(span);
                self.emit(match op {
                    UnOp::Neg => Op::Neg { dst, src: r },
                    UnOp::Not => Op::Not { dst, src: r },
                    _ => Op::BitNot { dst, src: r },
                });
            }
            UnOp::PreInc | UnOp::PreDec | UnOp::PostInc | UnOp::PostDec => {
                let place = self.place(e);
                let cur = self.alloc();
                self.load_place(&place, cur);
                let post = matches!(op, UnOp::PostInc | UnOp::PostDec);
                if post {
                    self.emit(Op::Move { dst, src: cur });
                }
                let value = if matches!(op, UnOp::PreInc | UnOp::PostInc) { 1 } else { -1 };
                self.at(span);
                self.emit(Op::AddInt { dst: cur, src: cur, value });
                self.store_place(&place, cur);
                if !post {
                    self.emit(Op::Move { dst, src: cur });
                }
            }
        }
    }

    fn binary(&mut self, op: BinOp, left: &Expr, right: &Expr, dst: Reg, span: Span) {
        match op {
            BinOp::And | BinOp::Or => {
                let t = self.alloc();
                self.expr_to(left, t);
                let skip = if op == BinOp::And { self.emit(Op::JumpIfNot { cond: t, offset: 0 }) } else { self.emit(Op::JumpIf { cond: t, offset: 0 }) };
                self.expr_to(right, t);
                self.patch(skip);
                self.emit(Op::Move { dst, src: t });
            }
            BinOp::Coalesce => {
                let t = self.alloc();
                self.expr_to(left, t);
                let skip = self.emit(Op::JumpIfNotNil { src: t, offset: 0 });
                self.expr_to(right, t);
                self.patch(skip);
                self.emit(Op::Move { dst, src: t });
            }
            BinOp::Interval => {
                let a = self.expr_any(left);
                let b = self.expr_any(right);
                self.at(span);
                self.emit(Op::NewRange { dst, from: a, to: b });
            }
            _ => {
                let a = self.expr_any(left);
                let b = self.expr_any(right);
                self.at(span);
                self.emit_binop(op, dst, a, b);
            }
        }
    }

    fn emit_binop(&mut self, op: BinOp, dst: Reg, a: Reg, b: Reg) {
        let o = match op {
            BinOp::Add => Op::Add { dst, a, b },
            BinOp::Sub => Op::Sub { dst, a, b },
            BinOp::Mul => Op::Mul { dst, a, b },
            BinOp::Div => Op::Div { dst, a, b },
            BinOp::Rem => Op::Rem { dst, a, b },
            BinOp::Shl => Op::Shl { dst, a, b },
            BinOp::Shr => Op::Shr { dst, a, b },
            BinOp::UShr => Op::UShr { dst, a, b },
            BinOp::BitAnd => Op::BitAnd { dst, a, b },
            BinOp::BitOr => Op::BitOr { dst, a, b },
            BinOp::BitXor => Op::BitXor { dst, a, b },
            BinOp::Eq => Op::Eq { dst, a, b },
            BinOp::NotEq => Op::NotEq { dst, a, b },
            BinOp::Lt => Op::Lt { dst, a, b },
            BinOp::Le => Op::Le { dst, a, b },
            BinOp::Gt => Op::Gt { dst, a, b },
            BinOp::Ge => Op::Ge { dst, a, b },
            BinOp::And | BinOp::Or | BinOp::Coalesce | BinOp::Interval => unreachable!("handled by the caller"),
        };
        self.emit(o);
    }

    fn ternary(&mut self, cond: &Expr, then: &Expr, els: &Expr, dst: Reg) {
        let t = self.build_target(dst);
        let c = self.expr_any(cond);
        let to_else = self.emit(Op::JumpIfNot { cond: c, offset: 0 });
        self.expr_to(then, t);
        let end = self.emit(Op::Jump { offset: 0 });
        self.patch(to_else);
        self.expr_to(els, t);
        self.patch(end);
        self.finish_target(t, dst);
    }

    // Assignment

    /// Evaluates what a target needs once, and says where the value lives.
    fn place(&mut self, target: &Expr) -> Place {
        let target = strip(target);
        match &target.kind {
            ExprKind::Ident(n) => match self.resolve(n) {
                Var::Local(Local { reg, cell: false }) => Place::Local(reg),
                Var::Local(Local { reg, cell: true }) => Place::Cell(reg),
                Var::Upval(i) => Place::Upval(i),
                Var::Global => Place::Global(self.const_str(n)),
            },
            ExprKind::Field { obj, name, .. } => {
                let o = self.expr_any(obj);
                Place::Field { obj: o, name: self.const_str(&name.name) }
            }
            ExprKind::Index { obj, index } => {
                let o = self.expr_any(obj);
                let i = self.expr_any(index);
                Place::Index { obj: o, index: i }
            }
            ExprKind::Error => Place::Nowhere,
            _ => {
                self.error(target.span, "cannot assign to this");
                Place::Nowhere
            }
        }
    }

    fn load_place(&mut self, p: &Place, dst: Reg) {
        match *p {
            Place::Local(reg) => {
                if reg != dst {
                    self.emit(Op::Move { dst, src: reg });
                }
            }
            Place::Cell(cell) => {
                self.emit(Op::GetCell { dst, cell });
            }
            Place::Upval(index) => {
                self.emit(Op::GetUpval { dst, index });
            }
            Place::Global(name) => {
                self.emit(Op::GetGlobal { dst, name });
            }
            Place::Field { obj, name } => {
                self.emit(Op::GetField { dst, obj, name });
            }
            Place::Index { obj, index } => {
                self.emit(Op::GetIndex { dst, obj, index });
            }
            Place::Nowhere => {
                self.emit(Op::LoadNil { dst });
            }
        }
    }

    fn store_place(&mut self, p: &Place, src: Reg) {
        match *p {
            Place::Local(reg) => {
                if reg != src {
                    self.emit(Op::Move { dst: reg, src });
                }
            }
            Place::Cell(cell) => {
                self.emit(Op::SetCell { cell, src });
            }
            Place::Upval(index) => {
                self.emit(Op::SetUpval { index, src });
            }
            Place::Global(name) => {
                self.emit(Op::SetGlobal { src, name });
            }
            Place::Field { obj, name } => {
                self.emit(Op::SetField { obj, name, src });
            }
            Place::Index { obj, index } => {
                self.emit(Op::SetIndex { obj, index, src });
            }
            Place::Nowhere => {}
        }
    }

    fn assign(&mut self, op: Option<BinOp>, target: &Expr, value: &Expr, dst: Reg, span: Span) {
        let place = self.place(target);
        match op {
            None => {
                if let Place::Local(reg) = place {
                    if self.writes_last(value) {
                        self.expr_to(value, reg);
                    } else {
                        let t = self.alloc();
                        self.expr_to(value, t);
                        self.emit(Op::Move { dst: reg, src: t });
                    }
                    if dst != reg {
                        self.emit(Op::Move { dst, src: reg });
                    }
                    return;
                }
                let v = self.expr_any(value);
                self.at(span);
                self.store_place(&place, v);
                if dst != v {
                    self.emit(Op::Move { dst, src: v });
                }
            }
            Some(BinOp::Coalesce) => {
                let cur = self.alloc();
                self.load_place(&place, cur);
                let done = self.emit(Op::JumpIfNotNil { src: cur, offset: 0 });
                self.expr_to(value, cur);
                self.store_place(&place, cur);
                self.patch(done);
                self.emit(Op::Move { dst, src: cur });
            }
            Some(op) => {
                let cur = self.alloc();
                self.load_place(&place, cur);
                let v = self.expr_any(value);
                self.at(span);
                self.emit_binop(op, cur, cur, v);
                self.store_place(&place, cur);
                self.emit(Op::Move { dst, src: cur });
            }
        }
    }
}
