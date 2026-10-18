//! Marking and sweeping: reachability, dropping, line and block reclamation.

mod common;

use std::collections::HashSet;

use common::*;
use rayscript_gc::{BLOCK_SIZE, HEADER_SIZE, Header, LINE_SIZE, MAX_OBJECT};

#[test]
fn randomized_graph_many_cycles() {
    let mut w = World::new(config(128), 0xC0FFEE);
    let mut roots: Vec<u32> = Vec::new();
    let (mut freed, mut kept) = (0, 0);
    for _ in 0..scaled(250, 4) {
        let new = w.rng.range(scaled(20, 5), scaled(400, 30));
        let mut fresh = Vec::new();
        for _ in 0..new {
            fresh.push(w.spawn(MAX_OBJECT).expect("heap is not full"));
        }
        // New objects point at old and new ones; old ones at new ones.
        for &id in &fresh {
            for i in 0..w.objs[&id].edges.len() {
                if w.rng.chance(1, 3) {
                    let to = w.random_id();
                    w.set_edge(id, i, to);
                }
            }
        }
        let n = w.objs.len() / 3;
        w.mutate(n);

        roots.retain(|r| w.rng.chance(7, 10) && w.objs.contains_key(r));
        for _ in 0..w.rng.range(1, 8) {
            roots.extend(w.random_id());
        }
        let stats = w.collect(&roots);
        assert_eq!(stats.objects_live, w.objs.len());
        freed += stats.objects_freed;
        kept += stats.objects_live;
    }
    assert!(w.dropped_total > 0);
    // The graph is sparse enough that both outcomes are exercised.
    assert!(freed > kept / 4 && kept > 0, "freed {freed}, kept {kept}");
}

#[test]
fn unreachable_cycles_and_self_loops_are_freed() {
    let mut w = World::new(config(64), 5);
    let ring: Vec<u32> = (0..50)
        .map(|_| {
            loop {
                let id = w.spawn(256).unwrap();
                if !w.objs[&id].edges.is_empty() {
                    break id;
                }
            }
        })
        .collect();
    for (i, &id) in ring.iter().enumerate() {
        w.set_edge(id, 0, Some(ring[(i + 1) % ring.len()]));
    }
    // Reachable through one root, the whole ring survives.
    let stats = w.collect(&ring[..1]);
    assert_eq!(stats.objects_live, ring.len());
    // Unreferenced, it all dies despite the cycle.
    let stats = w.collect(&[]);
    assert_eq!(stats.objects_freed, ring.len());
    assert_eq!(stats.objects_live, 0);
    assert_eq!(w.heap.stats().objects, 0);
}

#[test]
#[cfg_attr(miri, ignore)]
fn only_droppable_dead_objects_reach_the_callback() {
    let mut heap = heap(64);
    let mut plain = Vec::new();
    let mut droppable = Vec::new();
    for i in 0..scaled(3000, 100) {
        let size = [16, 40, 128, 129, 700, 5000][i % 6];
        let d = i % 2 == 0;
        let p = heap.alloc(size, 1, d);
        if d { droppable.push(p) } else { plain.push(p) }
    }
    heap.begin_collect();
    let mut dropped = Vec::new();
    let stats = heap.end_collect(|p| dropped.push(p));
    let want: HashSet<_> = droppable.iter().copied().collect();
    let got: HashSet<_> = dropped.iter().copied().collect();
    assert_eq!(got.len(), dropped.len(), "dropped twice");
    assert_eq!(got, want);
    assert!(plain.iter().all(|p| !got.contains(p)));
    assert_eq!(stats.objects_freed, droppable.len() + plain.len());
    assert_eq!(heap.stats().objects, 0);
    heap.verify();
}

#[test]
fn live_droppable_objects_are_dropped_only_once_they_die() {
    let mut heap = heap(64);
    let a = heap.alloc(48, 1, true);
    let b = heap.alloc(48, 1, true);
    let big = heap.alloc(3000, 1, true);

    heap.begin_collect();
    unsafe {
        assert!(heap.mark(a));
        assert!(heap.mark(big));
    }
    let mut dropped = Vec::new();
    heap.end_collect(|p| dropped.push(p));
    assert_eq!(dropped, vec![b]);
    heap.verify();

    // `a` and `big` survived with their droppable flag intact.
    heap.begin_collect();
    unsafe { assert!(heap.mark(big)) };
    let mut dropped = Vec::new();
    heap.end_collect(|p| dropped.push(p));
    assert_eq!(dropped, vec![a]);

    heap.begin_collect();
    let mut dropped = Vec::new();
    heap.end_collect(|p| dropped.push(p));
    assert_eq!(dropped, vec![big]);

    // Nothing is dropped twice.
    heap.begin_collect();
    heap.end_collect(|_| panic!("dropped an object that was already dropped"));
    assert_eq!(heap.stats().objects, 0);
    heap.verify();
}

#[test]
fn mark_api() {
    let mut heap = heap(64);
    let a = heap.alloc(64, 1, false);
    let b = heap.alloc(64, 1, false);
    unsafe {
        assert!(!heap.is_marked(a));
        assert!(!heap.is_marked(std::ptr::null()));
        heap.begin_collect();
        assert!(!heap.mark(std::ptr::null_mut()));
        assert!(heap.mark(a));
        assert!(!heap.mark(a), "a second mark must report already marked");
        assert!(heap.is_marked(a));
        assert!(!heap.is_marked(b));
        heap.end_collect(|_| {});
        // Between cycles survivors still read as marked.
        assert!(heap.is_marked(a));
        // The next cycle's colour is the other one, so `a` is unmarked again.
        heap.begin_collect();
        assert!(!heap.is_marked(a));
        assert!(heap.mark(a));
        assert!(heap.is_marked(a));
        heap.end_collect(|_| {});
        // And the one after that reuses the first colour without a reset.
        for _ in 0..5 {
            heap.begin_collect();
            assert!(!heap.is_marked(a));
            assert!(heap.mark(a));
            heap.end_collect(|_| {});
        }
    }
    assert!(heap.contains(a));
    assert!(!heap.contains(b));
    heap.verify();
}

#[test]
#[should_panic(expected = "not in this heap")]
fn marking_a_foreign_pointer_panics() {
    let mut heap = heap(64);
    let mut outside = Header::new(1);
    heap.begin_collect();
    unsafe { heap.mark(&mut outside) };
}

#[test]
#[should_panic(expected = "start of an allocation")]
fn marking_an_interior_pointer_panics() {
    let mut heap = heap(64);
    let p = heap.alloc(64, 1, false);
    heap.begin_collect();
    unsafe { heap.mark(p.cast::<u8>().add(1).cast()) };
}

#[test]
#[should_panic(expected = "inside a collection")]
fn begin_twice_panics() {
    let mut heap = heap(64);
    heap.begin_collect();
    heap.begin_collect();
}

#[test]
#[should_panic(expected = "outside a collection")]
fn end_without_begin_panics() {
    heap(64).end_collect(|_| {});
}

#[test]
fn contains_distinguishes_starts_from_everything_else() {
    let mut heap = heap(64);
    let small = heap.alloc(48, 1, false);
    let big = heap.alloc(700, 1, false);
    assert!(heap.contains(small));
    assert!(heap.contains(big));
    let at = |p: *mut Header, off: usize| p.cast::<u8>().wrapping_add(off).cast::<Header>();
    assert!(!heap.contains(at(small, 1)));
    assert!(!heap.contains(at(small, 16)));
    assert!(!heap.contains(at(big, LINE_SIZE)));
    assert!(!heap.contains(std::ptr::null()));
    assert!(!heap.contains(0x1000 as *const Header));
    let outside = Header::new(0);
    assert!(!heap.contains(&outside));
    heap.begin_collect();
    heap.end_collect(|_| {});
    assert!(
        !heap.contains(small),
        "a swept object is no longer an allocation"
    );
    assert!(!heap.contains(big));
}

#[test]
#[cfg_attr(miri, ignore)]
fn spans_survive_through_every_line() {
    let mut heap = heap(64);
    let keep = heap.alloc(5 * LINE_SIZE - 10, 1, false) as usize;
    let lines = 5;
    // Fill the neighbourhood with garbage, then sweep keeping only the span.
    for _ in 0..2000 {
        heap.alloc(40, 1, false);
    }
    heap.begin_collect();
    unsafe { assert!(heap.mark(keep as *mut Header)) };
    heap.end_collect(|_| {});
    heap.verify();
    // Whatever is allocated next must stay clear of all five lines.
    let mut rng = Rng::new(11);
    for _ in 0..6000 {
        let size = rng.range(HEADER_SIZE, 600);
        let p = heap.alloc(size, 1, false) as usize;
        let end = p + footprint(size);
        assert!(
            end <= keep || p >= keep + lines * LINE_SIZE,
            "allocation overlaps a live span"
        );
    }
    heap.verify();
}

#[test]
fn a_live_object_keeps_only_its_own_line() {
    let mut heap = heap(64);
    // Two lines of 16-byte objects; keep one in each line.
    let all: Vec<*mut Header> = (0..16).map(|_| heap.alloc(16, 1, false)).collect();
    heap.begin_collect();
    unsafe {
        assert!(heap.mark(all[3]));
        assert!(heap.mark(all[12]));
    }
    let stats = heap.end_collect(|_| {});
    assert_eq!(stats.objects_live, 2);
    assert_eq!(stats.objects_freed, 14);
    assert_eq!(stats.bytes_live, 32);
    heap.verify();
    // Both lines are still occupied, so new objects land elsewhere.
    let lines: HashSet<usize> = [all[3], all[12]]
        .iter()
        .map(|&p| p as usize / LINE_SIZE)
        .collect();
    for _ in 0..500 {
        let p = heap.alloc(16, 1, false) as usize;
        assert!(
            !lines.contains(&(p / LINE_SIZE)),
            "allocated into a live line"
        );
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn freed_lines_are_reused_in_place() {
    let mut heap = heap(64);
    let keep = heap.alloc(48, 1, false);
    for _ in 0..50_000 {
        heap.alloc(48, 1, false);
    }
    let blocks_before = heap.stats().blocks;
    for round in 0..scaled(6, 2) {
        heap.begin_collect();
        unsafe { heap.mark(keep) };
        heap.end_collect(|_| {});
        heap.verify();
        for _ in 0..50_000 {
            heap.alloc(48, 1, false);
        }
        assert!(
            heap.stats().blocks <= blocks_before,
            "round {round}: the heap grew instead of reusing freed lines"
        );
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn partly_live_blocks_hand_back_runs_of_lines() {
    let mut heap = heap(64);
    // Interleave a live and a dead 3-line object through several blocks.
    let mut live = Vec::new();
    for i in 0..400 {
        let p = heap.alloc(3 * LINE_SIZE - 5, 1, false);
        if i % 2 == 0 {
            live.push(p);
        }
    }
    let blocks_before = heap.stats().blocks;
    heap.begin_collect();
    for &p in &live {
        unsafe { heap.mark(p) };
    }
    let stats = heap.end_collect(|_| {});
    assert_eq!(stats.objects_live, live.len());
    assert_eq!(
        stats.blocks_released, 0,
        "every block still holds a live object"
    );
    heap.verify();
    // The dead objects' lines take another 200 of the same shape without growth.
    for _ in 0..200 {
        heap.alloc(3 * LINE_SIZE - 5, 1, false);
    }
    assert_eq!(heap.stats().blocks, blocks_before);
    heap.verify();
}

#[test]
fn teardown_drops_every_droppable_object() {
    let mut heap = heap(64);
    let mut want = HashSet::new();
    let mut rng = Rng::new(21);
    for i in 0..scaled(2000, 80) {
        let p = heap.alloc(rng.range(HEADER_SIZE, 3000), 1, i % 3 == 0);
        if i % 3 == 0 {
            want.insert(p);
        }
    }
    // An empty cycle sees every droppable object die.
    heap.begin_collect();
    let mut got = HashSet::new();
    heap.end_collect(|p| assert!(got.insert(p)));
    assert_eq!(got, want);
}

#[test]
fn dropping_the_heap_with_objects_in_it_is_safe() {
    let mut heap = heap(64);
    for _ in 0..1000 {
        heap.alloc(100, 1, true);
    }
    drop(heap);
}

#[test]
fn collecting_an_empty_heap() {
    let mut heap = heap(64);
    heap.begin_collect();
    let stats = heap.end_collect(|_| {});
    assert_eq!(
        stats.objects_freed + stats.objects_live + stats.blocks_released,
        0
    );
    assert_eq!(heap.stats().cycles, 1);
    let p = heap.alloc(HEADER_SIZE, 1, false);
    assert!(!p.is_null());
}

#[test]
fn stats_track_allocation_and_collection() {
    let mut heap = heap(64);
    let a = heap.alloc(40, 1, false);
    heap.alloc(200, 1, true);
    heap.alloc(MAX_OBJECT, 1, false);
    let s = heap.stats();
    assert_eq!(s.objects, 3);
    assert_eq!(s.bytes, 48 + 256 + MAX_OBJECT);
    assert_eq!(s.bytes_since_collect, s.bytes);
    assert_eq!(s.total_allocated_bytes, s.bytes as u64);
    assert_eq!(s.cycles, 0);
    heap.begin_collect();
    unsafe { heap.mark(a) };
    let c = heap.end_collect(|_| {});
    assert_eq!(c.objects_live, 1);
    assert_eq!(c.objects_freed, 2);
    assert_eq!(c.bytes_live, 48);
    assert_eq!(c.bytes_freed, 256 + MAX_OBJECT);
    let s = heap.stats();
    assert_eq!(s.live_bytes, 48);
    assert_eq!(s.bytes, 48);
    assert_eq!(s.objects, 1);
    assert_eq!(s.cycles, 1);
    assert_eq!(s.bytes_since_collect, 0);
    assert_eq!(s.total_freed_bytes, (256 + MAX_OBJECT) as u64);
    assert_eq!(s.blocks, 1);
    assert_eq!(BLOCK_SIZE, MAX_OBJECT);
}

#[test]
fn heap_moves_between_threads() {
    let mut heap = heap(64);
    let kept = heap.alloc(64, 9, true) as usize;
    let handle = std::thread::spawn(move || {
        let kept = kept as *mut Header;
        // Everything the heap handed out is usable from the new owner.
        unsafe { assert_eq!((*kept).kind, 9) };
        heap.begin_collect();
        unsafe { assert!(heap.mark(kept)) };
        heap.end_collect(|_| panic!("a live object was dropped"));
        let q = heap.alloc(64, 3, false);
        assert!(!q.is_null());
        heap.verify();
        heap
    });
    let heap = handle.join().unwrap();
    assert_eq!(heap.stats().objects, 2);
}
