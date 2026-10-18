//! A stand-in for `rayscript-gc`'s heap with the same interface, one allocation per object, so the VM can
//! be built and tested before the Immix heap replaces it.

use std::alloc::{Layout, alloc_zeroed, dealloc};

#[repr(C)]
pub struct Header {
    pub kind: u8,
    pub mark: u8,
    pub flags: u8,
    _pad: [u8; 5],
}

pub const HEADER_SIZE: usize = 8;
pub const MAX_OBJECT: usize = 32 * 1024;

const DROPPABLE: u8 = 1;

pub struct HeapConfig {
    pub max_bytes: usize,
    pub trigger_floor: usize,
    pub trigger_ceiling: usize,
    pub growth: usize,
    pub stress: bool,
}

impl Default for HeapConfig {
    fn default() -> Self {
        HeapConfig { max_bytes: 4 << 30, trigger_floor: 8 << 20, trigger_ceiling: 512 << 20, growth: 4, stress: false }
    }
}

#[derive(Default, Clone, Copy, Debug)]
pub struct CollectStats {
    pub objects_freed: usize,
    pub bytes_freed: usize,
    pub objects_live: usize,
    pub bytes_live: usize,
    pub blocks_released: usize,
}

#[derive(Default, Clone, Copy, Debug)]
pub struct HeapStats {
    pub objects: usize,
    pub bytes: usize,
    pub external: usize,
    pub cycles: usize,
}

pub struct Heap {
    config: HeapConfig,
    objects: Vec<(*mut Header, usize)>,
    bytes: usize,
    external: isize,
    trigger: usize,
    color: u8,
    cycles: usize,
}

unsafe impl Send for Heap {}

impl Heap {
    pub fn new(config: HeapConfig) -> Heap {
        let trigger = config.trigger_floor;
        Heap { config, objects: Vec::new(), bytes: 0, external: 0, trigger, color: 2, cycles: 0 }
    }

    pub fn alloc(&mut self, size: usize, kind: u8, droppable: bool) -> *mut Header {
        assert!((HEADER_SIZE..=MAX_OBJECT).contains(&size));
        let size = size.next_multiple_of(16);
        if self.bytes + size > self.config.max_bytes {
            return std::ptr::null_mut();
        }
        let p = unsafe { alloc_zeroed(Layout::from_size_align(size, 16).unwrap()) } as *mut Header;
        unsafe {
            (*p).kind = kind;
            (*p).flags = if droppable { DROPPABLE } else { 0 };
        }
        self.objects.push((p, size));
        self.bytes += size;
        p
    }

    pub fn should_collect(&self) -> bool {
        self.config.stress || self.bytes as isize + self.external > self.trigger as isize
    }

    pub fn track_external(&mut self, delta: isize) {
        self.external += delta;
    }

    pub fn begin_collect(&mut self) {
        self.color = if self.color == 2 { 4 } else { 2 };
    }

    pub unsafe fn mark(&mut self, obj: *mut Header) -> bool {
        if obj.is_null() || unsafe { (*obj).mark } == self.color {
            return false;
        }
        unsafe { (*obj).mark = self.color };
        true
    }

    pub unsafe fn is_marked(&self, obj: *const Header) -> bool {
        unsafe { (*obj).mark == self.color }
    }

    pub fn end_collect(&mut self, mut drop: impl FnMut(*mut Header)) -> CollectStats {
        let mut stats = CollectStats::default();
        let color = self.color;
        let mut live = Vec::with_capacity(self.objects.len());
        for (p, size) in self.objects.drain(..) {
            unsafe {
                if (*p).mark == color {
                    stats.objects_live += 1;
                    stats.bytes_live += size;
                    live.push((p, size));
                } else {
                    if (*p).flags & DROPPABLE != 0 {
                        drop(p);
                    }
                    stats.objects_freed += 1;
                    stats.bytes_freed += size;
                    dealloc(p as *mut u8, Layout::from_size_align(size, 16).unwrap());
                }
            }
        }
        self.objects = live;
        self.bytes = stats.bytes_live;
        self.cycles += 1;
        let want = (self.bytes + self.external.max(0) as usize) * self.config.growth;
        self.trigger = want.clamp(self.config.trigger_floor, self.config.trigger_ceiling);
        stats
    }

    pub fn stats(&self) -> HeapStats {
        HeapStats { objects: self.objects.len(), bytes: self.bytes, external: self.external.max(0) as usize, cycles: self.cycles }
    }

    pub fn for_each_object(&self, mut f: impl FnMut(*mut Header)) {
        for (p, _) in &self.objects {
            f(*p);
        }
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        for (p, size) in self.objects.drain(..) {
            unsafe { dealloc(p as *mut u8, Layout::from_size_align(size, 16).unwrap()) };
        }
    }
}
