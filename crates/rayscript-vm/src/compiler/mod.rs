//! The compiler: syntax tree to register bytecode. Two passes: `analyze` finds the locals closures
//! capture (they live in cells), then this emits, one function at a time. Module-level `var` and
//! `function` are globals, looked up by name when they run, so they can be replaced while a script lives.

mod analyze;
mod control;
mod expr;
mod markup;

use crate::bytecode::{Const, LocalInfo, Module, Op, Proto, Reg, UpvalDesc};
use rayscript_syntax::ast::{self, Item};
use rayscript_syntax::span::{Diagnostic, Span};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// Compiles a parsed script. `source` is its text, kept for error positions.
pub fn compile(name: &str, source: &str, module: &ast::Module) -> Result<Module, Vec<Diagnostic>> {
    let captures = analyze::analyze(module);
    let mut c = Compiler { captured: captures.captured, fns: Vec::new(), protos: Vec::new(), diags: Vec::new() };
    c.begin_function("main", 0, false);
    c.module_items(module);
    c.emit(Op::ReturnNil);
    let main = c.end_function();
    if !c.diags.is_empty() {
        return Err(c.diags);
    }
    Ok(Module { name: name.to_string(), source: Rc::from(source), protos: c.protos, main })
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Local {
    pub reg: Reg,
    /// The register holds a cell, because a closure captures the variable.
    pub cell: bool,
}

struct Scope {
    locals: Vec<(String, Local)>,
    /// The first register above this scope's locals when it began.
    mark: Reg,
    /// Where each local of the scope is in the function's `infos`, to close its range when the scope ends.
    infos: Vec<usize>,
}

pub(crate) struct LoopCtx {
    pub breaks: Vec<usize>,
    pub continues: Vec<usize>,
    /// How many `try` blocks were open when the loop began, so a jump out of one closes them.
    pub tries: usize,
}

#[derive(Hash, PartialEq, Eq)]
enum ConstKey {
    Int(i64),
    Float(u64),
    Str(Rc<str>),
}

pub(crate) struct FnState {
    name: Rc<str>,
    params: u16,
    has_rest: bool,
    code: Vec<Op>,
    offsets: Vec<u32>,
    consts: Vec<Const>,
    const_map: HashMap<ConstKey, u32>,
    scopes: Vec<Scope>,
    infos: Vec<LocalInfo>,
    free: Reg,
    max: Reg,
    upvals: Vec<UpvalDesc>,
    pub loops: Vec<LoopCtx>,
    pub tries: usize,
    /// The source offset of the code being emitted.
    cur: u32,
    is_main: bool,
}

/// What a name means where it is used.
pub(crate) enum Var {
    Local(Local),
    Upval(u16),
    Global,
}

pub(crate) struct Compiler {
    captured: HashSet<u32>,
    pub(crate) fns: Vec<FnState>,
    pub(crate) protos: Vec<Proto>,
    pub(crate) diags: Vec<Diagnostic>,
}

impl Compiler {
    pub(crate) fn f(&mut self) -> &mut FnState {
        self.fns.last_mut().unwrap()
    }

    pub(crate) fn error(&mut self, span: Span, message: impl Into<String>) {
        self.diags.push(Diagnostic::error(message, span));
    }

    // Functions

    pub(crate) fn begin_function(&mut self, name: &str, params: u16, has_rest: bool) {
        let is_main = self.fns.is_empty();
        self.fns.push(FnState {
            name: Rc::from(name),
            params,
            has_rest,
            code: Vec::new(),
            offsets: Vec::new(),
            consts: Vec::new(),
            const_map: HashMap::new(),
            scopes: vec![Scope { locals: Vec::new(), mark: 0, infos: Vec::new() }],
            infos: Vec::new(),
            free: params,
            max: params,
            upvals: Vec::new(),
            loops: Vec::new(),
            tries: 0,
            cur: 0,
            is_main,
        });
    }

    /// Finishes the function being compiled and returns its index among the protos.
    pub(crate) fn end_function(&mut self) -> u32 {
        let mut f = self.fns.pop().unwrap();
        let end = f.code.len() as u32;
        for info in &mut f.infos {
            info.end = info.end.min(end);
        }
        let proto = Proto { name: f.name, params: f.params, has_rest: f.has_rest, registers: f.max.max(1), code: f.code, offsets: f.offsets, consts: f.consts, upvals: f.upvals, locals: f.infos };
        self.protos.push(proto);
        (self.protos.len() - 1) as u32
    }

    // Emitting

    pub(crate) fn at(&mut self, span: Span) {
        self.f().cur = span.start;
    }

    pub(crate) fn emit(&mut self, op: Op) -> usize {
        let f = self.f();
        f.code.push(op);
        let cur = f.cur;
        f.offsets.push(cur);
        f.code.len() - 1
    }

    pub(crate) fn here(&mut self) -> usize {
        self.f().code.len()
    }

    /// Points the jump at `at` to the next instruction emitted.
    pub(crate) fn patch(&mut self, at: usize) {
        let target = self.here();
        self.patch_to(at, target);
    }

    pub(crate) fn patch_to(&mut self, at: usize, target: usize) {
        let off = target as i64 - (at as i64 + 1);
        let off = off as i32;
        match &mut self.f().code[at] {
            Op::Jump { offset }
            | Op::JumpIf { offset, .. }
            | Op::JumpIfNot { offset, .. }
            | Op::JumpIfNil { offset, .. }
            | Op::JumpIfNotNil { offset, .. }
            | Op::IterNext { offset, .. }
            | Op::ForRange { offset, .. } => *offset = off,
            Op::TryBegin { catch_offset, .. } => *catch_offset = off,
            other => panic!("patching an instruction that does not jump: {other:?}"),
        }
    }

    /// A jump back to `target`, an instruction already emitted.
    pub(crate) fn jump_back(&mut self, target: usize) {
        let at = self.emit(Op::Jump { offset: 0 });
        self.patch_to(at, target);
    }

    pub(crate) fn const_str(&mut self, s: &str) -> u32 {
        let key = ConstKey::Str(Rc::from(s));
        if let Some(i) = self.f().const_map.get(&key) {
            return *i;
        }
        let f = self.f();
        f.consts.push(Const::Str(Rc::from(s)));
        let i = (f.consts.len() - 1) as u32;
        f.const_map.insert(key, i);
        i
    }

    pub(crate) fn const_int(&mut self, v: i64) -> u32 {
        let f = self.f();
        if let Some(i) = f.const_map.get(&ConstKey::Int(v)) {
            return *i;
        }
        f.consts.push(Const::Int(v));
        let i = (f.consts.len() - 1) as u32;
        f.const_map.insert(ConstKey::Int(v), i);
        i
    }

    pub(crate) fn const_float(&mut self, v: f64) -> u32 {
        let f = self.f();
        if let Some(i) = f.const_map.get(&ConstKey::Float(v.to_bits())) {
            return *i;
        }
        f.consts.push(Const::Float(v));
        let i = (f.consts.len() - 1) as u32;
        f.const_map.insert(ConstKey::Float(v.to_bits()), i);
        i
    }

    // Registers and scopes

    pub(crate) fn alloc(&mut self) -> Reg {
        let f = self.f();
        if f.free == Reg::MAX {
            let span = Span::new(f.cur, f.cur + 1);
            self.error(span, "this function needs too many registers; split it up");
            return 0;
        }
        let r = f.free;
        f.free += 1;
        f.max = f.max.max(f.free);
        r
    }

    /// `count` registers in a row, for the arguments of a call.
    pub(crate) fn alloc_n(&mut self, count: u16) -> Reg {
        let first = self.f().free;
        for _ in 0..count {
            self.alloc();
        }
        first
    }

    pub(crate) fn mark(&mut self) -> Reg {
        self.f().free
    }

    pub(crate) fn free_to(&mut self, mark: Reg) {
        self.f().free = mark;
    }

    pub(crate) fn push_scope(&mut self) {
        let mark = self.f().free;
        self.f().scopes.push(Scope { locals: Vec::new(), mark, infos: Vec::new() });
    }

    pub(crate) fn pop_scope(&mut self) {
        let s = self.f().scopes.pop().unwrap();
        let end = self.f().code.len() as u32;
        for i in &s.infos {
            self.f().infos[*i].end = end;
        }
        self.f().free = s.mark;
    }

    /// Records a named local for the debugger, in scope from here to the end of the scope it is declared in.
    fn note_local(&mut self, name: &str, local: Local) {
        let start = self.f().code.len() as u32;
        let f = self.f();
        f.infos.push(LocalInfo { name: Rc::from(name), reg: local.reg, cell: local.cell, start, end: u32::MAX });
        let at = f.infos.len() - 1;
        f.scopes.last_mut().unwrap().infos.push(at);
    }

    /// Whether the code being emitted is the module's own statements, outside every block.
    pub(crate) fn at_module_level(&mut self) -> bool {
        let f = self.f();
        f.is_main && f.scopes.len() == 1
    }

    /// Makes `name` a local in `reg`; `decl` is the start of its declaring identifier.
    pub(crate) fn declare(&mut self, name: &str, reg: Reg, decl: u32) -> Local {
        let cell = self.captured.contains(&decl);
        let local = Local { reg, cell };
        self.f().scopes.last_mut().unwrap().locals.push((name.to_string(), local));
        self.note_local(name, local);
        local
    }

    /// Makes `name` a local in `reg`, in a cell or not as given.
    pub(crate) fn declare_as(&mut self, name: &str, reg: Reg, cell: bool) -> Local {
        let local = Local { reg, cell };
        self.f().scopes.last_mut().unwrap().locals.push((name.to_string(), local));
        self.note_local(name, local);
        local
    }

    /// The local `name` of the innermost scope that has it, if the current function does.
    pub(crate) fn local_of(&mut self, name: &str) -> Option<Local> {
        let level = self.fns.len() - 1;
        self.lookup_in(level, name)
    }

    pub(crate) fn is_captured(&self, decl: u32) -> bool {
        self.captured.contains(&decl)
    }

    fn lookup_in(&self, level: usize, name: &str) -> Option<Local> {
        for scope in self.fns[level].scopes.iter().rev() {
            for (n, l) in scope.locals.iter().rev() {
                if n == name {
                    return Some(*l);
                }
            }
        }
        None
    }

    fn add_upval(&mut self, level: usize, desc: UpvalDesc) -> u16 {
        let f = &mut self.fns[level];
        if let Some(i) = f.upvals.iter().position(|u| *u == desc) {
            return i as u16;
        }
        f.upvals.push(desc);
        (f.upvals.len() - 1) as u16
    }

    fn find_upvalue(&mut self, level: usize, name: &str) -> Option<u16> {
        if level == 0 {
            return None;
        }
        let parent = level - 1;
        if let Some(local) = self.lookup_in(parent, name) {
            if !local.cell {
                let span = Span::new(self.fns[level].cur, self.fns[level].cur + 1);
                self.error(span, format!("internal error: `{name}` is captured but was not put in a cell"));
            }
            return Some(self.add_upval(level, UpvalDesc { from_parent_local: true, index: local.reg }));
        }
        let idx = self.find_upvalue(parent, name)?;
        Some(self.add_upval(level, UpvalDesc { from_parent_local: false, index: idx }))
    }

    pub(crate) fn resolve(&mut self, name: &str) -> Var {
        let level = self.fns.len() - 1;
        if let Some(l) = self.lookup_in(level, name) {
            return Var::Local(l);
        }
        match self.find_upvalue(level, name) {
            Some(i) => Var::Upval(i),
            None => Var::Global,
        }
    }

    // Module

    fn module_items(&mut self, module: &ast::Module) {
        // Named functions at the top are defined before anything runs, so a script may call one below it.
        for item in &module.items {
            if let Item::Stmt(e) = item {
                if let ast::ExprKind::Function(f) = &e.kind {
                    if f.name.is_some() {
                        let m = self.mark();
                        self.at(e.span);
                        let t = self.alloc();
                        self.function_value(f, t);
                        let name = self.const_str(&f.name.as_ref().unwrap().name);
                        self.emit(Op::DefGlobal { src: t, name });
                        self.free_to(m);
                    }
                }
            }
        }
        for item in &module.items {
            match item {
                Item::Import(i) => self.import(i),
                Item::Native(n) => self.native_block(n),
                Item::Stmt(e) => {
                    if let ast::ExprKind::Function(f) = &e.kind {
                        if f.name.is_some() {
                            continue;
                        }
                    }
                    self.statement(e);
                }
            }
        }
    }

    fn import(&mut self, i: &ast::Import) {
        self.at(i.span);
        let path = i.path.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(".");
        let k = self.const_str(&path);
        if i.wildcard {
            self.emit(Op::ImportAll { path: k });
            return;
        }
        let m = self.mark();
        let t = self.alloc();
        self.emit(Op::Import { dst: t, path: k });
        let alias = i.alias.as_ref().or(i.path.last()).map(|a| a.name.clone()).unwrap_or_default();
        let name = self.const_str(&alias);
        self.emit(Op::DefGlobal { src: t, name });
        self.free_to(m);
    }

    fn native_block(&mut self, n: &ast::NativeBlock) {
        if n.library.is_empty() || n.library.contains('\0') {
            self.error(n.library_span, "a native library needs a nonempty name without NUL characters");
            return;
        }
        let ty = |t: &Option<ast::Type>| t.as_ref().map(rayscript_syntax::dump::ty).unwrap_or_else(|| "Void".to_string());
        let mut names = std::collections::HashSet::new();
        for h in &n.handles {
            self.at(h.span);
            if !names.insert(&h.name.name) {
                self.error(h.span, "duplicate native handle name");
            }
            let mut free = None;
            let mut refcounted = false;
            for meta in &h.meta {
                match (meta.name.as_str(), meta.args.as_slice()) {
                    (
                        ":free",
                        [
                            ast::Expr {
                                kind: ast::ExprKind::Ident(name),
                                ..
                            },
                        ],
                    ) if free.is_none() => free = Some(name.clone()),
                    (":refcounted", []) if !refcounted => refcounted = true,
                    _ => self.error(
                        h.span,
                        "a native handle accepts one @:free(function) and optional @:refcounted",
                    ),
                }
            }
            let Some(free) = free else {
                self.error(
                    h.span,
                    "a native handle needs exactly one @:free(function) annotation",
                );
                continue;
            };
            if !n.functions.iter().any(|f| {
                f.name.name == free
                    && f.params.len() == 1
                    && ty(&f.params[0].ty) == h.name.name
                    && ty(&f.ret) == "Void"
                    && f.meta.is_empty()
            }) {
                self.error(
                    h.span,
                    format!(
                        "handle {} needs `function {}(value:{}):Void;` in its native block",
                        h.name.name, free, h.name.name
                    ),
                );
            }
            let flags = if refcounted {
                format!("{free};refcounted")
            } else {
                free.clone()
            };
            let spec = format!("{}\0{}\0handle\0{}", n.library, h.name.name, flags);
            let k = self.const_str(&spec);
            let m = self.mark();
            let t = self.alloc();
            self.emit(Op::BindNative { dst: t, spec: k });
            let name = self.const_str(&h.name.name);
            self.emit(Op::DefGlobal { src: t, name });
            self.free_to(m);
        }
        for c in &n.callbacks {
            self.at(c.span);
            if !names.insert(&c.name.name) { self.error(c.span, "duplicate native type name"); }
            for p in &c.params {
                if p.ty.is_none() || p.optional || p.rest || p.default.is_some() {
                    self.error(p.span, "callback arguments need explicit types without defaults, optional or variadic parameters");
                }
            }
            let failure = match c.meta.as_slice() {
                [] if ty(&c.ret) == "Void" => Some("null".to_string()),
                [m] if m.name == ":failure" => match m.args.as_slice() {
                    [e] => match &e.kind {
                        ast::ExprKind::Int(n) => Some(n.to_string()),
                        ast::ExprKind::Float(n) => Some(format!("{n:?}")),
                        ast::ExprKind::Bool(b) => Some(b.to_string()),
                        ast::ExprKind::Null => Some("null".to_string()),
                        ast::ExprKind::Unary { op: ast::UnOp::Neg, expr } => match expr.kind {
                            ast::ExprKind::Int(n) => n.checked_neg().map(|n| n.to_string()),
                            ast::ExprKind::Float(n) => Some(format!("{:?}", -n)),
                            _ => None,
                        },
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            };
            let Some(failure) = failure else {
                self.error(c.span, "a callback needs one @:failure(literal) annotation; Void callbacks may omit it");
                continue;
            };
            let signature = format!("{}->{}", c.params.iter().map(|p| ty(&p.ty)).collect::<Vec<_>>().join(","), ty(&c.ret));
            let k = self.const_str(&format!("{}\0{}\0callback\0{}\t{}", n.library, c.name.name, signature, failure));
            let m = self.mark();
            let t = self.alloc();
            self.emit(Op::BindNative { dst: t, spec: k });
            let name = self.const_str(&c.name.name);
            self.emit(Op::DefGlobal { src: t, name });
            self.free_to(m);
        }
        let structs = n.structs.iter().map(|st| {
            std::iter::once(st.name.name.clone()).chain(st.fields.iter().map(|f| format!("{}:{}", f.name.name, rayscript_syntax::dump::ty(&f.ty)))).collect::<Vec<_>>().join("\t")
        }).collect::<Vec<_>>().join("\n");
        for st in &n.structs {
            self.at(st.span);
            if !names.insert(&st.name.name) { self.error(st.span, "duplicate native type name"); }
            let k = self.const_str(&format!("{}\0{}\0struct\0{}", n.library, st.name.name, structs));
            let m = self.mark();
            let t = self.alloc();
            self.emit(Op::BindNative { dst: t, spec: k });
            let name = self.const_str(&st.name.name);
            self.emit(Op::DefGlobal { src: t, name });
            self.free_to(m);
        }
        for c in &n.callbacks {
            self.at(c.span);
            let k = self.const_str(&format!("{}\0{}\0callback-check\0", n.library, c.name.name));
            let m = self.mark();
            let t = self.alloc();
            self.emit(Op::BindNative { dst: t, spec: k });
            self.free_to(m);
        }
        for f in &n.functions {
            for param in &f.params {
                if param.rest || param.optional || param.default.is_some() {
                    self.error(param.span, format!("{}.{}: native arguments cannot be optional, variadic or have defaults", n.library, f.name.name));
                }
            }
            self.at(f.span);
            let ty = |t: &Option<ast::Type>| t.as_ref().map(rayscript_syntax::dump::ty).unwrap_or_else(|| "Void".to_string());
            let sig = format!("{}->{}", f.params.iter().map(|p| ty(&p.ty)).collect::<Vec<_>>().join(","), ty(&f.ret));
            let mut flags = Vec::new();
            for meta in &f.meta {
                if meta.name == ":owner" {
                    let parameter = match meta.args.as_slice() {
                        [ast::Expr { kind: ast::ExprKind::Ident(name), .. }] => f.params.iter().position(|p| p.name.name == *name),
                        _ => None,
                    };
                    match parameter {
                        Some(index) => flags.push(format!("owner={index}")),
                        None => self.error(meta.span, "@:owner needs the name of a native parameter"),
                    }
                } else {
                    flags.push(meta.name.trim_start_matches(':').to_string());
                }
            }
            let flags = flags.join(",");
            let spec = format!("{}\0{}\0{}\0{}", n.library, f.name.name, sig, flags);
            let k = self.const_str(&spec);
            let m = self.mark();
            let t = self.alloc();
            self.emit(Op::BindNative { dst: t, spec: k });
            let name = self.const_str(&f.name.name);
            self.emit(Op::DefGlobal { src: t, name });
            self.free_to(m);
        }
    }
}
