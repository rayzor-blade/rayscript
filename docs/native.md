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
| Declared callback | C function pointer | Matching callback object or null; arguments and struct fields only. |
| `Bytes` | `unsigned char *` | Fixed-size buffer; arguments only. |
| Declared struct, `Ptr<Struct>` | `struct T *` | Struct view, struct array or null; structs are never passed by value. |

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

`Out(T)` supports scalar types, `CString`, `Ptr`, declared handles and structs. For a struct, `Out<Struct>` is a pointer-to-pointer slot; `alloc(Struct)` supplies a direct writable struct pointer. Its `.value` is null until a call writes it. Each call zeroes the slot and copies or wraps its result immediately after returning, including when the C function returns an error code. Error-code handling remains the script's responsibility. Slots cannot be passed twice in one call. A slot containing an open handle cannot be reused until that handle is closed; this prevents losing an owned allocation. `.value` is read-only.

`@:owner(parameter)` retains the named handle for returned handles, pointers and struct views, including output slots. Owner arguments must be open handles. Child handles trace their owners through the collector and retain their native resources independently of heap sweep order. An owner can outlive its last direct script reference while a child remains reachable.

`handle.close()` immediately invalidates all its aliases and borrowed pointers, and can be called repeatedly. It frees child handles before the handle. When called during a native callback, the frees wait until the outermost C call returns. Calling the declared free function has the same effect. Closed handles compare equal to null and are false in conditions; passing one to a native function raises an error. Repeated returns of the same live address and handle type share ownership; conflicting owner declarations raise an error.

Unreachable handles queue their frees during collection. The VM drains that queue after the heap sweep, on the VM thread. A queued child retains its owner until its free function finishes. VM teardown uses the same order. Destructors must not call back into the VM. During an active C call, collection queues frees until C has returned.

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

## Structs and arrays

```haxe
native "fixture" {
  struct Pair { x:Int32; y:Double; }
  struct Nested { tag:UInt8; pair:Pair; tail:UInt16; }
  struct Node { value:Int64; next:Ptr<Node>; name:CString; }
  function fill_pair(pair:Pair, x:Int32, y:Double):Void;
  function fill_pairs(pairs:Pair, count:SizeT):Void;
  function pairs_value(pairs:Pair, count:SizeT):Double;
}
var pair = alloc(Pair);
pair.x = 2;
pair.y = 0.5;
fill_pair(pair, 3, 4.5);
var array = alloc(Pair, 3);
fill_pairs(array, array.length);
array[1].x = 8;
trace(pairs_value(array, array.length));
```

Layouts use C field alignment, inter-field padding and tail padding. `Type.size` and `Type.alignment` describe the layout. Native blocks resolve forward references between structs, allow recursive `Ptr<T>` links and reject recursive inline values. Empty and packed structs, unions and fixed-size array fields are unsupported.

In a function signature, `Pair` and `Ptr<Pair>` both mean a pointer. In a field declaration, `pair:Pair` embeds a struct and `pair:Ptr<Pair>` stores a pointer. Fields support native scalars, `CString`, `Ptr`, declared handles, embedded structs and pointers to structs. Struct values are not passed or returned by value.

`alloc(Type)` returns zeroed struct storage. `alloc(Type, count)` returns a fixed-length array with read-only `.length`; count may be zero. Arrays require an index before field access. Indexes are checked, and an element or nested view retains the entire allocation. Struct assignments copy the C bytes while retaining pointer dependencies and assigned C strings. Changing a source string field later does not invalidate a previously copied string pointer. Native struct allocations contribute to GC pressure once per shared allocation.

`borrow(Type, pointer[, count])` creates a view without copying. The pointer must be aligned; known VM storage bounds are checked. Native pointer bounds remain part of the registered library's memory contract. A count creates an indexed array view. Native struct results and `Out<Struct>.value` are borrowed views; `@:owner(handle)` links them to a C allocation's lifetime. Closing that owner invalidates the views and their borrowed fields. Structs themselves have no `close()` method.

Returned pointers into known VM storage retain the struct allocation or byte buffer and its field retainers. The VM also captures managed struct pointers and handles that C writes into VM-owned pointer fields before collection. Native code must not retain VM storage after a call unless the script keeps it alive for that use.

Assigning a `CString` field copies NUL-terminated UTF-8 storage. Assigned pointers and handles are traced while their field still refers to them. Reading a handle field borrows its lifetime; a pointer already managed by the VM shares that existing resource. Borrowed handles cannot be freed with `close()`. Calls reject struct fields referring to closed managed handles; fields in live storage can be cleared or replaced.

Non-null pointer-bearing fields in C-owned storage are read-only, including `CString` and pointer-bearing embedded struct copies. Such writes could leave C referring to storage whose VM view has been collected. Scalar writes and clearing pointers to null are supported; use `alloc(Type)` for fully writable structs. Views backed by VM byte buffers are writable and retain the buffer.

## Callbacks

```haxe
native "fixture" {
  @:failure(-1) callback Visit(value:Int32, data:Ptr):Int32;
  struct Options { callback:Visit; data:Ptr; label:CString; }
  function visit(callback:Visit, data:Ptr, count:Int32):Int32;
}
var total = 0;
var callback = Visit((value, data) -> { total += value; return 0; });
visit(callback, null, 3);
```

`Visit(function)` allocates a C function pointer backed by a script closure or embedding native function. A declared callback can be a function argument or struct field. Types are checked by declaration identity; null supplies a null function pointer. Reading a field assigned by script returns its callback object. Unmanaged C function pointers cannot be invoked or wrapped as script callbacks.

Callback arguments support scalars, `CString`, `Ptr`, declared structs (including `Ptr<Struct>`) and borrowed handles. Results support scalars, `CString`, `Ptr` and `Void`. `Out`, `Bytes`, callback parameters and owned handle results are unsupported callback signatures; use `Ptr` for C buffer arguments. Strings received from C are copied. Structs, handles and raw pointers received by a callback are borrowed for that callback invocation; their views expire when it returns. Copy any data needed later. Borrowed handles cannot be closed. Bounds and ownership of native memory remain part of the trusted C declaration's contract.

Non-void callbacks require one `@:failure(literal)` annotation, checked against the result type. Signed integer and floating-point literals and booleans are accepted; pointer and `CString` results require null. Void callbacks may omit the annotation or use `@:failure(null)`. If the callback throws or its result cannot be converted, C receives this failure value. Remaining callbacks in that C call also return the failure value without invoking script. After C returns and output slots have been decoded, the original thrown value is rethrown. Nested native calls have their own error scope, so script may catch an inner call's error and continue the outer callback.

Callbacks can call native functions recursively, including the binding that invoked them. Active output slots cannot be reused by nested calls. A callback always runs on its creating VM thread. Worker callbacks are marshaled to that thread and wait for their result. Callbacks can wait for nested native calls, but cannot suspend on host promises or arbitrary script awaits. Calls from another thread without an active worker call return the failure value and are unsupported.

Active calls retain their argument graph, callback arguments and pointer results, including replaced struct strings and callback fields, through the return from C. A callback may run GC or close an owned handle: closure storage and passed buffers remain allocated, and actual handle frees wait until the outermost call returns. Closed handles become invalid to script immediately. Callback string results remain NUL-terminated and valid until the enclosing C call returns.

If C retains a callback or VM pointer beyond the call, the script must keep the callback and its data reachable for the entire registration, and unregister it before releasing them. Native code must stop invoking a callback before its VM is destroyed. Calls outside an active native call are unsupported. A stored function pointer does not itself root script values.

## Blocking calls and Krio fibers

```haxe
native "fixture" {
  @:blocking function sleep_value(ms:SizeT, value:Int64):Int64;
}
var task = spawn(() -> sleep_value(50, 42));
// Other script fibers can run while C is blocked.
trace(await task);
```

`@:blocking` implicitly parks the calling script fiber and resumes it with the C result or deferred callback exception. It does not require an explicit `await` at the native call. `Vm.call` and `Vm.run_source` drive the required native events synchronously for embedding callers; the scheduler's ordinary `tick` and `run_until_idle` poll events without waiting for outstanding calls. Callback dispatch is synchronous and can drive nested native calls. Errors returned as C status codes remain the script's responsibility.

Script interpreter turns execute on thread-affine Krio fibers on the VM thread. Their register stacks and bytecode frames remain explicit for GC, exception handling and debugging. Each fiber creates a guarded 1 MiB Krio stack when first run and releases it when finished or cancelled. No VM borrow survives a yield between interpreter turns. Native-to-script nesting is bounded independently of physical stack addresses.

Each registered library instance creates one OS worker thread on its first blocking call. That thread hosts Krio fibers for C calls. After the lane exists, ordinary calls into the same instance also use it to preserve serialization and thread-local library state. Unrelated calls are queued. A callback on the lane parks its native Krio fiber while the VM handles it, allowing explicit same-library reentry on that same OS thread. The stacks remain on their creating threads; no Krio fiber migrates between threads.

A callback from a thread created by C is also marshaled to the VM, and that C thread waits for the reply. Reentering the same library from such a callback is rejected: its lane thread may be blocked inside C waiting for that thread. Calls into other library instances are supported. Native destructors still run on the VM thread after all active native calls have returned; the library's ownership contract must permit that.

Pending calls retain their arguments, callbacks and native storage through completion. Reads, writes and reuse of worker-visible buffers, structs and handles from other script fibers raise a busy-storage error. The active callback may access its call's storage while C is paused. Buffers and dependencies exposed by a callback remain protected until C finishes. Active output slots cannot be reused. C-retained registrations still require roots and correct synchronization: scripts must not mutate registered data while C uses it outside a callback.

Cancelling a script fiber does not terminate arbitrary C code. Its pending call keeps its storage and library alive until C returns, and owned outputs are still captured for cleanup. VM teardown drains in-flight calls, replies to further callbacks with their failure values without running script, then stops the lanes before dropping heap objects or unloading libraries. A native call that never returns requires the embedding application to terminate its process.

Embedders can call `Vm.set_native_waker` with a thread-safe wake function, then call `Vm.poll_native` on the VM thread when woken. `Vm.has_native_work` reports calls still in flight. The extension host uses this mechanism to wake its protocol loop for callbacks and completions independently of stdin or timers. `serve_host` accepts an embedder-configured `Host`; the standard host still registers no native libraries by default.

## Checking

```sh
cargo test -p rayscript-host -p rayscript-vm
cargo run -p rayscript-vm --example native
cargo test --release -p rayscript-vm --test native scalar_call_overhead -- --ignored --nocapture
```

The `rayscript-ffi-fixture` workspace crate builds a Rust C-ABI shared library before the VM's native-call tests. It covers integer widths, booleans, floating-point values, mixed signatures, strings, nulls, void results, typed outputs, buffers, owned handles, C layouts, nested structs, arrays, borrowed views and callbacks. Tests also cover registration and symbol failures, argument errors, library lifetime, owner tracing, child-before-parent frees, use after close, aliases and allocations during repeated scalar calls. The ignored release benchmark compares a scalar call with a Rust closure native; measurements belong in the tracker.

## Limits

Native manifest trust and hash checks remain unimplemented. Function annotations other than `@:owner` and `@:blocking` are rejected. The initial target is macOS arm64; other targets require their own ABI tests. Structs by value, unions and variadic calls are outside v1.
