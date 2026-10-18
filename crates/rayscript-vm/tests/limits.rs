use rayscript_vm::Vm;
use std::time::{Duration, Instant};

fn on_big_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new().stack_size(32 << 20).spawn(f).unwrap().join().unwrap();
}

#[test]
fn a_call_from_a_native_that_runs_too_long_is_stopped_and_says_where() {
    on_big_stack(|| {
        let mut vm = Vm::new();
        vm.set_call_fuel(10_000);
        let src = "try { [1].map(x -> {\n  while (true) { }\n}); } catch (e) { return e; }";
        let r = vm.run_source("t.rays", src).unwrap();
        let text = vm.display(r);
        assert!(text.contains("ran for more than 10000 steps") && text.contains("t.rays:2:"), "{text}");
        // The script goes on, and a short callback is fine.
        let r = vm.run_source("t.rays", "return [1, 2, 3].map(x -> x * 2).join(',');").unwrap();
        assert_eq!(vm.display(r), "2,4,6");
        assert_eq!(vm.fiber_count(), 0, "the stopped fiber is gone");
    });
}

#[test]
fn giving_up_a_turn_inside_a_call_is_not_running_too_long() {
    on_big_stack(|| {
        let mut vm = Vm::new();
        vm.set_call_fuel(1_000);
        let r = vm.run_source("t.rays", "return [1, 2].map(x -> { for (i in 0...50) { yield(); } return x; }).join(',');").unwrap();
        assert_eq!(vm.display(r), "1,2");
    });
}

#[test]
fn a_spawned_fiber_that_fails_unawaited_is_kept_for_the_host() {
    let mut vm = Vm::new();
    vm.run_source("t.rays", "spawn(() -> { throw 'lost'; }); var f = spawn(() -> { throw 'caught'; }); try { await f; } catch (e) { }").unwrap();
    let failures = vm.take_detached_failures();
    assert_eq!(failures.len(), 1, "{:?}", failures.iter().map(|f| &f.1).collect::<Vec<_>>());
    assert_eq!(failures[0].0.message, "lost");
    assert!(failures[0].1.contains("lost"));
    assert!(vm.take_detached_failures().is_empty());
}

#[test]
fn a_flush_stops_at_its_deadline_and_the_next_one_finishes() {
    on_big_stack(|| {
        let mut vm = Vm::new();
        // Five hundred effects that each count a run, all waiting on one signal.
        let ran = vm
            .run_source(
                "t.rays",
                "var s = signal(0); var runs = [0];
                 for (i in 0...500) { effect(() -> { s.value; runs[0] = runs[0] + 1; }); }
                 runs[0] = 0;
                 s.value = 1;
                 return runs;",
            )
            .unwrap();
        vm.root(ran);
        let runs = |vm: &Vm| vm.display(ran.as_obj().unwrap().array().items[0]).parse::<i64>().unwrap();
        vm.set_view_deadline(Some(Instant::now() - Duration::from_secs(1)));
        vm.view_flush();
        let first = runs(&vm);
        assert!(first > 0 && first < 500, "a flush past its deadline still makes progress, but not all of it: {first}");
        assert!(vm.view_step(1000), "work is left");
        vm.set_view_deadline(None);
        vm.view_flush();
        assert_eq!(runs(&vm), 500);
        assert!(!vm.view_step(1000));
    });
}
