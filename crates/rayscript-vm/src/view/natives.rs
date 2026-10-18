//! What scripts call: the reactive primitives, mounting, and the functions markup compiles to.

use super::reactive::Role;
use super::template::Tpl;
use crate::object::Kind;
use crate::stdlib::{str_arg, throw};
use crate::value::Value;
use crate::vm::{NativeResult, Vm};

fn is_callable(v: Value) -> bool {
    matches!(v, Value::Native(_)) || matches!(v, Value::Obj(o) if o.kind() == Kind::Closure)
}

fn function_arg(vm: &mut Vm, args: &[Value], i: usize, what: &str) -> Result<Value, Value> {
    match args.get(i) {
        Some(v) if is_callable(*v) => Ok(*v),
        other => throw(vm, format!("{what} needs a function, not {}", other.map(|v| crate::vm::type_name(*v)).unwrap_or("nothing"))),
    }
}

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).copied().unwrap_or(Value::Nil)
}

pub(super) fn install(vm: &mut Vm) {
    vm.define_native("signal", |vm, args| vm.new_signal(arg(args, 0)));
    vm.define_native("computed", |vm, args| {
        let func = function_arg(vm, args, 0, "computed")?;
        Ok(Value::Obj(vm.new_reactive(Role::Computed { func }, true)?))
    });
    vm.define_native("effect", |vm, args| {
        let func = function_arg(vm, args, 0, "effect")?;
        let r = vm.new_reactive(Role::Effect { func }, true)?;
        // It runs once now, to find out what it reads.
        if let Err(e) = vm.update_if_necessary(r) {
            vm.dispose(r);
            return Err(e);
        }
        Ok(Value::Obj(r))
    });
    vm.define_native("untrack", |vm, args| {
        let func = function_arg(vm, args, 0, "untrack")?;
        vm.untracked(|vm| vm.call(func, &[]))
    });
    vm.define_native("onCleanup", |vm, args| {
        let func = function_arg(vm, args, 0, "onCleanup")?;
        match vm.view.owner {
            Some(owner) => {
                owner.reactive().cleanups.push(func);
                Ok(Value::Nil)
            }
            None => throw(vm, "onCleanup must be called inside a component or an effect"),
        }
    });
    vm.define_native("flush", |vm, _| {
        vm.view_flush();
        Ok(Value::Nil)
    });
    vm.define_native("mount", |vm, args| {
        let name = if args.len() > 1 { str_arg(vm, args, 1, "mount")?.to_string() } else { "main".to_string() };
        let root = vm.view_mount(arg(args, 0), &name)?;
        Ok(Value::Int(root as i64))
    });
    vm.define_native("unmount", |vm, args| match arg(args, 0) {
        Value::Int(id) => Ok(Value::Bool(vm.view_unmount(id as u32))),
        _ => throw(vm, "unmount requires the id mount returned"),
    });

    // What markup compiles to.
    vm.define_native("__el", |vm, args| vm.new_template(Tpl::Element { tag: arg(args, 0), statics: arg(args, 1), dynamics: arg(args, 2), children: arg(args, 3) }));
    vm.define_native("__frag", |vm, args| vm.new_template(Tpl::Fragment { children: arg(args, 0) }));
    vm.define_native("__hole", |vm, args| vm.new_template(Tpl::Hole { func: arg(args, 0) }));
    vm.define_native("__if", |vm, args| vm.new_template(Tpl::If { cond: arg(args, 0), then: arg(args, 1), els: arg(args, 2) }));
    vm.define_native("__for", |vm, args| vm.new_template(Tpl::For { iter: arg(args, 0), body: arg(args, 1) }));

    vm.define_method(Kind::Signal, "get", |vm, args| Ok(vm.signal_get(args[0].as_obj().unwrap())));
    vm.define_method(Kind::Signal, "peek", |_, args| Ok(args[0].as_obj().unwrap().signal().value));
    vm.define_method(Kind::Signal, "set", |vm, args| {
        vm.signal_set(args[0].as_obj().unwrap(), arg(args, 1));
        Ok(arg(args, 1))
    });
    vm.define_method(Kind::Signal, "update", |vm, args| {
        let s = args[0].as_obj().unwrap();
        let func = function_arg(vm, args, 1, "update")?;
        let next = vm.call(func, &[s.signal().value])?;
        vm.signal_set(s, next);
        Ok(next)
    });
    vm.define_method(Kind::Signal, "notify", |vm, args| {
        vm.signal_notify(args[0].as_obj().unwrap());
        Ok(Value::Nil)
    });
    vm.define_method(Kind::Reactive, "get", |vm, args| vm.reactive_get(args[0].as_obj().unwrap(), true));
    vm.define_method(Kind::Reactive, "peek", |vm, args| vm.reactive_get(args[0].as_obj().unwrap(), false));
    vm.define_method(Kind::Reactive, "dispose", |vm, args| -> NativeResult {
        vm.dispose(args[0].as_obj().unwrap());
        Ok(Value::Nil)
    });
}
