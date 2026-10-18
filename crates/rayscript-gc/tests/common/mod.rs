//! Helpers shared by the integration tests: a PRNG, a test-only object layout
//! whose payload holds pointers, a tracer, and a model of the heap to check
//! the collector against.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use rayscript_gc::{
    BLOCK_SIZE, CollectStats, HEADER_SIZE, Header, Heap, HeapConfig, LINE_SIZE, MAX_OBJECT,
};

/// Whether the tests run under Miri, where loops must be short.
pub const MIRI: bool = cfg!(miri);

/// `full` natively, `miri` under Miri.
pub const fn scaled(full: usize, miri: usize) -> usize {
    if MIRI { miri } else { full }
}

/// Loop counts for the long runs: `debug` in a debug build, `release` in an
/// optimised one, `miri` under Miri.
pub const fn long(debug: usize, release: usize, miri: usize) -> usize {
    if MIRI {
        miri
    } else if cfg!(debug_assertions) {
        debug
    } else {
        release
    }
}

/// A small heap config: the side tables scale with `max_bytes`.
pub fn config(max_mib: usize) -> HeapConfig {
    HeapConfig {
        max_bytes: max_mib * 1024 * 1024,
        ..HeapConfig::default()
    }
}

pub fn heap(max_mib: usize) -> Heap {
    Heap::new(config(max_mib))
}

/// xorshift64*.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..n`.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() >> 11) as usize % n
    }

    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }

    pub fn chance(&mut self, num: usize, den: usize) -> bool {
        self.below(den) < num
    }
}

/// What the heap should report for an allocation of `size` bytes: a small
/// object rounds to 16, a larger one to whole lines.
pub fn footprint(size: usize) -> usize {
    if size <= LINE_SIZE {
        size.next_multiple_of(16)
    } else {
        size.next_multiple_of(LINE_SIZE)
    }
}

/// The distinct byte `i` of an object tagged `tag` should hold.
pub fn pattern(tag: u32, i: usize) -> u8 {
    (tag as u8).wrapping_mul(31).wrapping_add(i as u8) ^ 0xA5
}

// -- Node: header, id, edge count, edges, then pattern bytes ----------------

/// Bytes before the edges.
pub const NODE_FIXED: usize = 16;
const ID_AT: usize = HEADER_SIZE;
const EDGES_LEN_AT: usize = HEADER_SIZE + 4;
const PTR: usize = size_of::<*mut Header>();

pub fn node_size(edges: usize, pad: usize) -> usize {
    NODE_FIXED + edges * PTR + pad
}

/// Fills a fresh node's body: id, edge count, null edges, pattern padding.
///
/// # Safety
/// `p` is a fresh allocation of `size` bytes.
pub unsafe fn node_init(p: *mut Header, id: u32, edges: usize, size: usize) {
    unsafe {
        let b = p.cast::<u8>();
        b.add(ID_AT).cast::<u32>().write(id);
        b.add(EDGES_LEN_AT).cast::<u32>().write(edges as u32);
        for i in 0..edges {
            node_set_edge(p, i, std::ptr::null_mut());
        }
        for i in NODE_FIXED + edges * PTR..size {
            b.add(i).write(pattern(id, i));
        }
    }
}

pub unsafe fn node_id(p: *const Header) -> u32 {
    unsafe { p.cast::<u8>().add(ID_AT).cast::<u32>().read() }
}

pub unsafe fn node_edges(p: *const Header) -> usize {
    unsafe { p.cast::<u8>().add(EDGES_LEN_AT).cast::<u32>().read() as usize }
}

pub unsafe fn node_edge(p: *const Header, i: usize) -> *mut Header {
    unsafe {
        p.cast::<u8>()
            .add(NODE_FIXED + i * PTR)
            .cast::<*mut Header>()
            .read()
    }
}

pub unsafe fn node_set_edge(p: *mut Header, i: usize, to: *mut Header) {
    unsafe {
        p.cast::<u8>()
            .add(NODE_FIXED + i * PTR)
            .cast::<*mut Header>()
            .write(to)
    }
}

/// Whether the pattern padding of a node of `size` bytes is intact.
pub unsafe fn node_pad_intact(p: *const Header, size: usize) -> bool {
    unsafe {
        let b = p.cast::<u8>();
        let id = node_id(p);
        let edges = node_edges(p);
        (NODE_FIXED + edges * PTR..size).all(|i| b.add(i).read() == pattern(id, i))
    }
}

// -- The model --------------------------------------------------------------

/// What the test knows about one object, independent of the heap's memory.
pub struct Obj {
    pub ptr: *mut Header,
    pub size: usize,
    pub kind: u8,
    pub droppable: bool,
    /// Model edges, by object id; `None` is a null edge.
    pub edges: Vec<Option<u32>>,
}

/// A heap plus a shadow graph of every object allocated and not yet swept.
pub struct World {
    pub heap: Heap,
    pub objs: HashMap<u32, Obj>,
    pub rng: Rng,
    next_id: u32,
    /// Ids the drop callback has reported, ever.
    pub dropped_total: usize,
}

impl World {
    pub fn new(config: HeapConfig, seed: u64) -> World {
        World {
            heap: Heap::new(config),
            objs: HashMap::new(),
            rng: Rng::new(seed),
            next_id: 1,
            dropped_total: 0,
        }
    }

    /// Allocates a node of a random shape, at most `max_size` bytes; `None`
    /// if the heap is full.
    pub fn spawn(&mut self, max_size: usize) -> Option<u32> {
        let size = self.random_size(max_size);
        self.spawn_sized(size)
    }

    /// Allocates a node of exactly `size` bytes (at least `NODE_FIXED`).
    pub fn spawn_sized(&mut self, size: usize) -> Option<u32> {
        let edges_cap = (size - NODE_FIXED) / PTR;
        let edges = if edges_cap == 0 {
            0
        } else {
            self.rng.below(edges_cap.min(6) + 1)
        };
        let droppable = self.rng.chance(1, 2);
        let id = self.next_id;
        let kind = (id % 200) as u8 + 1;
        let p = self.heap.alloc(size, kind, droppable);
        if p.is_null() {
            return None;
        }
        self.next_id += 1;
        // Fresh memory is zero apart from the kind, whether new or recycled.
        assert!(
            (1..size).all(|i| unsafe { p.cast::<u8>().add(i).read() } == 0),
            "allocation of {size} bytes is not zeroed"
        );
        unsafe { node_init(p, id, edges, size) };
        self.objs.insert(
            id,
            Obj {
                ptr: p,
                size,
                kind,
                droppable,
                edges: vec![None; edges],
            },
        );
        Some(id)
    }

    /// A size skewed toward small objects, with the occasional large one.
    fn random_size(&mut self, max_size: usize) -> usize {
        let max_size = max_size.clamp(NODE_FIXED, MAX_OBJECT);
        let hi = match self.rng.below(100) {
            0..=69 => 64,
            70..=89 => 128,
            90..=97 => 1024,
            _ => max_size,
        };
        self.rng.range(NODE_FIXED, hi.min(max_size))
    }

    pub fn ptr(&self, id: u32) -> *mut Header {
        self.objs[&id].ptr
    }

    pub fn random_id(&mut self) -> Option<u32> {
        if self.objs.is_empty() {
            return None;
        }
        let n = self.rng.below(self.objs.len());
        self.objs.keys().nth(n).copied()
    }

    /// Points edge `i` of `from` at `to`, in memory and in the model.
    pub fn set_edge(&mut self, from: u32, i: usize, to: Option<u32>) {
        let target = to.map_or(std::ptr::null_mut(), |t| self.objs[&t].ptr);
        let obj = self.objs.get_mut(&from).unwrap();
        obj.edges[i] = to;
        unsafe { node_set_edge(obj.ptr, i, target) };
    }

    /// Rewires a random edge of up to `n` random objects.
    pub fn mutate(&mut self, n: usize) {
        for _ in 0..n {
            let Some(from) = self.random_id() else { return };
            let len = self.objs[&from].edges.len();
            if len == 0 {
                continue;
            }
            let i = self.rng.below(len);
            let to = if self.rng.chance(1, 8) {
                None
            } else {
                self.random_id()
            };
            self.set_edge(from, i, to);
        }
    }

    /// Ids reachable from `roots` in the model.
    pub fn reachable(&self, roots: &[u32]) -> HashSet<u32> {
        let mut seen = HashSet::new();
        let mut stack: Vec<u32> = roots.to_vec();
        while let Some(id) = stack.pop() {
            if seen.insert(id) {
                stack.extend(self.objs[&id].edges.iter().flatten());
            }
        }
        seen
    }

    /// Runs a full cycle from `roots`, tracing through the objects' memory,
    /// and checks the outcome against the model. Returns the stats.
    pub fn collect(&mut self, roots: &[u32]) -> CollectStats {
        let expected = self.reachable(roots);

        // The heap holds exactly the objects the model knows.
        let mut present = HashSet::new();
        self.heap
            .for_each_object(|p| assert!(present.insert(p), "object enumerated twice"));
        let known: HashSet<_> = self.objs.values().map(|o| o.ptr).collect();
        assert_eq!(present, known, "heap and model disagree before the cycle");

        self.heap.begin_collect();
        let mut gray = Vec::new();
        for &r in roots {
            let p = self.ptr(r);
            if unsafe { self.heap.mark(p) } {
                gray.push(p);
            }
        }
        while let Some(p) = gray.pop() {
            assert!(unsafe { self.heap.is_marked(p) });
            for i in 0..unsafe { node_edges(p) } {
                let child = unsafe { node_edge(p, i) };
                if unsafe { self.heap.mark(child) } {
                    gray.push(child);
                }
            }
        }

        let mut dropped = Vec::new();
        let kinds: HashMap<u32, u8> = self.objs.iter().map(|(&id, o)| (id, o.kind)).collect();
        let stats = self.heap.end_collect(|p| {
            // The object is still intact when the callback sees it.
            let id = unsafe { node_id(p) };
            assert_eq!(
                unsafe { (*p).kind },
                kinds[&id],
                "kind clobbered before the drop"
            );
            dropped.push(id);
        });

        // Exactly the dead droppable objects were dropped, once each.
        let dropped_set: HashSet<u32> = dropped.iter().copied().collect();
        assert_eq!(
            dropped_set.len(),
            dropped.len(),
            "an object was dropped twice"
        );
        let want: HashSet<u32> = self
            .objs
            .iter()
            .filter(|(id, o)| o.droppable && !expected.contains(id))
            .map(|(&id, _)| id)
            .collect();
        assert_eq!(dropped_set, want, "wrong set of objects dropped");
        self.dropped_total += dropped.len();

        // Exactly the reachable objects remain.
        let mut after = HashSet::new();
        self.heap.for_each_object(|p| {
            after.insert(p);
        });
        let survivors: HashSet<_> = expected.iter().map(|id| self.objs[id].ptr).collect();
        assert_eq!(after, survivors, "survivors are not the reachable set");
        for &p in &after {
            assert!(self.heap.contains(p));
        }
        let dead = self.objs.len() - expected.len();
        assert_eq!(stats.objects_freed, dead);
        assert_eq!(stats.objects_live, expected.len());
        assert_eq!(
            stats.bytes_live,
            expected
                .iter()
                .map(|id| footprint(self.objs[id].size))
                .sum::<usize>()
        );
        assert_eq!(
            stats.bytes_freed,
            self.objs
                .iter()
                .filter(|(id, _)| !expected.contains(id))
                .map(|(_, o)| footprint(o.size))
                .sum::<usize>()
        );

        // Survivors keep their contents and their edges.
        for &id in &expected {
            let o = &self.objs[&id];
            unsafe {
                assert_eq!(node_id(o.ptr), id);
                assert_eq!((*o.ptr).kind, o.kind);
                assert_eq!(node_edges(o.ptr), o.edges.len());
                for (i, e) in o.edges.iter().enumerate() {
                    let want = e.map_or(std::ptr::null_mut(), |t| self.objs[&t].ptr);
                    assert_eq!(node_edge(o.ptr, i), want, "edge {i} of object {id} changed");
                }
                assert!(
                    node_pad_intact(o.ptr, o.size),
                    "payload of object {id} was clobbered"
                );
                assert!(self.heap.is_marked(o.ptr));
            }
        }
        self.objs.retain(|id, _| expected.contains(id));
        self.heap.verify();
        stats
    }

    /// Collects when due or when `alloc` has failed, then retries.
    pub fn spawn_collecting(&mut self, max_size: usize, roots: &[u32]) -> u32 {
        if self.heap.should_collect() {
            self.collect(roots);
        }
        if let Some(id) = self.spawn(max_size) {
            return id;
        }
        self.collect(roots);
        self.spawn(max_size).expect("heap full of live objects")
    }
}

/// Number of blocks a heap of `bytes` spans.
pub fn blocks(bytes: usize) -> usize {
    bytes / BLOCK_SIZE
}
