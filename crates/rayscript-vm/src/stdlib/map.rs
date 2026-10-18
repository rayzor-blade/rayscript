use super::*;
use crate::object::MapObj;

fn m<'a>(args: &[Value]) -> &'a mut MapObj {
    args[0].as_obj().expect("a method of Map is called on a map").map()
}

pub fn install(vm: &mut Vm) {
    vm.define_method(Kind::Map, "get", |_, args| Ok(m(args).map.get(args.get(1).copied().unwrap_or(Value::Nil)).unwrap_or(Value::Nil)));
    vm.define_method(Kind::Map, "set", |_, args| {
        m(args).map.set(args.get(1).copied().unwrap_or(Value::Nil), args.get(2).copied().unwrap_or(Value::Nil));
        Ok(Value::Nil)
    });
    vm.define_method(Kind::Map, "exists", |_, args| Ok(Value::Bool(m(args).map.contains(args.get(1).copied().unwrap_or(Value::Nil)))));
    vm.define_method(Kind::Map, "remove", |_, args| Ok(Value::Bool(m(args).map.remove(args.get(1).copied().unwrap_or(Value::Nil)))));
    vm.define_method(Kind::Map, "clear", |_, args| {
        m(args).map.clear();
        Ok(Value::Nil)
    });
    // Haxe's `keys()` and `iterator()` give iterators; here an array a `for` loop walks the same way.
    vm.define_method(Kind::Map, "keys", |vm, args| {
        let ks = m(args).map.iter().map(|(k, _)| k).collect();
        Ok(vm.new_array(ks))
    });
    vm.define_method(Kind::Map, "values", |vm, args| {
        let vs = m(args).map.iter().map(|(_, v)| v).collect();
        Ok(vm.new_array(vs))
    });
    vm.define_method(Kind::Map, "iterator", |vm, args| {
        let vs = m(args).map.iter().map(|(_, v)| v).collect();
        Ok(vm.new_array(vs))
    });
    vm.define_method(Kind::Map, "copy", |vm, args| {
        let out = vm.new_map();
        let entries: Vec<(Value, Value)> = m(args).map.iter().collect();
        for (k, v) in entries {
            out.as_obj().unwrap().map().map.set(k, v);
        }
        Ok(out)
    });
    vm.define_method(Kind::Map, "toString", |vm, args| {
        let s = vm.display(args[0]);
        Ok(vm.new_string(&s))
    });
}
