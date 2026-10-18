//! The collection trigger: floor, ceiling, growth, external bytes, stress,
//! and the idle heartbeat.

mod common;

use std::time::Duration;

use common::*;
use rayscript_gc::{Header, Heap, HeapConfig};

const MIB: usize = 1024 * 1024;

fn quiet(floor: usize, ceiling: usize, growth: usize) -> Heap {
    let mut heap = Heap::new(HeapConfig {
        max_bytes: 128 * MIB,
        trigger_floor: floor,
        trigger_ceiling: ceiling,
        growth,
        stress: false,
    });
    heap.set_heartbeat(None);
    heap
}

/// A heap whose last collection kept `n` 4096-byte objects.
fn with_live(floor: usize, ceiling: usize, growth: usize, n: usize) -> Heap {
    let mut heap = quiet(floor, ceiling, growth);
    let objs: Vec<*mut Header> = (0..n).map(|_| heap.alloc(4000, 1, false)).collect();
    heap.begin_collect();
    for &p in &objs {
        unsafe { heap.mark(p) };
    }
    heap.end_collect(|_| {});
    assert_eq!(heap.stats().live_bytes, n * 4096);
    heap
}

#[test]
#[cfg_attr(miri, ignore)]
fn fires_when_allocation_passes_the_floor() {
    let mut heap = quiet(MIB, 64 * MIB, 4);
    assert_eq!(heap.stats().trigger_threshold, MIB);
    loop {
        let since = heap.stats().bytes_since_collect;
        assert_eq!(heap.should_collect(), since >= MIB, "at {since} bytes");
        if since >= MIB {
            break;
        }
        heap.alloc(64, 1, false);
    }
    heap.begin_collect();
    heap.end_collect(|_| {});
    assert!(!heap.should_collect(), "a collection resets the volume");
}

#[test]
fn external_bytes_count_toward_the_trigger() {
    let mut heap = quiet(MIB, 64 * MIB, 4);
    heap.alloc(64, 1, false);
    assert!(!heap.should_collect());
    heap.track_external(MIB as isize);
    assert!(heap.should_collect());
    assert_eq!(heap.stats().external, MIB);
    // Releasing what was just taken cancels the pressure.
    heap.track_external(-(MIB as isize));
    assert!(!heap.should_collect());
    assert_eq!(heap.stats().external, 0);
}

#[test]
#[cfg_attr(miri, ignore)]
fn external_bytes_and_allocation_add_up() {
    let mut heap = quiet(MIB, 64 * MIB, 4);
    heap.track_external(MIB as isize / 2);
    while heap.stats().bytes_since_collect < MIB / 2 - 64 {
        assert!(!heap.should_collect());
        heap.alloc(64, 1, false);
    }
    heap.alloc(64, 1, false);
    assert!(heap.should_collect());
}

#[test]
fn releases_never_bank_credit_or_underflow() {
    let mut heap = quiet(MIB, 64 * MIB, 4);
    heap.track_external(100);
    heap.track_external(-10_000);
    let s = heap.stats();
    assert_eq!(s.external, 0);
    // The cycle that frees an object's buffer reports it afterwards; that
    // must not let later growth hide.
    heap.track_external(MIB as isize / 2);
    heap.begin_collect();
    heap.end_collect(|_| {});
    heap.track_external(-(MIB as isize) / 2);
    assert!(!heap.should_collect());
    heap.track_external(MIB as isize);
    assert!(
        heap.should_collect(),
        "a release before growth banked credit"
    );
    heap.track_external(isize::MIN);
    assert_eq!(heap.stats().external, 0);
}

#[test]
fn threshold_is_the_floor_for_a_small_live_set() {
    // 25 objects = 100 KiB live; 2x = 200 KiB, below the 1 MiB floor.
    let heap = with_live(MIB, 8 * MIB, 2, 25);
    assert_eq!(heap.stats().trigger_threshold, MIB);
}

#[test]
#[cfg_attr(miri, ignore)]
fn threshold_is_growth_times_live_between_floor_and_ceiling() {
    // 2 MiB live, 2x = 4 MiB.
    let heap = with_live(MIB, 8 * MIB, 2, 512);
    assert_eq!(heap.stats().live_bytes, 2 * MIB);
    assert_eq!(heap.stats().trigger_threshold, 4 * MIB);
}

#[test]
#[cfg_attr(miri, ignore)]
fn threshold_is_capped_by_the_ceiling() {
    // 6 MiB live, 2x = 12 MiB, capped at 8 MiB.
    let heap = with_live(MIB, 8 * MIB, 2, 1536);
    assert_eq!(heap.stats().trigger_threshold, 8 * MIB);
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_live_set_above_the_ceiling_sets_the_threshold_to_itself() {
    // 10 MiB live exceeds the 8 MiB ceiling: collect once the volume equals it.
    let heap = with_live(MIB, 8 * MIB, 2, 2560);
    assert_eq!(heap.stats().live_bytes, 10 * MIB);
    assert_eq!(heap.stats().trigger_threshold, 10 * MIB);
}

#[test]
#[cfg_attr(miri, ignore)]
fn growth_of_one_or_zero_means_the_live_size() {
    assert_eq!(
        with_live(MIB, 64 * MIB, 1, 512).stats().trigger_threshold,
        2 * MIB
    );
    assert_eq!(
        with_live(MIB, 64 * MIB, 0, 512).stats().trigger_threshold,
        2 * MIB
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn the_ceiling_never_drops_below_the_floor() {
    let heap = with_live(4 * MIB, MIB, 2, 512);
    assert_eq!(heap.stats().trigger_threshold, 4 * MIB);
}

#[test]
#[cfg_attr(miri, ignore)]
fn the_threshold_gates_the_next_collection() {
    let mut heap = with_live(MIB, 8 * MIB, 2, 512);
    let threshold = heap.stats().trigger_threshold;
    let mut allocated = 0;
    while !heap.should_collect() {
        heap.alloc(4000, 1, false);
        allocated += 4096;
        assert!(allocated <= threshold + 4096, "never fired");
    }
    assert!(allocated >= threshold);
}

#[test]
fn stress_makes_every_poll_true() {
    let mut heap = Heap::new(HeapConfig {
        stress: true,
        ..config(64)
    });
    assert!(heap.should_collect(), "even before any allocation");
    heap.alloc(64, 1, false);
    assert!(heap.should_collect());
    heap.begin_collect();
    heap.end_collect(|_| {});
    assert!(heap.should_collect(), "even straight after a collection");
}

#[test]
fn stress_collection_every_allocation_stays_correct() {
    let mut w = World::new(
        HeapConfig {
            stress: true,
            ..config(64)
        },
        99,
    );
    let mut roots: Vec<u32> = Vec::new();
    for step in 0..scaled(500, 40) {
        let id = w.spawn_collecting(2048, &roots);
        assert!(w.heap.should_collect());
        if step % 5 == 0 {
            roots.push(id);
        }
        if roots.len() > 12 {
            roots.swap_remove(w.rng.below(roots.len()));
        }
        roots.retain(|r| w.objs.contains_key(r));
    }
    assert!(w.heap.stats().cycles > 0);
}

#[test]
#[cfg_attr(miri, ignore)]
fn heartbeat_asks_for_a_collection_after_idle_time() {
    let mut heap = quiet(64 * MIB, 128 * MIB, 4);
    heap.set_heartbeat(Some(Duration::from_millis(20)));
    // Nothing to collect: never.
    assert!((0..4096).all(|_| !heap.should_collect()));
    heap.alloc(64, 1, false);
    std::thread::sleep(Duration::from_millis(60));
    assert!(
        (0..4096).any(|_| heap.should_collect()),
        "heartbeat never fired"
    );

    // A collection restarts the idle clock.
    heap.set_heartbeat(Some(Duration::from_secs(3600)));
    heap.begin_collect();
    heap.end_collect(|_| {});
    heap.alloc(64, 1, false);
    assert!((0..4096).all(|_| !heap.should_collect()));

    // And it can be turned off.
    heap.set_heartbeat(None);
    std::thread::sleep(Duration::from_millis(30));
    assert!((0..4096).all(|_| !heap.should_collect()));
}

#[test]
fn a_collection_clears_external_pressure() {
    let mut heap = quiet(MIB, 64 * MIB, 4);
    heap.track_external(2 * MIB as isize);
    assert!(heap.should_collect());
    heap.begin_collect();
    heap.end_collect(|_| {});
    assert!(!heap.should_collect());
    // The bytes are still owned outside the heap, and only growth counts.
    assert_eq!(heap.stats().external, 2 * MIB);
    heap.track_external(MIB as isize / 2);
    assert!(!heap.should_collect());
}
