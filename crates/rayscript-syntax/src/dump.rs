//! A compact S-expression form of the tree, for tests and for looking at what the parser made.

use crate::ast::*;
use std::fmt::Write;

pub fn dump_module(m: &Module) -> String {
    m.items.iter().map(dump_item).collect::<Vec<_>>().join("\n")
}

pub fn dump_item(item: &Item) -> String {
    match item {
        Item::Import(i) => {
            let mut s = format!("(import {}", i.path.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join("."));
            if i.wildcard {
                s.push_str(".*");
            }
            if let Some(a) = &i.alias {
                write!(s, " as {}", a.name).unwrap();
            }
            s.push(')');
            s
        }
        Item::Native(n) => {
            let mut s = format!("(native {:?}", n.library);
            for f in &n.functions {
                write!(s, " (fn {} ({})", f.name.name, params(&f.params)).unwrap();
                if let Some(r) = &f.ret {
                    write!(s, " {}", ty(r)).unwrap();
                }
                for m in &f.meta {
                    write!(s, " @{}", m.name).unwrap();
                }
                s.push(')');
            }
            s.push(')');
            s
        }
        Item::Stmt(e) => dump_expr(e),
    }
}

fn params(ps: &[Param]) -> String {
    ps.iter()
        .map(|p| {
            let mut s = String::new();
            if p.rest {
                s.push_str("...");
            }
            if p.optional {
                s.push('?');
            }
            s.push_str(&p.name.name);
            if let Some(t) = &p.ty {
                write!(s, ":{}", ty(t)).unwrap();
            }
            if let Some(d) = &p.default {
                write!(s, "={}", dump_expr(d)).unwrap();
            }
            s
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn ty(t: &Type) -> String {
    match &t.kind {
        TypeKind::Path { path, params } => {
            let mut s = path.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(".");
            if !params.is_empty() {
                write!(s, "<{}>", params.iter().map(ty).collect::<Vec<_>>().join(",")).unwrap();
            }
            s
        }
        TypeKind::Func { args, ret } => format!("({})->{}", args.iter().map(ty).collect::<Vec<_>>().join(","), ty(ret)),
        TypeKind::Anon(fields) => format!("{{{}}}", fields.iter().map(|f| format!("{}{}:{}", if f.optional { "?" } else { "" }, f.name.name, ty(&f.ty))).collect::<Vec<_>>().join(",")),
        TypeKind::Optional(t) => format!("?{}", ty(t)),
    }
}

fn binop(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::UShr => ">>>",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Eq => "==",
        BinOp::NotEq => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::Coalesce => "??",
        BinOp::Interval => "...",
    }
}

fn list(es: &[Expr]) -> String {
    es.iter().map(dump_expr).collect::<Vec<_>>().join(" ")
}

fn interp(parts: &[InterpPart]) -> String {
    parts
        .iter()
        .map(|p| match p {
            InterpPart::Lit(s) => format!("{s:?}"),
            InterpPart::Expr(e) => dump_expr(e),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn dump_expr(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Null => "null".into(),
        ExprKind::Bool(b) => b.to_string(),
        ExprKind::Int(v) => v.to_string(),
        ExprKind::Float(v) => format!("{v:?}"),
        ExprKind::Str(s) => format!("{s:?}"),
        ExprKind::Interp(p) => format!("(interp {})", interp(p)),
        ExprKind::Ident(n) => n.clone(),
        ExprKind::Paren(i) => format!("(paren {})", dump_expr(i)),
        ExprKind::Array(items) => format!("(array{}{})", if items.is_empty() { "" } else { " " }, list(items)),
        ExprKind::Map(pairs) => format!("(map {})", pairs.iter().map(|(k, v)| format!("({} {})", dump_expr(k), dump_expr(v))).collect::<Vec<_>>().join(" ")),
        ExprKind::Object(fields) => format!("(object{})", fields.iter().map(|f| format!(" ({} {})", f.name, dump_expr(&f.value))).collect::<String>()),
        ExprKind::Comprehension { map, body } => format!("(comp {} {})", if *map { "map" } else { "array" }, dump_expr(body)),
        ExprKind::Block(stmts) => format!("(block{}{})", if stmts.is_empty() { "" } else { " " }, list(stmts)),
        ExprKind::Var(v) => {
            let decls = v
                .decls
                .iter()
                .map(|d| {
                    let mut s = format!("({}", d.name.name);
                    if let Some(t) = &d.ty {
                        write!(s, ":{}", ty(t)).unwrap();
                    }
                    if let Some(i) = &d.init {
                        write!(s, " {}", dump_expr(i)).unwrap();
                    }
                    s.push(')');
                    s
                })
                .collect::<Vec<_>>()
                .join(" ");
            let mods = v.modifiers.iter().map(|m| format!("{m:?}").to_lowercase()).collect::<Vec<_>>().join(" ");
            let metas = v.meta.iter().map(|m| format!("@{} ", m.name)).collect::<String>();
            format!("({metas}{}{} {decls})", if mods.is_empty() { String::new() } else { format!("{mods} ") }, if v.is_final { "final" } else { "var" })
        }
        ExprKind::Function(f) => {
            let metas = f.meta.iter().map(|m| format!("@{} ", m.name)).collect::<String>();
            let mods = f.modifiers.iter().map(|m| format!("{m:?}").to_lowercase()).collect::<Vec<_>>().join(" ");
            let ret = f.ret.as_ref().map(|t| format!(":{}", ty(t))).unwrap_or_default();
            format!("({metas}{}fn {} ({}){} {})", if mods.is_empty() { String::new() } else { format!("{mods} ") }, f.name.as_ref().map(|n| n.name.as_str()).unwrap_or("_"), params(&f.params), ret, dump_expr(&f.body))
        }
        ExprKind::Lambda(l) => format!("(lambda ({}) {})", params(&l.params), dump_expr(&l.body)),
        ExprKind::Field { obj, name, safe } => format!("({} {} {})", if *safe { "?." } else { "." }, dump_expr(obj), name.name),
        ExprKind::Index { obj, index } => format!("([] {} {})", dump_expr(obj), dump_expr(index)),
        ExprKind::Call { callee, args } => format!("(call {}{}{})", dump_expr(callee), if args.is_empty() { "" } else { " " }, list(args)),
        ExprKind::New { path, args } => format!("(new {}{}{})", path.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join("."), if args.is_empty() { "" } else { " " }, list(args)),
        ExprKind::Unary { op, expr } => {
            let name = match op {
                UnOp::Neg => "neg",
                UnOp::Not => "not",
                UnOp::BitNot => "bitnot",
                UnOp::PreInc => "pre++",
                UnOp::PreDec => "pre--",
                UnOp::PostInc => "post++",
                UnOp::PostDec => "post--",
            };
            format!("({name} {})", dump_expr(expr))
        }
        ExprKind::Binary { op, left, right } => format!("({} {} {})", binop(*op), dump_expr(left), dump_expr(right)),
        ExprKind::Assign { op, target, value } => format!("({}= {} {})", op.map(binop).unwrap_or(""), dump_expr(target), dump_expr(value)),
        ExprKind::Ternary { cond, then, els } => format!("(?: {} {} {})", dump_expr(cond), dump_expr(then), dump_expr(els)),
        ExprKind::If { cond, then, els } => format!("(if {} {}{})", dump_expr(cond), dump_expr(then), els.as_ref().map(|e| format!(" {}", dump_expr(e))).unwrap_or_default()),
        ExprKind::While { cond, body } => format!("(while {} {})", dump_expr(cond), dump_expr(body)),
        ExprKind::DoWhile { body, cond } => format!("(do {} {})", dump_expr(body), dump_expr(cond)),
        ExprKind::For { key, var, iter, body } => format!("(for {}{} {} {})", key.as_ref().map(|k| format!("{}=>", k.name)).unwrap_or_default(), var.name, dump_expr(iter), dump_expr(body)),
        ExprKind::Return(v) => format!("(return{})", v.as_ref().map(|e| format!(" {}", dump_expr(e))).unwrap_or_default()),
        ExprKind::Break => "(break)".into(),
        ExprKind::Continue => "(continue)".into(),
        ExprKind::Throw(v) => format!("(throw {})", dump_expr(v)),
        ExprKind::Try { body, catches } => format!(
            "(try {}{})",
            dump_expr(body),
            catches.iter().map(|c| format!(" (catch {}{} {})", c.name.name, c.ty.as_ref().map(|t| format!(":{}", ty(t))).unwrap_or_default(), dump_expr(&c.body))).collect::<String>()
        ),
        ExprKind::Switch { subject, cases, default } => {
            let mut s = format!("(switch {}", dump_expr(subject));
            for c in cases {
                write!(s, " (case ({}){} {})", c.patterns.iter().map(pattern).collect::<Vec<_>>().join(" | "), c.guard.as_ref().map(|g| format!(" if {}", dump_expr(g))).unwrap_or_default(), dump_expr(&c.body)).unwrap();
            }
            if let Some(d) = default {
                write!(s, " (default {})", dump_expr(d)).unwrap();
            }
            s.push(')');
            s
        }
        ExprKind::Await(v) => format!("(await {})", dump_expr(v)),
        ExprKind::Markup(m) => format!("(markup {})", markup(m)),
        ExprKind::Error => "<error>".into(),
    }
}

fn pattern(p: &Pattern) -> String {
    match &p.kind {
        PatternKind::Wildcard => "_".into(),
        PatternKind::Bind(n) => n.clone(),
        PatternKind::Lit(e) => dump_expr(e),
        PatternKind::Array { items, rest } => {
            let mut parts: Vec<String> = items.iter().map(pattern).collect();
            if let Some(r) = rest {
                parts.push(format!("...{}", r.as_ref().map(|i| i.name.as_str()).unwrap_or("")));
            }
            format!("[{}]", parts.join(" "))
        }
        PatternKind::Object(fields) => format!("{{{}}}", fields.iter().map(|(n, p)| format!("{}:{}", n.name, pattern(p))).collect::<Vec<_>>().join(" ")),
        PatternKind::Ctor { path, args } => {
            let name = path.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(".");
            if args.is_empty() { name } else { format!("{name}({})", args.iter().map(pattern).collect::<Vec<_>>().join(" ")) }
        }
    }
}

fn nodes(ns: &[MarkupNode]) -> String {
    ns.iter().map(|n| format!(" {}", markup(n))).collect()
}

pub fn markup(n: &MarkupNode) -> String {
    match n {
        MarkupNode::Element(e) => {
            let mut s = format!("<{}", e.tag.name);
            for a in &e.attrs {
                match a {
                    Attr::Named { name, value, .. } => match value {
                        AttrValue::Flag => write!(s, " @{}", name.name).unwrap(),
                        AttrValue::Str(v) => write!(s, " @{}={:?}", name.name, v).unwrap(),
                        AttrValue::Interp(p) => write!(s, " @{}=(interp {})", name.name, interp(p)).unwrap(),
                        AttrValue::Expr(x) => write!(s, " @{}={}", name.name, dump_expr(x)).unwrap(),
                    },
                    Attr::Spread { expr, .. } => write!(s, " ...{}", dump_expr(expr)).unwrap(),
                }
            }
            if e.self_closing {
                s.push_str(" />");
            } else {
                s.push('>');
                s.push_str(&nodes(&e.children));
                s.push_str(&format!(" </{}>", e.tag.name));
            }
            s
        }
        MarkupNode::Fragment { children, .. } => format!("<>{} </>", nodes(children)),
        MarkupNode::Text { text, .. } => format!("(text {text:?})"),
        MarkupNode::Hole { expr, .. } => format!("(hole {})", dump_expr(expr)),
        MarkupNode::If { cond, then, els, .. } => format!("(if {} (then{}){})", dump_expr(cond), nodes(then), els.as_ref().map(|e| format!(" (else{})", nodes(e))).unwrap_or_default()),
        MarkupNode::For { key, var, iter, body, .. } => format!("(for {}{} {} (body{}))", key.as_ref().map(|k| format!("{}=>", k.name)).unwrap_or_default(), var.name, dump_expr(iter), nodes(body)),
    }
}
