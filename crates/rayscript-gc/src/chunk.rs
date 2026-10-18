//! Chunks: the memory the heap reserves from the system.

use std::alloc::{Layout, alloc_zeroed, dealloc};

use crate::geometry::{CHUNK_SIZE, QUANTUM};

/// A zeroed, chunk-aligned run of `CHUNK_SIZE` bytes.
pub(crate) struct Chunk {
    raw: *mut u8,
    layout: Layout,
    base: *mut u8,
}

impl Chunk {
    /// Reserves a chunk, or `None` if the system refuses.
    ///
    /// The request is twice the chunk at 16-byte alignment so the allocator
    /// hands back lazily zeroed pages; an over-aligned request would be
    /// zero-filled eagerly. The chunk is the aligned half.
    pub(crate) fn reserve() -> Option<Chunk> {
        let layout = Layout::from_size_align(2 * CHUNK_SIZE, QUANTUM).ok()?;
        // SAFETY: the layout has a nonzero size.
        let raw = unsafe { alloc_zeroed(layout) };
        if raw.is_null() {
            return None;
        }
        let pad = raw.addr().wrapping_neg() & (CHUNK_SIZE - 1);
        Some(Chunk {
            raw,
            layout,
            base: raw.wrapping_add(pad),
        })
    }

    /// The chunk-aligned start; `CHUNK_SIZE` bytes are valid from here.
    pub(crate) fn base(&self) -> *mut u8 {
        self.base
    }
}

impl Drop for Chunk {
    fn drop(&mut self) {
        // SAFETY: `raw` and `layout` are exactly what `alloc_zeroed` was given.
        unsafe { dealloc(self.raw, self.layout) };
    }
}
