//! The first pass: which local variables a nested function reads or writes. Those live in cells,
//! so the closure and its maker share them; the rest stay in registers. Variables are named by the
//! start of their declaring identifier. The scoping here is the scoping of the emitter's, step for
//! step, including the thunks markup is lowered to.

use rayscript_syntax::ast::*;
use std::collections::{HashMap, HashSet};

pub struct Captures {
    pub captured: HashSet<u32>,
}

struct Scope {
    names: HashMap<String, (u32, u32)>,
}

struct Analyzer {
    scopes: Vec<Scope>,
    /// How many functions deep the code being walked is; the module's own statements are 0.
    depth: u32,
    captured: HashSet<u32>,
}

pub fn analyze(module: &Module) -> Captures {
    let mut a = Analyzer { scopes: vec![Scope { names: HashMap::new() }], depth: 0, captured: HashSet::new() };
    for item in &module.items {
        if let Item::Stmt(e) = item {
            a.top_level(e);
        }
    }
    Captures { captured: a.captured }
}

impl Analyzer {
    fn push(&mut self) {
        self.scopes.push(Scope { names: HashMap::new() });
    }

    fn pop(&mut self) {
        self.scopes.pop();
    }

    fn declare(&mut self, id: &Ident) {
        let depth = self.depth;
        self.scopes.last_mut().unwrap().names.insert(id.name.clone(), (id.span.start, depth));
    }

    fn use_name(&mut self, name: &str) {
        for scope in self.scopes.iter().rev() {
            if let Some((decl, depth)) = scope.names.get(name) {
                if *depth < self.depth {
                    self.captured.insert(*decl);
                }
                return;
            }
        }
    }

    /// The module's own `var` and `function` are globals, not locals; anything else is walked as usual.
    fn top_level(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Var(v) => {
                for d in &v.decls {
                    if let Some(i) = &d.init {
                        self.expr(i);
                    }
                }
            }
            ExprKind::Function(f) if f.name.is_some() => self.function(f.params.as_slice(), &f.body, None),
            _ => self.expr(e),
        }
    }

    /// A function's parameters and body, in a scope one deeper; `name` is the function's own, declared outside.
    fn function(&mut self, params: &[Param], body: &Expr, name: Option<&Ident>) {
        if let Some(n) = name {
            self.declare(n);
        }
        self.depth += 1;
        self.push();
        for p in params {
            self.declare(&p.name);
            if let Some(d) = &p.default {
                self.expr(d);
            }
        }
        self.expr(body);
        self.pop();
        self.depth -= 1;
    }

    /// Code the emitter makes a function of, with no parameters: a markup hole.
    fn thunk(&mut self, f: impl FnOnce(&mut Analyzer)) {
        self.depth += 1;
        self.push();
        f(self);
        self.pop();
        self.depth -= 1;
    }

    fn exprs(&mut self, es: &[Expr]) {
        for e in es {
            self.expr(e);
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Null | ExprKind::Bool(_) | ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Break | ExprKind::Continue | ExprKind::Error => {}
            ExprKind::Interp(parts) => {
                for p in parts {
                    if let InterpPart::Expr(x) = p {
                        self.expr(x);
                    }
                }
            }
            ExprKind::Ident(n) => self.use_name(n),
            ExprKind::Paren(i) => self.expr(i),
            ExprKind::Array(items) => self.exprs(items),
            ExprKind::Map(pairs) => {
                for (k, v) in pairs {
                    self.expr(k);
                    self.expr(v);
                }
            }
            ExprKind::Object(fields) => {
                for f in fields {
                    self.expr(&f.value);
                }
            }
            ExprKind::Comprehension { body, .. } => {
                self.push();
                self.expr(body);
                self.pop();
            }
            ExprKind::Block(stmts) => {
                self.push();
                self.exprs(stmts);
                self.pop();
            }
            ExprKind::Var(v) => {
                for d in &v.decls {
                    if let Some(i) = &d.init {
                        self.expr(i);
                    }
                    self.declare(&d.name);
                }
            }
            ExprKind::Function(f) => self.function(&f.params, &f.body, f.name.as_ref()),
            ExprKind::Lambda(l) => self.function(&l.params, &l.body, None),
            ExprKind::Field { obj, .. } => self.expr(obj),
            ExprKind::Index { obj, index } => {
                self.expr(obj);
                self.expr(index);
            }
            ExprKind::Call { callee, args } => {
                self.expr(callee);
                self.exprs(args);
            }
            ExprKind::New { path, args } => {
                if let Some(first) = path.first() {
                    self.use_name(&first.name);
                }
                self.exprs(args);
            }
            ExprKind::Unary { expr, .. } => self.expr(expr),
            ExprKind::Binary { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            ExprKind::Assign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            ExprKind::Ternary { cond, then, els } => {
                self.expr(cond);
                self.expr(then);
                self.expr(els);
            }
            ExprKind::If { cond, then, els } => {
                self.expr(cond);
                self.expr(then);
                if let Some(e) = els {
                    self.expr(e);
                }
            }
            ExprKind::While { cond, body } => {
                self.expr(cond);
                self.expr(body);
            }
            ExprKind::DoWhile { body, cond } => {
                self.expr(body);
                self.expr(cond);
            }
            ExprKind::For { key, var, iter, body } => {
                self.expr(iter);
                self.push();
                if let Some(k) = key {
                    self.declare(k);
                }
                self.declare(var);
                self.expr(body);
                self.pop();
            }
            ExprKind::Return(v) => {
                if let Some(v) = v {
                    self.expr(v);
                }
            }
            ExprKind::Throw(v) | ExprKind::Await(v) => self.expr(v),
            ExprKind::Try { body, catches } => {
                self.expr(body);
                for c in catches {
                    self.push();
                    self.declare(&c.name);
                    self.expr(&c.body);
                    self.pop();
                }
            }
            ExprKind::Switch { subject, cases, default } => {
                self.expr(subject);
                for c in cases {
                    self.push();
                    for p in &c.patterns {
                        self.pattern(p);
                    }
                    if let Some(g) = &c.guard {
                        self.expr(g);
                    }
                    self.expr(&c.body);
                    self.pop();
                }
                if let Some(d) = default {
                    self.expr(d);
                }
            }
            ExprKind::Markup(m) => self.markup(m),
        }
    }

    fn pattern(&mut self, p: &Pattern) {
        match &p.kind {
            PatternKind::Wildcard => {}
            PatternKind::Bind(n) => {
                let depth = self.depth;
                self.scopes.last_mut().unwrap().names.insert(n.clone(), (p.span.start, depth));
            }
            PatternKind::Lit(e) => self.expr(e),
            PatternKind::Array { items, rest } => {
                for i in items {
                    self.pattern(i);
                }
                if let Some(Some(id)) = rest {
                    self.declare(id);
                }
            }
            PatternKind::Object(fields) => {
                for (_, sub) in fields {
                    self.pattern(sub);
                }
            }
            PatternKind::Ctor { path, args } => {
                if let Some(first) = path.first() {
                    self.use_name(&first.name);
                }
                for a in args {
                    self.pattern(a);
                }
            }
        }
    }

    // Markup is lowered to calls with thunks for everything that can change, so those are functions here.

    fn markup(&mut self, m: &MarkupNode) {
        match m {
            MarkupNode::Element(e) => {
                // A component's name is a variable.
                if e.tag.name.chars().next().is_some_and(|c| c.is_uppercase()) {
                    self.use_name(e.tag.name.split('.').next().unwrap_or(""));
                }
                for a in &e.attrs {
                    match a {
                        Attr::Named { value, .. } => match value {
                            AttrValue::Expr(x) => self.thunk(|s| s.expr(x)),
                            AttrValue::Interp(parts) => self.thunk(|s| {
                                for p in parts {
                                    if let InterpPart::Expr(x) = p {
                                        s.expr(x);
                                    }
                                }
                            }),
                            _ => {}
                        },
                        Attr::Spread { expr, .. } => self.thunk(|s| s.expr(expr)),
                    }
                }
                for c in &e.children {
                    self.markup(c);
                }
            }
            MarkupNode::Fragment { children, .. } => {
                for c in children {
                    self.markup(c);
                }
            }
            MarkupNode::Text { .. } => {}
            MarkupNode::Hole { expr, .. } => self.thunk(|s| s.expr(expr)),
            MarkupNode::If { cond, then, els, .. } => {
                self.thunk(|s| s.expr(cond));
                self.thunk(|s| {
                    for c in then {
                        s.markup(c);
                    }
                });
                if let Some(els) = els {
                    self.thunk(|s| {
                        for c in els {
                            s.markup(c);
                        }
                    });
                }
            }
            MarkupNode::For { key, var, iter, body, .. } => {
                self.thunk(|s| s.expr(iter));
                self.depth += 1;
                self.push();
                if let Some(k) = key {
                    self.declare(k);
                }
                self.declare(var);
                for c in body {
                    self.markup(c);
                }
                self.pop();
                self.depth -= 1;
            }
        }
    }
}
