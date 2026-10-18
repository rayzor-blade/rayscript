use super::*;
use crate::object::ArrayObj;
use crate::vm::ops;
use std::cmp::Ordering;

fn arr<'a>(args: &[Value]) -> &'a mut ArrayObj {
    args[0].as_obj().expect("a method of Array is called on an array").array()
}

/// A negative position counts from the end, as Haxe's `slice` and `splice` do; the result is within 0..=len.
fn clamp_pos(p: i64, len: usize) -> usize {
    if p < 0 { (len as i64 + p).max(0) as usize } else { (p as usize).min(len) }
}

fn default_order(a: Value, b: Value) -> Ordering {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(&y),
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            let f = |v: Value| match v {
                Value::Int(i) => i as f64,
                Value::Float(f) => f,
                _ => 0.0,
            };
            f(a).partial_cmp(&f(b)).unwrap_or(Ordering::Equal)
        }
        (Value::Obj(x), Value::Obj(y)) if x.kind() == Kind::String && y.kind() == Kind::String => x.str().cmp(y.str()),
        _ => Ordering::Equal,
    }
}

pub fn install(vm: &mut Vm) {
    vm.define_method(Kind::Array, "push", |_, args| {
        let a = arr(args);
        a.items.push(args.get(1).copied().unwrap_or(Value::Nil));
        Ok(Value::Int(a.items.len() as i64))
    });
    vm.define_method(Kind::Array, "pop", |_, args| Ok(arr(args).items.pop().unwrap_or(Value::Nil)));
    vm.define_method(Kind::Array, "shift", |_, args| {
        let a = arr(args);
        Ok(if a.items.is_empty() { Value::Nil } else { a.items.remove(0) })
    });
    vm.define_method(Kind::Array, "unshift", |_, args| {
        arr(args).items.insert(0, args.get(1).copied().unwrap_or(Value::Nil));
        Ok(Value::Nil)
    });
    vm.define_method(Kind::Array, "insert", |vm, args| {
        let at = int_arg(vm, args, 1, "insert's position")?;
        let a = arr(args);
        let at = clamp_pos(at, a.items.len());
        a.items.insert(at, args.get(2).copied().unwrap_or(Value::Nil));
        Ok(Value::Nil)
    });
    vm.define_method(Kind::Array, "remove", |_, args| {
        let a = arr(args);
        let x = args.get(1).copied().unwrap_or(Value::Nil);
        match a.items.iter().position(|v| ops::equal(*v, x)) {
            Some(i) => {
                a.items.remove(i);
                Ok(Value::Bool(true))
            }
            None => Ok(Value::Bool(false)),
        }
    });
    vm.define_method(Kind::Array, "indexOf", |vm, args| {
        let x = args.get(1).copied().unwrap_or(Value::Nil);
        let from = opt_int(args, 2).unwrap_or(0);
        let a = arr(args);
        let from = clamp_pos(from, a.items.len());
        let _ = vm;
        Ok(Value::Int(a.items[from..].iter().position(|v| ops::equal(*v, x)).map(|i| (i + from) as i64).unwrap_or(-1)))
    });
    vm.define_method(Kind::Array, "lastIndexOf", |_, args| {
        let x = args.get(1).copied().unwrap_or(Value::Nil);
        Ok(Value::Int(arr(args).items.iter().rposition(|v| ops::equal(*v, x)).map(|i| i as i64).unwrap_or(-1)))
    });
    vm.define_method(Kind::Array, "contains", |_, args| {
        let x = args.get(1).copied().unwrap_or(Value::Nil);
        Ok(Value::Bool(arr(args).items.iter().any(|v| ops::equal(*v, x))))
    });
    vm.define_method(Kind::Array, "concat", |vm, args| {
        let mut items = arr(args).items.clone();
        items.extend(elements(vm, args.get(1).copied().unwrap_or(Value::Nil))?);
        Ok(vm.new_array(items))
    });
    vm.define_method(Kind::Array, "copy", |vm, args| {
        let items = arr(args).items.clone();
        Ok(vm.new_array(items))
    });
    vm.define_method(Kind::Array, "slice", |vm, args| {
        let a = arr(args);
        let start = clamp_pos(opt_int(args, 1).unwrap_or(0), a.items.len());
        let end = match opt_int(args, 2) {
            Some(e) => clamp_pos(e, a.items.len()),
            None => a.items.len(),
        };
        let items = if start < end { a.items[start..end].to_vec() } else { Vec::new() };
        Ok(vm.new_array(items))
    });
    vm.define_method(Kind::Array, "splice", |vm, args| {
        let a = arr(args);
        let pos = clamp_pos(int_arg(vm, args, 1, "splice's position")?, a.items.len());
        let len = (int_arg(vm, args, 2, "splice's length")?.max(0) as usize).min(a.items.len() - pos);
        let removed: Vec<Value> = a.items.drain(pos..pos + len).collect();
        Ok(vm.new_array(removed))
    });
    vm.define_method(Kind::Array, "reverse", |_, args| {
        arr(args).items.reverse();
        Ok(Value::Nil)
    });
    vm.define_method(Kind::Array, "resize", |vm, args| {
        let n = int_arg(vm, args, 1, "resize's length")?.max(0) as usize;
        arr(args).items.resize(n, Value::Nil);
        Ok(Value::Nil)
    });
    vm.define_method(Kind::Array, "sort", |vm, args| {
        let mut items = arr(args).items.clone();
        let mut failure: Option<Value> = None;
        match args.get(1).copied() {
            Some(f) if !f.is_nil() => {
                items.sort_by(|a, b| {
                    if failure.is_some() {
                        return Ordering::Equal;
                    }
                    match vm.call(f, &[*a, *b]) {
                        Ok(Value::Int(n)) => n.cmp(&0),
                        Ok(Value::Float(n)) => n.partial_cmp(&0.0).unwrap_or(Ordering::Equal),
                        Ok(_) => Ordering::Equal,
                        Err(e) => {
                            failure = Some(e);
                            Ordering::Equal
                        }
                    }
                });
            }
            _ => items.sort_by(|a, b| default_order(*a, *b)),
        }
        if let Some(e) = failure {
            return Err(e);
        }
        arr(args).items = items;
        Ok(Value::Nil)
    });
    vm.define_method(Kind::Array, "map", |vm, args| {
        let f = args.get(1).copied().unwrap_or(Value::Nil);
        let items = arr(args).items.clone();
        let mut out = Vec::with_capacity(items.len());
        for (i, x) in items.iter().enumerate() {
            out.push(vm.call(f, &[*x, Value::Int(i as i64)])?);
        }
        Ok(vm.new_array(out))
    });
    vm.define_method(Kind::Array, "filter", |vm, args| {
        let f = args.get(1).copied().unwrap_or(Value::Nil);
        let items = arr(args).items.clone();
        let mut out = Vec::new();
        for (i, x) in items.iter().enumerate() {
            if vm.call(f, &[*x, Value::Int(i as i64)])?.truthy() {
                out.push(*x);
            }
        }
        Ok(vm.new_array(out))
    });
    vm.define_method(Kind::Array, "forEach", |vm, args| {
        let f = args.get(1).copied().unwrap_or(Value::Nil);
        let items = arr(args).items.clone();
        for (i, x) in items.iter().enumerate() {
            vm.call(f, &[*x, Value::Int(i as i64)])?;
        }
        Ok(Value::Nil)
    });
    vm.define_method(Kind::Array, "join", |vm, args| {
        let sep = match args.get(1) {
            Some(v) if !v.is_nil() => vm.display(*v),
            _ => ",".to_string(),
        };
        let s = arr(args).items.iter().map(|v| vm.display(*v)).collect::<Vec<_>>().join(&sep);
        Ok(vm.new_string(&s))
    });
    vm.define_method(Kind::Array, "iterator", |vm, args| {
        let items = arr(args).items.clone();
        Ok(vm.new_array(items))
    });
    vm.define_method(Kind::Array, "toString", |vm, args| {
        let s = vm.display(args[0]);
        Ok(vm.new_string(&s))
    });
}
