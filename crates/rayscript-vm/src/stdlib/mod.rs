//! The standard library, in Haxe's own API: the methods of arrays, strings and maps, and the modules
//! `Std`, `Math`, `StringTools`, `Lambda`, `Reflect` and `haxe.Json`. Each is registered the way a host
//! embedding the VM would register its own, so scripts import them the same way.

mod array;
pub mod json;
mod map;
mod modules;
mod string;

use crate::object::Kind;
use crate::value::Value;
use crate::vm::{NativeResult, Vm};

pub fn install(vm: &mut Vm) {
    vm.define_native("trace", |vm, args| {
        let text = args.iter().map(|a| vm.display(*a)).collect::<Vec<_>>().join(" ");
        match vm.trace_sink.clone() {
            Some(sink) => sink(&text),
            None => println!("{text}"),
        }
        Ok(Value::Nil)
    });
    vm.define_native("yield", |vm, _| {
        vm.yield_flag = true;
        Ok(Value::Nil)
    });
    vm.define_native("spawn", |vm, args| {
        let Some((f, rest)) = args.split_first() else { return Err(vm.new_string("spawn needs a function")) };
        vm.spawn(*f, rest)
    });
    vm.define_native("cancel", |vm, args| {
        if let Some(f) = args.first() {
            vm.cancel(*f);
        }
        Ok(Value::Nil)
    });
    vm.define_native("gc", |vm, _| {
        vm.collect();
        Ok(Value::Nil)
    });
    // `new Array()` and `new Map()` call these.
    vm.define_native("Array", |vm, _| Ok(vm.new_array(Vec::new())));
    vm.define_native("Map", |vm, _| Ok(vm.new_map()));
    for alias in ["StringMap", "IntMap", "ObjectMap"] {
        vm.define_native(alias, |vm, _| Ok(vm.new_map()));
    }
    array::install(vm);
    string::install(vm);
    map::install(vm);
    modules::install(vm);
    json::install(vm);
    // `toString` of the values that have no methods of their own.
    for tag in [100u8, 101, 102, 103] {
        vm.define_method_tag(tag, "toString", "toString", |vm, args| {
            let s = vm.display(args[0]);
            Ok(vm.new_string(&s))
        });
    }
    vm.define_method(Kind::Fiber, "isDone", |_, args| Ok(Value::Bool(args[0].as_obj().is_some_and(|o| o.fiber().fiber.is_finished()))));
    vm.define_method(Kind::Fiber, "cancel", |vm, args| {
        vm.cancel(args[0]);
        Ok(Value::Nil)
    });
    vm.define_method(Kind::Fiber, "result", |_, args| Ok(args[0].as_obj().map(|o| o.fiber().fiber.result).unwrap_or(Value::Nil)));
}

// Helpers for reading the arguments natives are given.

pub(crate) fn throw<T>(vm: &mut Vm, message: impl AsRef<str>) -> Result<T, Value> {
    Err(vm.new_string(message.as_ref()))
}

pub(crate) fn int_arg(vm: &mut Vm, args: &[Value], i: usize, what: &str) -> Result<i64, Value> {
    match args.get(i) {
        Some(Value::Int(n)) => Ok(*n),
        Some(Value::Float(f)) if f.fract() == 0.0 => Ok(*f as i64),
        other => throw(vm, format!("{what} needs an integer, not {}", other.map(|v| crate::vm::type_name(*v)).unwrap_or("nothing"))),
    }
}

pub(crate) fn float_arg(vm: &mut Vm, args: &[Value], i: usize, what: &str) -> Result<f64, Value> {
    match args.get(i) {
        Some(Value::Int(n)) => Ok(*n as f64),
        Some(Value::Float(f)) => Ok(*f),
        other => throw(vm, format!("{what} needs a number, not {}", other.map(|v| crate::vm::type_name(*v)).unwrap_or("nothing"))),
    }
}

pub(crate) fn str_arg<'a>(vm: &mut Vm, args: &[Value], i: usize, what: &str) -> Result<&'a str, Value> {
    match args.get(i) {
        Some(Value::Obj(o)) if o.kind() == Kind::String => Ok(o.str()),
        other => throw(vm, format!("{what} needs a string, not {}", other.map(|v| crate::vm::type_name(*v)).unwrap_or("nothing"))),
    }
}

pub(crate) fn opt_int(args: &[Value], i: usize) -> Option<i64> {
    match args.get(i) {
        Some(Value::Int(n)) => Some(*n),
        Some(Value::Float(f)) => Some(*f as i64),
        _ => None,
    }
}

pub(crate) fn new_str(vm: &mut Vm, s: &str) -> NativeResult {
    Ok(vm.new_string(s))
}

/// The elements of anything a loop can walk, copied out: an array, a range, a map's values, a string's characters.
pub(crate) fn elements(vm: &mut Vm, v: Value) -> Result<Vec<Value>, Value> {
    match v {
        Value::Obj(o) => match o.kind() {
            Kind::Array => Ok(o.array().items.clone()),
            Kind::Range => Ok((o.range().from..o.range().to).map(Value::Int).collect()),
            Kind::Map => Ok(o.map().map.iter().map(|(_, v)| v).collect()),
            Kind::String => {
                let chars: Vec<String> = o.str().chars().map(|c| c.to_string()).collect();
                Ok(chars.iter().map(|c| vm.new_string(c)).collect())
            }
            _ => throw(vm, format!("{} is not something to loop over", o.kind().name())),
        },
        other => throw(vm, format!("{} is not something to loop over", crate::vm::type_name(other))),
    }
}
