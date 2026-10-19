# RayScript

A dynamic scripting language with Haxe syntax and HXX markup, run by a bytecode VM written in Rust. RayDE uses it for extensions.

```
function area(shape) {
  return switch (shape.kind) {
    case 'circle': 3.14159 * shape.r * shape.r;
    case 'rect': shape.w * shape.h;
    default: throw 'unknown shape ' + shape.kind;
  };
}

var shapes = [{kind: 'circle', r: 2}, {kind: 'rect', w: 3, h: 4}, {kind: 'triangle'}];

var total = 0;
for (s in shapes) {
  try {
    total += area(s);
  } catch (e) {
    trace('skipped: $e');
  }
}

var counts = ['a' => 1, 'b' => 2];
counts['c'] = 3;
var names = [for (k => v in counts) '$k=$v'];

function counter() {
  var n = 0;
  return () -> ++n;
}
var next = counter();
next();

// A fiber runs alongside its caller; await waits for its result.
var slow = spawn(() -> { yield(); return 'done'; });

return {total: total, names: names, next: next(), fiber: await slow};
```

```
$ cargo run -p rayscript-cli -- run examples/hello.rays
skipped: unknown shape triangle
{total: 24.56636, names: [a=1,b=2,c=3], next: 2, fiber: done}
```

## Views

Markup is an expression. `signal` and `computed` hold state; `mount` draws a view and keeps it up to date.

```
var count = signal(0);
var items = signal(['milk', 'eggs']);
var label = computed(() -> count.value == 1 ? '1 click' : '${count.value} clicks');

mount(() -> <div class="flex flex-col gap-2">
  <p>{label}</p>
  <button onClick={() -> count.value++}>Add</button>
  <if {count.value > 2}><b>many</b></if>
  <ul>
    <for {item in items}><li>{item}</li></for>
  </ul>
</div>);
```

```
$ cargo run -p rayscript-cli -- view examples/counter.rays
<#root name="main">
  <div class="flex flex-col gap-2">
    <p>0 clicks</p>
    <button>Add</button>
    <ul>
      <li>milk</li>
      <li>eggs</li>
    </ul>
  </div>
</#root>
```

## Command line

```
rayscript run <file.rays>     run the script; a top-level `return` is printed
rayscript check <file.rays>   report errors, run nothing
rayscript parse <file.rays>   print the syntax tree
rayscript dump <file.rays>    print the bytecode
rayscript view <file.rays>    run, then print the elements the views made
```

`import a.b.C` loads `a/b/C.rays` beside the script. The standard library is imported the same way (`import haxe.Json;`, `import Math.floor;`).

## Embedding

```rust
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
```

`cargo run -p rayscript-vm --example embed` prints `42`. Run a VM on a thread with a stack of 16 MiB or more: nested calls from natives are limited by stack used.

## Crates

| Crate | Contents |
| --- | --- |
| `rayscript-syntax` | Lexer, parser, syntax tree, diagnostics. |
| `rayscript-vm` | Compiler, VM, fibers, standard library, view runtime, debugger. |
| `rayscript-gc` | Garbage-collected heap used by the VM. |
| `rayscript-host` | `rayscript-host`: runs one extension in its own process and talks to the editor in JSON lines. |
| `rayscript-cli` | The `rayscript` command. |

## Status

- `native "lib" { function f(a:Int):Int; }` parses and compiles; loading libraries is not implemented yet.
- No release yet; build from source with `cargo build --release`. `cargo test` runs the suite.

MIT OR Apache-2.0.
