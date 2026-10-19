use rayscript_vm::{Value, Vm};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    time::Instant,
};

thread_local! { static COUNT_ALLOCATIONS: Cell<bool> = const { Cell::new(false) }; static ALLOCATIONS: Cell<usize> = const { Cell::new(0) }; }
struct TrackingAllocator;
#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = COUNT_ALLOCATIONS.try_with(|on| {
            if on.get() {
                ALLOCATIONS.with(|n| n.set(n.get() + 1));
            }
        });
        unsafe { System.alloc(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, old: Layout, size: usize) -> *mut u8 {
        let _ = COUNT_ALLOCATIONS.try_with(|on| {
            if on.get() {
                ALLOCATIONS.with(|n| n.set(n.get() + 1));
            }
        });
        unsafe { System.realloc(ptr, old, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

fn with_vm(f: impl FnOnce(&mut Vm) + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(16 << 20)
        .spawn(move || {
            let mut vm = Vm::new();
            // This fixture's symbols have the declarations used below and no external initialization requirements.
            unsafe {
                vm.register_native_library("fixture", rayscript_ffi_fixture::LIBRARY_PATH);
            }
            f(&mut vm);
        })
        .unwrap()
        .join()
        .unwrap();
}
fn bind(vm: &mut Vm, declaration: &str, name: &str) -> Value {
    vm.run_source(
        "native.rays",
        &format!("native \"fixture\" {{ {declaration} }} return {name};"),
    )
    .unwrap()
}
fn call_text(vm: &mut Vm, function: Value, args: &[Value]) -> Result<String, String> {
    vm.call(function, args)
        .map(|v| vm.display(v))
        .map_err(|v| vm.display(v))
}

#[test]
fn every_integer_width_round_trips_its_boundaries() {
    with_vm(|vm| {
        for (name, ty, low, high) in [
            ("identity_i8", "Int8", i8::MIN as i64, i8::MAX as i64),
            ("identity_i16", "Int16", i16::MIN as i64, i16::MAX as i64),
            ("identity_i32", "Int32", i32::MIN as i64, i32::MAX as i64),
            ("identity_i64", "Int64", i64::MIN, i64::MAX),
            ("identity_u8", "UInt8", 0, u8::MAX as i64),
            ("identity_u16", "UInt16", 0, u16::MAX as i64),
            ("identity_u32", "UInt32", 0, u32::MAX as i64),
        ] {
            let f = bind(vm, &format!("function {name}(n:{ty}):{ty};"), name);
            for n in [low, 0, high] {
                assert_eq!(
                    call_text(vm, f, &[Value::Int(n)]).unwrap(),
                    n.to_string(),
                    "{ty}"
                );
            }
            if high < i64::MAX {
                assert!(
                    call_text(vm, f, &[Value::Int(high + 1)])
                        .unwrap_err()
                        .contains("range")
                );
            }
            if low > i64::MIN {
                assert!(
                    call_text(vm, f, &[Value::Int(low - 1)])
                        .unwrap_err()
                        .contains("range")
                );
            }
        }
        for (name, ty) in [
            ("identity_u64", "UInt64"),
            ("identity_i64", "Int"),
            ("identity_size", "SizeT"),
        ] {
            let f = bind(vm, &format!("function {name}(n:{ty}):{ty};"), name);
            for n in [i64::MIN, -1, 0, 1, i64::MAX] {
                assert_eq!(call_text(vm, f, &[Value::Int(n)]).unwrap(), n.to_string());
            }
        }
    });
}

#[test]
fn booleans_floats_mixed_arguments_and_void_results() {
    with_vm(|vm| {
        let boolean = bind(vm, "function identity_bool(n:Bool):Bool;", "identity_bool");
        assert_eq!(
            call_text(vm, boolean, &[Value::Bool(true)]).unwrap(),
            "true"
        );
        assert_eq!(
            call_text(vm, boolean, &[Value::Bool(false)]).unwrap(),
            "false"
        );
        for (name, ty, max) in [
            ("identity_float", "Float", f32::MAX as f64),
            ("identity_double", "Double", f64::MAX),
        ] {
            let f = bind(vm, &format!("function {name}(n:{ty}):{ty};"), name);
            for n in [-max, -2.5, 0., 1.25, max, f64::INFINITY, f64::NEG_INFINITY] {
                let Value::Float(result) = vm.call(f, &[Value::Float(n)]).unwrap() else {
                    panic!("expected Float")
                };
                assert_eq!(result, n);
            }
            assert!(
                matches!(vm.call(f, &[Value::Float(f64::NAN)]).unwrap(), Value::Float(n) if n.is_nan())
            );
            assert_eq!(call_text(vm, f, &[Value::Int(3)]).unwrap(), "3");
            if ty == "Float" {
                assert!(
                    call_text(vm, f, &[Value::Float(f64::MAX)])
                        .unwrap_err()
                        .contains("range")
                );
            }
        }
        let mixed = bind(
            vm,
            "function mixed(a:Int8, b:UInt16, c:Int64, d:Float, e:Double):Double;",
            "mixed",
        );
        assert_eq!(
            call_text(
                vm,
                mixed,
                &[
                    Value::Int(-2),
                    Value::Int(60000),
                    Value::Int(-300),
                    Value::Float(1.25),
                    Value::Float(2.5)
                ]
            )
            .unwrap(),
            "59701.75"
        );
        let store = bind(vm, "function store(n:Int):Void;", "store");
        assert_eq!(call_text(vm, store, &[Value::Int(17)]).unwrap(), "null");
        let stored = bind(vm, "function stored():Int;", "stored");
        assert_eq!(call_text(vm, stored, &[]).unwrap(), "17");
    });
}

#[test]
fn strings_are_copied_both_ways_and_null_and_invalid_text_are_explicit() {
    with_vm(|vm| {
        let echo = bind(
            vm,
            "function identity_cstring(text:CString):CString;",
            "identity_cstring",
        );
        let input = vm.new_string("h\u{e9}llo \u{1f30d}");
        let first = vm.call(echo, &[input]).unwrap();
        vm.root(first);
        assert_eq!(vm.display(first), "h\u{e9}llo \u{1f30d}");
        for _ in 0..10 {
            let value = vm.new_string("replacement");
            vm.call(echo, &[value]).unwrap();
        }
        assert_eq!(vm.display(first), "h\u{e9}llo \u{1f30d}");
        assert!(matches!(vm.call(echo, &[Value::Nil]).unwrap(), Value::Nil));
        let nul = vm.new_string("a\0b");
        assert!(
            call_text(vm, echo, &[nul])
                .unwrap_err()
                .contains("interior NUL")
        );
        assert!(
            call_text(vm, echo, &[Value::Int(1)])
                .unwrap_err()
                .contains("argument 1")
        );
        let greeting = bind(vm, "function greeting():CString;", "greeting");
        assert_eq!(call_text(vm, greeting, &[]).unwrap(), "h\u{e9}llo");
        let null = bind(vm, "function null_string():CString;", "null_string");
        assert!(matches!(vm.call(null, &[]).unwrap(), Value::Nil));
        let invalid = bind(vm, "function invalid_utf8():CString;", "invalid_utf8");
        assert!(
            call_text(vm, invalid, &[])
                .unwrap_err()
                .contains("not UTF-8")
        );
        let length = bind(
            vm,
            "function string_length(s:CString):SizeT;",
            "string_length",
        );
        assert_eq!(call_text(vm, length, &[input]).unwrap(), "11");
        assert_eq!(call_text(vm, length, &[Value::Nil]).unwrap(), "0");
    });
}

#[test]
fn binding_and_call_errors_name_the_library_symbol_and_argument() {
    with_vm(|vm| {
        let error = vm
            .run_source(
                "missing.rays",
                "native \"not_registered\" { function add(a:Int, b:Int):Int; }",
            )
            .unwrap_err();
        assert!(
            error.contains("not_registered.add") && error.contains("not registered"),
            "{error}"
        );
        unsafe {
            vm.register_native_library("absent", "/no/such/rayscript-native-library");
        }
        let error = vm
            .run_source(
                "missing.rays",
                "native \"absent\" { function add(a:Int, b:Int):Int; }",
            )
            .unwrap_err();
        assert!(
            error.contains("absent.add") && error.contains("cannot load"),
            "{error}"
        );
        for (declaration, message) in [
            ("function no_such_symbol():Int;", "symbol not found"),
            (
                "function add(a:Unknown, b:Int):Int;",
                "unsupported native type `Unknown`",
            ),
            ("function add(a:Void, b:Int):Int;", "Void is only a result"),
            ("function add(a, b:Int):Int;", "each native argument needs"),
            (
                "@:blocking function add(a:Int, b:Int):Int;",
                "not available yet",
            ),
            ("function add(?a:Int, b:Int):Int;", "optional, variadic"),
            ("function add(a:Int = 1, b:Int):Int;", "optional, variadic"),
            ("function add(...a:Int):Int;", "optional, variadic"),
        ] {
            let error = vm
                .run_source(
                    "bad.rays",
                    &format!("native \"fixture\" {{ {declaration} }}"),
                )
                .unwrap_err();
            assert!(
                error.contains("fixture.") && error.contains(message),
                "{declaration}: {error}"
            );
        }
        let f = bind(vm, "function add(a:Int, b:Int):Int;", "add");
        assert!(
            call_text(vm, f, &[])
                .unwrap_err()
                .contains("fixture.add: expected 2 arguments, got 0")
        );
        assert!(
            call_text(vm, f, &[Value::Int(1), Value::Bool(false)])
                .unwrap_err()
                .contains("fixture.add: argument 2: expected Int")
        );
        let script = "native \"fixture\" { function add(a:Int, b:Int):Int; } try { add('bad', 1); } catch (error) { return error; }";
        let error = vm.run_source("catch.rays", script).unwrap();
        assert!(vm.display(error).contains("fixture.add: argument 1"));
    });
}

#[test]
fn existing_functions_keep_their_library_after_a_registration_changes() {
    with_vm(|vm| {
        let old = bind(vm, "function add(a:Int, b:Int):Int;", "add");
        unsafe {
            vm.register_native_library("fixture", "/no/such/replacement");
        }
        vm.collect();
        assert_eq!(
            call_text(vm, old, &[Value::Int(4), Value::Int(5)]).unwrap(),
            "9"
        );
        assert!(
            vm.run_source(
                "new.rays",
                "native \"fixture\" { function add(a:Int, b:Int):Int; }"
            )
            .unwrap_err()
            .contains("cannot load")
        );
    });
}

#[test]
fn repeated_scalar_calls_do_not_allocate() {
    with_vm(|vm| {
        let f = bind(vm, "function add(a:Int, b:Int):Int;", "add");
        let args = [Value::Int(1), Value::Int(2)];
        ALLOCATIONS.with(|n| n.set(0));
        COUNT_ALLOCATIONS.with(|on| on.set(true));
        for _ in 0..1000 {
            std::hint::black_box(vm.call(f, &args).unwrap());
        }
        COUNT_ALLOCATIONS.with(|on| on.set(false));
        assert_eq!(ALLOCATIONS.with(Cell::get), 0);
    });
}

#[test]
#[ignore = "release-mode native call overhead benchmark"]
fn scalar_call_overhead() {
    with_vm(|vm| {
        let foreign = bind(vm, "function add(a:Int, b:Int):Int;", "add");
        let native = vm.native("rust_add", |_, args| match args {
            [Value::Int(a), Value::Int(b)] => Ok(Value::Int(a.wrapping_add(*b))),
            _ => unreachable!(),
        });
        let args = [Value::Int(1), Value::Int(2)];
        let mut time = |f| {
            let start = Instant::now();
            for _ in 0..1_000_000 {
                std::hint::black_box(vm.call(f, std::hint::black_box(&args)).unwrap());
            }
            start.elapsed().as_secs_f64() * 1e9 / 1_000_000.
        };
        let mut deltas = Vec::new();
        for _ in 0..7 {
            let rust = time(native);
            let ffi = time(foreign);
            println!(
                "Rust {rust:.1} ns, FFI {ffi:.1} ns, overhead {:.1} ns",
                ffi - rust
            );
            deltas.push(ffi - rust);
        }
        deltas.sort_by(f64::total_cmp);
        println!("median overhead {:.1} ns", deltas[deltas.len() / 2]);
    });
}
