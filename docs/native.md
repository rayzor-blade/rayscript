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

Registration is unsafe: loading a library executes its initialization code, and a C symbol carries no signature information the VM can verify. Script declarations must match the exported ABI. Bound functions, pointers and handles keep their library loaded even if the registration is later replaced. The extension host registers no libraries by default; native manifest trust and hash checks are not implemented yet.

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
| `Ptr` | `void *` | Opaque pointer, handle, `Bytes` or null argument; opaque pointer or null result. |
| `Out<T>` | `T *` | Typed output slot constructed with `Out(T)`; arguments only. |
| Declared handle | Opaque pointer | Matching handle or null. |
| `Bytes` | `unsigned char *` | Fixed-size buffer; arguments only. |

Integer operations and comparisons in the VM remain signed, including values transported through `UInt64`. No value is converted through a floating-point number to represent an unsigned integer.

A string argument is copied into NUL-terminated storage for the duration of the call. An interior NUL is an error. A returned C string is copied into a script string before argument storage is released; the C side retains ownership of the memory. A null pointer returns null, and invalid UTF-8 is an error. A library must not retain a string argument after returning.

Native functions take exactly their declared argument count. Defaults, optional and variadic parameters are rejected. Scalar call interfaces and argument storage are prepared at bind time and reused. C string arguments and string results allocate their copies.

## Output slots and handles

```haxe
native "fixture" {
  @:free(close_session) handle Session;
  @:free(free_record) handle Record;
  function open_session(out:Out<Session>, id:Int64):Int32;
  @:owner(session)
  function get_record(out:Out<Record>, session:Session, id:Int64):Int32;
  function record_value(record:Record):Int64;
  function close_session(session:Session):Void;
  function free_record(record:Record):Void;
}
var out = Out(Session);
var code = open_session(out, 7);
if (code != 0) throw code;
var session = out.value;
var record = Out(Record);
code = get_record(record, session, 3);
if (code != 0) throw code;
trace(record_value(record.value));
session.close();
```

Native blocks declare handles before binding their functions, regardless of source order. Each handle requires one `@:free(function)` annotation and a matching `function free(value:Handle):Void;` declaration in the same block. The free symbol is resolved when the handle type is bound. Native type names become script values accepted by `Out(T)`.

`Out(T)` supports scalar types, `CString`, `Ptr` and declared handles. Its `.value` is null until a call writes it. Each call zeroes the slot and copies or wraps its result immediately after returning, including when the C function returns an error code. Error-code handling remains the script's responsibility. Slots cannot be passed twice in one call. A slot containing an open handle cannot be reused until that handle is closed; this prevents losing an owned allocation. `.value` is read-only.

`@:owner(parameter)` retains the named handle for returned handles and pointers, including output slots. Owner arguments must be open handles. Child handles trace their owners through the collector and retain their native resources independently of heap sweep order. An owner can outlive its last direct script reference while a child remains reachable.

`handle.close()` immediately frees child handles before the handle, invalidates all its aliases and borrowed pointers, and can be called repeatedly. Calling the declared free function has the same effect. Closed handles compare equal to null and are false in conditions; passing one to a native function raises an error. Repeated returns of the same live address and handle type share ownership; conflicting owner declarations raise an error.

Unreachable handles queue their frees during collection. The VM drains that queue after the heap sweep, on the VM thread. A queued child retains its owner until its free function finishes. VM teardown uses the same order. Destructors must not call back into the VM; callbacks are a later stage.

## Byte buffers

```haxe
native "fixture" {
  function fill_bytes(bytes:Bytes, length:SizeT, value:UInt8):Void;
}
var bytes = Bytes(3); // zeroed storage
fill_bytes(bytes, bytes.length, 65);
bytes[1] = 66;
trace(bytes.toString()); // ABA
var independent = bytes.copy();
```

`Bytes(length)` allocates a zeroed fixed-size buffer. `Bytes(string)` copies UTF-8 bytes without a trailing NUL; `Bytes(array)` copies checked integers in `0..255`. `.length` is read-only. Index reads outside the buffer return null; writes outside the buffer or outside the byte range raise errors. `.toString()` copies UTF-8 text and rejects invalid encoding. `.copy()` creates an independent buffer.

`Bytes(pointer, length)` copies native data into a new buffer. Null is accepted only with length zero. When the source is another `Bytes`, the length is checked against its capacity. For opaque pointers and handles, the registered library and trusted script must guarantee valid readable memory for the given length. Native buffer arguments likewise require a correct length and must not be retained by C after the call. A raw `Ptr` does not transfer ownership; `@:owner` is required for borrowed pointers that depend on a handle.

## Checking

```sh
cargo test -p rayscript-host -p rayscript-vm
cargo run -p rayscript-vm --example native
cargo test --release -p rayscript-vm --test native scalar_call_overhead -- --ignored --nocapture
```

The `rayscript-ffi-fixture` workspace crate builds a Rust C-ABI shared library before the VM's native-call tests. It covers integer widths, booleans, floating-point values, mixed signatures, strings, nulls, void results, typed outputs, buffers and owned handles. Tests also cover registration and symbol failures, argument errors, library lifetime, owner tracing, child-before-parent frees, use after close, aliases and allocations during repeated scalar calls. The ignored release benchmark compares a scalar call with a Rust closure native; measurements belong in the tracker.

## Limits

Structs, callbacks and worker lanes are not implemented. Function annotations other than `@:owner`, including `@:blocking`, are rejected rather than silently ignored. The initial target is macOS arm64; other targets require their own ABI tests. Structs by value, unions and variadic calls are outside v1.
