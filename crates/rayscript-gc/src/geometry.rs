//! Sizes the heap is carved into.

use crate::header::MAX_OBJECT;

/// Bytes in a block, the unit the pool hands out; also the largest object.
pub const BLOCK_SIZE: usize = MAX_OBJECT;
/// Bytes in a line, the unit of reuse after a sweep.
pub const LINE_SIZE: usize = 128;
/// Bytes in a chunk, the unit reserved from the system; chunk-aligned.
pub const CHUNK_SIZE: usize = 1 << CHUNK_SHIFT;

pub(crate) const CHUNK_SHIFT: u32 = 25;
/// Allocation granule: every object starts on, and is sized in, a quantum.
pub(crate) const QUANTUM: usize = 16;

pub(crate) const LINES_PER_BLOCK: usize = BLOCK_SIZE / LINE_SIZE;
pub(crate) const QUANTA_PER_LINE: usize = LINE_SIZE / QUANTUM;
pub(crate) const QUANTA_PER_BLOCK: usize = BLOCK_SIZE / QUANTUM;
pub(crate) const BLOCKS_PER_CHUNK: usize = CHUNK_SIZE / BLOCK_SIZE;
/// `u64` words of line-mark bits per block.
pub(crate) const LINE_WORDS: usize = LINES_PER_BLOCK / 64;

/// Address bits the chunk map covers.
pub(crate) const ADDRESS_BITS: u32 = if usize::BITS > 48 { 48 } else { usize::BITS };

/// Largest `max_bytes` honoured; bounds the side tables.
#[cfg(target_pointer_width = "64")]
pub(crate) const HARD_MAX_BYTES: usize = 1 << 40;
#[cfg(not(target_pointer_width = "64"))]
pub(crate) const HARD_MAX_BYTES: usize = 1 << 30;

const _: () = assert!(LINE_SIZE.is_power_of_two() && BLOCK_SIZE.is_power_of_two());
const _: () = assert!(LINES_PER_BLOCK.is_multiple_of(64));
const _: () = assert!(QUANTA_PER_LINE == 8);
const _: () =
    assert!(BLOCK_SIZE.is_multiple_of(LINE_SIZE) && CHUNK_SIZE.is_multiple_of(BLOCK_SIZE));
