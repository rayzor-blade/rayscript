//! Values: nil, booleans, numbers, host functions, and references to heap objects.

use crate::heap::Header;
use std::ptr::NonNull;

/// A reference to an object on the heap. The heap does not move objects, and a collection frees an
/// object only when nothing the VM can reach refers to it, so a reference taken from a root stays good;
/// one taken out of an object must not be held across a point where the VM may collect.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Obj(pub(crate) NonNull<Header>);

#[derive(Clone, Copy, Debug)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Obj(Obj),
    /// A function the host provides, by its place in the VM's table.
    Native(u32),
}

impl Value {
    pub fn is_nil(&self) -> bool {
        matches!(self, Value::Nil) || crate::vm::ffi::closed(*self)
    }

    /// Null, false and closed native handles or borrowed pointers are false.
    pub fn truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false)) && !crate::vm::ffi::closed(*self)
    }

    pub fn as_obj(&self) -> Option<Obj> {
        match self {
            Value::Obj(o) => Some(*o),
            _ => None,
        }
    }
}
