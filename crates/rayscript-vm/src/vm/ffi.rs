//! C calls with typed output slots and traced native ownership. Library names are resolved only through the embedder's explicit registrations.

use super::{NativeResult, Vm};
use crate::{Value, object::Kind};
use libffi::middle::{Cif, CodePtr, Type};
use libloading::Library;
use std::{
    cell::{Cell, RefCell, UnsafeCell},
    collections::HashMap,
    ffi::{CStr, CString, c_char, c_void},
    path::{Path, PathBuf},
    rc::{Rc, Weak},
};

#[derive(Default)]
pub(crate) struct Libraries {
    paths: HashMap<String, PathBuf>,
    loaded: HashMap<String, Rc<Library>>,
    types: HashMap<(String, String), Rc<HandleType>>,
    releases: Rc<RefCell<Vec<Release>>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Scalar {
    Void,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Size,
    Bool,
    Float,
    Double,
    CString,
}

impl Scalar {
    fn parse(name: &str) -> Result<Self, String> {
        Ok(match name {
            "Void" => Self::Void,
            "Int8" => Self::I8,
            "Int16" => Self::I16,
            "Int32" => Self::I32,
            "Int64" | "Int" => Self::I64,
            "UInt8" => Self::U8,
            "UInt16" => Self::U16,
            "UInt32" => Self::U32,
            "UInt64" => Self::U64,
            "SizeT" => Self::Size,
            "Bool" => Self::Bool,
            "Float" => Self::Float,
            "Double" => Self::Double,
            "CString" => Self::CString,
            _ => return Err(format!("unsupported native type `{name}`")),
        })
    }
    fn ffi(self) -> Type {
        match self {
            Self::Void => Type::void(),
            Self::I8 => Type::i8(),
            Self::I16 => Type::i16(),
            Self::I32 => Type::i32(),
            Self::I64 => Type::i64(),
            Self::U8 | Self::Bool => Type::u8(),
            Self::U16 => Type::u16(),
            Self::U32 => Type::u32(),
            Self::U64 => Type::u64(),
            Self::Size => Type::usize(),
            Self::Float => Type::f32(),
            Self::Double => Type::f64(),
            Self::CString => Type::pointer(),
        }
    }
}

/// Every slot is large and aligned enough for a scalar or libffi's widened integer result.
#[derive(Clone, Copy)]
#[repr(C)]
union Slot {
    i8_: i8,
    i16_: i16,
    i32_: i32,
    i64_: i64,
    u8_: u8,
    u16_: u16,
    u32_: u32,
    u64_: u64,
    size: usize,
    float: f32,
    double: f64,
    ptr: *const c_char,
}

struct Arguments {
    slots: Box<[Slot]>,
    pointers: Box<[*mut c_void]>,
    strings: Box<[Option<CString>]>,
}

impl Arguments {
    fn new(count: usize) -> Self {
        let slots = vec![Slot { u64_: 0 }; count].into_boxed_slice();
        let pointers = vec![std::ptr::null_mut(); count].into_boxed_slice();
        let strings = (0..count)
            .map(|_| None)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            slots,
            pointers,
            strings,
        }
    }
}

#[derive(Clone)]
enum NativeType {
    Scalar(Scalar),
    Ptr,
    Bytes,
    Handle(Rc<HandleType>),
    Out(Box<NativeType>),
}

impl NativeType {
    fn parse(name: &str, library: &str, libraries: &Libraries) -> Result<Self, String> {
        if let Some(inner) = name.strip_prefix("Out<").and_then(|s| s.strip_suffix('>')) {
            let ty = Self::parse(inner, library, libraries)?;
            if matches!(ty, Self::Out(_) | Self::Bytes | Self::Scalar(Scalar::Void)) {
                return Err(format!("unsupported output type `{inner}`"));
            }
            return Ok(Self::Out(Box::new(ty)));
        }
        Ok(match name {
            "Ptr" => Self::Ptr,
            "Bytes" => Self::Bytes,
            _ => match libraries.types.get(&(library.into(), name.into())) {
                Some(ty) => Self::Handle(ty.clone()),
                None => Self::Scalar(Scalar::parse(name)?),
            },
        })
    }

    fn ffi(&self) -> Type {
        match self {
            Self::Scalar(s) => s.ffi(),
            _ => Type::pointer(),
        }
    }

    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Scalar(a), Self::Scalar(b)) => a == b,
            (Self::Ptr, Self::Ptr) | (Self::Bytes, Self::Bytes) => true,
            (Self::Handle(a), Self::Handle(b)) => Rc::ptr_eq(a, b),
            (Self::Out(a), Self::Out(b)) => a.same(b),
            _ => false,
        }
    }
}

struct HandleType {
    name: String,
    free_name: String,
    _library: Rc<Library>,
    free: unsafe extern "C" fn(*mut c_void),
    live: RefCell<HashMap<usize, Weak<Resource>>>,
}

struct Resource {
    ptr: Cell<*mut c_void>,
    ty: Rc<HandleType>,
    parent: Option<Rc<Resource>>,
    children: RefCell<Vec<Weak<Resource>>>,
    releases: Rc<RefCell<Vec<Release>>>,
}

struct Release {
    ptr: *mut c_void,
    ty: Rc<HandleType>,
    // Retained until the child has been freed, independently of heap sweep order.
    _parent: Option<Rc<Resource>>,
}

impl Resource {
    fn close(&self) {
        let ptr = self.ptr.replace(std::ptr::null_mut());
        if ptr.is_null() {
            return;
        }
        for child in self.children.borrow().iter().filter_map(Weak::upgrade) {
            child.close();
        }
        self.ty.live.borrow_mut().remove(&(ptr as usize));
        // The declaration and library registration guarantee a one-pointer C destructor.
        unsafe { (self.ty.free)(ptr) };
    }
}

impl Drop for Resource {
    fn drop(&mut self) {
        let ptr = self.ptr.get();
        if !ptr.is_null() {
            self.ty.live.borrow_mut().remove(&(ptr as usize));
            self.releases.borrow_mut().push(Release {
                ptr,
                ty: self.ty.clone(),
                _parent: self.parent.take(),
            });
        }
    }
}

enum Foreign {
    Type(NativeType),
    Pointer {
        ptr: *mut c_void,
        owner: Option<Rc<Resource>>,
        _library: Rc<Library>,
    },
    Handle(Rc<Resource>),
    Out {
        ty: NativeType,
        slot: Box<UnsafeCell<Slot>>,
    },
}

fn foreign(value: Value) -> Option<&'static Foreign> {
    value
        .as_obj()
        .filter(|o| o.kind() == Kind::Host)?
        .host_ref()
        .data
        .downcast_ref()
}

pub(crate) fn closed(value: Value) -> bool {
    match foreign(value) {
        Some(Foreign::Handle(resource)) => resource.ptr.get().is_null(),
        Some(Foreign::Pointer {
            owner: Some(owner), ..
        }) => owner.ptr.get().is_null(),
        _ => false,
    }
}

pub(crate) fn display_host(obj: crate::value::Obj) -> Option<String> {
    Some(match foreign(Value::Obj(obj))? {
        Foreign::Type(_) => "<native type>".into(),
        Foreign::Pointer { .. } => if closed(Value::Obj(obj)) {
            "null"
        } else {
            "<pointer>"
        }
        .into(),
        Foreign::Handle(r) => {
            if r.ptr.get().is_null() {
                "null".into()
            } else {
                format!("<handle {}>", r.ty.name)
            }
        }
        Foreign::Out { .. } => "<out>".into(),
    })
}

pub(crate) fn get_field(obj: crate::value::Obj, name: &str) -> Option<Result<Value, String>> {
    match (foreign(Value::Obj(obj))?, name) {
        (Foreign::Out { .. }, "value") => Some(Ok(obj
            .host_ref()
            .references
            .first()
            .copied()
            .unwrap_or(Value::Nil))),
        _ => None,
    }
}

pub(crate) fn install(vm: &mut Vm) {
    for name in [
        "Void", "Int8", "Int16", "Int32", "Int64", "Int", "UInt8", "UInt16", "UInt32", "UInt64",
        "SizeT", "Bool", "Float", "Double", "CString", "Ptr",
    ] {
        let ty = if name == "Ptr" {
            NativeType::Ptr
        } else {
            NativeType::Scalar(Scalar::parse(name).unwrap())
        };
        let token = vm.new_host(0, Box::new(Foreign::Type(ty)));
        vm.define_global(name, token);
    }
    vm.define_native("Out", |vm, args| {
        let [value] = args else {
            return Err(vm.err("Out needs one native type"));
        };
        let ty = match foreign(*value) {
            Some(Foreign::Type(ty))
                if !matches!(
                    ty,
                    NativeType::Out(_) | NativeType::Bytes | NativeType::Scalar(Scalar::Void)
                ) =>
            {
                ty.clone()
            }
            _ => return Err(vm.err("Out needs a scalar, pointer or handle type")),
        };
        Ok(vm.new_host(
            0,
            Box::new(Foreign::Out {
                ty,
                slot: Box::new(UnsafeCell::new(Slot { u64_: 0 })),
            }),
        ))
    });
    vm.define_method(Kind::Host, "close", |vm, args| {
        let [value] = args else {
            return Err(vm.err("close needs a handle and no arguments"));
        };
        match foreign(*value) {
            Some(Foreign::Handle(resource)) => {
                resource.close();
                vm.foreign.drain();
                Ok(Value::Nil)
            }
            _ => Err(vm.err("close needs a native handle")),
        }
    });
    vm.define_native("Bytes", |vm, args| {
        if let [source, Value::Int(length)] = args {
            let length = usize::try_from(*length)
                .ok()
                .filter(|n| *n <= isize::MAX as usize)
                .ok_or_else(|| vm.err("Bytes copy length out of range"))?;
            let ptr = pointer(*source).map_err(|e| vm.err(e))?;
            if let Value::Obj(o) = source {
                if o.kind() == Kind::Bytes && length > o.bytes().len() {
                    return Err(vm.err("Bytes copy exceeds source buffer"));
                }
            }
            if length == 0 {
                return Ok(vm.new_bytes(Vec::new()));
            }
            if ptr.is_null() {
                return Err(vm.err("cannot copy a null pointer"));
            }
            let mut data = Vec::new();
            data.try_reserve_exact(length)
                .map_err(|_| vm.err("Bytes allocation failed"))?;
            // Explicit library registration trusts script pointer bounds as well as C signatures.
            data.extend_from_slice(unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), length) });
            return Ok(vm.new_bytes(data));
        }
        let [value] = args else {
            return Err(vm.err("Bytes needs a length, String or byte array"));
        };
        let data = match value {
            Value::Int(n) if *n >= 0 => {
                let size = usize::try_from(*n).map_err(|_| vm.err("Bytes length out of range"))?;
                let mut data = Vec::new();
                data.try_reserve_exact(size)
                    .map_err(|_| vm.err("Bytes allocation failed"))?;
                data.resize(size, 0);
                data
            }
            Value::Obj(o) if o.kind() == Kind::String => o.str().as_bytes().to_vec(),
            Value::Obj(o) if o.kind() == Kind::Array => {
                let mut data = Vec::with_capacity(o.array().items.len());
                for value in &o.array().items {
                    let Value::Int(n) = value else {
                        return Err(vm.err("Bytes elements need Int in 0..255"));
                    };
                    data.push(
                        u8::try_from(*n)
                            .map_err(|_| vm.err("Bytes elements need Int in 0..255"))?,
                    );
                }
                data
            }
            _ => return Err(vm.err("Bytes needs a nonnegative length, String or byte array")),
        };
        Ok(vm.new_bytes(data))
    });
    vm.define_method(Kind::Bytes, "toString", |vm, args| {
        if args.len() != 1 {
            return Err(vm.err("Bytes.toString takes no arguments"));
        }
        let data = args[0].as_obj().unwrap().bytes().slice();
        let text = std::str::from_utf8(data).map_err(|_| vm.err("Bytes is not UTF-8"))?;
        Ok(vm.new_string(text))
    });
    vm.define_method(Kind::Bytes, "copy", |vm, args| {
        if args.len() != 1 {
            return Err(vm.err("Bytes.copy takes no arguments"));
        }
        Ok(vm.new_bytes(args[0].as_obj().unwrap().bytes().slice().to_vec()))
    });
}

impl Libraries {
    pub(crate) fn drain(&mut self) {
        // Drop each entry outside the queue borrow: releasing its owner may enqueue another free.
        loop {
            let next = self.releases.borrow_mut().pop();
            let Some(release) = next else { break };
            unsafe { (release.ty.free)(release.ptr) };
        }
    }
}

struct Bound {
    library: Rc<Library>,
    code: CodePtr,
    cif: Cif,
    args: Vec<NativeType>,
    ret: NativeType,
    name: String,
    owner: Option<usize>,
    destructor: bool,
    scratch: RefCell<Arguments>,
}

impl Vm {
    /// A fixed-size native buffer. Scripts read and write bytes by index.
    pub fn new_bytes(&mut self, data: Vec<u8>) -> Value {
        self.alloc(Kind::Bytes, |header| crate::object::BytesObj {
            header,
            data: data
                .into_iter()
                .map(UnsafeCell::new)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        })
        .map(Value::Obj)
        .unwrap_or(Value::Nil)
    }

    /// Registers a logical name for native declarations; binding loads the registered path lazily.
    /// Existing functions and handles retain their library when a registration is replaced.
    ///
    /// # Safety
    /// Library initialization and teardown must be safe, and every script declaration must match
    /// its symbol's C ABI, allocation, ownership and buffer bounds contracts. Scripts must be trusted.
    pub unsafe fn register_native_library(&mut self, name: &str, path: impl AsRef<Path>) {
        self.foreign
            .paths
            .insert(name.into(), path.as_ref().to_path_buf());
        self.foreign.loaded.remove(name);
        self.foreign.types.retain(|(library, _), _| library != name);
    }

    fn native_library(&mut self, library: &str) -> Result<Rc<Library>, String> {
        if let Some(loaded) = self.foreign.loaded.get(library) {
            return Ok(loaded.clone());
        }
        let path = self
            .foreign
            .paths
            .get(library)
            .ok_or_else(|| format!("native library `{library}` is not registered"))?;
        let loaded = Rc::new(
            unsafe { Library::new(path) }
                .map_err(|e| format!("cannot load {}: {e}", path.display()))?,
        );
        self.foreign.loaded.insert(library.into(), loaded.clone());
        Ok(loaded)
    }

    pub(crate) fn bind_native(&mut self, spec: &str) -> Result<Value, String> {
        let parts: Vec<_> = spec.split('\0').collect();
        let [library, symbol, signature, flags] = parts.as_slice() else {
            return Err("invalid native binding descriptor".into());
        };
        let name = format!("{library}.{symbol}");
        self.prepare_native(library, symbol, signature, flags, &name)
            .map_err(|e| format!("{name}: {e}"))
    }

    fn prepare_native(
        &mut self,
        library: &str,
        symbol: &str,
        signature: &str,
        flags: &str,
        name: &str,
    ) -> Result<Value, String> {
        if signature == "handle" {
            if Scalar::parse(symbol).is_ok() || ["Ptr", "Bytes", "Out"].contains(&symbol) {
                return Err("handle name conflicts with a built-in native type".into());
            }
            let key = (library.into(), symbol.into());
            let ty = if let Some(existing) = self.foreign.types.get(&key) {
                if existing.free_name != flags {
                    return Err("handle already declared with a different free function".into());
                }
                existing.clone()
            } else {
                let loaded = self.native_library(library)?;
                let free =
                    *unsafe { loaded.get::<unsafe extern "C" fn(*mut c_void)>(flags.as_bytes()) }
                        .map_err(|e| format!("free symbol `{flags}` not found: {e}"))?;
                let ty = Rc::new(HandleType {
                    name: name.into(),
                    free_name: flags.into(),
                    _library: loaded,
                    free,
                    live: RefCell::new(HashMap::new()),
                });
                self.foreign.types.insert(key, ty.clone());
                ty
            };
            return Ok(self.new_host(0, Box::new(Foreign::Type(NativeType::Handle(ty)))));
        }
        let owner = if flags.is_empty() {
            None
        } else if let Some(index) = flags.strip_prefix("owner=") {
            Some(
                index
                    .parse::<usize>()
                    .map_err(|_| "invalid owner annotation")?,
            )
        } else {
            return Err(format!(
                "native annotations `{flags}` are not available yet"
            ));
        };
        let (args, ret) = signature
            .split_once("->")
            .ok_or("invalid native signature")?;
        let args = if args.is_empty() {
            Vec::new()
        } else {
            args.split(',')
                .map(|s| NativeType::parse(s, library, &self.foreign))
                .collect::<Result<Vec<_>, _>>()?
        };
        if args
            .iter()
            .any(|a| matches!(a, NativeType::Scalar(Scalar::Void)))
        {
            return Err(
                "each native argument needs a scalar type; Void is only a result type".into(),
            );
        }
        let ret = NativeType::parse(ret, library, &self.foreign)?;
        if matches!(ret, NativeType::Out(_) | NativeType::Bytes) {
            return Err("Out and Bytes are argument types only".into());
        }
        if let Some(index) = owner {
            if !matches!(args.get(index), Some(NativeType::Handle(_))) {
                return Err("@:owner parameter must be a handle".into());
            }
            if !matches!(ret, NativeType::Handle(_) | NativeType::Ptr) && !args.iter().any(|a| matches!(a, NativeType::Out(t) if matches!(**t, NativeType::Handle(_) | NativeType::Ptr))) {
                return Err("@:owner needs a handle or pointer result or output".into());
            }
        }
        let destructor = matches!(args.as_slice(), [NativeType::Handle(ty)] if ty.free_name == symbol)
            && matches!(ret, NativeType::Scalar(Scalar::Void));
        let cif = Cif::try_new(args.iter().map(NativeType::ffi), ret.ffi())
            .map_err(|e| format!("invalid native signature: {e:?}"))?;
        let loaded = self.native_library(library)?;
        let code = CodePtr::from_fun(
            *unsafe { loaded.get::<unsafe extern "C" fn()>(symbol.as_bytes()) }
                .map_err(|e| format!("symbol not found: {e}"))?,
        );
        let bound = Bound {
            code,
            scratch: RefCell::new(Arguments::new(args.len())),
            library: loaded,
            cif,
            args,
            ret,
            name: name.into(),
            owner,
            destructor,
        };
        Ok(self.native(name, move |vm, args| bound.call(vm, args)))
    }
}

impl Bound {
    fn call(&self, vm: &mut Vm, args: &[Value]) -> NativeResult {
        let result = self.invoke(vm, args);
        vm.foreign.drain();
        result.map_err(|message| vm.err(format!("{}: {message}", self.name)))
    }

    fn invoke(&self, vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
        if args.len() != self.args.len() {
            return Err(format!(
                "expected {} arguments, got {}",
                self.args.len(),
                args.len()
            ));
        }
        if self.destructor {
            if matches!(args[0], Value::Nil) {
                return Ok(Value::Nil);
            }
            if let (NativeType::Handle(ty), Some(Foreign::Handle(r))) =
                (&self.args[0], foreign(args[0]))
            {
                if Rc::ptr_eq(&r.ty, ty) {
                    r.close();
                    return Ok(Value::Nil);
                }
            }
            return Err("argument 1: expected matching native handle".into());
        }
        let owner = self
            .owner
            .map(|index| match foreign(args[index]) {
                Some(Foreign::Handle(r)) if !r.ptr.get().is_null() => Ok((args[index], r.clone())),
                _ => Err("@:owner argument must be an open handle".to_string()),
            })
            .transpose()?;
        let mut scratch = self
            .scratch
            .try_borrow_mut()
            .map_err(|_| "reentrant native calls are not available yet")?;
        let result = (|| {
            for (index, (ty, value)) in self.args.iter().zip(args).enumerate() {
                if matches!(ty, NativeType::Out(_))
                    && args[..index].iter().any(|v| v.as_obj() == value.as_obj())
                {
                    return Err(format!(
                        "argument {}: an Out slot cannot be used twice in one call",
                        index + 1
                    ));
                }
                self.encode(ty, *value, &mut scratch, index)
                    .map_err(|e| format!("argument {}: {e}", index + 1))?;
            }
            let base = scratch.slots.as_mut_ptr();
            for (index, pointer) in scratch.pointers.iter_mut().enumerate() {
                *pointer = unsafe { base.add(index).cast() };
            }
            for (ty, value) in self.args.iter().zip(args) {
                if matches!(ty, NativeType::Out(_)) {
                    value.as_obj().unwrap().host().references.clear();
                }
            }
            let mut result = Slot { u64_: 0 };
            // Registered declarations guarantee C layouts, symbol ABI and the bounds of pointer arguments.
            unsafe {
                libffi::raw::ffi_call(
                    self.cif.as_raw_ptr(),
                    Some(*self.code.as_fun()),
                    (&mut result as *mut Slot).cast(),
                    scratch.pointers.as_mut_ptr(),
                );
            }
            let mut result = unsafe { self.decode(&self.ret, result, vm, owner.as_ref(), true) };
            // Decode every output, including when a different output has an invalid string result.
            for (ty, value) in self.args.iter().zip(args) {
                if let NativeType::Out(inner) = ty {
                    let slot = match foreign(*value) {
                        Some(Foreign::Out { slot, .. }) => unsafe { *slot.get() },
                        _ => unreachable!(),
                    };
                    match unsafe { self.decode(inner, slot, vm, owner.as_ref(), false) } {
                        Ok(decoded) => value.as_obj().unwrap().host().references = vec![decoded],
                        Err(error) => {
                            if result.is_ok() {
                                result = Err(error);
                            }
                        }
                    }
                }
            }
            result
        })();
        for string in scratch.strings.iter_mut() {
            *string = None;
        }
        result
    }

    fn encode(
        &self,
        ty: &NativeType,
        value: Value,
        scratch: &mut Arguments,
        index: usize,
    ) -> Result<(), String> {
        let ptr = match ty {
            NativeType::Scalar(s) => return encode(*s, value, scratch, index),
            NativeType::Ptr => pointer(value)?,
            NativeType::Bytes => match value {
                Value::Obj(o) if o.kind() == Kind::Bytes => o.bytes().as_mut_ptr().cast(),
                _ => return Err("expected Bytes".into()),
            },
            NativeType::Handle(ty) => match foreign(value) {
                Some(Foreign::Handle(r)) if Rc::ptr_eq(ty, &r.ty) => {
                    if r.ptr.get().is_null() {
                        return Err("handle is closed".into());
                    }
                    r.ptr.get()
                }
                _ if matches!(value, Value::Nil) => std::ptr::null_mut(),
                _ => return Err(format!("expected handle {} or null", ty.name)),
            },
            NativeType::Out(ty) => {
                let Some(Foreign::Out { ty: actual, slot }) = foreign(value) else {
                    return Err("expected Out slot".into());
                };
                if !ty.same(actual) {
                    return Err("Out slot has the wrong native type".into());
                }
                if matches!(**ty, NativeType::Handle(_)) {
                    let previous = value
                        .as_obj()
                        .unwrap()
                        .host_ref()
                        .references
                        .first()
                        .copied()
                        .unwrap_or(Value::Nil);
                    if !previous.is_nil() {
                        return Err(
                            "close the previous output handle before reusing its Out slot".into(),
                        );
                    }
                }
                unsafe {
                    *slot.get() = Slot { u64_: 0 };
                }
                slot.get().cast()
            }
        };
        scratch.slots[index] = Slot { ptr: ptr.cast() };
        Ok(())
    }

    unsafe fn decode(
        &self,
        ty: &NativeType,
        slot: Slot,
        vm: &mut Vm,
        owner: Option<&(Value, Rc<Resource>)>,
        widened: bool,
    ) -> Result<Value, String> {
        let data = match ty {
            NativeType::Scalar(s) => return unsafe { decode(*s, slot, vm, widened) },
            NativeType::Ptr if unsafe { slot.ptr.is_null() } => return Ok(Value::Nil),
            NativeType::Ptr => Foreign::Pointer {
                ptr: unsafe { slot.ptr.cast_mut().cast() },
                owner: owner.map(|(_, r)| r.clone()),
                _library: self.library.clone(),
            },
            NativeType::Handle(ty) => {
                let ptr = unsafe { slot.ptr.cast_mut().cast::<c_void>() };
                if ptr.is_null() {
                    return Ok(Value::Nil);
                }
                let existing = ty
                    .live
                    .borrow()
                    .get(&(ptr as usize))
                    .and_then(Weak::upgrade);
                let resource = if let Some(existing) = existing {
                    let same_owner = match (&existing.parent, owner) {
                        (None, None) => true,
                        (Some(a), Some((_, b))) => Rc::ptr_eq(a, b),
                        _ => false,
                    };
                    if !same_owner {
                        return Err("native handle returned with a different owner".into());
                    }
                    existing
                } else {
                    let resource = Rc::new(Resource {
                        ptr: Cell::new(ptr),
                        ty: ty.clone(),
                        parent: owner.map(|(_, r)| r.clone()),
                        children: RefCell::new(Vec::new()),
                        releases: vm.foreign.releases.clone(),
                    });
                    if let Some((_, parent)) = owner {
                        let mut children = parent.children.borrow_mut();
                        children.retain(|c| c.strong_count() > 0);
                        children.push(Rc::downgrade(&resource));
                    }
                    ty.live
                        .borrow_mut()
                        .insert(ptr as usize, Rc::downgrade(&resource));
                    resource
                };
                Foreign::Handle(resource)
            }
            _ => return Err("invalid native result type".into()),
        };
        let value = vm.new_host(0, Box::new(data));
        if let (Some(obj), Some((owner, _))) = (value.as_obj(), owner) {
            obj.host().references.push(*owner);
        }
        Ok(value)
    }
}

fn pointer(value: Value) -> Result<*mut c_void, String> {
    if matches!(value, Value::Nil) {
        return Ok(std::ptr::null_mut());
    }
    if closed(value) {
        return Err("pointer or handle is closed".into());
    }
    match foreign(value) {
        Some(Foreign::Pointer { ptr, .. }) => Ok(*ptr),
        Some(Foreign::Handle(r)) => Ok(r.ptr.get()),
        _ => match value {
            Value::Obj(o) if o.kind() == Kind::Bytes => Ok(o.bytes().as_mut_ptr().cast()),
            _ => Err("expected Ptr, handle, Bytes or null".into()),
        },
    }
}

fn encode(ty: Scalar, value: Value, scratch: &mut Arguments, index: usize) -> Result<(), String> {
    let integer = || {
        if let Value::Int(n) = value {
            Ok(n)
        } else {
            Err("expected Int".to_string())
        }
    };
    macro_rules! checked {
        ($t:ty, $field:ident) => {
            Slot {
                $field: <$t>::try_from(integer()?)
                    .map_err(|_| format!("outside {} range", stringify!($t)))?,
            }
        };
    }
    scratch.slots[index] = match ty {
        Scalar::I8 => checked!(i8, i8_),
        Scalar::I16 => checked!(i16, i16_),
        Scalar::I32 => checked!(i32, i32_),
        Scalar::I64 => Slot { i64_: integer()? },
        Scalar::U8 => checked!(u8, u8_),
        Scalar::U16 => checked!(u16, u16_),
        Scalar::U32 => checked!(u32, u32_),
        Scalar::U64 => Slot {
            u64_: integer()? as u64,
        },
        Scalar::Size => Slot {
            size: if usize::BITS == 64 {
                integer()? as usize
            } else {
                usize::try_from(integer()?).map_err(|_| "outside SizeT range")?
            },
        },
        Scalar::Bool => Slot {
            u8_: if let Value::Bool(b) = value {
                u8::from(b)
            } else {
                return Err("expected Bool".into());
            },
        },
        Scalar::Float | Scalar::Double => {
            let n = match value {
                Value::Float(n) => n,
                Value::Int(n) => n as f64,
                _ => return Err("expected Float or Int".into()),
            };
            if ty == Scalar::Float {
                if n.is_finite() && !(n as f32).is_finite() {
                    return Err("outside Float range".into());
                }
                Slot { float: n as f32 }
            } else {
                Slot { double: n }
            }
        }
        Scalar::CString => {
            let ptr = match value {
                Value::Nil => std::ptr::null(),
                Value::Obj(o) if o.kind() == Kind::String => {
                    let text =
                        CString::new(o.str()).map_err(|_| "CString contains an interior NUL")?;
                    scratch.strings[index] = Some(text);
                    scratch.strings[index].as_ref().unwrap().as_ptr()
                }
                _ => return Err("expected String or null".into()),
            };
            Slot { ptr }
        }
        Scalar::Void => return Err("Void cannot be an argument".into()),
    };
    Ok(())
}

unsafe fn decode(ty: Scalar, result: Slot, vm: &mut Vm, widened: bool) -> Result<Value, String> {
    // libffi widens integral results smaller than ffi_arg, unlike floating-point and pointer results.
    Ok(unsafe {
        match ty {
            Scalar::Void => Value::Nil,
            Scalar::I8 => Value::Int(
                (if widened {
                    result.size as i8
                } else {
                    result.i8_
                }) as i64,
            ),
            Scalar::I16 => Value::Int(
                (if widened {
                    result.size as i16
                } else {
                    result.i16_
                }) as i64,
            ),
            Scalar::I32 => Value::Int(
                (if widened {
                    result.size as i32
                } else {
                    result.i32_
                }) as i64,
            ),
            Scalar::I64 => Value::Int(result.i64_),
            Scalar::U8 => Value::Int(
                (if widened {
                    result.size as u8
                } else {
                    result.u8_
                }) as i64,
            ),
            Scalar::U16 => Value::Int(
                (if widened {
                    result.size as u16
                } else {
                    result.u16_
                }) as i64,
            ),
            Scalar::U32 => Value::Int(
                (if widened {
                    result.size as u32
                } else {
                    result.u32_
                }) as i64,
            ),
            Scalar::U64 => Value::Int(result.u64_ as i64),
            Scalar::Size => Value::Int(result.size as i64),
            Scalar::Bool => Value::Bool(if widened {
                result.size != 0
            } else {
                result.u8_ != 0
            }),
            Scalar::Float => Value::Float(result.float as f64),
            Scalar::Double => Value::Float(result.double),
            Scalar::CString if result.ptr.is_null() => Value::Nil,
            Scalar::CString => {
                let text = CStr::from_ptr(result.ptr)
                    .to_str()
                    .map_err(|_| "CString result is not UTF-8")?;
                vm.new_string(text)
            }
        }
    })
}
