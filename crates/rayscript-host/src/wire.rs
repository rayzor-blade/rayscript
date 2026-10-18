//! How operations, errors and values are written on the wire: one JSON value to a line.

use rayscript_vm::stdlib::json::quote;
use rayscript_vm::view::{PropValue, ViewOp};
use rayscript_vm::{Value, Vm};

fn prop(v: &PropValue, out: &mut String) {
    match v {
        PropValue::Nil => out.push_str("null"),
        PropValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        PropValue::Int(i) => out.push_str(&i.to_string()),
        PropValue::Float(f) if f.is_finite() => out.push_str(&f.to_string()),
        PropValue::Float(_) => out.push_str("null"),
        PropValue::Str(s) => quote(s, out),
        PropValue::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                prop(item, out);
            }
            out.push(']');
        }
        PropValue::Map(fields) => {
            out.push('{');
            for (i, (k, v)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                quote(k, out);
                out.push(':');
                prop(v, out);
            }
            out.push('}');
        }
    }
}

/// One operation as a JSON array: its name, then its arguments.
pub fn encode_op(op: &ViewOp, out: &mut String) {
    match op {
        ViewOp::Create { id, tag } => {
            out.push_str(&format!("[\"create\",{id},"));
            quote(tag, out);
        }
        ViewOp::CreateText { id, text } => {
            out.push_str(&format!("[\"createText\",{id},"));
            quote(text, out);
        }
        ViewOp::SetText { id, text } => {
            out.push_str(&format!("[\"setText\",{id},"));
            quote(text, out);
        }
        ViewOp::SetProp { id, name, value } => {
            out.push_str(&format!("[\"set\",{id},"));
            quote(name, out);
            out.push(',');
            prop(value, out);
        }
        ViewOp::RemoveProp { id, name } => {
            out.push_str(&format!("[\"unset\",{id},"));
            quote(name, out);
        }
        ViewOp::Listen { id, event } => {
            out.push_str(&format!("[\"listen\",{id},"));
            quote(event, out);
        }
        ViewOp::Unlisten { id, event } => {
            out.push_str(&format!("[\"unlisten\",{id},"));
            quote(event, out);
        }
        ViewOp::Insert { parent, child, before } => match before {
            Some(b) => out.push_str(&format!("[\"insert\",{parent},{child},{b}")),
            None => out.push_str(&format!("[\"insert\",{parent},{child},null")),
        },
        ViewOp::Remove { id } => out.push_str(&format!("[\"remove\",{id}")),
    }
    out.push(']');
}

/// `{"t":"ops","ops":[...]}`
pub fn ops_line(ops: &[ViewOp]) -> String {
    let mut out = String::with_capacity(32 + ops.len() * 24);
    out.push_str("{\"t\":\"ops\",\"ops\":[");
    for (i, op) in ops.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        encode_op(op, &mut out);
    }
    out.push_str("]}");
    out
}

fn prop_of(vm: &Vm, v: Value) -> PropValue {
    match v {
        Value::Nil | Value::Native(_) => PropValue::Nil,
        Value::Bool(b) => PropValue::Bool(b),
        Value::Int(i) => PropValue::Int(i),
        Value::Float(f) => PropValue::Float(f),
        Value::Obj(o) => match o.kind() {
            rayscript_vm::object::Kind::String => PropValue::Str(o.str().to_string()),
            rayscript_vm::object::Kind::Array => PropValue::List(o.array().items.iter().map(|x| prop_of(vm, *x)).collect()),
            rayscript_vm::object::Kind::Object => PropValue::Map(vm.object_fields(v).into_iter().map(|(k, x)| (k, prop_of(vm, x))).collect()),
            _ => PropValue::Nil,
        },
    }
}

/// The operations of an `ops` message, read back: for a client written in Rust, and for tests.
pub fn decode_ops(vm: &mut Vm, ops: Value) -> Result<Vec<ViewOp>, String> {
    let Value::Obj(list) = ops else { return Err("ops is not a list".into()) };
    if list.kind() != rayscript_vm::object::Kind::Array {
        return Err("ops is not a list".into());
    }
    let mut out = Vec::new();
    for item in list.array().items.clone() {
        let Value::Obj(a) = item else { return Err("an operation is not a list".into()) };
        if a.kind() != rayscript_vm::object::Kind::Array {
            return Err("an operation is not a list".into());
        }
        let parts = a.array().items.clone();
        let name = vm.string_of(parts.first().copied().unwrap_or(Value::Nil)).unwrap_or("").to_string();
        let int = |i: usize| match parts.get(i) {
            Some(Value::Int(n)) => Ok(*n as u32),
            _ => Err(format!("`{name}` needs a number as argument {i}")),
        };
        let text = |vm: &Vm, i: usize| match parts.get(i).and_then(|v| vm.string_of(*v)) {
            Some(s) => Ok(s.to_string()),
            None => Err(format!("`{name}` needs text as argument {i}")),
        };
        out.push(match name.as_str() {
            "create" => ViewOp::Create { id: int(1)?, tag: text(vm, 2)? },
            "createText" => ViewOp::CreateText { id: int(1)?, text: text(vm, 2)? },
            "setText" => ViewOp::SetText { id: int(1)?, text: text(vm, 2)? },
            "set" => ViewOp::SetProp { id: int(1)?, name: text(vm, 2)?, value: prop_of(vm, parts.get(3).copied().unwrap_or(Value::Nil)) },
            "unset" => ViewOp::RemoveProp { id: int(1)?, name: text(vm, 2)? },
            "listen" => ViewOp::Listen { id: int(1)?, event: text(vm, 2)? },
            "unlisten" => ViewOp::Unlisten { id: int(1)?, event: text(vm, 2)? },
            "insert" => ViewOp::Insert { parent: int(1)?, child: int(2)?, before: if matches!(parts.get(3), Some(Value::Int(_))) { Some(int(3)?) } else { None } },
            "remove" => ViewOp::Remove { id: int(1)? },
            other => return Err(format!("unknown operation `{other}`")),
        });
    }
    Ok(out)
}
