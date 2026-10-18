//! RayScript's compiler to bytecode, and the VM that runs it in fibers.

pub mod bytecode;
pub mod compiler;
pub mod fiber;
pub use rayscript_gc as heap;
pub mod object;
pub mod stdlib;
pub mod value;
pub mod view;
pub mod vm;

pub use compiler::compile;
pub use value::{Obj, Value};
pub use fiber::FiberState;
pub use vm::{Budget, DebugFrame, DebugVar, ModuleResolver, NativeFn, NativePtr, NativeResult, PauseReason, Resolved, Slice, StepMode, Vm};
