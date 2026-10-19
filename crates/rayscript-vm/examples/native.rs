//! A scalar C call against the shared library the fixture crate builds.
use rayscript_vm::Vm;

fn main() {
    let mut vm = Vm::new();
    // The fixture has no initialization requirements and exports add with this exact C signature.
    unsafe {
        vm.register_native_library("fixture", rayscript_ffi_fixture::LIBRARY_PATH);
    }
    let value = vm
        .run_source(
            "native.rays",
            r#"
        native "fixture" { function add(a:Int64, b:Int64):Int64; }
        return add(20, 22);
    "#,
        )
        .unwrap();
    println!("{}", vm.display(value));
}
