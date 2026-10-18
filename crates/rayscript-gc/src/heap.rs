//! The heap: block pool, bump regions, marking, and the sweep.

use std::cell::Cell;
use std::collections::HashMap;
use std::fmt;
use std::ptr;
use std::time::{Duration, Instant};

use crate::chunk::Chunk;
use crate::config::HeapConfig;
use crate::geometry::{
    ADDRESS_BITS, BLOCK_SIZE, BLOCKS_PER_CHUNK, CHUNK_SHIFT, CHUNK_SIZE, HARD_MAX_BYTES, LINE_SIZE,
    LINE_WORDS, LINES_PER_BLOCK, QUANTA_PER_BLOCK, QUANTA_PER_LINE, QUANTUM,
};
use crate::header::{HEADER_SIZE, Header, MAX_OBJECT};
use crate::stats::{CollectStats, HeapStats};

/// Start-byte code of an object that owns whole lines; its length is the
/// span-size table's entry for its first line. A small object stores its
/// size in quanta (1..=8) instead.
const SPAN: u8 = QUANTA_PER_LINE as u8 + 1;
/// The size code of a start byte.
const CODE_MASK: u8 = 0x0F;
/// Start-byte bit of an object the sweep reports to the drop callback.
const DROPPABLE: u8 = 0x10;

/// The two mark colours a cycle alternates between, so last cycle's
/// survivors read as unmarked in this one without a reset pass. Neither is 0,
/// the mark of a fresh object.
const COLOR_A: u8 = 2;
const COLOR_B: u8 = 4;

/// Idle time after which a poll may ask for a collection by default.
const HEARTBEAT: Duration = Duration::from_secs(30);
/// The clock is read once per this many polls.
const HEARTBEAT_POLLS: u32 = 1024;

/// A run of lines the allocator is bumping through, as byte offsets inside
/// one block. Empty when `cur == limit`.
#[derive(Clone, Copy)]
struct Region {
    base: *mut u8,
    block: u32,
    cur: u32,
    limit: u32,
}

impl Region {
    const EMPTY: Region = Region {
        base: ptr::null_mut(),
        block: 0,
        cur: 0,
        limit: 0,
    };
}

/// A run of free lines the last sweep found, waiting to become a region.
#[derive(Clone, Copy)]
struct Span {
    block: u32,
    first: u16,
    lines: u16,
}

/// What a sweep counted.
#[derive(Default)]
struct Swept {
    objects_freed: usize,
    bytes_freed: usize,
    objects_live: usize,
    bytes_live: usize,
    blocks_released: usize,
}

/// A single-threaded, non-moving Immix heap.
///
/// Memory is reserved in chunk-aligned chunks and carved into 32 KiB blocks
/// of 128-byte lines. An object of at most one line bump-allocates inside a
/// region of free lines and never straddles a line; a larger object takes
/// whole lines and shares none. Allocation starts and span sizes live in side
/// tables keyed by block, reserved once for the largest heap so they never
/// move. Every allocation begins with a [`Header`], whose `mark` byte is its
/// liveness claim. Nothing moves, so pointers stay valid until the object is
/// swept.
///
/// A collection is driven by the owner: [`begin_collect`](Heap::begin_collect),
/// then [`mark`](Heap::mark) on the roots and on each newly marked object's
/// children, then [`end_collect`](Heap::end_collect). To tear a heap down,
/// run an empty cycle (`begin_collect` then `end_collect` with no marks) so
/// the drop callback sees every droppable object; dropping the `Heap` itself
/// frees memory without running it.
///
/// The heap is not `Sync`:
///
/// ```compile_fail
/// fn needs_sync<T: Sync>() {}
/// needs_sync::<rayscript_gc::Heap>();
/// ```
pub struct Heap {
    config: HeapConfig,
    max_blocks: usize,
    max_slots: usize,

    /// Chunks by slot; a slot is empty once its chunk is released. A block
    /// number is `slot * BLOCKS_PER_CHUNK` plus its index in the chunk.
    chunks: Vec<Option<Chunk>>,
    /// Chunk address (by `CHUNK_SHIFT`) to its slot + 1, else 0.
    chunk_map: Box<[u32]>,
    /// Block numbers not holding objects; popped from the end, which keeps
    /// the lowest block first.
    free_blocks: Vec<u32>,
    blocks_in_use: usize,

    /// Per block: 1 if it holds objects or is (part of) a region.
    in_use: Box<[u8]>,
    /// Per quantum: 0 if no object starts there, else the object's size code
    /// in quanta (or `SPAN`) with `DROPPABLE` or'd in.
    starts: Box<[u8]>,
    /// Per line: lines owned by the span starting there, else 0.
    span_lines: Box<[u16]>,
    /// Per line: 1 if a droppable object starts in it, so the sweep must
    /// visit its objects even if nothing in the line is live.
    line_drop: Box<[u8]>,
    /// One bit per line: a live object was marked in it this cycle.
    line_marks: Box<[u64]>,

    small: Region,
    medium: Region,
    /// Free-line runs found by the sweep, consumed from the end.
    recycle: Vec<Span>,
    /// Upper bound on the longest run in `recycle`.
    max_span: usize,

    /// The colour that means "marked" in the open or last cycle.
    live_color: u8,
    collecting: bool,

    allocated_bytes: usize,
    object_count: usize,
    live_bytes: usize,
    bytes_since_collect: usize,
    external_bytes: usize,
    external_since_collect: usize,
    threshold: usize,
    cycles: usize,
    total_allocated: u64,
    total_freed: u64,

    heartbeat: Option<Duration>,
    last_collect: Instant,
    polls: Cell<u32>,
}

// SAFETY: the heap exclusively owns its chunks and tables; the raw pointers in
// it point only into memory it owns. Objects are reached through `&mut Heap`
// or through pointers the owner keeps on one thread at a time, so moving the
// heap to another thread moves everything it can touch with it.
unsafe impl Send for Heap {}

impl fmt::Debug for Heap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Heap")
            .field("config", &self.config)
            .field("stats", &self.stats())
            .finish()
    }
}

impl Heap {
    /// An empty heap. Side tables are reserved for `config.max_bytes` up
    /// front (the system commits them lazily); chunks are reserved as needed.
    pub fn new(config: HeapConfig) -> Heap {
        let max_blocks = (config.max_bytes.min(HARD_MAX_BYTES) / BLOCK_SIZE).max(1);
        let max_slots = max_blocks.div_ceil(BLOCKS_PER_CHUNK);
        let blocks = max_slots * BLOCKS_PER_CHUNK;
        Heap {
            config,
            max_blocks,
            max_slots,
            chunks: Vec::new(),
            chunk_map: vec![0; 1usize << (ADDRESS_BITS - CHUNK_SHIFT)].into_boxed_slice(),
            free_blocks: Vec::new(),
            blocks_in_use: 0,
            in_use: vec![0; blocks].into_boxed_slice(),
            starts: vec![0; blocks * QUANTA_PER_BLOCK].into_boxed_slice(),
            span_lines: vec![0; blocks * LINES_PER_BLOCK].into_boxed_slice(),
            line_drop: vec![0; blocks * LINES_PER_BLOCK].into_boxed_slice(),
            line_marks: vec![0; blocks * LINE_WORDS].into_boxed_slice(),
            small: Region::EMPTY,
            medium: Region::EMPTY,
            recycle: Vec::new(),
            max_span: 0,
            live_color: COLOR_A,
            collecting: false,
            allocated_bytes: 0,
            object_count: 0,
            live_bytes: 0,
            bytes_since_collect: 0,
            external_bytes: 0,
            external_since_collect: 0,
            threshold: config.trigger_floor,
            cycles: 0,
            total_allocated: 0,
            total_freed: 0,
            heartbeat: Some(HEARTBEAT),
            last_collect: Instant::now(),
            polls: Cell::new(0),
        }
    }

    /// The configuration the heap was made with.
    pub fn config(&self) -> &HeapConfig {
        &self.config
    }

    // -- Address resolution -------------------------------------------------

    /// Block number holding `addr`, if it lies in a live chunk.
    #[inline]
    fn block_of(&self, addr: usize) -> Option<usize> {
        let slot = *self.chunk_map.get(addr >> CHUNK_SHIFT)?;
        if slot == 0 {
            return None;
        }
        Some((slot as usize - 1) * BLOCKS_PER_CHUNK + (addr & (CHUNK_SIZE - 1)) / BLOCK_SIZE)
    }

    /// Start of block `b`, which must lie in a live chunk.
    fn block_base(&self, b: usize) -> *mut u8 {
        let chunk = self.chunks[b / BLOCKS_PER_CHUNK]
            .as_ref()
            .expect("block outside a live chunk");
        chunk
            .base()
            .wrapping_add((b % BLOCKS_PER_CHUNK) * BLOCK_SIZE)
    }

    /// The eight start bytes of the line whose first quantum is `qi`, as one
    /// word: zero when no object starts in the line.
    #[inline]
    fn line_starts(&self, qi: usize) -> u64 {
        let mut w = [0u8; QUANTA_PER_LINE];
        w.copy_from_slice(&self.starts[qi..qi + QUANTA_PER_LINE]);
        u64::from_ne_bytes(w)
    }

    /// Bytes an object with start byte `code`, first line `line`, occupies.
    #[inline]
    fn footprint(&self, line: usize, code: u8) -> usize {
        if code & CODE_MASK == SPAN {
            self.span_lines[line] as usize * LINE_SIZE
        } else {
            (code & CODE_MASK) as usize * QUANTUM
        }
    }

    // -- Block pool ---------------------------------------------------------

    /// Reserves a chunk into the lowest empty slot and pools its blocks.
    /// Called only when the pool is empty, so the new blocks pop lowest first.
    fn add_chunk(&mut self) -> bool {
        let slot = match self.chunks.iter().position(Option::is_none) {
            Some(slot) => slot,
            None if self.chunks.len() < self.max_slots => self.chunks.len(),
            None => return false,
        };
        let Some(chunk) = Chunk::reserve() else {
            return false;
        };
        let index = chunk.base().addr() >> CHUNK_SHIFT;
        if index >= self.chunk_map.len() {
            return false;
        }
        self.chunk_map[index] = slot as u32 + 1;
        if slot == self.chunks.len() {
            self.chunks.push(Some(chunk));
        } else {
            self.chunks[slot] = Some(chunk);
        }
        let first = slot * BLOCKS_PER_CHUNK;
        self.free_blocks
            .extend((first..first + BLOCKS_PER_CHUNK).rev().map(|b| b as u32));
        true
    }

    /// A free block, marked in use; `None` at the heap cap or when the
    /// system refuses a chunk.
    fn acquire_block(&mut self) -> Option<usize> {
        if self.blocks_in_use >= self.max_blocks {
            return None;
        }
        if self.free_blocks.is_empty() && !self.add_chunk() {
            return None;
        }
        let b = self.free_blocks.pop()? as usize;
        debug_assert_eq!(self.in_use[b], 0);
        self.in_use[b] = 1;
        self.blocks_in_use += 1;
        Some(b)
    }

    /// Releases empty chunks while at least `keep_free_bytes` of free blocks
    /// remain, then rebuilds the pool lowest block first. Returns the chunks
    /// released.
    fn rebuild_pool(&mut self, keep_free_bytes: usize) -> usize {
        let mut free = 0;
        let mut empty = Vec::new();
        for (slot, chunk) in self.chunks.iter().enumerate() {
            if chunk.is_none() {
                continue;
            }
            let lo = slot * BLOCKS_PER_CHUNK;
            let n = self.in_use[lo..lo + BLOCKS_PER_CHUNK]
                .iter()
                .filter(|&&u| u == 0)
                .count();
            free += n;
            if n == BLOCKS_PER_CHUNK {
                empty.push(slot);
            }
        }
        let mut released = 0;
        for &slot in empty.iter().rev() {
            if (free - BLOCKS_PER_CHUNK) * BLOCK_SIZE < keep_free_bytes {
                break;
            }
            free -= BLOCKS_PER_CHUNK;
            let chunk = self.chunks[slot].take().expect("empty chunk is present");
            self.chunk_map[chunk.base().addr() >> CHUNK_SHIFT] = 0;
            released += 1;
        }
        while matches!(self.chunks.last(), Some(None)) {
            self.chunks.pop();
        }
        self.free_blocks.clear();
        for slot in (0..self.chunks.len()).rev() {
            if self.chunks[slot].is_none() {
                continue;
            }
            for b in (slot * BLOCKS_PER_CHUNK..(slot + 1) * BLOCKS_PER_CHUNK).rev() {
                if self.in_use[b] == 0 {
                    self.free_blocks.push(b as u32);
                }
            }
        }
        released
    }

    /// Returns every empty chunk to the system and reports how many. The
    /// next allocation that needs memory reserves a chunk again.
    pub fn trim(&mut self) -> usize {
        assert!(!self.collecting, "trim inside a collection");
        self.rebuild_pool(0)
    }

    // -- Regions ------------------------------------------------------------

    fn region_at(&self, block: usize, cur: usize, limit: usize) -> Region {
        Region {
            base: self.block_base(block),
            block: block as u32,
            cur: cur as u32,
            limit: limit as u32,
        }
    }

    /// A region of at least `lines` lines: a recycled run if one fits, else
    /// a fresh block. `None` once the heap is exhausted.
    fn take_region(&mut self, lines: usize) -> Option<Region> {
        if lines <= self.max_span {
            match self.recycle.iter().rposition(|s| s.lines as usize >= lines) {
                Some(i) => {
                    let s = self.recycle.swap_remove(i);
                    let first = s.first as usize * LINE_SIZE;
                    let limit = first + s.lines as usize * LINE_SIZE;
                    return Some(self.region_at(s.block as usize, first, limit));
                }
                None => self.max_span = lines - 1,
            }
        }
        let b = self.acquire_block()?;
        Some(self.region_at(b, 0, BLOCK_SIZE))
    }

    /// Gives the lines the medium region did not use back as a recycled run.
    fn retire_medium(&mut self) {
        let r = std::mem::replace(&mut self.medium, Region::EMPTY);
        debug_assert_eq!(r.cur as usize % LINE_SIZE, 0);
        let lines = (r.limit - r.cur) as usize / LINE_SIZE;
        if lines > 0 {
            self.recycle.push(Span {
                block: r.block,
                first: (r.cur as usize / LINE_SIZE) as u16,
                lines: lines as u16,
            });
            self.max_span = self.max_span.max(lines);
        }
    }

    // -- Allocation ---------------------------------------------------------

    /// Allocates `size` bytes: zeroed, 16-byte aligned, with `header.kind`
    /// set to `kind` and `header.mark` 0. `size` must be in
    /// `HEADER_SIZE..=MAX_OBJECT` (it panics otherwise) and is rounded up to
    /// 16 bytes; an object larger than a line is rounded up to whole lines.
    ///
    /// `droppable` objects are passed to the drop callback of `end_collect`
    /// when they die; the others are reclaimed without a callback.
    ///
    /// Never starts a collection. Returns null only when `max_bytes` would be
    /// exceeded; collect and retry.
    #[inline]
    pub fn alloc(&mut self, size: usize, kind: u8, droppable: bool) -> *mut Header {
        assert!(
            (HEADER_SIZE..=MAX_OBJECT).contains(&size),
            "alloc: size {size} is outside {HEADER_SIZE}..={MAX_OBJECT}"
        );
        debug_assert!(!self.collecting, "alloc during a collection");
        let flag = if droppable { DROPPABLE } else { 0 };
        let rounded = size.next_multiple_of(QUANTUM);
        let (p, footprint) = if size <= LINE_SIZE {
            (self.alloc_small(rounded, flag), rounded)
        } else {
            let lines = size.div_ceil(LINE_SIZE);
            (self.alloc_medium(lines, flag), lines * LINE_SIZE)
        };
        if p.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: `p` starts `rounded` bytes the allocator just reserved for
        // this object; nothing else refers to them.
        unsafe {
            ptr::write_bytes(p, 0, rounded);
            (*p.cast::<Header>()).kind = kind;
        }
        self.allocated_bytes += footprint;
        self.object_count += 1;
        self.bytes_since_collect += footprint;
        self.total_allocated += footprint as u64;
        p.cast()
    }

    /// Bump `size` bytes (a multiple of 16, at most a line) out of the small
    /// region, never across a line. Null once the heap is exhausted.
    fn alloc_small(&mut self, size: usize, flag: u8) -> *mut u8 {
        let size = size as u32;
        loop {
            let mut r = self.small;
            let mut at = r.cur;
            if (at as usize % LINE_SIZE) + size as usize > LINE_SIZE {
                at = at.next_multiple_of(LINE_SIZE as u32);
            }
            if at + size <= r.limit {
                r.cur = at + size;
                self.small = r;
                let (b, at) = (r.block as usize, at as usize);
                self.starts[b * QUANTA_PER_BLOCK + at / QUANTUM] =
                    (size as usize / QUANTUM) as u8 | flag;
                if flag != 0 {
                    self.line_drop[b * LINES_PER_BLOCK + at / LINE_SIZE] = 1;
                }
                return r.base.wrapping_add(at);
            }
            match self.take_region(1) {
                Some(region) => self.small = region,
                None => return ptr::null_mut(),
            }
        }
    }

    /// Take `lines` whole lines out of the medium region. Null once the heap
    /// is exhausted.
    fn alloc_medium(&mut self, lines: usize, flag: u8) -> *mut u8 {
        let need = (lines * LINE_SIZE) as u32;
        loop {
            let mut r = self.medium;
            if r.cur + need <= r.limit {
                let at = r.cur;
                r.cur += need;
                self.medium = r;
                let (b, at) = (r.block as usize, at as usize);
                let line = b * LINES_PER_BLOCK + at / LINE_SIZE;
                self.starts[b * QUANTA_PER_BLOCK + at / QUANTUM] = SPAN | flag;
                self.span_lines[line] = lines as u16;
                if flag != 0 {
                    self.line_drop[line] = 1;
                }
                return r.base.wrapping_add(at);
            }
            self.retire_medium();
            match self.take_region(lines) {
                Some(region) => self.medium = region,
                None => return ptr::null_mut(),
            }
        }
    }

    // -- Trigger ------------------------------------------------------------

    /// Whether a collection is due: allocation volume plus external growth
    /// since the last collection has reached the trigger, the heap is in
    /// stress mode, or it has sat idle with garbage past the heartbeat.
    pub fn should_collect(&self) -> bool {
        if self.config.stress {
            return true;
        }
        let pressure = self
            .bytes_since_collect
            .saturating_add(self.external_since_collect);
        if pressure >= self.threshold {
            return true;
        }
        // The clock is read rarely: a long-idle heap with some garbage gets
        // collected on a heartbeat rather than never.
        let Some(idle) = self.heartbeat else {
            return false;
        };
        if self.bytes_since_collect == 0 {
            return false;
        }
        let polls = self.polls.get().wrapping_add(1);
        self.polls.set(polls);
        polls.is_multiple_of(HEARTBEAT_POLLS) && self.last_collect.elapsed() >= idle
    }

    /// Sets how long the heap may sit with new garbage before `should_collect`
    /// asks for a collection; `None` turns the heartbeat off.
    pub fn set_heartbeat(&mut self, idle: Option<Duration>) {
        self.heartbeat = idle;
    }

    /// Reports bytes an object owns outside the heap (a `Vec` or `String`
    /// buffer): positive when taken, negative when released. Growth counts
    /// toward the trigger; a release only cancels growth since the last
    /// collection, never earlier.
    pub fn track_external(&mut self, delta: isize) {
        let n = delta.unsigned_abs();
        if delta >= 0 {
            self.external_bytes = self.external_bytes.saturating_add(n);
            self.external_since_collect = self.external_since_collect.saturating_add(n);
        } else {
            self.external_bytes = self.external_bytes.saturating_sub(n);
            self.external_since_collect = self.external_since_collect.saturating_sub(n);
        }
    }

    // -- Collection ---------------------------------------------------------

    /// Opens a cycle: the other colour now means marked, so last cycle's
    /// survivors count as unmarked without a reset pass. Must be paired with
    /// [`end_collect`](Heap::end_collect), and nothing allocates in between.
    pub fn begin_collect(&mut self) {
        assert!(!self.collecting, "begin_collect inside a collection");
        self.live_color = if self.live_color == COLOR_A {
            COLOR_B
        } else {
            COLOR_A
        };
        self.collecting = true;
    }

    /// Claims `obj` live for the open cycle. Returns true if it was not yet
    /// marked, in which case the caller traces its children; false if it was,
    /// or if `obj` is null.
    ///
    /// # Safety
    /// A non-null `obj` must point at the header of an object this heap has
    /// allocated and not yet swept.
    #[inline]
    pub unsafe fn mark(&mut self, obj: *mut Header) -> bool {
        if obj.is_null() {
            return false;
        }
        debug_assert!(self.collecting, "mark outside a collection");
        // SAFETY: the caller guarantees `obj` is a live allocation, so its
        // header is readable.
        if unsafe { (*obj).mark } == self.live_color {
            return false;
        }
        let addr = obj.addr();
        let b = self
            .block_of(addr)
            .expect("mark: pointer is not in this heap");
        let off = addr & (BLOCK_SIZE - 1);
        let code = self.starts[b * QUANTA_PER_BLOCK + off / QUANTUM];
        assert!(
            code != 0 && off.is_multiple_of(QUANTUM),
            "mark: pointer is not the start of an allocation"
        );
        // SAFETY: as above, and the header is writable.
        unsafe { (*obj).mark = self.live_color };
        // The lines the object covers are live; the sweep frees the rest
        // without reading them.
        let first = b * LINES_PER_BLOCK + off / LINE_SIZE;
        let lines = if code & CODE_MASK == SPAN {
            self.span_lines[first] as usize
        } else {
            1
        };
        for l in first..first + lines {
            self.line_marks[l / 64] |= 1u64 << (l % 64);
        }
        true
    }

    /// Whether `obj` is marked for the open cycle (or, between cycles, for
    /// the last one). False for null.
    ///
    /// # Safety
    /// A non-null `obj` must point at the header of an object this heap has
    /// allocated and not yet swept.
    #[inline]
    pub unsafe fn is_marked(&self, obj: *const Header) -> bool {
        // SAFETY: the caller guarantees `obj` is a live allocation.
        !obj.is_null() && unsafe { (*obj).mark } == self.live_color
    }

    /// Closes the cycle: calls `drop(obj)` once for each unmarked droppable
    /// object, before its memory can be reused, then reclaims the lines and
    /// blocks of every unmarked object, returns empty chunks beyond the free
    /// capacity the next cycle needs, and sets the next trigger from the live
    /// size.
    ///
    /// The callback may touch only the object it is given; other objects may
    /// already be dead. It must not panic: the heap is left half-swept.
    pub fn end_collect(&mut self, mut drop: impl FnMut(*mut Header)) -> CollectStats {
        assert!(self.collecting, "end_collect outside a collection");
        // The open regions are swept like any other lines.
        self.small = Region::EMPTY;
        self.medium = Region::EMPTY;
        let before = (self.allocated_bytes, self.object_count);
        let swept = self.sweep(&mut drop);
        debug_assert_eq!(
            (
                swept.bytes_live + swept.bytes_freed,
                swept.objects_live + swept.objects_freed
            ),
            before
        );
        self.blocks_in_use -= swept.blocks_released;

        let live = swept.bytes_live;
        let floor = self.config.trigger_floor;
        let ceiling = self.config.trigger_ceiling.max(live).max(floor);
        self.threshold = live
            .saturating_mul(self.config.growth.max(1))
            .clamp(floor, ceiling);
        let chunks_released = self.rebuild_pool(self.threshold);

        self.allocated_bytes = live;
        self.object_count = swept.objects_live;
        self.live_bytes = live;
        self.bytes_since_collect = 0;
        self.external_since_collect = 0;
        self.total_freed += swept.bytes_freed as u64;
        self.cycles += 1;
        self.last_collect = Instant::now();
        self.collecting = false;
        CollectStats {
            objects_freed: swept.objects_freed,
            bytes_freed: swept.bytes_freed,
            objects_live: swept.objects_live,
            bytes_live: swept.bytes_live,
            blocks_released: swept.blocks_released,
            chunks_released,
        }
    }

    /// Clears the start byte at `qi` of the dead object whose first line is
    /// `line`, and its span size if it has one. Returns its footprint.
    fn free_start(&mut self, qi: usize, line: usize, code: u8) -> usize {
        let bytes = self.footprint(line, code);
        if code & CODE_MASK == SPAN {
            self.span_lines[line] = 0;
        }
        self.starts[qi] = 0;
        bytes
    }

    /// Frees every unmarked object and rebuilds the runs of free lines.
    ///
    /// A line with no marked object and no droppable start holds only dead
    /// plain objects, so its start bytes are cleared without reading the
    /// objects. Every other line is walked object by object, reading each
    /// header's mark byte.
    fn sweep(&mut self, drop: &mut impl FnMut(*mut Header)) -> Swept {
        self.recycle.clear();
        self.max_span = 0;
        let color = self.live_color;
        let mut s = Swept::default();
        for b in 0..self.chunks.len() * BLOCKS_PER_CHUNK {
            if self.in_use[b] == 0 {
                continue;
            }
            let base = self.block_base(b);
            let line0 = b * LINES_PER_BLOCK;
            let quantum0 = b * QUANTA_PER_BLOCK;
            // Taking the marks leaves the table clear for the next cycle.
            let mut marks = [0u64; LINE_WORDS];
            for (i, w) in marks.iter_mut().enumerate() {
                *w = std::mem::take(&mut self.line_marks[b * LINE_WORDS + i]);
            }
            let live_line = |l: usize| marks[l / 64] & (1u64 << (l % 64)) != 0;

            for l in 0..LINES_PER_BLOCK {
                let line = line0 + l;
                let qi = quantum0 + l * QUANTA_PER_LINE;
                if self.line_starts(qi) == 0 {
                    continue;
                }
                if !live_line(l) && self.line_drop[line] == 0 {
                    for k in 0..QUANTA_PER_LINE {
                        let code = self.starts[qi + k];
                        if code != 0 {
                            s.objects_freed += 1;
                            s.bytes_freed += self.free_start(qi + k, line, code);
                        }
                    }
                    continue;
                }
                // The line keeps its droppable flag only for a survivor the
                // next sweep may have to drop.
                let mut keep_drop = false;
                for k in 0..QUANTA_PER_LINE {
                    let code = self.starts[qi + k];
                    if code == 0 {
                        continue;
                    }
                    let obj = base
                        .wrapping_add(l * LINE_SIZE + k * QUANTUM)
                        .cast::<Header>();
                    // SAFETY: a start byte records an allocation inside this
                    // live chunk, and chunk memory is initialised.
                    if unsafe { (*obj).mark } == color {
                        s.objects_live += 1;
                        s.bytes_live += self.footprint(line, code);
                        keep_drop |= code & DROPPABLE != 0;
                    } else {
                        if code & DROPPABLE != 0 {
                            drop(obj);
                        }
                        s.objects_freed += 1;
                        s.bytes_freed += self.free_start(qi + k, line, code);
                    }
                }
                self.line_drop[line] = u8::from(keep_drop);
            }

            if marks == [0; LINE_WORDS] {
                self.in_use[b] = 0;
                s.blocks_released += 1;
                continue;
            }
            // Runs of unmarked lines hold nothing: become bump regions.
            let mut l = 0;
            while l < LINES_PER_BLOCK {
                if live_line(l) {
                    l += 1;
                    continue;
                }
                let first = l;
                while l < LINES_PER_BLOCK && !live_line(l) {
                    l += 1;
                }
                self.recycle.push(Span {
                    block: b as u32,
                    first: first as u16,
                    lines: (l - first) as u16,
                });
                self.max_span = self.max_span.max(l - first);
            }
        }
        // Consumed from the end, so the lowest addresses come first.
        self.recycle.reverse();
        s
    }

    // -- Inspection ---------------------------------------------------------

    /// A snapshot of the counters.
    pub fn stats(&self) -> HeapStats {
        let chunks = self.chunks.iter().flatten().count();
        HeapStats {
            objects: self.object_count,
            bytes: self.allocated_bytes,
            external: self.external_bytes,
            cycles: self.cycles,
            live_bytes: self.live_bytes,
            blocks: self.blocks_in_use,
            free_blocks: self.free_blocks.len(),
            chunks,
            reserved_bytes: chunks * CHUNK_SIZE,
            bytes_since_collect: self.bytes_since_collect,
            trigger_threshold: self.threshold,
            total_allocated_bytes: self.total_allocated,
            total_freed_bytes: self.total_freed,
        }
    }

    /// Calls `f` with every allocation, dead but unswept ones included, in
    /// block then address order. For tests and debugging.
    pub fn for_each_object(&self, mut f: impl FnMut(*mut Header)) {
        for b in 0..self.chunks.len() * BLOCKS_PER_CHUNK {
            if self.in_use[b] == 0 {
                continue;
            }
            let base = self.block_base(b);
            for l in 0..LINES_PER_BLOCK {
                let qi = b * QUANTA_PER_BLOCK + l * QUANTA_PER_LINE;
                if self.line_starts(qi) == 0 {
                    continue;
                }
                for k in 0..QUANTA_PER_LINE {
                    if self.starts[qi + k] != 0 {
                        f(base.wrapping_add(l * LINE_SIZE + k * QUANTUM).cast());
                    }
                }
            }
        }
    }

    /// Whether `p` is the start of an allocation not yet swept. For debugging.
    pub fn contains(&self, p: *const Header) -> bool {
        let addr = p.addr();
        let Some(b) = self.block_of(addr) else {
            return false;
        };
        let off = addr & (BLOCK_SIZE - 1);
        self.in_use[b] != 0
            && off.is_multiple_of(QUANTUM)
            && self.starts[b * QUANTA_PER_BLOCK + off / QUANTUM] != 0
    }

    /// Checks the heap's tables and counters against each other and panics on
    /// the first inconsistency. Walks the whole heap; for tests and debugging.
    pub fn verify(&self) {
        // Per block: which lines are claimed by an object, a recycled run, or
        // a region, so no two claims overlap.
        let mut claimed: HashMap<usize, [bool; LINES_PER_BLOCK]> = HashMap::new();
        let (mut bytes, mut objects, mut in_use) = (0, 0, 0);
        for b in 0..self.chunks.len() * BLOCKS_PER_CHUNK {
            let q0 = b * QUANTA_PER_BLOCK;
            let l0 = b * LINES_PER_BLOCK;
            if self.chunks[b / BLOCKS_PER_CHUNK].is_none() || self.in_use[b] == 0 {
                assert!(
                    self.starts[q0..q0 + QUANTA_PER_BLOCK]
                        .iter()
                        .all(|&c| c == 0)
                        && self.span_lines[l0..l0 + LINES_PER_BLOCK]
                            .iter()
                            .all(|&n| n == 0)
                        && self.line_drop[l0..l0 + LINES_PER_BLOCK]
                            .iter()
                            .all(|&d| d == 0),
                    "block {b} is not in use but its tables are not clear"
                );
                continue;
            }
            in_use += 1;
            let owned = claimed.entry(b).or_insert([false; LINES_PER_BLOCK]);
            for l in 0..LINES_PER_BLOCK {
                let qi = q0 + l * QUANTA_PER_LINE;
                let starts = self.line_starts(qi) != 0;
                assert!(
                    !(starts && owned[l]),
                    "block {b} line {l}: an object starts inside a span"
                );
                let mut taken = 0u8;
                let mut droppable = false;
                for k in 0..QUANTA_PER_LINE {
                    let code = self.starts[qi + k];
                    if code == 0 {
                        continue;
                    }
                    objects += 1;
                    droppable |= code & DROPPABLE != 0;
                    let size = code & CODE_MASK;
                    if size == SPAN {
                        let n = self.span_lines[l0 + l] as usize;
                        assert!(
                            k == 0 && taken == 0,
                            "block {b} line {l}: span shares a line"
                        );
                        assert!(
                            n >= 1 && l + n <= LINES_PER_BLOCK,
                            "block {b} line {l}: bad span"
                        );
                        for claim in &mut owned[l..l + n] {
                            *claim = true;
                        }
                        taken = 0xFF;
                        bytes += n * LINE_SIZE;
                    } else {
                        let size = size as usize;
                        assert!(
                            (1..=QUANTA_PER_LINE).contains(&size) && k + size <= QUANTA_PER_LINE
                        );
                        let mask = (((1u16 << size) - 1) << k) as u8;
                        assert!(taken & mask == 0, "block {b} line {l}: objects overlap");
                        taken |= mask;
                        owned[l] = true;
                        bytes += size * QUANTUM;
                    }
                }
                assert_eq!(
                    self.line_drop[l0 + l] != 0,
                    droppable,
                    "block {b} line {l}: droppable flag is wrong"
                );
                if !starts {
                    assert_eq!(
                        self.span_lines[l0 + l],
                        0,
                        "block {b} line {l}: stray span size"
                    );
                }
            }
        }
        assert_eq!(bytes, self.allocated_bytes, "allocated bytes");
        assert_eq!(objects, self.object_count, "object count");
        assert_eq!(in_use, self.blocks_in_use, "blocks in use");
        assert!(in_use <= self.max_blocks, "more blocks in use than the cap");
        assert!(
            self.line_marks.iter().all(|&w| w == 0) || self.collecting,
            "line marks left over"
        );

        let mut claim = |b: usize, lo: usize, hi: usize| {
            let lines = claimed
                .get_mut(&b)
                .expect("claim in a block that is not in use");
            for (l, taken) in lines.iter_mut().enumerate().take(hi).skip(lo) {
                assert!(!*taken, "block {b} line {l}: claimed twice");
                *taken = true;
            }
        };
        for s in &self.recycle {
            claim(
                s.block as usize,
                s.first as usize,
                s.first as usize + s.lines as usize,
            );
            assert!(
                s.lines as usize <= self.max_span,
                "recycled run longer than max_span"
            );
        }
        for r in [self.small, self.medium] {
            if r.limit == 0 {
                continue;
            }
            assert!(r.cur <= r.limit && r.limit as usize <= BLOCK_SIZE);
            let b = r.block as usize;
            assert_eq!(self.in_use[b], 1, "region in a block that is not in use");
            assert_eq!(r.base, self.block_base(b), "region base");
            // The partly used line holds no start past the cursor.
            let partial = r.cur as usize / LINE_SIZE;
            let past = (r.cur as usize % LINE_SIZE).div_ceil(QUANTUM);
            let qi = b * QUANTA_PER_BLOCK + partial * QUANTA_PER_LINE;
            if r.cur < r.limit {
                assert!(
                    self.starts[qi + past..qi + QUANTA_PER_LINE]
                        .iter()
                        .all(|&c| c == 0),
                    "an object starts beyond a region's cursor"
                );
            }
            claim(
                b,
                (r.cur as usize).div_ceil(LINE_SIZE),
                r.limit as usize / LINE_SIZE,
            );
        }

        let mut pooled = self.free_blocks.clone();
        pooled.sort_unstable();
        assert!(
            pooled.windows(2).all(|w| w[0] != w[1]),
            "block pooled twice"
        );
        for &b in &pooled {
            assert_eq!(self.in_use[b as usize], 0, "pooled block {b} is in use");
            assert!(
                self.chunks[b as usize / BLOCKS_PER_CHUNK].is_some(),
                "pooled block {b} has no chunk"
            );
        }
        let present = self.chunks.iter().flatten().count();
        assert_eq!(
            pooled.len() + in_use,
            present * BLOCKS_PER_CHUNK,
            "blocks unaccounted for"
        );
        for (slot, chunk) in self.chunks.iter().enumerate() {
            if let Some(chunk) = chunk {
                assert_eq!(
                    self.chunk_map[chunk.base().addr() >> CHUNK_SHIFT] as usize,
                    slot + 1
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapped(heap: &Heap) -> usize {
        heap.chunk_map.iter().filter(|&&slot| slot != 0).count()
    }

    #[test]
    fn the_chunk_map_follows_the_chunks() {
        let mut heap = Heap::new(HeapConfig {
            max_bytes: 128 << 20,
            trigger_floor: 1 << 20,
            ..HeapConfig::default()
        });
        for _ in 0..3 * BLOCKS_PER_CHUNK * 8 {
            heap.alloc(4000, 1, false);
        }
        assert_eq!(mapped(&heap), 3);
        heap.begin_collect();
        heap.end_collect(|_| {});
        assert_eq!(mapped(&heap), 1);
        heap.trim();
        assert_eq!(mapped(&heap), 0);
        heap.alloc(64, 1, false);
        assert_eq!(mapped(&heap), 1);
        heap.verify();
    }

    #[test]
    fn geometry() {
        assert_eq!(LINES_PER_BLOCK, 256);
        assert_eq!(QUANTA_PER_BLOCK, 2048);
        assert_eq!(BLOCKS_PER_CHUNK, 1024);
        const { assert!(SPAN as usize > QUANTA_PER_LINE && SPAN & DROPPABLE == 0) };
        const { assert!(COLOR_A != 0 && COLOR_B != 0 && COLOR_A != COLOR_B) };
    }
}
