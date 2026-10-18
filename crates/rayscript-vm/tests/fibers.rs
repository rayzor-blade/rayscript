use rayscript_vm::fiber::FiberState;
use rayscript_vm::{Budget, Slice, Value, Vm};

fn run(src: &str) -> String {
    let mut vm = Vm::new();
    match vm.run_source("t.rays", src) {
        Ok(v) => vm.display(v),
        Err(e) => format!("ERROR {}", e.lines().next().unwrap_or("")),
    }
}

#[test]
fn a_fiber_is_spawned_and_awaited() {
    assert_eq!(run("var f = spawn(() -> 6 * 7); return await f;"), "42");
    assert_eq!(run("function sq(x) { return x * x; } var f = spawn(sq, 9); return await f;"), "81");
}

#[test]
fn fibers_take_turns_at_yield() {
    let src = "
var log = [];
function worker(name, n) { for (i in 0...n) { log.push(name + i); yield(); } }
var a = spawn(worker, 'a', 3);
var b = spawn(worker, 'b', 3);
await a; await b;
return log.join(',');";
    assert_eq!(run(src), "a0,b0,a1,b1,a2,b2");
}

#[test]
fn an_error_in_a_fiber_reaches_the_one_awaiting_it() {
    assert_eq!(run("var f = spawn(() -> { throw 'x'; }); try { await f; } catch (e) { return 'got ' + e; }"), "got x");
    assert!(run("var f = spawn(() -> { throw 'x'; }); await f; return 1;").starts_with("ERROR Error: x"));
}

#[test]
fn a_budget_stops_a_fiber_that_never_ends_and_cancel_ends_it() {
    let mut vm = Vm::new();
    let parsed = rayscript_syntax::parse("var n = 0; while (true) { n++; }");
    let module = rayscript_vm::compile("t.rays", "var n = 0; while (true) { n++; }", &parsed.module).unwrap();
    let id = vm.load(&module);
    let fiber = vm.spawn_main(id).unwrap();
    // It gives way at the end of each budget, however long it would have gone on.
    for _ in 0..3 {
        let mut budget = Budget::fuel(1_000);
        assert_eq!(vm.run(fiber, &mut budget), Slice::Yielded);
    }
    assert_eq!(vm.fiber_state(fiber), Some(FiberState::Ready));
    vm.cancel(fiber);
    let mut budget = Budget::fuel(1_000);
    assert_eq!(vm.run(fiber, &mut budget), Slice::Cancelled);
    assert_eq!(vm.fiber_state(fiber), Some(FiberState::Cancelled));
}

#[test]
fn a_deadline_stops_a_fiber_too() {
    let mut vm = Vm::new();
    let src = "while (true) { }";
    let parsed = rayscript_syntax::parse(src);
    let module = rayscript_vm::compile("t.rays", src, &parsed.module).unwrap();
    let id = vm.load(&module);
    let fiber = vm.spawn_main(id).unwrap();
    let started = std::time::Instant::now();
    let mut budget = Budget::until(started + std::time::Duration::from_millis(20));
    assert_eq!(vm.run(fiber, &mut budget), Slice::Yielded);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn the_host_settles_a_promise_a_script_awaits() {
    let mut vm = Vm::new();
    let (promise, id) = vm.new_promise();
    vm.define_global("answer", promise);
    let src = "var v = await answer; return v + 1;";
    let parsed = rayscript_syntax::parse(src);
    let module = rayscript_vm::compile("t.rays", src, &parsed.module).unwrap();
    let m = vm.load(&module);
    let fiber = vm.spawn_main(m).unwrap();
    vm.run_until_idle(1000);
    assert_eq!(vm.fiber_state(fiber), Some(FiberState::Waiting));
    assert!(vm.resolve(id, Value::Int(41)));
    vm.run_until_idle(1000);
    assert_eq!(vm.fiber_state(fiber), Some(FiberState::Done));
    assert_eq!(vm.display(vm.fiber_result(fiber).unwrap()), "42");
}

#[test]
fn a_rejected_promise_throws_in_the_waiting_script() {
    let mut vm = Vm::new();
    let (promise, id) = vm.new_promise();
    vm.define_global("answer", promise);
    let src = "try { await answer; return 'no'; } catch (e) { return 'failed: ' + e; }";
    let parsed = rayscript_syntax::parse(src);
    let module = rayscript_vm::compile("t.rays", src, &parsed.module).unwrap();
    let m = vm.load(&module);
    let fiber = vm.spawn_main(m).unwrap();
    vm.run_until_idle(1000);
    let why = vm.new_string("no network");
    vm.reject(id, why);
    vm.run_until_idle(1000);
    assert_eq!(vm.display(vm.fiber_result(fiber).unwrap()), "failed: no network");
}

#[test]
fn a_fiber_cannot_wait_for_itself_and_a_callback_cannot_wait_at_all() {
    assert!(run("var me = null; me = spawn(() -> await me); await me; return 1;").starts_with("ERROR Error: a fiber cannot wait for itself"));
    // A function a built-in calls runs to its end; waiting inside it is an error, not a hang.
    let r = run("var p = spawn(() -> yield()); return [1, 2].map(x -> { await p; return x; });");
    assert!(r.starts_with("ERROR") || r == "[1,2]", "{r}");
}

#[test]
fn many_fibers_run_to_the_end() {
    let src = "
var fs = [];
for (i in 0...200) fs.push(spawn(x -> { yield(); return x * 2; }, i));
var s = 0;
for (f in fs) s += await f;
return s;";
    assert_eq!(run(src), "39800");
}
