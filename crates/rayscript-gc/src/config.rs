//! Heap sizing and collection-trigger tuning.

/// Default cap on the heap, in bytes.
#[cfg(target_pointer_width = "64")]
const DEFAULT_MAX_BYTES: usize = 4 * 1024 * 1024 * 1024;
#[cfg(not(target_pointer_width = "64"))]
const DEFAULT_MAX_BYTES: usize = 256 * 1024 * 1024;

/// How a [`Heap`](crate::Heap) is sized and when it asks to be collected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapConfig {
    /// Cap on the bytes of blocks the heap may hold, rounded down to whole
    /// 32 KiB blocks (at least one). `alloc` returns null beyond it. Safe to
    /// raise; lowering it below a program's live set makes `alloc` fail.
    pub max_bytes: usize,
    /// Allocation volume between collections while the live set is small.
    pub trigger_floor: usize,
    /// Cap on that volume, unless the live set is larger than the cap, in
    /// which case the volume is the live set.
    pub trigger_ceiling: usize,
    /// The next trigger as a multiple of the live set (at least 1).
    pub growth: usize,
    /// Makes `should_collect` always true, to expose missing roots. Safe to
    /// run with; slow.
    pub stress: bool,
}

impl Default for HeapConfig {
    fn default() -> Self {
        HeapConfig {
            max_bytes: DEFAULT_MAX_BYTES,
            trigger_floor: 8 * 1024 * 1024,
            trigger_ceiling: 512 * 1024 * 1024,
            growth: 4,
            stress: false,
        }
    }
}
