//! The objects on the heap: their layouts, how to read them, and how a collection traces and drops them.
//! Every object begins with a `Header`. The accessors hand out references whose lifetimes are not tied
//! to anything: they are good until the VM next collects, which happens only at a safepoint, never in
//! the middle of an operation.

use crate::fiber::Fiber;
use crate::heap::Header;
use crate::value::{Obj, Value};
use crate::view::{PropsObj, ReactiveObj, SignalObj, TemplateObj};
use std::any::Any;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Kind {
    String = 1,
    Array,
    Object,
    Map,
    Closure,
    Cell,
    Range,
    Fiber,
    Promise,
    Host,
    Signal,
    Reactive,
    Template,
    Props,
    Bytes,
}

impl Kind {
    fn from_u8(b: u8) -> Kind {
        match b {
            1 => Kind::String,
            2 => Kind::Array,
            3 => Kind::Object,
            4 => Kind::Map,
            5 => Kind::Closure,
            6 => Kind::Cell,
            7 => Kind::Range,
            8 => Kind::Fiber,
            9 => Kind::Promise,
            10 => Kind::Host,
            11 => Kind::Signal,
            12 => Kind::Reactive,
            13 => Kind::Template,
            14 => Kind::Props,
            15 => Kind::Bytes,
            _ => unreachable!("a header with an unknown kind"),
        }
    }

    /// Whether the objects of this kind own memory outside the heap, which the sweep must release.
    pub fn droppable(self) -> bool {
        !matches!(self, Kind::Cell | Kind::Range)
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::String => "String",
            Kind::Array => "Array",
            Kind::Object => "Object",
            Kind::Map => "Map",
            Kind::Closure => "Function",
            Kind::Cell => "Cell",
            Kind::Range => "Range",
            Kind::Fiber => "Fiber",
            Kind::Promise => "Promise",
            Kind::Host => "Host",
            Kind::Signal => "Signal",
            Kind::Reactive => "Computed",
            Kind::Template => "View",
            Kind::Props => "Props",
            Kind::Bytes => "Bytes",
        }
    }
}

#[repr(C)]
pub struct StringObj {
    pub header: Header,
    pub text: Box<str>,
}

#[repr(C)]
pub struct BytesObj {
    pub header: Header,
    pub data: Box<[std::cell::UnsafeCell<u8>]>,
}

impl BytesObj {
    pub fn len(&self) -> usize { self.data.len() }

    /// Interior mutable storage keeps repeated pointer arguments valid without exclusive reborrows.
    pub fn as_mut_ptr(&self) -> *mut u8 { self.data.as_ptr().cast::<u8>().cast_mut() }

    /// Read between native calls; C must not retain a buffer or write from another thread.
    pub fn slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.as_mut_ptr(), self.len()) }
    }
}

#[repr(C)]
pub struct ArrayObj {
    pub header: Header,
    pub items: Vec<Value>,
}

#[repr(C)]
pub struct ObjectObj {
    pub header: Header,
    pub fields: Fields,
}

#[repr(C)]
pub struct MapObj {
    pub header: Header,
    pub map: OrderedMap,
}

#[repr(C)]
pub struct ClosureObj {
    pub header: Header,
    pub proto: u32,
    pub upvals: Vec<Obj>,
}

#[repr(C)]
pub struct CellObj {
    pub header: Header,
    pub value: Value,
}

#[repr(C)]
pub struct RangeObj {
    pub header: Header,
    pub from: i64,
    pub to: i64,
}

#[repr(C)]
pub struct FiberObj {
    pub header: Header,
    pub fiber: Fiber,
}

#[derive(Clone, Copy)]
pub enum Settled {
    Pending,
    Resolved(Value),
    Rejected(Value),
}

/// What a host call returns for an answer that comes later: a fiber that awaits it waits for the host to settle it.
#[repr(C)]
pub struct PromiseObj {
    pub header: Header,
    pub state: Settled,
    pub waiters: Vec<Obj>,
}

/// An object the host owns, seen from a script as an opaque value with whatever methods the host gave its type.
#[repr(C)]
pub struct HostObj {
    pub header: Header,
    pub type_id: u32,
    pub data: Box<dyn Any>,
    /// Script values retained by opaque host data, traced by the collector.
    pub references: Vec<Value>,
}

impl Obj {
    pub fn as_ptr(self) -> *mut Header {
        self.0.as_ptr()
    }

    pub fn kind(self) -> Kind {
        Kind::from_u8(unsafe { (*self.0.as_ptr()).kind })
    }

    pub fn string<'a>(self) -> &'a StringObj {
        debug_assert_eq!(self.kind(), Kind::String);
        unsafe { &*(self.0.as_ptr() as *const StringObj) }
    }

    pub fn bytes<'a>(self) -> &'a BytesObj {
        debug_assert_eq!(self.kind(), Kind::Bytes);
        unsafe { &*(self.0.as_ptr() as *const BytesObj) }
    }

    pub fn array<'a>(self) -> &'a mut ArrayObj {
        debug_assert_eq!(self.kind(), Kind::Array);
        unsafe { &mut *(self.0.as_ptr() as *mut ArrayObj) }
    }

    pub fn object<'a>(self) -> &'a mut ObjectObj {
        debug_assert_eq!(self.kind(), Kind::Object);
        unsafe { &mut *(self.0.as_ptr() as *mut ObjectObj) }
    }

    pub fn map<'a>(self) -> &'a mut MapObj {
        debug_assert_eq!(self.kind(), Kind::Map);
        unsafe { &mut *(self.0.as_ptr() as *mut MapObj) }
    }

    pub fn closure<'a>(self) -> &'a mut ClosureObj {
        debug_assert_eq!(self.kind(), Kind::Closure);
        unsafe { &mut *(self.0.as_ptr() as *mut ClosureObj) }
    }

    pub fn cell<'a>(self) -> &'a mut CellObj {
        debug_assert_eq!(self.kind(), Kind::Cell);
        unsafe { &mut *(self.0.as_ptr() as *mut CellObj) }
    }

    pub fn range<'a>(self) -> &'a RangeObj {
        debug_assert_eq!(self.kind(), Kind::Range);
        unsafe { &*(self.0.as_ptr() as *const RangeObj) }
    }

    pub fn fiber<'a>(self) -> &'a mut FiberObj {
        debug_assert_eq!(self.kind(), Kind::Fiber);
        unsafe { &mut *(self.0.as_ptr() as *mut FiberObj) }
    }

    pub fn promise<'a>(self) -> &'a mut PromiseObj {
        debug_assert_eq!(self.kind(), Kind::Promise);
        unsafe { &mut *(self.0.as_ptr() as *mut PromiseObj) }
    }

    pub fn signal<'a>(self) -> &'a mut SignalObj {
        debug_assert_eq!(self.kind(), Kind::Signal);
        unsafe { &mut *(self.0.as_ptr() as *mut SignalObj) }
    }

    pub fn reactive<'a>(self) -> &'a mut ReactiveObj {
        debug_assert_eq!(self.kind(), Kind::Reactive);
        unsafe { &mut *(self.0.as_ptr() as *mut ReactiveObj) }
    }

    pub fn template<'a>(self) -> &'a mut TemplateObj {
        debug_assert_eq!(self.kind(), Kind::Template);
        unsafe { &mut *(self.0.as_ptr() as *mut TemplateObj) }
    }

    pub fn props<'a>(self) -> &'a mut PropsObj {
        debug_assert_eq!(self.kind(), Kind::Props);
        unsafe { &mut *(self.0.as_ptr() as *mut PropsObj) }
    }

    pub fn host_ref<'a>(self) -> &'a HostObj {
        debug_assert_eq!(self.kind(), Kind::Host);
        unsafe { &*(self.0.as_ptr() as *const HostObj) }
    }

    pub fn host<'a>(self) -> &'a mut HostObj {
        debug_assert_eq!(self.kind(), Kind::Host);
        unsafe { &mut *(self.0.as_ptr() as *mut HostObj) }
    }

    pub fn str<'a>(self) -> &'a str {
        &self.string().text
    }
}

/// Calls `f` with each value an object refers to, for the collector to mark.
pub fn trace_children(obj: Obj, f: &mut dyn FnMut(Value)) {
    match obj.kind() {
        Kind::String | Kind::Range | Kind::Bytes => {}
        Kind::Host => { for value in &obj.host_ref().references { f(*value); } }
        Kind::Array => {
            for v in &obj.array().items {
                f(*v);
            }
        }
        Kind::Object => {
            for (_, v) in obj.object().fields.iter() {
                f(*v);
            }
        }
        Kind::Map => {
            for (k, v) in obj.map().map.iter() {
                f(k);
                f(v);
            }
        }
        Kind::Closure => {
            for u in &obj.closure().upvals {
                f(Value::Obj(*u));
            }
        }
        Kind::Cell => f(obj.cell().value),
        Kind::Signal => {
            let s = obj.signal();
            f(s.value);
            for sub in &s.subs {
                f(Value::Obj(*sub));
            }
        }
        Kind::Reactive => obj.reactive().trace(f),
        Kind::Template => obj.template().trace(f),
        Kind::Props => {
            let p = obj.props();
            f(p.statics);
            f(p.dynamics);
            f(p.children);
        }
        Kind::Fiber => obj.fiber().fiber.trace(f),
        Kind::Promise => {
            let p = obj.promise();
            match p.state {
                Settled::Resolved(v) | Settled::Rejected(v) => f(v),
                Settled::Pending => {}
            }
            for w in &p.waiters {
                f(Value::Obj(*w));
            }
        }
    }
}

/// Releases what a dead object owns outside the heap.
///
/// # Safety
/// `hdr` must be a dead, fully initialised object of a droppable kind that is not used again.
pub unsafe fn drop_object(hdr: *mut Header) {
    let obj = Obj(unsafe { std::ptr::NonNull::new_unchecked(hdr) });
    unsafe {
        match obj.kind() {
            Kind::String => std::ptr::drop_in_place(hdr as *mut StringObj),
            Kind::Bytes => std::ptr::drop_in_place(hdr as *mut BytesObj),
            Kind::Array => std::ptr::drop_in_place(hdr as *mut ArrayObj),
            Kind::Object => std::ptr::drop_in_place(hdr as *mut ObjectObj),
            Kind::Map => std::ptr::drop_in_place(hdr as *mut MapObj),
            Kind::Closure => std::ptr::drop_in_place(hdr as *mut ClosureObj),
            Kind::Fiber => std::ptr::drop_in_place(hdr as *mut FiberObj),
            Kind::Promise => std::ptr::drop_in_place(hdr as *mut PromiseObj),
            Kind::Host => std::ptr::drop_in_place(hdr as *mut HostObj),
            Kind::Signal => std::ptr::drop_in_place(hdr as *mut SignalObj),
            Kind::Reactive => std::ptr::drop_in_place(hdr as *mut ReactiveObj),
            Kind::Template => std::ptr::drop_in_place(hdr as *mut TemplateObj),
            Kind::Props => std::ptr::drop_in_place(hdr as *mut PropsObj),
            Kind::Cell | Kind::Range => {}
        }
    }
}

/// An object's fields, in the order they were added; names are interned symbols. Looked up by a scan
/// while there are few, and through an index after that.
#[derive(Default)]
pub struct Fields {
    entries: Vec<(u32, Value)>,
    index: Option<HashMap<u32, usize>>,
}

const INDEX_AFTER: usize = 8;

impl Fields {
    pub fn new() -> Fields {
        Fields::default()
    }

    fn position(&self, sym: u32) -> Option<usize> {
        match &self.index {
            Some(ix) => ix.get(&sym).copied(),
            None => self.entries.iter().position(|(s, _)| *s == sym),
        }
    }

    pub fn get(&self, sym: u32) -> Option<Value> {
        self.position(sym).map(|i| self.entries[i].1)
    }

    pub fn has(&self, sym: u32) -> bool {
        self.position(sym).is_some()
    }

    pub fn set(&mut self, sym: u32, v: Value) {
        match self.position(sym) {
            Some(i) => self.entries[i].1 = v,
            None => {
                self.entries.push((sym, v));
                if let Some(ix) = &mut self.index {
                    ix.insert(sym, self.entries.len() - 1);
                } else if self.entries.len() > INDEX_AFTER {
                    self.index = Some(self.entries.iter().enumerate().map(|(i, (s, _))| (*s, i)).collect());
                }
            }
        }
    }

    pub fn remove(&mut self, sym: u32) -> bool {
        match self.position(sym) {
            Some(i) => {
                self.entries.remove(i);
                if self.index.is_some() {
                    self.index = Some(self.entries.iter().enumerate().map(|(i, (s, _))| (*s, i)).collect());
                }
                true
            }
            None => false,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &(u32, Value)> {
        self.entries.iter()
    }
}

/// What a map is keyed by: numbers and strings by value, anything else by identity.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum MapKey {
    Nil,
    Bool(bool),
    Int(i64),
    Float(u64),
    Str(Box<str>),
    Obj(usize),
    Native(u32),
}

impl MapKey {
    pub fn of(v: Value) -> MapKey {
        match v {
            Value::Nil => MapKey::Nil,
            Value::Bool(b) => MapKey::Bool(b),
            Value::Int(i) => MapKey::Int(i),
            Value::Float(f) => {
                // A float that is a whole number is the same key as that integer.
                if f.fract() == 0.0 && f.abs() < 9.0e15 { MapKey::Int(f as i64) } else { MapKey::Float(f.to_bits()) }
            }
            Value::Native(n) => MapKey::Native(n),
            Value::Obj(o) => {
                if o.kind() == Kind::String { MapKey::Str(o.str().into()) } else { MapKey::Obj(o.as_ptr() as usize) }
            }
        }
    }
}

/// A map that remembers the order its keys were added in, and survives removal during a walk.
#[derive(Default)]
pub struct OrderedMap {
    index: HashMap<MapKey, usize>,
    entries: Vec<Option<(Value, Value)>>,
    live: usize,
}

impl OrderedMap {
    pub fn new() -> OrderedMap {
        OrderedMap::default()
    }

    pub fn len(&self) -> usize {
        self.live
    }

    pub fn get(&self, k: Value) -> Option<Value> {
        self.index.get(&MapKey::of(k)).and_then(|i| self.entries[*i]).map(|(_, v)| v)
    }

    pub fn contains(&self, k: Value) -> bool {
        self.index.contains_key(&MapKey::of(k))
    }

    pub fn set(&mut self, k: Value, v: Value) {
        let key = MapKey::of(k);
        match self.index.get(&key) {
            Some(i) => self.entries[*i] = Some((k, v)),
            None => {
                self.entries.push(Some((k, v)));
                self.index.insert(key, self.entries.len() - 1);
                self.live += 1;
            }
        }
    }

    pub fn remove(&mut self, k: Value) -> bool {
        match self.index.remove(&MapKey::of(k)) {
            Some(i) => {
                self.entries[i] = None;
                self.live -= 1;
                if self.entries.len() > 32 && self.live * 2 < self.entries.len() {
                    self.compact();
                }
                true
            }
            None => false,
        }
    }

    fn compact(&mut self) {
        self.entries.retain(|e| e.is_some());
        self.index.clear();
        for (i, e) in self.entries.iter().enumerate() {
            if let Some((k, _)) = e {
                self.index.insert(MapKey::of(*k), i);
            }
        }
    }

    pub fn clear(&mut self) {
        self.index.clear();
        self.entries.clear();
        self.live = 0;
    }

    pub fn iter(&self) -> impl Iterator<Item = (Value, Value)> + '_ {
        self.entries.iter().flatten().copied()
    }

    /// The entry at or after `pos`, and the position to continue from: for a walk that tolerates changes.
    pub fn entry_from(&self, mut pos: usize) -> Option<(usize, Value, Value)> {
        while pos < self.entries.len() {
            if let Some((k, v)) = self.entries[pos] {
                return Some((pos + 1, k, v));
            }
            pos += 1;
        }
        None
    }
}

/// Names the VM and the scripts share: field names, method names and globals, each a small number.
#[derive(Default)]
pub struct Symbols {
    ids: HashMap<std::rc::Rc<str>, u32>,
    names: Vec<std::rc::Rc<str>>,
}

impl Symbols {
    pub fn intern(&mut self, name: &str) -> u32 {
        if let Some(i) = self.ids.get(name) {
            return *i;
        }
        let rc: std::rc::Rc<str> = std::rc::Rc::from(name);
        let i = self.names.len() as u32;
        self.names.push(rc.clone());
        self.ids.insert(rc, i);
        i
    }

    pub fn name(&self, sym: u32) -> &str {
        &self.names[sym as usize]
    }

    pub fn lookup(&self, name: &str) -> Option<u32> {
        self.ids.get(name).copied()
    }
}
