//! RayScript's heap: a single-threaded, non-moving Immix collector over raw
//! memory, modelled on WrenLift's Immix heap.
//!
//! The heap allocates and sweeps; the owner traces. A collection is
//! `begin_collect`, `mark` on every root and on the children of each newly
//! marked object, then `end_collect`. Objects are at most [`MAX_OBJECT`] bytes
//! and begin with a [`Header`]. Memory owned outside the heap is reported with
//! `track_external` so it counts toward the trigger.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

mod chunk;
mod config;
mod geometry;
mod header;
mod heap;
mod stats;

pub use config::HeapConfig;
pub use geometry::{BLOCK_SIZE, CHUNK_SIZE, LINE_SIZE};
pub use header::{HEADER_SIZE, Header, MAX_OBJECT};
pub use heap::Heap;
pub use stats::{CollectStats, HeapStats};
