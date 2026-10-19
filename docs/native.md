# Native calls

A native block binds C functions from a library registered by the embedding application:

```haxe
native "fixture" {
  function add(a:Int64, b:Int64):Int64;
  function greeting():CString;
}
return add(20, 22);
```

The VM resolves the logical name `fixture` through its registrations. It does not search for libraries named by a script. A missing registration, library, symbol or unsupported signature raises an error naming the library and function.

```rust
use rayscript_vm::Vm;

let mut vm = Vm::new();
// The embedding application guarantees the library's initialization, ABI and memory contracts.
unsafe { vm.register_native_library("fixture", "/absolute/path/to/library"); }
let result = vm.run_source("native.rays", r#"
    native "fixture" { function add(a:Int64, b:Int64):Int64; }
    return add(20, 22);
"#).unwrap();
assert_eq!(vm.display(result), "42");
```

Registration is unsafe: loading a library executes its initialization code, and a C symbol carries no signature information the VM can verify. Script declarations must match the exported ABI. A bound function keeps its library loaded even if the registration is later replaced. The extension host registers no libraries by default; native manifest trust and hash checks are not implemented yet.

## Types

| Declaration | C representation | Script value |
| --- | --- | --- |
| `Int8`, `Int16`, `Int32`, `Int64` | Signed integer of that width | `Int`; narrower arguments are range checked. |
| `Int` | Signed 64-bit integer | `Int`. |
| `UInt8`, `UInt16`, `UInt32` | Unsigned integer of that width | Nonnegative `Int`, range checked. |
| `UInt64` | Unsigned 64-bit integer | `Int` preserving all bits; a result above the signed maximum appears negative. |
| `SizeT` | `size_t` | `Int`, with the same bit representation as `UInt64` on a 64-bit target. |
| `Bool` | C `_Bool` | `Bool`; integers are not converted. |
| `Float` | C `float` | `Float` or `Int` argument, rounded to 32 bits; finite overflow is rejected. |
| `Double` | C `double` | `Float` or `Int` argument, converted to 64-bit floating point. |
| `CString` | `const char *` | UTF-8 `String` or null. |
| `Void` | `void` | Null result; not an argument type. |

Integer operations and comparisons in the VM remain signed, including values transported through `UInt64`. No value is converted through a floating-point number to represent an unsigned integer.

A string argument is copied into NUL-terminated storage for the duration of the call. An interior NUL is an error. A returned C string is copied into a script string before argument storage is released; the C side retains ownership of the memory. A null pointer returns null, and invalid UTF-8 is an error. A library must not retain a string argument after returning.

Native functions take exactly their declared argument count. Defaults, optional and variadic parameters are rejected. Scalar call interfaces and argument storage are prepared at bind time and reused. C string arguments and string results allocate their copies.

## Checking

```sh
cargo test -p rayscript-host -p rayscript-vm
cargo run -p rayscript-vm --example native
cargo test --release -p rayscript-vm --test native scalar_call_overhead -- --ignored --nocapture
```

The `rayscript-ffi-fixture` workspace crate builds a Rust C-ABI shared library before the VM's native-call tests. It covers integer widths, booleans, floating-point values, mixed signatures, strings, nulls and void results. Tests also cover registration and symbol failures, argument errors, library lifetime and allocations during repeated scalar calls. The ignored release benchmark compares a scalar call with a Rust closure native; measurements belong in the tracker.

## Limits

Pointers, out parameters, handles, byte buffers, structs, callbacks and worker lanes are not implemented. Native annotations, including `@:blocking`, are rejected rather than silently ignored. The initial target is macOS arm64; other targets require their own ABI tests. Structs by value, unions and variadic calls are outside v1.
