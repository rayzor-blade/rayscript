//! The header every allocation begins with.

/// Largest allocation: one block.
pub const MAX_OBJECT: usize = 32 * 1024;

/// Size of [`Header`], the least an allocation can ask for.
pub const HEADER_SIZE: usize = 8;

/// First field of every object. `kind` and `flags` belong to the owner of
/// the object; `mark` belongs to the collector and must not be written.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Header {
    /// The owner's type tag.
    pub kind: u8,
    /// The collector's liveness claim: 0 for a fresh object, else the colour
    /// of the cycle that last marked it.
    pub mark: u8,
    /// The owner's flag bits.
    pub flags: u8,
    _pad: [u8; 5],
}

const _: () = assert!(size_of::<Header>() == HEADER_SIZE);

impl Header {
    /// A header for `kind`, unmarked, with no flags.
    pub const fn new(kind: u8) -> Header {
        Header {
            kind,
            mark: 0,
            flags: 0,
            _pad: [0; 5],
        }
    }
}
