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

const HANDLES: &str = r#"
native "fixture" {
    @:free(close_session) handle Session;
    @:free(free_record) handle Record;
    function open_session(out:Out<Session>, id:Int64):Int32;
    function close_session(session:Session):Void;
    @:owner(session) function get_record(out:Out<Record>, session:Session, id:Int64):Int32;
    @:owner(session) function make_record(session:Session, id:Int64):Record;
    function free_record(record:Record):Void;
    function session_id(session:Session):Int64;
    function record_value(record:Record):Int64;
    function freed_count():SizeT;
    function freed_at(index:SizeT):Int64;
    function reset_freed():Void;
    @:owner(session) function session_ptr(session:Session):Ptr;
}
reset_freed();
function session(id) {
    var out = Out(Session);
    if (open_session(out, id) != 0) throw "open failed";
    return out.value;
}
"#;

fn run_handles(vm: &mut Vm, body: &str) -> Value {
    vm.run_source("handles.rays", &format!("{HANDLES}\n{body}"))
        .unwrap()
}

#[test]
fn typed_output_slots_cover_scalars_strings_and_pointers() {
    with_vm(|vm| {
        for (name, ty, literal, expected) in [
            ("out_i8", "Int8", "-128", "-128"),
            ("out_i16", "Int16", "-32768", "-32768"),
            ("out_i32", "Int32", "-2147483648", "-2147483648"),
            (
                "out_i64",
                "Int64",
                "-9223372036854775807",
                "-9223372036854775807",
            ),
            ("out_u8", "UInt8", "255", "255"),
            ("out_u16", "UInt16", "65535", "65535"),
            ("out_u32", "UInt32", "4294967295", "4294967295"),
            ("out_u64", "UInt64", "-1", "-1"),
            ("out_size", "SizeT", "-1", "-1"),
            ("out_bool", "Bool", "true", "true"),
            ("out_float", "Float", "1.25", "1.25"),
            ("out_double", "Double", "-2.5", "-2.5"),
        ] {
            let source = format!(
                r#"native "fixture" {{ function {name}(out:Out<{ty}>, value:{ty}):Void; }}
                var out = Out({ty}); {name}(out, {literal}); gc(); return out.value;"#
            );
            let value = vm.run_source("outputs.rays", &source).unwrap();
            assert_eq!(vm.display(value), expected, "{ty}");
        }
        let value = vm.run_source("pointer.rays", r#"
            native "fixture" {
                function static_ptr():Ptr;
                function identity_ptr(p:Ptr):Ptr;
                function read_ptr(p:Ptr):Int64;
                function out_ptr(out:Out<Ptr>):Void;
                function out_string(out:Out<CString>):Void;
            }
            var p = Out(Ptr); out_ptr(p);
            var s = Out(CString); out_string(s); gc();
            return [read_ptr(p.value), read_ptr(identity_ptr(static_ptr())), identity_ptr(null), s.value];
        "#).unwrap();
        assert_eq!(vm.display(value), "[12345,12345,null,output]");
        let error = vm
            .run_source(
                "bad-output.rays",
                r#"native "fixture" { function out_bad_string(out:Out<CString>):Void; }
            out_bad_string(Out(CString));"#,
            )
            .unwrap_err();
        assert!(error.contains("not UTF-8"), "{error}");
    });
}

#[test]
fn bytes_are_fixed_size_checked_buffers_and_native_writes_are_visible() {
    with_vm(|vm| {
        let value = vm.run_source("bytes.rays", r#"
            native "fixture" {
                function fill_bytes(bytes:Bytes, len:SizeT, value:UInt8):Void;
                function sum_bytes(bytes:Ptr, len:SizeT):UInt64;
            }
            var bytes = Bytes(3);
            fill_bytes(bytes, bytes.length, 65);
            var copy = bytes.copy(); bytes[1] = 66; gc();
            return [bytes.toString(), copy.toString(), sum_bytes(bytes, bytes.length), Bytes([0,255])[1], Bytes('é').length, bytes[-1], bytes[3]];
        "#).unwrap();
        assert_eq!(vm.display(value), "[ABA,AAA,196,255,2,null,null]");
        for source in [
            "Bytes(-1);",
            "Bytes([256]);",
            "Bytes([true]);",
            "Bytes(2)[2] = 1;",
            "Bytes(2)[-1] = 1;",
            "Bytes(2)[0] = -1;",
            "Bytes(2)[0] = 256;",
            "Bytes(2)[0] = true;",
            "Bytes([255]).toString();",
        ] {
            assert!(vm.run_source("bad-bytes.rays", source).is_err(), "{source}");
        }
    });
}

#[test]
fn closing_an_owner_frees_children_first_and_invalidates_every_alias() {
    with_vm(|vm| {
        let value = run_handles(
            vm,
            r#"
            var s = session(7);
            var out = Out(Record); get_record(out, s, 3);
            var child = out.value;
            var pointer = session_ptr(s);
            var before = record_value(child);
            s.close(); s.close(); close_session(s);
            var errors = 0;
            try { record_value(child); } catch (e) { errors += 1; }
            try { session_id(s); } catch (e) { errors += 1; }
            try { session_ptr(s); } catch (e) { errors += 1; }
            native "fixture" { function identity_ptr(p:Ptr):Ptr; }
            try { identity_ptr(pointer); } catch (e) { errors += 1; }
            return [before, freed_count(), freed_at(0), freed_at(1), s == null, child == null, !s, !pointer, errors];
        "#,
        );
        assert_eq!(vm.display(value), "[703,2,703,7,true,true,true,true,4]");
    });
}

#[test]
fn a_child_traces_its_owner_and_collection_frees_child_before_parent() {
    with_vm(|vm| {
        let record = run_handles(
            vm,
            r#"
            function child() {
                var s = session(8);
                var out = Out(Record); get_record(out, s, 4);
                return out.value;
            }
            return child();
        "#,
        );
        vm.define_global("kept", record);
        vm.collect();
        let count = vm.module_global(0, "freed_count").unwrap();
        let read = vm.module_global(0, "record_value").unwrap();
        assert_eq!(call_text(vm, count, &[]).unwrap(), "0");
        assert_eq!(call_text(vm, read, &[record]).unwrap(), "804");
        vm.define_global("kept", Value::Nil);
        vm.collect();
        assert_eq!(call_text(vm, count, &[]).unwrap(), "2");
        let at = vm.module_global(0, "freed_at").unwrap();
        assert_eq!(call_text(vm, at, &[Value::Int(0)]).unwrap(), "804");
        assert_eq!(call_text(vm, at, &[Value::Int(1)]).unwrap(), "8");
    });
}

#[test]
fn direct_free_and_out_reuse_share_the_same_handle_lifetime() {
    with_vm(|vm| {
        let value = run_handles(
            vm,
            r#"
            var out = Out(Session); open_session(out, 2);
            var first = out.value;
            var errors = 0;
            try { open_session(out, 3); } catch (e) { errors += 1; }
            close_session(first);
            open_session(out, 3);
            var second = out.value;
            var r = make_record(second, 5);
            free_record(r); r.close();
            close_session(second);
            var failed = Out(Session); var code = open_session(failed, -1);
            return [errors, first == null, second == null, r == null, freed_count(), freed_at(0), freed_at(1), freed_at(2), code, failed.value];
        "#,
        );
        assert_eq!(vm.display(value), "[1,true,true,true,3,2,305,3,-1,null]");
    });
}

#[test]
fn aliases_of_a_returned_handle_are_freed_once() {
    with_vm(|vm| {
        let value = run_handles(
            vm,
            r#"
            native "fixture" { function identity_ptr(value:Session):Session; }
            var s = session(9);
            var alias = identity_ptr(s);
            alias.close(); s.close();
            return [s == null, freed_count()];
        "#,
        );
        assert_eq!(vm.display(value), "[true,1]");
    });
}

#[test]
fn malformed_handle_declarations_and_output_type_mismatches_are_errors() {
    with_vm(|vm| {
        for source in [
            r#"native "fixture" { handle Session; }"#,
            r#"native "fixture" { @:free(12) handle Session; }"#,
            r#"native "fixture" { @:free(close_session) handle Session; }"#,
            r#"native "fixture" { @:owner(nope) function identity_ptr(p:Ptr):Ptr; }"#,
            r#"native "fixture" { @:owner(p) function identity_ptr(p:Ptr):Ptr; }"#,
            r#"native "fixture" { function out_i32(out:Out<Out<Int32>>, value:Int32):Void; }"#,
            "Out(Void);",
            "Out(Bytes);",
        ] {
            assert!(
                vm.run_source("bad-native.rays", source).is_err(),
                "{source}"
            );
        }
        for source in [
            r#"native "fixture" { function out_i32(out:Out<Int32>, value:Int32):Void; } out_i32(Out(Int64), 1);"#,
            r#"native "fixture" { function identity_ptr(p:Ptr):Ptr; } identity_ptr(123);"#,
            r#"native "fixture" { function sum_bytes(b:Bytes, len:SizeT):UInt64; } sum_bytes('abc', 3);"#,
        ] {
            let error = vm.run_source("bad-call.rays", source).unwrap_err();
            assert!(error.contains("argument 1"), "{error}");
        }
        let error = vm
            .run_source(
                "bad-handle.rays",
                &format!("{HANDLES}\nrecord_value(session(1));"),
            )
            .unwrap_err();
        assert!(error.contains("expected handle fixture.Record"), "{error}");
    });
}

#[test]
fn finalizers_retain_the_original_library_after_registration_changes_and_vm_drop() {
    std::thread::Builder::new()
        .stack_size(16 << 20)
        .spawn(|| {
            let mut observer = Vm::new();
            unsafe {
                observer.register_native_library("fixture", rayscript_ffi_fixture::LIBRARY_PATH);
            }
            let count = bind(
                &mut observer,
                "function freed_count():SizeT;",
                "freed_count",
            );
            {
                let mut vm = Vm::new();
                unsafe {
                    vm.register_native_library("fixture", rayscript_ffi_fixture::LIBRARY_PATH);
                }
                run_handles(
                    &mut vm,
                    "var s = session(6); var child = make_record(s, 2);",
                );
                unsafe {
                    vm.register_native_library("fixture", "/no/such/replacement");
                }
            }
            assert_eq!(call_text(&mut observer, count, &[]).unwrap(), "2");
            let at = bind(
                &mut observer,
                "function freed_at(index:SizeT):Int64;",
                "freed_at",
            );
            assert_eq!(
                call_text(&mut observer, at, &[Value::Int(0)]).unwrap(),
                "602"
            );
            assert_eq!(call_text(&mut observer, at, &[Value::Int(1)]).unwrap(), "6");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn copying_native_data_and_borrowed_pointer_ownership_are_explicit() {
    with_vm(|vm| {
        let value = vm.run_source("copy.rays", r#"
            native "fixture" { function greeting():Ptr; }
            var source = Bytes('abc');
            return [Bytes(greeting(), 6).toString(), Bytes(source, 2).toString(), Bytes(null, 0).length];
        "#).unwrap();
        assert_eq!(vm.display(value), "[héllo,ab,0]");
        for source in ["Bytes(null, 1);", "Bytes(null, -1);", "Bytes(Bytes(2), 3);"] {
            assert!(vm.run_source("bad-copy.rays", source).is_err(), "{source}");
        }
        let pointer = run_handles(
            vm,
            "function borrowed() { return session_ptr(session(12)); } return borrowed();",
        );
        vm.define_global("borrowed", pointer);
        vm.collect();
        let count = bind(vm, "function freed_count():SizeT;", "freed_count");
        assert_eq!(call_text(vm, count, &[]).unwrap(), "0");
        let read = bind(vm, "function read_ptr(p:Ptr):Int64;", "read_ptr");
        assert_eq!(call_text(vm, read, &[pointer]).unwrap(), "12");
        vm.define_global("borrowed", Value::Nil);
        vm.collect();
        assert_eq!(call_text(vm, count, &[]).unwrap(), "1");
    });
}

#[test]
fn aliased_buffers_are_valid_and_aliased_output_slots_are_rejected() {
    with_vm(|vm| {
        let value = vm
            .run_source(
                "aliases.rays",
                r#"
            native "fixture" {
                function copy_bytes(out:Bytes, source:Bytes, length:SizeT):Void;
                function two_outputs(a:Out<Int32>, b:Out<Int32>):Void;
            }
            var bytes = Bytes('test'); copy_bytes(bytes, bytes, bytes.length);
            var a = Out(Int32); var b = Out(Int32); two_outputs(a, b);
            var error = false;
            try { two_outputs(a, a); } catch (e) { error = true; }
            return [bytes.toString(), a.value, b.value, error];
        "#,
            )
            .unwrap();
        assert_eq!(vm.display(value), "[test,1,2,true]");
    });
}

#[test]
fn an_output_decode_error_still_wraps_other_owned_outputs() {
    with_vm(|vm| {
        let value = run_handles(
            vm,
            r#"
            native "fixture" {
                function handle_and_bad_string(text:Out<CString>, out:Out<Session>):Void;
            }
            var text = Out(CString); var out = Out(Session);
            var error = false;
            try { handle_and_bad_string(text, out); } catch (e) { error = true; }
            var id = session_id(out.value);
            out.value.close();
            return [error, text.value, id, freed_count()];
        "#,
        );
        assert_eq!(vm.display(value), "[true,null,15,1]");
    });
}

const STRUCTS: &str = r#"
native "fixture" {
    struct Nested { tag:UInt8; pair:Pair; tail:UInt16; }
    struct Pair { x:Int32; y:Double; }
    struct Node { value:Int64; next:Ptr<Node>; name:CString; }
    struct Fields {
        i8_:Int8; u8_:UInt8; i16_:Int16; u16_:UInt16;
        i32_:Int32; u32_:UInt32; i64_:Int64; u64_:UInt64;
        size:SizeT; flag:Bool; float:Float; double:Double;
        text:CString; data:Ptr;
    }
    function pair_size():SizeT;
    function pair_align():SizeT;
    function nested_size():SizeT;
    function nested_align():SizeT;
    function fields_size():SizeT;
    function fields_align():SizeT;
    function pair_value(pair:Pair):Double;
    function fill_pair(pair:Pair, x:Int32, y:Double):Void;
    function fill_nested(n:Nested):Void;
    function nested_value(n:Nested):Double;
    function fill_pairs(pairs:Pair, count:SizeT):Void;
    function pairs_value(pairs:Pair, count:SizeT):Double;
    function fill_fields(fields:Fields):Void;
    function check_fields(fields:Fields):Bool;
    function static_ptr():Ptr;
    function node_value(node:Node):Int64;
    function node_name(node:Node):CString;
    function set_node_name(node:Node):Void;
    function static_pair():Pair;
    function out_pair(out:Out<Pair>):Void;
    function unaligned_pair():Pair;
}
"#;

fn run_structs(vm: &mut Vm, body: &str) -> Value {
    vm.run_source("structs.rays", &format!("{STRUCTS}\n{body}"))
        .unwrap()
}

#[test]
fn c_struct_layout_padding_and_nested_fields_match_the_fixture() {
    with_vm(|vm| {
        let value = run_structs(
            vm,
            r#"
            var p = alloc(Pair);
            var zero = [p.x, p.y];
            p.x = 12; p.y = 0.5;
            var before = pair_value(p);
            fill_pair(p, 3, 4.5);
            var nested = alloc(Nested);
            fill_nested(nested);
            var fromC = [nested.tag, nested.pair.x, nested.pair.y, nested.tail];
            nested.pair = p; nested.tag = 1; nested.tail = 2;
            nested.pair.x = 8;
            return [Pair.size == pair_size(), Pair.alignment == pair_align(), Nested.size == nested_size(), Nested.alignment == nested_align(), zero, before, fromC, nested_value(nested), p.x];
        "#,
        );
        assert_eq!(
            vm.display(value),
            "[true,true,true,true,[0,0],12.5,[2,3,4.5,6],15.5,3]"
        );
    });
}

#[test]
fn every_scalar_struct_field_round_trips_through_c() {
    with_vm(|vm| {
        let value = run_structs(
            vm,
            r#"
            var f = alloc(Fields); fill_fields(f);
            var c = [f.i8_, f.u8_, f.i16_, f.u16_, f.i32_, f.u32_, f.i64_, f.u64_, f.size, f.flag, f.float, f.double, f.text];
            f.i8_ = -128; f.u8_ = 255; f.i16_ = -32768; f.u16_ = 65535;
            f.i32_ = -2147483648; f.u32_ = 4294967295; f.i64_ = -9223372036854775807; f.u64_ = -1;
            f.size = -1; f.flag = true; f.float = 1.25; f.double = -2.5;
            f.text = 'assigned'; f.data = static_ptr(); gc();
            return [Fields.size == fields_size(), Fields.alignment == fields_align(), c, check_fields(f)];
        "#,
        );
        assert_eq!(
            vm.display(value),
            "[true,true,[-128,255,-32768,65535,-2147483648,4294967295,-9223372036854775807,-1,-1,true,1.25,-2.5,fixture],true]"
        );
    });
}

#[test]
fn struct_arrays_use_c_stride_and_support_element_copy() {
    with_vm(|vm| {
        let value = run_structs(
            vm,
            r#"
            var a = alloc(Pair, 3); fill_pairs(a, a.length);
            var before = pairs_value(a, a.length);
            a[1].x = 20; a[2] = a[0];
            var copy = a[2].x; a[0].x = 10;
            return [a.length, before, copy, a[2].x, pairs_value(a, a.length), alloc(Pair, 0).length];
        "#,
        );
        assert_eq!(vm.display(value), "[3,6.75,0,0,31.75,0]");
    });
}

#[test]
fn nested_and_array_element_views_keep_their_allocation_alive() {
    with_vm(|vm| {
        let element = run_structs(
            vm,
            r#"
            function element() { var array = alloc(Pair, 4); fill_pairs(array, 4); return array[3]; }
            return element();
        "#,
        );
        vm.define_global("element", element);
        vm.collect();
        let read = vm.module_global(0, "pair_value").unwrap();
        assert_eq!(call_text(vm, read, &[element]).unwrap(), "6.25");
        let value = vm
            .run_source("view.rays", "element.x = 10; gc(); return element.y;")
            .unwrap();
        assert_eq!(vm.display(value), "3.25");
        let token = vm.module_global(0, "Nested").unwrap();
        vm.define_global("NestedType", token);
        let fill = vm.module_global(0, "fill_nested").unwrap();
        vm.define_global("fillNested", fill);
        let nested = vm.run_source("nested.rays", "function nested() { var n = alloc(NestedType); fillNested(n); return n.pair; } return nested();").unwrap();
        vm.define_global("nested", nested);
        vm.collect();
        assert_eq!(call_text(vm, read, &[nested]).unwrap(), "7.5");
    });
}

#[test]
fn recursive_pointer_fields_trace_targets_and_cstring_copies_remain_valid() {
    with_vm(|vm| {
        let node = run_structs(
            vm,
            r#"
            function nodes() {
                var a = alloc(Node); var b = alloc(Node);
                a.value = 3; b.value = 7; a.next = b; b.next = a; a.name = 'first';
                var copy = alloc(Node, 1); copy[0] = a;
                a.name = 'second';
                return copy[0];
            }
            return nodes();
        "#,
        );
        vm.define_global("node", node);
        vm.collect();
        let read = vm.module_global(0, "node_value").unwrap();
        let name = vm.module_global(0, "node_name").unwrap();
        assert_eq!(call_text(vm, read, &[node]).unwrap(), "10");
        assert_eq!(call_text(vm, name, &[node]).unwrap(), "first");
        let value = vm
            .run_source(
                "linked.rays",
                "node.next.value = 11; return [node.next.next.value, node.next.value, node.name];",
            )
            .unwrap();
        assert_eq!(vm.display(value), "[3,11,first]");
        let update = vm.module_global(0, "set_node_name").unwrap();
        vm.call(update, &[node]).unwrap();
        assert_eq!(call_text(vm, name, &[node]).unwrap(), "native");
    });
}

#[test]
fn struct_results_outputs_and_explicit_borrowed_arrays_are_pointers() {
    with_vm(|vm| {
        let value = run_structs(
            vm,
            r#"
            var p = static_pair();
            var out = Out(Pair); out_pair(out);
            var a = alloc(Pair, 3); fill_pairs(a, 3);
            var view = borrow(Pair, a, 3); view[2].x = 8;
            return [p.x, out.value.y, view.length, a[2].x, pair_value(view[2])];
        "#,
        );
        assert_eq!(vm.display(value), "[10,0.5,3,8,10.25]");
    });
}

const BOXES: &str = r#"
native "fixture" {
    @:free(close_box) handle Box;
    struct Header { session:Session; }
    function open_box(id:Int64):Box;
    function close_box(value:Box):Void;
    @:owner(value) function box_pair(value:Box):Pair;
    @:owner(value) function box_header(value:Box):Header;
}
"#;

#[test]
fn borrowed_structs_and_handle_fields_retain_owners_and_reject_access_after_close() {
    with_vm(|vm| {
        let header = vm.run_source("owned-view.rays", &format!("{HANDLES}\n{STRUCTS}\n{BOXES}\nfunction header() {{ return box_header(open_box(21)); }} return header();")).unwrap();
        vm.define_global("header", header);
        vm.collect();
        let read = vm.module_global(0, "session_id").unwrap();
        let value = vm
            .run_source("borrowed.rays", "return header.session;")
            .unwrap();
        vm.define_global("borrowed", value);
        vm.collect();
        assert_eq!(call_text(vm, read, &[value]).unwrap(), "21");
        assert!(
            vm.run_source("close-borrow.rays", "borrowed.close();")
                .unwrap_err()
                .contains("close needs a native handle")
        );
        let value = vm.run_source("close-owner.rays", &format!("{HANDLES}\n{STRUCTS}\n{BOXES}\nvar owner = open_box(22); var p = box_pair(owner); var h = box_header(owner); var session = h.session; owner.close(); var errors = 0; try {{ p.x; }} catch(e) {{ errors += 1; }} try {{ pair_value(p); }} catch(e) {{ errors += 1; }} try {{ session_id(session); }} catch(e) {{ errors += 1; }} return [errors, p == null, session == null];")).unwrap();
        assert_eq!(vm.display(value), "[3,true,true]");
    });
}

#[test]
fn struct_errors_are_reported_before_invalid_memory_access() {
    with_vm(|vm| {
        for declaration in [
            "struct Empty {}",
            "struct Bad { x:Void; }",
            "struct Bad { x:Missing; }",
            "struct Loop { inner:Loop; }",
            "struct A { b:B; } struct B { a:A; }",
            "struct Bad { x:Int32; x:Double; }",
            "struct Int32 { x:Int32; }",
            "struct Bad { x:Ptr<Int32>; }",
            "struct Bad { x:Out<Int32>; }",
        ] {
            let source = format!("native \"fixture\" {{ {declaration} }}");
            assert!(
                vm.run_source("bad-layout.rays", &source).is_err(),
                "{declaration}"
            );
        }
        for body in [
            "alloc(Pair, -1);",
            "alloc(Pair, 9223372036854775807);",
            "alloc(Int32);",
            "borrow(Pair, null);",
            "borrow(Pair, Bytes(1));",
            "borrow(Pair, alloc(Pair), 2);",
            "var p = alloc(Pair); p.x = 2147483648;",
            "var p = alloc(Pair); p.nope;",
            "alloc(Pair, 0)[0];",
            "alloc(Pair, 1)[-1];",
            "alloc(Pair, 1)[1];",
            "alloc(Pair, 1).x;",
            "alloc(Pair, 1)[0] = alloc(Node);",
            "alloc(Nested).pair = alloc(Node);",
            "Pair.size = 0;",
            "unaligned_pair();",
            "pair_value(alloc(Node));",
            "var n = alloc(Node); n.name = 'bad\\x00string';",
            "var n = alloc(Node); n.next = alloc(Pair);",
        ] {
            let source = format!("{STRUCTS}\n{body}");
            assert!(vm.run_source("bad-struct.rays", &source).is_err(), "{body}");
        }
    });
}

#[test]
fn bound_struct_calls_retain_types_and_libraries_after_registration_replacement() {
    with_vm(|vm| {
        let pair = run_structs(vm, "var p = alloc(Pair); p.x = 6; p.y = 0.5; return p;");
        let read = vm.module_global(0, "pair_value").unwrap();
        vm.define_global("kept", pair);
        unsafe {
            vm.register_native_library("fixture", "/no/such/replacement");
        }
        vm.collect();
        assert_eq!(call_text(vm, read, &[pair]).unwrap(), "6.5");
    });
}

#[test]
fn returned_aliases_share_vm_storage_and_keep_assigned_strings_alive() {
    with_vm(|vm| {
        let node = run_structs(
            vm,
            r#"
            native "fixture" { function identity_node(node:Node):Node; }
            function alias() { var a = alloc(Node); a.name = 'alias'; return identity_node(a); }
            return alias();
        "#,
        );
        vm.define_global("alias", node);
        vm.collect();
        let name = vm.module_global(0, "node_name").unwrap();
        assert_eq!(call_text(vm, name, &[node]).unwrap(), "alias");
        let result = vm
            .run_source(
                "alias-string.rays",
                "alias.name = 'changed'; gc(); return alias.name;",
            )
            .unwrap();
        assert_eq!(vm.display(result), "changed");
        let raw = vm.run_source("raw-alias.rays", &format!("{STRUCTS}\nnative \"fixture\" {{ function identity_ptr(p:Ptr):Ptr; }} function raw() {{ var p = alloc(Pair); p.x = 18; p.y = 0.5; return identity_ptr(p); }} return raw();")).unwrap();
        vm.define_global("raw", raw);
        vm.collect();
        let result = vm
            .run_source(
                "borrow-raw.rays",
                &format!("{STRUCTS}\nreturn pair_value(borrow(Pair, raw));"),
            )
            .unwrap();
        assert_eq!(vm.display(result), "18.5");
    });
}

#[test]
fn c_written_pointer_fields_retain_vm_allocations_and_managed_handles() {
    with_vm(|vm| {
        let node = run_structs(
            vm,
            r#"
            native "fixture" { function link_nodes(a:Node, b:Node):Void; }
            function linked() { var a = alloc(Node); var b = alloc(Node); a.value = 4; b.value = 9; link_nodes(a, b); return a; }
            return linked();
        "#,
        );
        vm.define_global("linked", node);
        vm.collect();
        let read = vm.module_global(0, "node_value").unwrap();
        assert_eq!(call_text(vm, read, &[node]).unwrap(), "13");
        let header = vm.run_source("managed-header.rays", &format!("{HANDLES}\nnative \"fixture\" {{ struct ManagedHeader {{ session:Session; }} function put_session(header:ManagedHeader, session:Session):Void; }} function managed() {{ var h = alloc(ManagedHeader); var s = session(23); put_session(h, s); return h; }} return managed();")).unwrap();
        vm.define_global("header", header);
        vm.collect();
        let count = vm.module_global(1, "freed_count").unwrap();
        assert_eq!(call_text(vm, count, &[]).unwrap(), "0");
        let result = vm
            .run_source(
                "read-header.rays",
                &format!("{HANDLES}\nreturn session_id(header.session);"),
            )
            .unwrap();
        assert_eq!(vm.display(result), "23");
        vm.define_global("header", Value::Nil);
        vm.collect();
        assert_eq!(call_text(vm, count, &[]).unwrap(), "1");
    });
}

#[test]
fn closed_pointer_fields_can_be_cleared_and_c_owned_pointer_writes_are_rejected() {
    with_vm(|vm| {
        let result = vm.run_source("repair.rays", &format!("{HANDLES}\nnative \"fixture\" {{ struct Holder {{ session:Session; }} function identity_ptr(p:Ptr):Ptr; }} var h = alloc(Holder); var s = session(24); h.session = s; s.close(); var invalid = false; try {{ identity_ptr(h); }} catch(e) {{ invalid = true; }} var closed = h.session == null; h.session = null; identity_ptr(h); return [invalid, closed, h.session];")).unwrap();
        assert_eq!(vm.display(result), "[true,true,null]");
        let result = run_structs(
            vm,
            r#"
            native "fixture" { function static_node():Node; }
            var n = static_node(); var rejected = false;
            try { n.name = 'temporary'; } catch(e) { rejected = true; }
            n.value = 6;
            var old = n.name; n.name = null;
            return [rejected, old, n.value, n.name];
        "#,
        );
        assert_eq!(vm.display(result), "[true,static,6,null]");
        let error = vm.run_source("past-end.rays", &format!("{STRUCTS}\nnative \"fixture\" {{ function one_past_pair(a:Pair, count:SizeT):Pair; }} one_past_pair(alloc(Pair, 3), 3);")).unwrap_err();
        assert!(error.contains("exceeds allocated storage"), "{error}");
        let result = vm.run_source("end-pointer.rays", &format!("{STRUCTS}\nnative \"fixture\" {{ function one_past_pair(a:Pair, count:SizeT):Ptr; }} var a = alloc(Pair, 3); var end = one_past_pair(a, 3); return [end != null, Bytes(end, 0).length, borrow(Pair, end, 0).length];")).unwrap();
        assert_eq!(vm.display(result), "[true,0,0]");
    });
}

#[test]
fn struct_storage_pressure_is_counted_once_across_aliases_and_released() {
    with_vm(|vm| {
        vm.run_source(
            "type.rays",
            "native \"fixture\" { struct Pair { x:Int32; y:Double; } }",
        )
        .unwrap();
        let token = vm.module_global(0, "Pair").unwrap();
        let alloc = vm.global("alloc").unwrap();
        let before = vm.heap_stats().external;
        let array = vm.call(alloc, &[token, Value::Int(8)]).unwrap();
        vm.define_global("array", array);
        let owned = vm.heap_stats().external;
        assert_eq!(owned - before, 128);
        let borrow = vm.global("borrow").unwrap();
        let alias = vm.call(borrow, &[token, array]).unwrap();
        vm.define_global("element", alias);
        assert_eq!(vm.heap_stats().external, owned);
        vm.define_global("array", Value::Nil);
        vm.collect();
        assert_eq!(vm.heap_stats().external, owned);
        vm.define_global("element", Value::Nil);
        vm.collect();
        assert_eq!(vm.heap_stats().external, before);
    });
}

#[test]
fn compatible_struct_declarations_are_shared_across_native_blocks() {
    with_vm(|vm| {
        let result = run_structs(
            vm,
            r#"
            native "fixture" {
                struct Pair { x:Int32; y:Double; }
                struct External { tag:UInt8; pair:Pair; tail:UInt16; }
                function fill_nested(n:External):Void;
            }
            var n = alloc(External); fill_nested(n);
            return [n.pair.x, n.pair.y, pair_value(n.pair)];
        "#,
        );
        assert_eq!(vm.display(result), "[3,4.5,7.5]");
        let error = vm
            .run_source(
                "conflict.rays",
                "native \"fixture\" { struct Pair { x:Int64; y:Double; } }",
            )
            .unwrap_err();
        assert!(error.contains("different definition"), "{error}");
    });
}
