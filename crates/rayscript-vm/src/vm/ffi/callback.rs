//! C closures enter only the active VM on its owning thread. No exception crosses C.

use super::*;
use std::{
    collections::HashSet,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::atomic::{AtomicBool, Ordering},
    thread::ThreadId,
};

type Prepared = RefCell<Option<Rc<Signature>>>;

pub(super) struct CallbackType {
    library: String,
    name: String,
    spec: String,
    arguments: String,
    ret: NativeType,
    failure: Slot,
    loaded: Rc<Library>,
    // Constructors retain signatures; a weak link avoids cycles through struct callback fields.
    prepared: RefCell<Weak<Prepared>>,
}

pub(super) fn bind(vm: &mut Vm, library: &str, name: &str, spec: &str) -> Result<Value, String> {
    let key = (library.into(), name.into());
    if Scalar::parse(name).is_ok()
        || ["Ptr", "Bytes", "Out"].contains(&name)
        || vm.foreign.types.contains_key(&key)
        || vm.foreign.structures.contains_key(&key)
    {
        return Err("callback name conflicts with another native type".into());
    }
    let ty = if let Some(ty) = vm.foreign.callbacks.get(&key) {
        if ty.spec != spec {
            return Err(
                "callback already declared with a different signature or failure value".into(),
            );
        }
        ty.clone()
    } else {
        let (signature, literal) = spec.split_once('\t').ok_or("invalid callback descriptor")?;
        let (arguments, result) = signature
            .split_once("->")
            .ok_or("invalid callback signature")?;
        let ret = if result == "Ptr" {
            NativeType::Ptr
        } else {
            NativeType::Scalar(Scalar::parse(result)?)
        };
        let failure = match &ret {
            NativeType::Ptr | NativeType::Scalar(Scalar::CString | Scalar::Void) => {
                if literal != "null" {
                    return Err(
                        "pointer, CString and Void callbacks need a null failure value".into(),
                    );
                }
                Slot { u64_: 0 }
            }
            NativeType::Scalar(s) => {
                let value = match literal {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    text => {
                        if let Ok(n) = text.parse::<i64>() {
                            Value::Int(n)
                        } else {
                            Value::Float(
                                text.parse::<f64>()
                                    .map_err(|_| "invalid callback failure value")?,
                            )
                        }
                    }
                };
                let mut scratch = Arguments::new(1);
                encode(*s, value, &mut scratch, 0)
                    .map_err(|e| format!("callback failure value: {e}"))?;
                scratch.slots[0]
            }
            _ => unreachable!(),
        };
        let ty = Rc::new(CallbackType {
            library: library.into(),
            name: name.into(),
            spec: spec.into(),
            arguments: arguments.into(),
            ret,
            failure,
            loaded: vm.native_library(library)?,
            prepared: RefCell::new(Weak::new()),
        });
        vm.foreign.callbacks.insert(key, ty.clone());
        ty
    };
    vm.foreign.frames.reserve(1);
    let prepared = Rc::new(RefCell::new(None));
    *ty.prepared.borrow_mut() = Rc::downgrade(&prepared);
    Ok(vm.native(name, move |vm, args| {
        let [callable] = args else {
            return Err(vm.err("a callback constructor needs one function"));
        };
        if !matches!(callable, Value::Native(_))
            && !callable.as_obj().is_some_and(|o| o.kind() == Kind::Closure)
        {
            return Err(vm.err("a callback constructor needs a function"));
        }
        let types = prepared
            .borrow()
            .clone()
            .ok_or_else(|| vm.err("callback signature is not initialized"))?;
        let callback = Callback::new(vm, ty.clone(), *callable, types).map_err(|e| vm.err(e))?;
        vm.foreign
            .callback_instances
            .retain(|w| w.strong_count() != 0);
        vm.foreign.callback_instances.push(Rc::downgrade(&callback));
        let value = vm.new_host(0, Box::new(Foreign::Callback(callback.clone())));
        callback.body.value.set(value);
        Ok(value)
    }))
}

impl CallbackType {
    fn arguments(&self, vm: &Vm) -> Result<Vec<NativeType>, String> {
        let args = if self.arguments.is_empty() {
            Vec::new()
        } else {
            self.arguments
                .split(',')
                .map(|name| NativeType::parse(name, &self.library, &vm.foreign))
                .collect::<Result<Vec<_>, _>>()?
        };
        if args.iter().any(|t| {
            matches!(
                t,
                NativeType::Bytes
                    | NativeType::Out(_)
                    | NativeType::Callback(_)
                    | NativeType::Scalar(Scalar::Void)
            )
        }) {
            return Err("callback arguments support scalars, Ptr, structs and handles".into());
        }
        Ok(args)
    }
}

pub(super) fn validate(vm: &Vm, library: &str, name: &str) -> Result<(), String> {
    let ty = vm
        .foreign
        .callbacks
        .get(&(library.into(), name.into()))
        .ok_or("unknown callback")?;
    let args = ty.arguments(vm)?;
    let prepared = ty
        .prepared
        .borrow()
        .upgrade()
        .ok_or("callback constructor is not initialized")?;
    let cif = Cif::try_new(args.iter().map(NativeType::ffi), ty.ret.ffi())
        .map_err(|e| format!("invalid callback ABI: {e:?}"))?;
    *prepared.borrow_mut() = Some(Rc::new(Signature { args, cif }));
    Ok(())
}

struct Signature {
    args: Vec<NativeType>,
    cif: Cif,
}

struct Body {
    ty: Rc<CallbackType>,
    signature: Rc<Signature>,
    callable: Value,
    value: Cell<Value>,
}

// Foreign threads may read only this immutable gate and set the rejection flag.
// Body is dereferenced only on the creating thread, with its VM active.
struct Gate {
    thread: ThreadId,
    vm: usize,
    ret: Scalar,
    failure: Slot,
    rejected: AtomicBool,
    body: *const Body,
}

pub(super) struct Callback {
    closure: *mut libffi::raw::ffi_closure,
    code: CodePtr,
    gate: Box<Gate>,
    body: Box<Body>,
}

impl Drop for Callback {
    fn drop(&mut self) {
        unsafe { libffi::low::closure_free(self.closure) };
    }
}

impl Callback {
    fn new(
        vm: &Vm,
        ty: Rc<CallbackType>,
        callable: Value,
        signature: Rc<Signature>,
    ) -> Result<Rc<Self>, String> {
        let body = Box::new(Body {
            ty: ty.clone(),
            signature,
            callable,
            value: Cell::new(Value::Nil),
        });
        let gate = Box::new(Gate {
            thread: std::thread::current().id(),
            vm: Rc::as_ptr(&vm.foreign.identity) as usize,
            ret: match ty.ret {
                NativeType::Scalar(s) => s,
                _ => Scalar::CString,
            },
            failure: ty.failure,
            rejected: AtomicBool::new(false),
            body: &*body,
        });
        let (closure, code) =
            libffi::low::try_closure_alloc().ok_or("cannot allocate native callback")?;
        let status = unsafe {
            libffi::raw::ffi_prep_closure_loc(
                closure,
                body.signature.cif.as_raw_ptr(),
                Some(trampoline),
                (&*gate as *const Gate).cast_mut().cast(),
                code.as_mut_ptr(),
            )
        };
        if status != libffi::raw::ffi_status_FFI_OK {
            unsafe { libffi::low::closure_free(closure) };
            return Err(format!("cannot prepare native callback: {status:?}"));
        }
        Ok(Rc::new(Self {
            closure,
            code,
            gate,
            body,
        }))
    }
    pub(super) fn callable(&self) -> Value {
        self.body.callable
    }
}

pub(super) fn pointer(value: Value, ty: &Rc<CallbackType>) -> Result<*mut c_void, String> {
    if matches!(value, Value::Nil) {
        return Ok(std::ptr::null_mut());
    }
    match foreign(value) {
        Some(Foreign::Callback(c)) if Rc::ptr_eq(&c.body.ty, ty) => Ok(c.code.as_mut_ptr()),
        _ => Err(format!("expected callback {} or null", ty.name)),
    }
}

pub(super) struct Frame {
    roots: Vec<Value>,
    pinned_strings: Vec<Rc<CString>>,
    return_strings: Vec<CString>,
    error: Option<Value>,
    pub(super) outputs: Vec<Value>,
}

impl Frame {
    pub(super) fn new(args: &[Value], types: &[NativeType]) -> Self {
        let mut frame = Self {
            roots: Vec::new(),
            pinned_strings: Vec::new(),
            return_strings: Vec::new(),
            error: None,
            outputs: args
                .iter()
                .zip(types)
                .filter_map(|(v, t)| matches!(t, NativeType::Out(_)).then_some(*v))
                .collect(),
        };
        frame.retain(args);
        frame
    }

    fn retain(&mut self, values: &[Value]) {
        let mut roots: Vec<_> = values
            .iter()
            .copied()
            .filter(|v| matches!(v, Value::Obj(_)))
            .collect();
        let mut seen = HashSet::new();
        let mut index = 0;
        // Snapshot before callbacks can replace pointer fields or captured values.
        while index < roots.len() {
            let value = roots[index];
            index += 1;
            if let Value::Obj(o) = value {
                if seen.insert(o) {
                    structure::pin_strings(value, &mut self.pinned_strings);
                    crate::object::trace_children(o, &mut |v| {
                        if matches!(v, Value::Obj(_)) {
                            roots.push(v);
                        }
                    });
                }
            }
        }
        self.roots.extend(roots);
    }
}

impl Libraries {
    pub(crate) fn trace(&self, f: &mut dyn FnMut(Value)) {
        for frame in &self.frames {
            for v in &frame.roots {
                f(*v);
            }
            if let Some(error) = frame.error {
                f(error);
            }
        }
    }
}

pub(super) fn finish(vm: &mut Vm) -> Option<Value> {
    let rejected = vm
        .foreign
        .callback_instances
        .iter()
        .filter_map(Weak::upgrade)
        .fold(false, |bad, c| {
            c.gate.rejected.swap(false, Ordering::Relaxed) || bad
        });
    if rejected && vm.foreign.frames.last().unwrap().error.is_none() {
        let error = vm.err("native callback requires its active VM on the creating thread");
        vm.foreign.frames.last_mut().unwrap().error = Some(error);
    }
    vm.foreign.frames.pop().unwrap().error
}

thread_local! { static ACTIVE: Cell<*mut Vm> = const { Cell::new(std::ptr::null_mut()) }; }
pub(super) struct Context(*mut Vm);
impl Context {
    pub(super) fn enter(vm: &mut Vm) -> Self {
        Self(ACTIVE.with(|active| active.replace(vm)))
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.set(self.0));
    }
}

// Integral closure results narrower than ffi_arg must be widened for libffi.
unsafe fn write_result(result: *mut c_void, ty: Scalar, slot: Slot) {
    unsafe {
        match ty {
            Scalar::Void => {}
            Scalar::I8 => result.cast::<isize>().write(slot.i8_ as isize),
            Scalar::I16 => result.cast::<isize>().write(slot.i16_ as isize),
            Scalar::I32 => result.cast::<isize>().write(slot.i32_ as isize),
            Scalar::U8 | Scalar::Bool => result.cast::<usize>().write(slot.u8_ as usize),
            Scalar::U16 => result.cast::<usize>().write(slot.u16_ as usize),
            Scalar::U32 => result.cast::<usize>().write(slot.u32_ as usize),
            Scalar::Float => result.cast::<f32>().write(slot.float),
            _ => result.cast::<Slot>().write(slot),
        }
    }
}

unsafe extern "C" fn trampoline(
    _cif: *mut libffi::raw::ffi_cif,
    result: *mut c_void,
    args: *mut *mut c_void,
    data: *mut c_void,
) {
    let gate = unsafe { &*data.cast::<Gate>() };
    unsafe { write_result(result, gate.ret, gate.failure) };
    if std::thread::current().id() != gate.thread {
        gate.rejected.store(true, Ordering::Relaxed);
        return;
    }
    let vm = ACTIVE.with(Cell::get);
    if vm.is_null() || unsafe { Rc::as_ptr(&(*vm).foreign.identity) as usize } != gate.vm {
        gate.rejected.store(true, Ordering::Relaxed);
        return;
    }
    let vm = unsafe { &mut *vm };
    if vm.foreign.frames.last().is_none_or(|f| f.error.is_some()) {
        return;
    }
    let body = unsafe { &*gate.body };
    let outcome = catch_unwind(AssertUnwindSafe(|| unsafe { body.invoke(vm, args) }));
    match outcome {
        Ok(Ok(slot)) => unsafe { write_result(result, gate.ret, slot) },
        other => {
            let error = match other {
                Ok(Err(error)) => error,
                _ => vm.err("panic in native callback"),
            };
            let frame = vm.foreign.frames.last_mut().unwrap();
            if frame.error.is_none() {
                frame.error = Some(error);
            }
        }
    }
}

struct Scope(Rc<Cell<bool>>);
impl Drop for Scope {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

impl Body {
    unsafe fn invoke(&self, vm: &mut Vm, raw: *mut *mut c_void) -> NativeResultSlot {
        let scope = Scope(Rc::new(Cell::new(false)));
        vm.foreign
            .frames
            .last_mut()
            .unwrap()
            .retain(&[self.value.get(), self.callable]);
        let mut args = Vec::with_capacity(self.signature.args.len());
        for (index, ty) in self.signature.args.iter().enumerate() {
            let ptr = unsafe { *raw.add(index) };
            if matches!(ty, NativeType::Scalar(Scalar::CString)) {
                let source = unsafe { ptr.cast::<*const c_char>().read() };
                let mut strings = Vec::new();
                structure::pin_cstring(vm, source, &mut strings);
                vm.foreign
                    .frames
                    .last_mut()
                    .unwrap()
                    .pinned_strings
                    .extend(strings);
                let keeper = structure::callback_argument(
                    vm,
                    &NativeType::Ptr,
                    source.cast_mut().cast(),
                    &self.ty.loaded,
                    scope.0.clone(),
                )
                .map_err(|e| vm.err(e))?;
                vm.foreign.frames.last_mut().unwrap().retain(&[keeper]);
            }
            let value = match ty {
                NativeType::Scalar(s) => {
                    let mut slot = Slot { u64_: 0 };
                    let size = match s {
                        Scalar::I8 | Scalar::U8 | Scalar::Bool => 1,
                        Scalar::I16 | Scalar::U16 => 2,
                        Scalar::I32 | Scalar::U32 | Scalar::Float => 4,
                        _ => 8,
                    };
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            ptr.cast::<u8>(),
                            (&mut slot as *mut Slot).cast::<u8>(),
                            size,
                        )
                    };
                    unsafe { decode(*s, slot, vm, false) }
                }
                _ => structure::callback_argument(
                    vm,
                    ty,
                    unsafe { ptr.cast::<*mut c_void>().read() },
                    &self.ty.loaded,
                    scope.0.clone(),
                ),
            }
            .map_err(|e| {
                vm.err(format!(
                    "callback {} argument {}: {e}",
                    self.ty.name,
                    index + 1
                ))
            })?;
            args.push(value);
            vm.foreign.frames.last_mut().unwrap().retain(&[value]);
        }
        let value = vm.call(self.callable, &args)?;
        vm.foreign.frames.last_mut().unwrap().retain(&[value]);
        match &self.ty.ret {
            NativeType::Scalar(Scalar::Void) => Ok(Slot { u64_: 0 }),
            NativeType::Scalar(s) => {
                let mut scratch = Arguments::new(1);
                encode(*s, value, &mut scratch, 0)
                    .map_err(|e| vm.err(format!("callback {} result: {e}", self.ty.name)))?;
                let slot = scratch.slots[0];
                if let Some(text) = scratch.strings[0].take() {
                    vm.foreign
                        .frames
                        .last_mut()
                        .unwrap()
                        .return_strings
                        .push(text);
                }
                Ok(slot)
            }
            NativeType::Ptr => super::pointer(value)
                .map(|ptr| Slot { ptr: ptr.cast() })
                .map_err(|e| vm.err(format!("callback {} result: {e}", self.ty.name))),
            _ => unreachable!(),
        }
    }
}
type NativeResultSlot = Result<Slot, Value>;
