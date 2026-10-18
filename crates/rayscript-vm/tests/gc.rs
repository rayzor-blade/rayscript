use rayscript_vm::heap::HeapConfig;
use rayscript_vm::Vm;

fn small_heap() -> Vm {
    Vm::with_config(HeapConfig { trigger_floor: 64 * 1024, trigger_ceiling: 1 << 20, ..HeapConfig::default() })
}

#[test]
fn garbage_is_collected_while_a_script_runs() {
    let mut vm = small_heap();
    let r = vm.run_source("t.rays", "var keep = []; for (i in 0...20000) { var t = [i, [i, i], {a: i}]; if (i % 1000 == 0) keep.push(t); } return keep.length;").unwrap();
    assert_eq!(vm.display(r), "20");
    assert!(vm.collections() > 0, "no collection ran");
    vm.collect();
    // Only what the script still holds is left: not twenty thousand temporaries.
    assert!(vm.heap_stats().objects < 2_000, "{} objects left", vm.heap_stats().objects);
}

#[test]
fn what_is_reachable_survives_every_collection() {
    let mut vm = small_heap();
    let src = "
function node(v, next) { return {v: v, next: next}; }
var list = null;
for (i in 0...3000) list = node(i, list);
var sum = 0;
var n = list;
while (n != null) { sum += n.v; n = n.next; }
var counters = [for (i in 0...50) { var c = 0; () -> { c += i; return c; } }];
for (round in 0...100) for (f in counters) f();
var total = 0;
for (f in counters) total += f();
return sum * 1000000 + total;";
    let r = vm.run_source("t.rays", src).unwrap();
    // sum 0..3000 = 4498500; each counter i has been called 101 times: 101 * i; total 101 * 1225.
    assert_eq!(vm.display(r), (4498500i64 * 1_000_000 + 101 * 1225).to_string());
    assert!(vm.collections() > 0);
}

#[test]
fn a_cycle_is_collected() {
    let mut vm = small_heap();
    vm.run_source("t.rays", "for (i in 0...5000) { var a = {}; var b = {a: a}; a.b = b; }").unwrap();
    vm.collect();
    assert!(vm.heap_stats().objects < 1_000, "{} objects left", vm.heap_stats().objects);
}

#[test]
fn a_finished_fiber_gives_back_its_memory() {
    let mut vm = small_heap();
    let src = "var fs = []; for (i in 0...300) { var f = spawn(x -> { var a = []; for (j in 0...50) a.push(j); return a.length; }, i); fs.push(f); } var s = 0; for (f in fs) s += await f; fs = null; return s;";
    let r = vm.run_source("t.rays", src).unwrap();
    assert_eq!(vm.display(r), "15000");
    vm.collect();
    assert!(vm.heap_stats().objects < 1_500, "{} objects left", vm.heap_stats().objects);
}

#[test]
fn a_refused_allocation_fails_the_script_not_the_process() {
    let mut vm = Vm::with_config(HeapConfig { max_bytes: 256 * 1024, trigger_floor: 32 * 1024, ..HeapConfig::default() });
    let err = vm.run_source("t.rays", "var keep = []; while (true) keep.push([1, 2, 3, 4]);").unwrap_err();
    assert!(err.contains("out of memory"), "{err}");
    // What the failed script made stays alive while its module does; unloaded, the VM has the memory back.
    vm.unload_module(0);
    vm.collect();
    let r = vm.run_source("t.rays", "return 1 + 1;").unwrap();
    assert_eq!(vm.display(r), "2");
}

#[test]
fn a_native_may_hold_values_while_its_callback_runs() {
    let mut vm = Vm::with_config(HeapConfig { stress: true, ..HeapConfig::default() });
    let src = "var r = [1,2,3,4,5,6].map(x -> { var s = 0; for (i in 0...3) { s += i; } return {a: x, b: [x, x + 1], c: 'v' + x}; }); return r.map(o -> o.c + o.b[1]).join(',');";
    let r = vm.run_source("t.rays", src).unwrap();
    assert_eq!(vm.display(r), "v12,v23,v34,v45,v56,v67");
}
