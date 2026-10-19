//! Runs a script with a native function the host provides.

use rayscript_vm::{Value, Vm};

fn main() {
    let mut vm = Vm::new();
    vm.define_native("double", |_, args| match args.first() {
        Some(Value::Int(n)) => Ok(Value::Int(n * 2)),
        _ => Ok(Value::Nil),
    });
    match vm.run_source("embed.rays", "return double(21);") {
        Ok(v) => println!("{}", vm.display(v)),
        Err(e) => eprint!("{e}"),
    }
}
