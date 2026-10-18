//! Allocation: sizes, alignment, zeroing, layout rules.

mod common;

use std::collections::HashSet;

use common::*;
use rayscript_gc::{BLOCK_SIZE, HEADER_SIZE, Header, LINE_SIZE, MAX_OBJECT};

/// Fills `size` bytes after the header with a pattern tagged by `tag`.
unsafe fn fill(p: *mut Header, size: usize, tag: u32) {
    unsafe {
        for i in HEADER_SIZE..size {
            p.cast::<u8>().add(i).write(pattern(tag, i));
        }
    }
}

unsafe fn intact(p: *const Header, size: usize, tag: u32) -> bool {
    unsafe { (HEADER_SIZE..size).all(|i| p.cast::<u8>().add(i).read() == pattern(tag, i)) }
}

/// Whether everything after the kind byte is zero (the kind is set by `alloc`).
unsafe fn zeroed(p: *const Header, size: usize) -> bool {
    unsafe { (1..size).all(|i| p.cast::<u8>().add(i).read() == 0) }
}

#[test]
fn header_layout() {
    assert_eq!(size_of::<Header>(), HEADER_SIZE);
    assert_eq!(HEADER_SIZE, 8);
    assert_eq!(MAX_OBJECT, BLOCK_SIZE);
    assert_eq!(std::mem::offset_of!(Header, kind), 0);
    assert_eq!(std::mem::offset_of!(Header, mark), 1);
    assert_eq!(std::mem::offset_of!(Header, flags), 2);
}

#[test]
fn heap_is_send() {
    fn send<T: Send>() {}
    send::<rayscript_gc::Heap>();
}

#[test]
fn fresh_object_header() {
    let mut heap = heap(64);
    let p = heap.alloc(HEADER_SIZE, 77, false);
    assert!(!p.is_null());
    unsafe {
        assert_eq!((*p).kind, 77);
        assert_eq!((*p).mark, 0);
        assert_eq!((*p).flags, 0);
        assert!(!heap.is_marked(p));
    }
    assert!(heap.contains(p));
    heap.verify();
}

/// Allocates one object per size, writes a distinct pattern into each, and
/// checks alignment, zeroing, non-overlap, and that nothing was clobbered.
fn check_sizes(sizes: &[usize]) {
    let mut heap = heap(256);
    let mut held: Vec<(*mut Header, usize, u32)> = Vec::new();
    for (n, &size) in sizes.iter().enumerate() {
        let tag = n as u32 + 1;
        let p = heap.alloc(size, (n % 250) as u8 + 1, n % 3 == 0);
        assert!(!p.is_null(), "size {size} failed");
        assert_eq!(p as usize % 16, 0, "size {size} is misaligned");
        unsafe {
            assert!(zeroed(p, size), "size {size} is not zeroed");
            assert_eq!((*p).mark, 0);
            fill(p, size, tag);
        }
        held.push((p, size, tag));
    }
    // Footprints are disjoint.
    let mut extents: Vec<(usize, usize)> = held
        .iter()
        .map(|&(p, size, _)| (p as usize, p as usize + footprint(size)))
        .collect();
    extents.sort_unstable();
    for w in extents.windows(2) {
        assert!(w[0].1 <= w[1].0, "allocations overlap");
    }
    for &(p, size, tag) in &held {
        unsafe { assert!(intact(p, size, tag), "size {size} was clobbered") };
        assert!(heap.contains(p));
    }
    let mut seen = HashSet::new();
    heap.for_each_object(|p| {
        seen.insert(p);
    });
    assert_eq!(seen.len(), held.len());
    assert!(held.iter().all(|&(p, _, _)| seen.contains(&p)));
    heap.verify();
}

#[test]
fn every_small_size() {
    let step = scaled(1, 7);
    let sizes: Vec<usize> = (HEADER_SIZE..=512).step_by(step).collect();
    check_sizes(&sizes);
}

#[test]
fn sizes_across_the_range() {
    let mut sizes: Vec<usize> = (HEADER_SIZE..=MAX_OBJECT)
        .step_by(scaled(7, 1327))
        .collect();
    // Boundaries around lines, quanta and the block.
    for edge in [
        8, 15, 16, 17, 31, 32, 33, 127, 128, 129, 143, 144, 145, 255, 256, 257, 4095, 4096, 4097,
        16384, 32767, 32768,
    ] {
        sizes.push(edge);
    }
    check_sizes(&sizes);
}

#[test]
#[cfg_attr(miri, ignore)]
fn mixed_sizes_thousands_of_times() {
    let mut heap = heap(256);
    let mut rng = Rng::new(7);
    let mut held: Vec<(*mut Header, usize, u32)> = Vec::new();
    for n in 0..scaled(20_000, 300) {
        let size = match rng.below(10) {
            0..=5 => rng.range(HEADER_SIZE, 128),
            6..=8 => rng.range(129, 2048),
            _ => rng.range(2049, MAX_OBJECT),
        };
        let p = heap.alloc(size, 1, rng.chance(1, 2));
        assert!(!p.is_null());
        assert_eq!(p as usize % 16, 0);
        unsafe {
            assert!(zeroed(p, size));
            fill(p, size, n as u32);
        }
        held.push((p, size, n as u32));
    }
    for &(p, size, tag) in &held {
        unsafe { assert!(intact(p, size, tag), "object {tag} was clobbered") };
    }
    heap.verify();
}

#[test]
fn small_objects_never_straddle_a_line() {
    let mut heap = heap(64);
    let mut rng = Rng::new(3);
    for _ in 0..scaled(20_000, 300) {
        let size = rng.range(HEADER_SIZE, LINE_SIZE);
        let p = heap.alloc(size, 1, false) as usize;
        let rounded = size.next_multiple_of(16);
        assert_eq!(
            p / LINE_SIZE,
            (p + rounded - 1) / LINE_SIZE,
            "size {size} straddles a line"
        );
    }
    heap.verify();
}

#[test]
fn large_objects_own_whole_lines() {
    let mut heap = heap(64);
    let a = heap.alloc(700, 1, false) as usize;
    assert_eq!(a % LINE_SIZE, 0);
    let lines = 700usize.div_ceil(LINE_SIZE);
    // A small object never lands in the span's last line.
    let b = heap.alloc(24, 1, false) as usize;
    assert!(
        b < a || b >= a + lines * LINE_SIZE,
        "small object inside a span"
    );
    // Nor does the next large one start before the span ends.
    let c = heap.alloc(200, 1, false) as usize;
    assert!(c < a || c >= a + lines * LINE_SIZE);
    assert_eq!(c % LINE_SIZE, 0);
    heap.verify();
}

#[test]
fn a_block_sized_object_fills_a_block() {
    let mut heap = heap(64);
    let a = heap.alloc(MAX_OBJECT, 1, false) as usize;
    let b = heap.alloc(MAX_OBJECT, 1, false) as usize;
    assert_eq!(a % BLOCK_SIZE, 0);
    assert_eq!(b % BLOCK_SIZE, 0);
    assert_ne!(a / BLOCK_SIZE, b / BLOCK_SIZE);
    let stats = heap.stats();
    assert_eq!(stats.bytes, 2 * MAX_OBJECT);
    assert_eq!(stats.blocks, 2);
    heap.verify();
}

#[test]
#[should_panic(expected = "outside")]
fn too_small_panics() {
    heap(64).alloc(HEADER_SIZE - 1, 1, false);
}

#[test]
#[should_panic(expected = "outside")]
fn too_large_panics() {
    heap(64).alloc(MAX_OBJECT + 1, 1, false);
}

#[test]
fn reused_memory_is_zeroed() {
    let mut heap = heap(64);
    let sizes = [16usize, 40, 128, 129, 500, 4000];
    for round in 0..3 {
        for i in 0..scaled(400, 20) {
            let size = sizes[i % sizes.len()];
            let p = heap.alloc(size, 1, false);
            unsafe {
                assert!(
                    zeroed(p, size),
                    "round {round}: recycled memory is not zeroed"
                );
                for off in HEADER_SIZE..size {
                    p.cast::<u8>().add(off).write(0xAB);
                }
            }
        }
        // Nothing is marked, so everything is swept and its lines recycled.
        heap.begin_collect();
        heap.end_collect(|_| {});
        heap.verify();
    }
}

#[test]
fn leftover_lines_of_a_medium_region_serve_small_objects() {
    let mut heap = heap(64);
    // 129 lines each: the second does not fit beside the first in a block.
    let size = 129 * LINE_SIZE;
    let a = heap.alloc(size, 1, false) as usize;
    let b = heap.alloc(size, 1, false) as usize;
    assert_ne!(a / BLOCK_SIZE, b / BLOCK_SIZE);
    // The first block's unused lines are recycled for small objects.
    let small = heap.alloc(32, 1, false) as usize;
    assert_eq!(small / BLOCK_SIZE, a / BLOCK_SIZE);
    assert_eq!(heap.stats().blocks, 2);
    heap.verify();
}

#[test]
fn exact_line_objects_pack_without_gaps() {
    let mut heap = heap(64);
    let first = heap.alloc(LINE_SIZE, 1, false) as usize;
    for i in 1..LINES_IN_A_BLOCK {
        let p = heap.alloc(LINE_SIZE, 1, false) as usize;
        assert_eq!(p, first + i * LINE_SIZE);
    }
    heap.verify();
}

const LINES_IN_A_BLOCK: usize = BLOCK_SIZE / LINE_SIZE;

#[test]
fn quanta_pack_within_a_line() {
    let mut heap = heap(64);
    let first = heap.alloc(16, 1, false) as usize;
    assert_eq!(first % LINE_SIZE, 0);
    for i in 1..8 {
        assert_eq!(heap.alloc(16, 1, false) as usize, first + i * 16);
    }
    // The ninth starts the next line.
    assert_eq!(heap.alloc(16, 1, false) as usize, first + LINE_SIZE);
    heap.verify();
}
