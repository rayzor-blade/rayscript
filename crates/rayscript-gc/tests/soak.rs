//! Long randomized runs through the whole allocate, mutate, collect loop.

mod common;

use common::*;
use rayscript_gc::{HeapConfig, MAX_OBJECT};

/// Allocates `steps` objects of the sizes `pick` returns while keeping a
/// churning set of roots, collecting whenever the heap asks (or refuses).
fn soak(seed: u64, steps: usize, floor: usize, max_mib: usize, pick: fn(&mut Rng) -> usize) {
    let mut w = World::new(
        HeapConfig {
            max_bytes: max_mib * 1024 * 1024,
            trigger_floor: floor,
            trigger_ceiling: 16 * 1024 * 1024,
            growth: 2,
            stress: false,
        },
        seed,
    );
    w.heap.set_heartbeat(None);
    let mut roots: Vec<u32> = Vec::new();
    for step in 0..steps {
        let size = pick(&mut w.rng);
        // The same retry path the VM uses: collect when due, and once more
        // if the heap refuses.
        if w.heap.should_collect() {
            w.collect(&roots);
            roots.retain(|r| w.objs.contains_key(r));
        }
        let id = match w.spawn_sized(size) {
            Some(id) => id,
            None => {
                w.collect(&roots);
                roots.retain(|r| w.objs.contains_key(r));
                w.spawn_sized(size).expect("live set exceeds the heap")
            }
        };
        // Hang the new object off something older.
        if w.rng.chance(1, 3)
            && let Some(from) = w.random_id()
        {
            let len = w.objs[&from].edges.len();
            if len > 0 && from != id {
                let i = w.rng.below(len);
                w.set_edge(from, i, Some(id));
            }
        }
        if step % 7 == 0 {
            roots.push(id);
        }
        while roots.len() > 24 {
            roots.swap_remove(w.rng.below(roots.len()));
        }
        if step % 11 == 0 {
            w.mutate(8);
        }
    }
    w.collect(&roots);
    w.collect(&[]);
    assert_eq!(w.heap.stats().objects, 0);
    assert!(
        w.heap.stats().cycles > 10,
        "only {} cycles",
        w.heap.stats().cycles
    );
    assert!(w.dropped_total > 0);
}

#[test]
fn soak_small_objects() {
    soak(1, long(60_000, 1_500_000, 400), 32 * 1024, 64, |rng| {
        rng.range(16, 128)
    });
}

#[test]
fn soak_mixed_sizes() {
    soak(
        2,
        long(40_000, 1_000_000, 400),
        128 * 1024,
        64,
        |rng| match rng.below(20) {
            0 => rng.range(2_000, MAX_OBJECT),
            1..=4 => rng.range(129, 2_000),
            _ => rng.range(16, 128),
        },
    );
}

#[test]
fn soak_large_objects() {
    soak(3, long(3_000, 60_000, 60), 256 * 1024, 128, |rng| {
        rng.range(129, MAX_OBJECT)
    });
}

#[test]
fn soak_block_sized_objects() {
    soak(4, long(1_500, 40_000, 40), 512 * 1024, 64, |rng| {
        if rng.chance(1, 2) {
            MAX_OBJECT
        } else {
            rng.range(16, 4000)
        }
    });
}

#[test]
fn soak_awkward_sizes_around_line_boundaries() {
    soak(5, long(40_000, 1_000_000, 400), 64 * 1024, 64, |rng| {
        let base = [112, 128, 144, 256, 384, 4096][rng.below(6)];
        base + rng.range(0, 2) * 16 - rng.range(0, 1) * 16
    });
}

#[test]
fn soak_under_a_tight_cap() {
    // The live set nearly fills the heap, so collections and refusals alternate.
    soak(6, long(30_000, 800_000, 300), 64 * 1024, 4, |rng| {
        rng.range(16, 600)
    });
}
