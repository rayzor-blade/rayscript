//! RayScript's compiler to bytecode, and the VM that runs it in fibers.

pub mod bytecode;
pub mod compiler;
pub mod fiber;
pub mod heap;
pub mod object;
pub mod stdlib;
pub mod value;
pub mod vm;

pub use compiler::compile;
pub use value::{Obj, Value};
pub use vm::{Budget, ModuleResolver, NativeFn, NativePtr, NativeResult, Resolved, Slice, Vm};
