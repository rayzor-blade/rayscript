use rayscript_vm::heap::HeapConfig;
use rayscript_vm::Vm;

/// Runs a script and shows what it returned, or the error it ended with; with and without a collection
/// at every poll.
fn run(src: &str) -> String {
    let src = src.to_string();
    std::thread::Builder::new().stack_size(32 << 20).spawn(move || run_here(&src)).unwrap().join().unwrap()
}

fn run_here(src: &str) -> String {
    let mut results = Vec::new();
    for stress in [false, true] {
        let mut vm = Vm::with_config(HeapConfig { stress, ..HeapConfig::default() });
        results.push(match vm.run_source("t.rays", src) {
            Ok(v) => vm.display(v),
            Err(e) => format!("ERROR {}", e.lines().next().unwrap_or("")),
        });
    }
    assert_eq!(results[0], results[1], "a collection at every poll changed the answer");
    results.remove(0)
}

#[test]
fn an_effect_runs_at_once_and_then_once_per_flush() {
    assert_eq!(run("var s = signal(1); var log = []; effect(() -> log.push(s.value)); s.value = 2; s.value = 3; flush(); return log.join(',');"), "1,3");
}

#[test]
fn a_computed_value_is_lazy_and_cached() {
    let src = "var n = 0; var s = signal(1); var c = computed(() -> { n++; return s.value * 2; });
               var a = n; c.value; c.value; var b = n; s.value = 2; var d = c.value; return [a, b, d, n].join(',');";
    assert_eq!(run(src), "0,1,4,2");
}

#[test]
fn a_diamond_runs_its_effect_once_and_never_sees_half_an_update() {
    let src = "var a = signal(1); var b = computed(() -> a.value + 1); var c = computed(() -> a.value * 2); var log = [];
               effect(() -> log.push(b.value + c.value));
               a.value = 2; flush(); a.value = 3; flush();
               return log.join(',');";
    assert_eq!(run(src), "4,7,10");
}

#[test]
fn a_computed_value_that_comes_out_the_same_stops_the_update() {
    let src = "var a = signal(1); var parity = computed(() -> a.value % 2); var runs = 0;
               effect(() -> { parity.value; runs++; });
               a.value = 3; flush(); a.value = 5; flush(); a.value = 4; flush();
               return runs;";
    assert_eq!(run(src), "2");
}

#[test]
fn cleanups_run_before_the_next_run_and_when_disposed() {
    let src = "var s = signal(0); var log = [];
               var e = effect(() -> { var v = s.value; log.push('run' + v); onCleanup(() -> log.push('clean' + v)); });
               s.value = 1; flush(); e.dispose(); s.value = 2; flush();
               return log.join(',');";
    assert_eq!(run(src), "run0,clean0,run1,clean1");
    let src = "var s = signal(0); var log = [];
               var e = effect(() -> { var v = s.value; log.push('run' + v); return () -> log.push('clean' + v); });
               s.value = 1; flush(); e.dispose();
               return log.join(',');";
    assert_eq!(run(src), "run0,clean0,run1,clean1");
}

#[test]
fn an_effect_inside_an_effect_goes_when_the_outer_runs_again() {
    let src = "var outer = signal(0); var inner = signal(0); var log = [];
               effect(() -> { var o = outer.value; effect(() -> log.push('i' + o + ':' + inner.value)); });
               inner.value = 1; flush(); outer.value = 1; flush(); inner.value = 2; flush();
               return log.join(',');";
    assert_eq!(run(src), "i0:0,i0:1,i1:1,i1:2");
}

#[test]
fn untrack_reads_without_subscribing() {
    let src = "var s = signal(0); var t = signal(0); var runs = 0;
               effect(() -> { runs++; t.value; untrack(() -> s.value); });
               s.value = 1; flush(); t.value = 1; flush();
               return runs;";
    assert_eq!(run(src), "2");
}

#[test]
fn signals_have_methods_and_a_value() {
    assert_eq!(run("var s = signal(1); s.set(5); s.update(v -> v * 2); return [s.get(), s.peek(), s.value].join(',');"), "10,10,10");
    assert_eq!(run("var s = signal(1); var c = computed(() -> s.value + 1); try { c.value = 3; } catch (e) { return e; }"), "a computed value is read only");
    assert_eq!(run("var s = signal([1]); var runs = 0; effect(() -> { s.value; runs++; }); s.value.push(2); flush(); s.notify(); flush(); return runs;"), "2");
}

#[test]
fn an_effect_that_fails_at_creation_throws_to_its_creator() {
    assert_eq!(run("try { effect(() -> throw 'no'); } catch (e) { return e; }"), "no");
}

#[test]
fn an_effect_that_fails_later_is_reported_and_the_rest_go_on() {
    let mut vm = Vm::new();
    vm.run_source("t.rays", "var s = signal(0); var seen = signal(0); effect(() -> { if (s.value == 1) throw 'later'; }); effect(() -> { seen.value = s.value; }); s.value = 1; flush();").unwrap();
    let errors = vm.view_take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("later"));
}

#[test]
fn effects_that_keep_triggering_each_other_are_stopped() {
    let mut vm = Vm::new();
    vm.run_source("t.rays", "var s = signal(0); effect(() -> { s.value = s.value + 1; }); flush();").unwrap();
    let errors = vm.view_take_errors();
    assert!(errors.iter().any(|e| e.contains("keep triggering")), "{errors:?}");
}

#[test]
fn a_chain_of_computed_values_is_followed_to_a_depth_and_no_further() {
    let chain = |n: usize| format!("var c = computed(() -> 0); for (i in 0...{n}) {{ var prev = c; c = computed(() -> prev.value + 1); }} return c.value;");
    assert_eq!(run(&chain(30)), "30");
    assert!(run(&chain(500)).contains("too deeply"));
}

#[test]
fn script_cannot_nest_callbacks_without_end() {
    let r = run("function f(n) { return [n].map(x -> f(x + 1)); } return f(0);");
    assert!(r.contains("nested too deeply"), "{r}");
}
