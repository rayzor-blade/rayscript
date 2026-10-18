//! What the heap reports about itself.

/// What one collection did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CollectStats {
    /// Objects swept, droppable or not.
    pub objects_freed: usize,
    /// Footprint of those objects: a small object's size rounded to 16, a
    /// larger one's rounded to whole 128-byte lines.
    pub bytes_freed: usize,
    /// Objects that survived.
    pub objects_live: usize,
    /// Footprint of the survivors.
    pub bytes_live: usize,
    /// Blocks that held nothing live and went back to the pool.
    pub blocks_released: usize,
    /// Empty chunks returned to the system.
    pub chunks_released: usize,
}

/// A snapshot of the heap's counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeapStats {
    /// Objects held now: survivors of the last collection plus new allocations.
    pub objects: usize,
    /// Footprint of those objects.
    pub bytes: usize,
    /// Bytes objects own outside the heap, as reported to `track_external`.
    pub external: usize,
    /// Collections completed.
    pub cycles: usize,
    /// Footprint of the objects the last collection kept; 0 before the first.
    pub live_bytes: usize,
    /// Blocks holding objects or handed to the allocator.
    pub blocks: usize,
    /// Blocks in the pool, ready to be handed out.
    pub free_blocks: usize,
    /// Chunks reserved from the system.
    pub chunks: usize,
    /// Bytes in those chunks.
    pub reserved_bytes: usize,
    /// Footprint allocated since the last collection.
    pub bytes_since_collect: usize,
    /// Pressure (allocated plus external growth) at which a collection is due.
    pub trigger_threshold: usize,
    /// Footprint ever allocated.
    pub total_allocated_bytes: u64,
    /// Footprint ever swept.
    pub total_freed_bytes: u64,
}
