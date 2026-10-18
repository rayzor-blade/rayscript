use rayscript_vm::heap::HeapConfig;
use rayscript_vm::Vm;

/// Runs a script and shows what it returned, or the error it ended with.
fn eval_in(mut vm: Vm, src: &str) -> String {
    match vm.run_source("t.rays", src) {
        Ok(v) => vm.display(v),
        Err(e) => format!("ERROR {}", e.lines().next().unwrap_or("")),
    }
}

/// Every script is run twice: as is, and with the heap collected at every poll, so a value the collector
/// loses shows up as a different answer.
fn is(src: &str, want: &str) {
    assert_eq!(eval_in(Vm::new(), src), want, "for {src:?}");
    let stressed = Vm::with_config(HeapConfig { stress: true, ..HeapConfig::default() });
    assert_eq!(eval_in(stressed, src), want, "under a collection at every poll, for {src:?}");
}

#[test]
fn arithmetic_and_precedence() {
    is("return 1 + 2 * 3;", "7");
    is("return (1 + 2) * 3;", "9");
    is("return 10 - 4 - 3;", "3");
    is("return 7 / 2;", "3.5");
    is("return 7 % 4;", "3");
    is("return -5 + 2;", "-3");
    is("return 1 << 4 | 3;", "19");
    is("return 0xFF & 0x0F;", "15");
    is("return 2.5 * 2;", "5");
    is("return 1 + 2.5;", "3.5");
    is("return 5 > 3 && 2 < 4;", "true");
    is("return !(1 == 2);", "true");
    is("return 3 == 3.0;", "true");
}

#[test]
fn strings() {
    is("return 'a' + 'b';", "ab");
    is("return 'n=' + 5;", "n=5");
    is("return 1 + 'x';", "1x");
    is("var n = 3; return 'n is $n and ${n * 2}';", "n is 3 and 6");
    is("return 'abc' < 'abd';", "true");
    is("return 'abc' == 'abc';", "true");
    is("return \"line\\nbreak\";", "line\nbreak");
}

#[test]
fn variables_and_scopes() {
    is("var a = 1; var b = a + 1; return a + b;", "3");
    is("var a = 1; { var a = 2; } return a;", "1");
    is("var x = 1; x += 4; x *= 2; return x;", "10");
    is("var i = 0; i++; ++i; return i;", "2");
    is("var i = 5; var j = i++; return j * 10 + i;", "56");
    is("var s = null; return s ?? 'dflt';", "dflt");
    is("var s = null; s ??= 7; return s;", "7");
    is("return true ? 'y' : 'n';", "y");
    is("var a; return a;", "null");
}

#[test]
fn functions_and_recursion() {
    is("function add(a, b) { return a + b; } return add(2, 3);", "5");
    is("function fact(n) { if (n <= 1) return 1; return n * fact(n - 1); } return fact(10);", "3628800");
    is("function f(a, b = 10) { return a + b; } return f(1) + f(1, 2);", "14");
    is("function f(a, ...r) { return r.length; } return f(1, 2, 3, 4);", "3");
    is("function f(a, b) { return b; } return f(1);", "null");
    is("function f() { 5 } return f();", "5");
    is("return (function(x) { return x * 2; })(4);", "8");
    // Called before its definition: top-level functions are defined first.
    is("return g(); function g() { return 'ok'; }", "ok");
}

#[test]
fn closures_share_what_they_capture() {
    is("function counter() { var n = 0; return () -> { n++; return n; }; } var c = counter(); c(); c(); return c();", "3");
    is("function make() { var n = 0; return [() -> n, () -> n = n + 10]; } var p = make(); p[1](); return p[0]();", "10");
    is("var fs = []; for (i in 0...3) fs.push(() -> i); return fs[0]() + fs[1]() * 10 + fs[2]() * 100;", "210");
    is("function outer() { var x = 1; function inner() { return x + 1; } x = 5; return inner(); } return outer();", "6");
    is("var add = a -> b -> a + b; return add(1)(2);", "3");
    is("function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); } return fib(15);", "610");
}

#[test]
fn control_flow() {
    is("var s = 0; for (i in 0...5) s += i; return s;", "10");
    is("var s = 0; var i = 0; while (i < 5) { s += i; i++; } return s;", "10");
    is("var i = 0; do { i++; } while (i < 3); return i;", "3");
    is("var s = 0; for (i in 0...10) { if (i == 3) continue; if (i == 6) break; s += i; } return s;", "12");
    is("var r = if (1 < 2) 'a' else 'b'; return r;", "a");
    is("var n = 0; for (i in 0...3) for (j in 0...3) n++; return n;", "9");
    is("var s = ''; for (c in 'abc') s += c.toUpperCase(); return s;", "ABC");
}

#[test]
fn collections() {
    is("var a = [1, 2, 3]; a.push(4); return a.length + a[3];", "8");
    is("return [1, 2, 3];", "[1,2,3]");
    is("var a = [1, 2]; a[5] = 9; return a;", "[1,2,null,null,null,9]");
    is("var m = ['a' => 1, 'b' => 2]; m['c'] = 3; return m['a'] + m['c'];", "4");
    is("var m = ['a' => 1]; return m.exists('a') && !m.exists('z');", "true");
    is("var o = {x: 1, y: 2}; o.z = 3; return o.x + o.y + o.z;", "6");
    is("return {a: 1, b: 'x'};", "{a: 1, b: x}");
    is("var s = 0; for (x in [1, 2, 3]) s += x; return s;", "6");
    is("var s = ''; for (k => v in ['a' => 1, 'b' => 2]) s += k + v; return s;", "a1b2");
    is("var s = 0; for (i => v in [10, 20]) s += i * v; return s;", "20");
    is("return [for (x in 0...4) x * x];", "[0,1,4,9]");
    is("return [for (x in 0...6) if (x % 2 == 0) x];", "[0,2,4]");
    is("var m = [for (x in 0...3) 'k$x' => x]; return m['k2'];", "2");
    is("var o = null; return o?.x;", "null");
    is("var o = {x: {y: 5}}; return o?.x?.y;", "5");
}

#[test]
fn switch_and_patterns() {
    is("function f(x) { return switch (x) { case 1: 'one'; case 2, 3: 'few'; default: 'many'; }; } return f(1) + f(3) + f(9);", "onefewmany");
    is("return switch ([1, 2]) { case [a, b]: a + b; default: -1; };", "3");
    is("return switch ([1, 2, 3, 4]) { case [a, ...r]: r.length; default: -1; };", "3");
    is("return switch ({x: 5, y: 6}) { case {x: a, y: b}: a * b; default: 0; };", "30");
    is("return switch (7) { case n if (n > 5): 'big'; case n: 'small'; };", "big");
    is("return switch ('b') { case 'a': 1; case 'b': 2; };", "2");
    is("return switch (99) { case 1: 'x'; };", "null");
    is("return switch (null) { case _: 'any'; };", "any");
}

#[test]
fn errors_are_caught_and_rethrown() {
    is("try { throw 'bad'; } catch (e) { return 'caught ' + e; } return 'no';", "caught bad");
    is("try { return 1; } catch (e) { return 2; }", "1");
    is("return try { 10 } catch (e) { 20 };", "10");
    is("var r = try { null.x } catch (e) { 'handled' }; return r;", "handled");
    is("try { try { throw 'a'; } catch (e:Int) { return 'int'; } } catch (e:String) { return 'string ' + e; }", "string a");
    is("function f() { throw 'deep'; } try { f(); } catch (e) { return e; }", "deep");
    is("var s = ''; for (i in 0...3) { try { if (i == 1) continue; s += i; } catch (e) {} } return s;", "02");
    is("try { 1 / 0; return 'inf'; } catch (e) { return 'err'; }", "inf");
    is("try { var a = [1]; a.nope(); } catch (e) { return 'no method'; }", "no method");
}

#[test]
fn uncaught_errors_fail_with_a_trace() {
    let mut vm = Vm::new();
    let err = vm.run_source("t.rays", "function f() {\n  throw 'boom';\n}\nfunction g() {\n  f();\n}\ng();").unwrap_err();
    assert!(err.starts_with("error: boom"), "{err}");
    assert!(err.contains("at f (t.rays:2:3)"), "{err}");
    assert!(err.contains("at g (t.rays:5:3)"), "{err}");
    is("return undefinedName;", "ERROR error: `undefinedName` is not defined");
    is("var a = null; return a.x;", "ERROR error: cannot read `x` of null");
    is("return 1 - 'a';", "ERROR error: cannot use `-` on Int and String");
}

#[test]
fn syntax_and_compile_errors_are_reported() {
    let mut vm = Vm::new();
    let e = vm.run_source("t.rays", "var = 1;").unwrap_err();
    assert!(e.contains("t.rays:1:5: error: expected a variable name"), "{e}");
    let e = vm.run_source("t.rays", "break;").unwrap_err();
    assert!(e.contains("`break` is only for inside a loop"), "{e}");
}
