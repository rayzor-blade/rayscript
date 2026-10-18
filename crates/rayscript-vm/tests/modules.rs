use rayscript_vm::{ModuleResolver, Resolved, Value, Vm};
use std::cell::Cell;
use std::rc::Rc;

fn run_in(vm: &mut Vm, src: &str) -> String {
    match vm.run_source("main.rays", src) {
        Ok(v) => vm.display(v),
        Err(e) => format!("ERROR {}", e.lines().next().unwrap_or("")),
    }
}

fn run(src: &str) -> String {
    run_in(&mut Vm::new(), src)
}

#[test]
fn the_standard_library_is_imported_like_any_module() {
    assert_eq!(run("import haxe.Json; return Json.stringify({a: [1, 2], b: 'x'});"), "{\"a\":[1,2],\"b\":\"x\"}");
    assert_eq!(run("return Math.floor(2.7) + Std.parseInt('12');"), "14");
    assert_eq!(run("import Math.floor; return floor(9.9);"), "9");
    assert_eq!(run("import Std as S; return S.string(5) + '!';"), "5!");
    assert_eq!(run("import StringTools.*; return trim('  hi ') + lpad('7', '0', 3);"), "hi007");
    assert_eq!(run("return Lambda.fold([1, 2, 3, 4], (x, acc) -> acc + x, 0);"), "10");
    assert_eq!(run("return Reflect.fields({a: 1, b: 2}).join('+');"), "a+b");
    assert_eq!(run("var o = haxe.Json.parse('{\"a\": [1, 2.5, true, null, \"s\"]}'); return o.a;"), "[1,2.5,true,null,s]");
}

#[test]
fn json_round_trips() {
    assert_eq!(run("return Json.stringify(Json.parse('[{\"k\": 1}, {\"k\": 2}]'));"), "[{\"k\":1},{\"k\":2}]");
    assert_eq!(run("return Json.stringify({a: 1}, null, '  ');"), "{\n  \"a\": 1\n}");
    assert!(run("return Json.parse('{bad');").starts_with("ERROR Error: invalid JSON"));
}

struct Scripts;

impl ModuleResolver for Scripts {
    fn resolve(&mut self, vm: &mut Vm, path: &str) -> Option<Resolved> {
        match path {
            "util.math" => Some(Resolved::Script { name: "util/math.rays".into(), source: "function double(x) { return x * 2; }\nvar K = 5;\nvar count = 1;".into() }),
            "util.broken" => Some(Resolved::Script { name: "util/broken.rays".into(), source: "var = ;".into() }),
            "util.a" => Some(Resolved::Script { name: "util/a.rays".into(), source: "import util.b; var x = 1;".into() }),
            "util.b" => Some(Resolved::Script { name: "util/b.rays".into(), source: "import util.a; var y = 2;".into() }),
            "ashui.ui" => {
                // A host module made on demand, as a bridge to another process would be.
                let o = vm.new_object();
                let f = vm.native("ashui.ui.Button", |vm, args| {
                    let label = vm.display(args.first().copied().unwrap_or(Value::Nil));
                    Ok(vm.new_string(&format!("<button {label}>")))
                });
                vm.set_member(o, "Button", f);
                Some(Resolved::Value(o))
            }
            _ => None,
        }
    }
}

#[test]
fn a_script_module_is_imported_from_what_the_host_supplies() {
    let mut vm = Vm::new();
    vm.set_resolver(Box::new(Scripts));
    assert_eq!(run_in(&mut vm, "import util.math; return math.double(math.K);"), "10");
    assert_eq!(run_in(&mut vm, "import util.math as m; return m.double(4);"), "8");
    assert_eq!(run_in(&mut vm, "import util.math.*; return double(K);"), "10");
    // Each module keeps its own names.
    assert_eq!(run_in(&mut vm, "import util.math; var count = 100; return count + math.count;"), "101");
    // A module runs once, however often it is imported.
    assert_eq!(run_in(&mut vm, "import util.math; math.count = 50; return 0;"), "0");
    assert_eq!(run_in(&mut vm, "import util.math; return math.count;"), "50");
}

#[test]
fn a_host_module_comes_from_the_resolver_on_demand() {
    let mut vm = Vm::new();
    vm.set_resolver(Box::new(Scripts));
    assert_eq!(run_in(&mut vm, "import ashui.ui.Button; return Button('ok');"), "<button ok>");
    assert_eq!(run_in(&mut vm, "import ashui.ui; return ui.Button('x');"), "<button x>");
}

#[test]
fn import_errors_say_what_is_wrong() {
    let mut vm = Vm::new();
    vm.set_resolver(Box::new(Scripts));
    assert_eq!(run_in(&mut vm, "import nope.Thing; return 1;"), "ERROR Error: cannot find the module `nope.Thing`");
    let e = run_in(&mut vm, "import util.broken; return 1;");
    assert!(e.starts_with("ERROR Error: Error: expected a variable name"), "{e}");
    let e = run_in(&mut vm, "import util.a; return 1;");
    assert!(e.contains("imports itself"), "{e}");
}

#[test]
fn a_host_function_may_hold_state() {
    let mut vm = Vm::new();
    let hits = Rc::new(Cell::new(0));
    let h = hits.clone();
    vm.define_native("hit", move |_, _| {
        h.set(h.get() + 1);
        Ok(Value::Int(h.get()))
    });
    assert_eq!(run_in(&mut vm, "hit(); hit(); return hit();"), "3");
    assert_eq!(hits.get(), 3);
}

#[test]
fn a_host_object_is_a_value_a_script_holds() {
    let mut vm = Vm::new();
    let obj = vm.new_host(7, Box::new(String::from("editor buffer")));
    vm.define_global("buffer", obj);
    assert_eq!(run_in(&mut vm, "return buffer;"), "<host object>");
    assert_eq!(vm.global("buffer").unwrap().as_obj().unwrap().host().type_id, 7);
}
