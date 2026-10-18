//! Memory reuse, block and chunk release, and the heap cap.

mod common;

use common::*;
use rayscript_gc::{CHUNK_SIZE, HEADER_SIZE, Header, Heap, HeapConfig, LINE_SIZE, MAX_OBJECT};

const MIB: usize = 1024 * 1024;

fn sized(max_mib: usize, floor: usize) -> Heap {
    let mut heap = Heap::new(HeapConfig {
        max_bytes: max_mib * MIB,
        trigger_floor: floor,
        trigger_ceiling: 512 * MIB,
        growth: 4,
        stress: false,
    });
    heap.set_heartbeat(None);
    heap
}

/// Allocates 4000-byte objects (4096-byte footprint, eight to a block).
fn fill_blocks(heap: &mut Heap, n: usize) -> Vec<*mut Header> {
    (0..n).map(|_| heap.alloc(4000, 1, false)).collect()
}

// -- Reuse ------------------------------------------------------------------

#[test]
#[cfg_attr(miri, ignore)]
fn a_steady_live_set_does_not_grow_the_heap() {
    let mut heap = sized(256, MIB);
    let mut rng = Rng::new(31);

    // A live set with a pattern in each object, so reuse can't clobber it.
    let mut live: Vec<(*mut Header, usize, u32)> = Vec::new();
    for n in 0..scaled(3000, 60) {
        let size = rng.range(HEADER_SIZE, 1500);
        let p = heap.alloc(size, 1, n % 3 == 0);
        for i in HEADER_SIZE..size {
            unsafe { p.cast::<u8>().add(i).write(pattern(n as u32, i)) };
        }
        live.push((p, size, n as u32));
    }
    let live_bytes: usize = live.iter().map(|&(_, s, _)| footprint(s)).sum();

    let mut warm_peak = 0;
    let mut steady_peak = 0;
    let cycles = scaled(120, 6);
    for cycle in 0..cycles {
        // Garbage in a spread of sizes, mostly small.
        while heap.stats().bytes_since_collect < 2 * MIB {
            let size = match rng.below(10) {
                0..=6 => rng.range(HEADER_SIZE, LINE_SIZE),
                7..=8 => rng.range(129, 2000),
                _ => rng.range(2001, 9000),
            };
            heap.alloc(size, 2, rng.chance(1, 4));
        }
        let before = heap.stats();
        heap.begin_collect();
        for &(p, _, _) in &live {
            unsafe { assert!(heap.mark(p)) };
        }
        let stats = heap.end_collect(|_| {});
        assert_eq!(stats.objects_live, live.len());
        assert_eq!(stats.bytes_live, live_bytes);
        let after = heap.stats();
        assert_eq!(after.bytes, live_bytes);
        assert_eq!(after.objects, live.len());
        assert_eq!(
            after.chunks, 1,
            "cycle {cycle}: the heap spilled out of one chunk"
        );

        if cycle < cycles / 4 {
            warm_peak = warm_peak.max(before.blocks);
        } else {
            steady_peak = steady_peak.max(before.blocks);
        }
        if cycle % 16 == 0 {
            heap.verify();
        }
    }
    // After warm-up the peak is the same shape of cycle again, give or take
    // the random mix.
    assert!(
        steady_peak <= warm_peak + warm_peak / 8 + 4,
        "blocks grew from {warm_peak} to {steady_peak}"
    );
    for &(p, size, tag) in &live {
        assert!(
            (HEADER_SIZE..size).all(|i| unsafe { p.cast::<u8>().add(i).read() } == pattern(tag, i)),
            "live object {tag} was clobbered"
        );
    }
}

// -- Block and chunk release --------------------------------------------------

#[test]
#[cfg_attr(miri, ignore)]
fn dead_blocks_return_to_the_pool() {
    let mut heap = sized(256, MIB);
    fill_blocks(&mut heap, 800); // 100 blocks
    assert_eq!(heap.stats().blocks, 100);
    heap.begin_collect();
    let stats = heap.end_collect(|_| {});
    assert_eq!(stats.blocks_released, 100);
    let s = heap.stats();
    assert_eq!(s.blocks, 0);
    assert_eq!(s.free_blocks, 1024, "the chunk's blocks are all pooled");
    heap.verify();
    // The pool serves the next allocations without reserving more.
    fill_blocks(&mut heap, 800);
    assert_eq!(heap.stats().chunks, 1);
}

#[test]
#[cfg_attr(miri, ignore)]
fn empty_chunks_are_released() {
    let mut heap = sized(512, MIB);
    fill_blocks(&mut heap, 25_000); // 3125 blocks: four chunks
    assert_eq!(heap.stats().chunks, 4);
    assert_eq!(heap.stats().reserved_bytes, 4 * CHUNK_SIZE);
    let mut gone: Vec<*mut Header> = Vec::new();
    heap.for_each_object(|p| gone.push(p));

    heap.begin_collect();
    let stats = heap.end_collect(|_| {});
    assert_eq!(stats.blocks_released, 3125);
    // One chunk stays to cover the next cycle's allocation volume.
    assert_eq!(stats.chunks_released, 3);
    assert_eq!(heap.stats().chunks, 1);
    heap.verify();
    assert!(gone.iter().all(|&p| !heap.contains(p)));

    // `trim` gives the rest back; allocation reserves again on demand.
    assert_eq!(heap.trim(), 1);
    assert_eq!(heap.stats().chunks, 0);
    assert_eq!(heap.stats().reserved_bytes, 0);
    assert_eq!(heap.stats().free_blocks, 0);
    heap.verify();
    let p = heap.alloc(64, 1, false);
    assert!(!p.is_null() && heap.contains(p));
    assert_eq!(heap.stats().chunks, 1);
    heap.verify();
}

#[test]
#[cfg_attr(miri, ignore)]
fn the_next_trigger_decides_how_much_free_memory_is_kept() {
    // A 64 MiB trigger wants two chunks of free capacity kept.
    let mut heap = sized(512, 64 * MIB);
    fill_blocks(&mut heap, 25_000);
    assert_eq!(heap.stats().chunks, 4);
    heap.begin_collect();
    heap.end_collect(|_| {});
    assert_eq!(heap.stats().trigger_threshold, 64 * MIB);
    assert_eq!(heap.stats().chunks, 2);
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_chunk_holding_a_live_object_is_kept() {
    let mut heap = sized(512, MIB);
    let all = fill_blocks(&mut heap, 25_000);
    // The last object lies in the highest chunk; the lower three empty out.
    let keep = *all.last().unwrap();
    unsafe { keep.cast::<u8>().add(100).write(0x77) };
    let low = all[0];
    heap.begin_collect();
    unsafe { assert!(heap.mark(keep)) };
    let stats = heap.end_collect(|_| {});
    assert_eq!(stats.objects_live, 1);
    assert_eq!(stats.chunks_released, 3);
    assert_eq!(heap.stats().chunks, 1);
    assert_eq!(unsafe { keep.cast::<u8>().add(100).read() }, 0x77);
    assert!(heap.contains(keep));
    assert!(!heap.contains(low));
    assert_eq!(heap.trim(), 0, "the live chunk is not empty");
    heap.verify();

    // Allocation fills the live chunk's free blocks, then reserves anew.
    let more = fill_blocks(&mut heap, 9000);
    assert!(more.iter().all(|p| !p.is_null()));
    assert!(heap.stats().chunks >= 2);
    heap.verify();
}

#[test]
#[cfg_attr(miri, ignore)]
fn released_chunks_do_not_disturb_survivors() {
    let mut w = World::new(config(512), 17);
    let mut keep = Vec::new();
    // Large garbage fills two chunks; the survivors all lie in the second.
    for i in 0..6000 {
        let id = w.spawn_sized(8000).unwrap();
        if i >= 5990 {
            keep.push(id);
        }
    }
    assert!(w.heap.stats().chunks > 1);
    let stats = w.collect(&keep);
    assert!(stats.chunks_released > 0);
    w.collect(&keep);
    w.heap.trim();
    w.collect(&keep);
    for _ in 0..500 {
        w.spawn_sized(5000).unwrap();
    }
    w.collect(&keep);
}

// -- The cap ---------------------------------------------------------------

#[test]
#[cfg_attr(miri, ignore)]
fn alloc_returns_null_at_the_cap_then_recovers() {
    // 4 MiB is 128 blocks.
    let mut heap = sized(4, 64 * MIB);
    let mut held = Vec::new();
    loop {
        let p = heap.alloc(1000, 1, false);
        if p.is_null() {
            break;
        }
        held.push(p);
        assert!(held.len() < 100_000, "the cap never bit");
    }
    // 1000 bytes round up to eight lines: 32 to a block.
    assert_eq!(held.len(), 128 * 32);
    assert_eq!(heap.stats().blocks, 128);
    // Refusal is repeatable and costs nothing.
    assert!(heap.alloc(1000, 1, false).is_null());
    assert!(heap.alloc(HEADER_SIZE, 1, false).is_null());
    assert!(heap.alloc(MAX_OBJECT, 1, false).is_null());
    assert_eq!(heap.stats().objects, held.len());
    heap.verify();

    heap.begin_collect();
    heap.end_collect(|_| {});
    for _ in 0..held.len() {
        assert!(!heap.alloc(1000, 1, false).is_null());
    }
    assert!(heap.alloc(1000, 1, false).is_null());
    heap.verify();
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_collection_frees_only_what_is_dead() {
    let mut heap = sized(4, 64 * MIB);
    let all: Vec<*mut Header> = std::iter::from_fn(|| {
        let p = heap.alloc(MAX_OBJECT, 1, false);
        (!p.is_null()).then_some(p)
    })
    .collect();
    assert_eq!(all.len(), 128, "one block-sized object per block");
    // Keep half; the other half's blocks come back.
    heap.begin_collect();
    for &p in all.iter().step_by(2) {
        unsafe { heap.mark(p) };
    }
    let stats = heap.end_collect(|_| {});
    assert_eq!(stats.blocks_released, 64);
    for _ in 0..64 {
        assert!(!heap.alloc(MAX_OBJECT, 1, false).is_null());
    }
    assert!(heap.alloc(MAX_OBJECT, 1, false).is_null());
    assert!(heap.alloc(16, 1, false).is_null());
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_fragmented_full_heap_refuses_large_objects_but_serves_small_ones() {
    let mut heap = sized(4, 64 * MIB);
    // Fill every line with a 128-byte object, then keep every other one.
    let all: Vec<*mut Header> = std::iter::from_fn(|| {
        let p = heap.alloc(LINE_SIZE, 1, false);
        (!p.is_null()).then_some(p)
    })
    .collect();
    assert_eq!(all.len(), 4 * MIB / LINE_SIZE);
    heap.begin_collect();
    for &p in all.iter().step_by(2) {
        unsafe { heap.mark(p) };
    }
    heap.end_collect(|_| {});
    heap.verify();
    // No block is free and no run is longer than one line.
    assert!(heap.alloc(2 * LINE_SIZE, 1, false).is_null());
    assert!(heap.alloc(MAX_OBJECT, 1, false).is_null());
    let mut n = 0;
    while !heap.alloc(LINE_SIZE, 1, false).is_null() {
        n += 1;
    }
    assert_eq!(n, all.len() / 2, "every freed line is usable");
    heap.verify();
}

#[test]
fn a_cap_below_one_block_still_holds_one_block() {
    let mut heap = Heap::new(HeapConfig {
        max_bytes: 1000,
        ..config(1)
    });
    let p = heap.alloc(MAX_OBJECT, 1, false);
    assert!(!p.is_null());
    assert_eq!(heap.stats().blocks, 1);
    assert!(heap.alloc(MAX_OBJECT, 1, false).is_null());
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_tiny_heap_recovers_after_exhaustion() {
    // 256 KiB cap with a 32 KiB trigger: allocate until refused, collect with
    // nothing live, allocate again.
    let mut heap = Heap::new(HeapConfig {
        max_bytes: 256 * 1024,
        trigger_floor: 32 * 1024,
        ..HeapConfig::default()
    });
    let mut n = 0;
    while !heap.alloc(48, 1, true).is_null() {
        n += 1;
        assert!(n < 1_000_000);
    }
    assert!(heap.should_collect());
    heap.begin_collect();
    let mut dropped = 0;
    heap.end_collect(|_| dropped += 1);
    assert_eq!(dropped, n);
    assert!(!heap.alloc(48, 1, true).is_null());
}

// -- Placement order ---------------------------------------------------------

#[test]
#[cfg_attr(miri, ignore)]
fn freed_blocks_are_reused_lowest_address_first() {
    let mut heap = sized(256, MIB);
    let all = fill_blocks(&mut heap, 800);
    let lowest = all.iter().map(|&p| p as usize).min().unwrap();
    heap.begin_collect();
    heap.end_collect(|_| {});
    assert_eq!(heap.alloc(4000, 1, false) as usize, lowest);
    // The pool keeps handing out ascending blocks.
    let next = heap.alloc(MAX_OBJECT, 1, false) as usize;
    assert_eq!(next, lowest + 32 * 1024);
}

#[test]
fn recycled_lines_are_reused_lowest_address_first() {
    let mut heap = sized(64, MIB);
    // Eight line-sized objects in one block; keep every other one.
    let all: Vec<*mut Header> = (0..8).map(|_| heap.alloc(LINE_SIZE, 1, false)).collect();
    heap.begin_collect();
    for &p in all.iter().step_by(2) {
        unsafe { heap.mark(p) };
    }
    heap.end_collect(|_| {});
    // Free lines are the odd ones, and they come back in address order.
    for &want in all.iter().skip(1).step_by(2) {
        assert_eq!(heap.alloc(LINE_SIZE, 1, false), want);
    }
    heap.verify();
}
