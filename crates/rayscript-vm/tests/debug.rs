use rayscript_vm::{FiberState, PauseReason, StepMode, Vm};

const SCRIPT: &str = "var total = 0;
function add(a, b) {
  var sum = a + b;
  return sum;
}
var x = add(1, 2);
total = total + x;
var items = [1, {k: 'v'}];
var s = signal(7);
total = total + 1;
";

fn started(src: &str) -> (Vm, rayscript_vm::Value) {
    let mut vm = Vm::new();
    vm.debug_set_breakpoints("t.rays", &[]);
    let f = vm.start_source("t.rays", src).unwrap();
    (vm, f)
}

fn at(vm: &Vm) -> (String, u32) {
    let f = vm.debug_frames();
    (f[0].function.clone(), f[0].line)
}

fn names(vars: &[rayscript_vm::DebugVar]) -> Vec<String> {
    vars.iter().map(|v| format!("{}={}", v.name, v.value)).collect()
}

#[test]
fn a_breakpoint_stops_the_fiber_before_its_line_and_resuming_finishes_it() {
    let mut vm = Vm::new();
    assert_eq!(vm.debug_set_breakpoints("t.rays", &[4]), Vec::<u32>::new(), "no module is loaded yet, so nothing is placed");
    let fiber = vm.start_source("t.rays", SCRIPT).unwrap();
    vm.run_until_idle(10_000);
    assert_eq!(vm.debug_paused(), Some(PauseReason::Breakpoint));
    let frames = vm.debug_frames();
    assert_eq!(frames.iter().map(|f| (f.function.as_str(), f.line)).collect::<Vec<_>>(), vec![("add", 4), ("main", 6)]);
    assert_eq!(names(&vm.debug_locals(0)), vec!["a=1", "b=2", "sum=3"]);
    assert_eq!(vm.fiber_state(fiber), Some(FiberState::Ready), "stopped, not ended");
    // Nothing runs while it is stopped.
    assert_eq!(vm.tick(10_000), 0);
    assert!(vm.debug_resume(None));
    vm.run_until_idle(10_000);
    assert_eq!(vm.debug_paused(), None);
    assert_eq!(vm.fiber_state(fiber), Some(FiberState::Done));
}

#[test]
fn a_breakpoint_moves_to_the_next_line_with_code_and_is_dropped_past_the_end() {
    let mut vm = Vm::new();
    vm.start_source("t.rays", SCRIPT).unwrap();
    // Line 5 is a closing brace that has no code of its own; line 99 is past the script.
    let placed = vm.debug_set_breakpoints("t.rays", &[5, 99, 3]);
    assert_eq!(placed, vec![6, 3]);
    vm.run_until_idle(10_000);
    assert_eq!(at(&vm), ("main".to_string(), 6), "line 6 runs first: the call on it is what runs line 3");
    vm.debug_resume(None);
    vm.run_until_idle(10_000);
    assert_eq!(at(&vm), ("add".to_string(), 3));
}

#[test]
fn breakpoints_set_before_a_module_loads_take_hold_when_it_does() {
    let mut vm = Vm::new();
    vm.debug_set_breakpoints("t.rays", &[7]);
    vm.start_source("t.rays", SCRIPT).unwrap();
    vm.run_until_idle(10_000);
    assert_eq!(at(&vm), ("main".to_string(), 7));
    assert_eq!(names(&vm.debug_globals(0)), vec!["add=<function add>", "total=0", "x=3"], "what the module has defined so far, by name");
}

#[test]
fn stepping_goes_over_into_and_out_of_calls() {
    let mut vm = Vm::new();
    vm.debug_set_breakpoints("t.rays", &[6]);
    vm.start_source("t.rays", SCRIPT).unwrap();
    vm.run_until_idle(10_000);
    assert_eq!(at(&vm), ("main".to_string(), 6));
    // Over: the call on line 6 runs, and the next stop is line 7.
    vm.debug_resume(Some(StepMode::Over));
    vm.run_until_idle(10_000);
    assert_eq!((vm.debug_paused(), at(&vm)), (Some(PauseReason::Step), ("main".to_string(), 7)));
    assert_eq!(names(&vm.debug_globals(0)).last().unwrap(), "x=3");
    // Over again: line 8, and the array exists after it.
    vm.debug_resume(Some(StepMode::Over));
    vm.run_until_idle(10_000);
    assert_eq!(at(&vm).1, 8);
    vm.debug_resume(None);
    vm.run_until_idle(10_000);
    assert_eq!(vm.debug_paused(), None);

    // Into: from line 6 the next stop is the first line of `add`; out of it returns to the caller.
    let mut vm = Vm::new();
    vm.debug_set_breakpoints("t.rays", &[6]);
    vm.start_source("t.rays", SCRIPT).unwrap();
    vm.run_until_idle(10_000);
    vm.debug_resume(Some(StepMode::In));
    vm.run_until_idle(10_000);
    assert_eq!(at(&vm), ("add".to_string(), 3));
    vm.debug_resume(Some(StepMode::Out));
    vm.run_until_idle(10_000);
    assert_eq!(vm.debug_paused(), Some(PauseReason::Step));
    assert_eq!(vm.debug_frames()[0].function, "main", "back in the caller");
    vm.debug_resume(None);
    vm.run_until_idle(10_000);
    assert_eq!(vm.debug_paused(), None);
}

#[test]
fn variables_are_expanded_and_expressions_evaluated_in_a_frame() {
    let mut vm = Vm::new();
    vm.debug_set_breakpoints("t.rays", &[10]);
    vm.start_source("t.rays", SCRIPT).unwrap();
    vm.run_until_idle(10_000);
    let globals = vm.debug_globals(0);
    let items = globals.iter().find(|v| v.name == "items").unwrap().clone();
    assert_eq!((items.value.as_str(), items.type_name), ("[1,{k: v}]", "Array"));
    let inside = vm.debug_expand(items.reference);
    assert_eq!(names(&inside), vec!["0=1", "1={k: v}"]);
    assert_eq!(names(&vm.debug_expand(inside[1].reference)), vec!["k=v"]);
    let signal = globals.iter().find(|v| v.name == "s").unwrap().clone();
    assert_eq!(names(&vm.debug_expand(signal.reference)), vec!["value=7"]);
    assert_eq!(vm.debug_evaluate(0, "items[1].k").unwrap().value, "v");
    assert_eq!(vm.debug_evaluate(0, "s.value").unwrap().value, "7");
    assert_eq!(vm.debug_evaluate(0, "total").unwrap().value, "3");
    assert!(vm.debug_evaluate(0, "nothing").unwrap_err().contains("not defined"));
    assert!(vm.debug_evaluate(0, "items.nope").unwrap_err().contains("no `nope`"));
    assert!(vm.debug_evaluate(0, "1 + 2").unwrap_err().contains("not a variable"));
    // What was shown is gone once the fiber goes on.
    vm.debug_resume(None);
    assert!(vm.debug_expand(items.reference).is_empty());
}

#[test]
fn a_pause_asked_for_stops_a_loop() {
    let mut vm = Vm::new();
    let fiber = vm.start_source("t.rays", "var n = 0;\nwhile (true) {\n  n++;\n}\n").unwrap();
    // A slice that is spent, then a pause, then the next slice stops on its first instruction.
    let mut budget = rayscript_vm::Budget::fuel(100);
    assert_eq!(vm.run(fiber, &mut budget), rayscript_vm::Slice::Yielded);
    vm.debug_pause();
    assert_eq!(vm.run(fiber, &mut rayscript_vm::Budget::fuel(100)), rayscript_vm::Slice::Paused);
    assert_eq!(vm.debug_paused(), Some(PauseReason::Request));
    assert!(vm.debug_frames()[0].line >= 2);
    assert!(vm.debug_evaluate(0, "n").unwrap().value.parse::<i64>().unwrap() > 0);
    vm.debug_resume(None);
    vm.run(fiber, &mut rayscript_vm::Budget::fuel(100));
    assert_eq!(vm.debug_paused(), None, "going on does not stop again until asked");
}

#[test]
fn a_call_a_native_makes_is_not_stopped_in() {
    let mut vm = Vm::new();
    vm.debug_set_breakpoints("t.rays", &[2, 4]);
    vm.start_source("t.rays", "var r = [1, 2].map(x -> {\n  return x * 2;\n});\nvar done = true;\n").unwrap();
    vm.run_until_idle(10_000);
    assert_eq!(at(&vm), ("main".to_string(), 4), "the callback on line 2 runs without stopping");
}

#[test]
fn a_script_with_no_breakpoints_runs_as_before() {
    let (mut vm, fiber) = started("return 1 + 2;");
    vm.run_until_idle(10_000);
    assert_eq!(vm.fiber_state(fiber), Some(FiberState::Done));
    assert_eq!(vm.debug_paused(), None);
}

#[test]
fn a_breakpoint_in_a_loop_stops_on_every_pass() {
    let mut vm = Vm::new();
    vm.debug_set_breakpoints("t.rays", &[3]);
    let fiber = vm.start_source("t.rays", "var total = 0;\nfor (i in 0...4) {\n  total = total + i;\n}\nreturn total;\n").unwrap();
    let mut seen = Vec::new();
    for _ in 0..10 {
        vm.run_until_idle(10_000);
        if vm.debug_paused().is_none() {
            break;
        }
        seen.push(vm.debug_evaluate(0, "i").unwrap().value);
        vm.debug_resume(None);
    }
    assert_eq!(seen, ["0", "1", "2", "3"]);
    assert_eq!(vm.fiber_state(fiber), Some(FiberState::Done));
    assert_eq!(vm.display(vm.fiber_result(fiber).unwrap()), "6");
}

#[test]
fn removing_the_breakpoints_takes_the_stops_out_of_the_code() {
    let mut vm = Vm::new();
    vm.debug_set_breakpoints("t.rays", &[3]);
    let fiber = vm.start_source("t.rays", "var total = 0;\nfor (i in 0...4) {\n  total = total + i;\n}\nreturn total;\n").unwrap();
    vm.run_until_idle(10_000);
    assert_eq!(vm.debug_paused(), Some(PauseReason::Breakpoint));
    vm.debug_set_breakpoints("t.rays", &[]);
    vm.debug_resume(None);
    vm.run_until_idle(10_000);
    assert_eq!(vm.debug_paused(), None);
    assert_eq!(vm.fiber_state(fiber), Some(FiberState::Done));
}

#[test]
fn a_breakpoint_set_while_stopped_is_hit_by_the_same_fiber_later() {
    let mut vm = Vm::new();
    vm.debug_set_breakpoints("t.rays", &[6]);
    vm.start_source("t.rays", SCRIPT).unwrap();
    vm.run_until_idle(10_000);
    assert_eq!(at(&vm).1, 6);
    vm.debug_set_breakpoints("t.rays", &[6, 9]);
    vm.debug_resume(None);
    vm.run_until_idle(10_000);
    assert_eq!(at(&vm), ("main".to_string(), 9));
}
