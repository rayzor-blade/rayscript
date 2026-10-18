//! Timings for the view runtime. They assert nothing; run them in release mode:
//! `cargo test -p rayscript-vm --release --test view_bench -- --ignored --nocapture`

use rayscript_vm::{Value, Vm};
use std::time::Instant;

fn on_big_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap();
}

fn report(what: &str, runs: usize, started: Instant, ops: usize) {
    let t = started.elapsed();
    println!("{what:<48} {:>10.1} us each  {:>9} ops", t.as_secs_f64() * 1e6 / runs as f64, ops);
}

#[test]
#[ignore]
fn timings() {
    on_big_stack(|| {
        let mut vm = Vm::new();
        let started = Instant::now();
        let handles = vm
            .run_source(
                "t.rays",
                "var rows = signal([for (i in 0...10000) i]);
                 var tick = signal(0);
                 var lit = signal(false);
                 mount(() -> <div>
                   <p>{tick}</p>
                   <ul><for {r in rows}><li class={lit.value ? 'on' : 'off'}>row {r}</li></for></ul>
                 </div>);
                 return [() -> tick.value++, (a) -> rows.value = a, () -> lit.value = !lit.value];",
            )
            .unwrap();
        vm.root(handles);
        vm.view_pump(1_000_000);
        let ops = vm.view_take_ops().len();
        report("mount 10,000 rows (3 effects each)", 1, started, ops);
        println!("{:<48} {:>10} nodes", "", vm.view_node_count());
        let items = handles.as_obj().unwrap().array().items.clone();

        let n = 2000;
        let started = Instant::now();
        let mut ops = 0;
        for _ in 0..n {
            vm.call(items[0], &[]).unwrap();
            vm.view_pump(1_000_000);
            ops += vm.view_take_ops().len();
        }
        report("one hole changes, 10,000 rows present", n, started, ops);

        let started = Instant::now();
        let mut ops = 0;
        for i in 0..20 {
            let arr: Vec<Value> = (0..10000).map(|v| Value::Int(if v == 5000 { 100_000 + i } else { v })).collect();
            let arr = vm.new_array(arr);
            vm.call(items[1], &[arr]).unwrap();
            vm.view_pump(1_000_000);
            ops += vm.view_take_ops().len();
        }
        report("one of 10,000 items replaced", 20, started, ops);

        let started = Instant::now();
        let mut ops = 0;
        for _ in 0..20 {
            vm.call(items[2], &[]).unwrap();
            vm.view_pump(1_000_000);
            ops += vm.view_take_ops().len();
        }
        report("one signal sets a class on 10,000 rows", 20, started, ops);

        let started = Instant::now();
        let mut ops = 0;
        for i in 0..20 {
            let mut v: Vec<i64> = (0..10000).collect();
            v.reverse();
            if i % 2 == 1 {
                v.reverse();
            }
            let arr = vm.new_array(v.into_iter().map(Value::Int).collect());
            vm.call(items[1], &[arr]).unwrap();
            vm.view_pump(1_000_000);
            ops += vm.view_take_ops().len();
        }
        report("10,000 rows reversed", 20, started, ops);
    });
}

#[test]
#[ignore]
fn components() {
    on_big_stack(|| {
        let mut vm = Vm::new();
        let started = Instant::now();
        vm.run_source(
            "t.rays",
            "function Row(props) { var n = signal(props.i); return <li onClick={() -> n.value++}>{props.label}: {n}</li>; }
             mount(() -> <ul><for {i in 0...2000}><Row i={i} label='row'/></for></ul>);",
        )
        .unwrap();
        vm.view_pump(1_000_000);
        let ops = vm.view_take_ops().len();
        report("mount 2,000 components with state", 1, started, ops);
    });
}
