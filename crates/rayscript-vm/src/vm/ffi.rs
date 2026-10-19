//! Scalar C calls. Library names are resolved only through the embedder's explicit registrations.

use super::{NativeResult, Vm};
use crate::{Value, object::Kind};
use libffi::middle::{Cif, CodePtr, Type};
use libloading::Library;
use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::{CStr, CString, c_char, c_void},
    path::{Path, PathBuf},
    rc::Rc,
};

#[derive(Default)]
pub(crate) struct Libraries {
    paths: HashMap<String, PathBuf>,
    loaded: HashMap<String, Rc<Library>>,
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

struct Bound {
    // A symbol and its call interface never outlive the library they came from.
    _library: Rc<Library>,
    code: CodePtr,
    cif: Cif,
    args: Vec<Scalar>,
    ret: Scalar,
    name: String,
    scratch: RefCell<Arguments>,
}

impl Vm {
    /// Registers a logical name for native declarations; no library is loaded until a binding uses it.
    /// Replacing a registration affects later bindings. Existing functions retain their original library.
    ///
    /// # Safety
    /// The library's initialization and teardown must be safe to execute, and every script declaration
    /// using this name must match its C symbol's ABI and memory contract. Only trusted scripts may bind it.
    pub unsafe fn register_native_library(&mut self, name: &str, path: impl AsRef<Path>) {
        self.foreign
            .paths
            .insert(name.into(), path.as_ref().to_path_buf());
        self.foreign.loaded.remove(name);
    }

    pub(crate) fn bind_native(&mut self, spec: &str) -> Result<Value, String> {
        let parts: Vec<_> = spec.split('\0').collect();
        let [library, symbol, signature, flags] = parts.as_slice() else {
            return Err("invalid native binding descriptor".into());
        };
        let name = format!("{library}.{symbol}");
        let prepare = || -> Result<(Vec<Scalar>, Scalar, Cif), String> {
            if !flags.is_empty() {
                return Err(format!(
                    "native annotations `{flags}` are not available yet"
                ));
            }
            let (args, ret) = signature
                .split_once("->")
                .ok_or("invalid native signature")?;
            let args = if args.is_empty() {
                Vec::new()
            } else {
                args.split(',')
                    .map(Scalar::parse)
                    .collect::<Result<Vec<_>, _>>()?
            };
            if args.contains(&Scalar::Void) {
                return Err(
                    "each native argument needs a scalar type; Void is only a result type".into(),
                );
            }
            let ret = Scalar::parse(ret)?;
            let cif = Cif::try_new(args.iter().map(|arg| arg.ffi()), ret.ffi())
                .map_err(|e| format!("invalid native signature: {e:?}"))?;
            Ok((args, ret, cif))
        };
        let (args, ret, cif) = prepare().map_err(|e| format!("{name}: {e}"))?;
        let path = self
            .foreign
            .paths
            .get(*library)
            .ok_or_else(|| format!("{name}: native library `{library}` is not registered"))?;
        let loaded = if let Some(loaded) = self.foreign.loaded.get(*library) {
            loaded.clone()
        } else {
            // Registration transfers the initialization and ABI contract to the embedding application.
            let loaded = Rc::new(
                unsafe { Library::new(path) }
                    .map_err(|e| format!("{name}: cannot load {}: {e}", path.display()))?,
            );
            self.foreign
                .loaded
                .insert(library.to_string(), loaded.clone());
            loaded
        };
        // Only the address is retained; the owning Rc keeps the symbol valid for every call.
        let code = unsafe { loaded.get::<unsafe extern "C" fn()>(symbol.as_bytes()) }
            .map_err(|e| format!("{name}: symbol not found: {e}"))?;
        let bound = Bound {
            code: CodePtr::from_fun(*code),
            scratch: RefCell::new(Arguments::new(args.len())),
            _library: loaded,
            cif,
            args,
            ret,
            name: name.clone(),
        };
        Ok(self.native(&name, move |vm, args| bound.call(vm, args)))
    }
}

impl Bound {
    fn call(&self, vm: &mut Vm, args: &[Value]) -> NativeResult {
        self.invoke(vm, args)
            .map_err(|message| vm.err(format!("{}: {message}", self.name)))
    }

    fn invoke(&self, vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
        if args.len() != self.args.len() {
            return Err(format!(
                "expected {} arguments, got {}",
                self.args.len(),
                args.len()
            ));
        }
        // Scalar calls reuse bind-time storage. Reentry will be handled with callback frames when callbacks land.
        let mut scratch = self
            .scratch
            .try_borrow_mut()
            .map_err(|_| "reentrant native calls are not available yet")?;
        let result = (|| {
            for (index, (ty, value)) in self.args.iter().zip(args).enumerate() {
                encode(*ty, *value, &mut scratch, index)
                    .map_err(|e| format!("argument {}: {e}", index + 1))?;
            }
            // Derive addresses after the mutable slot writes, so no cached pointer crosses a new Rust borrow.
            let base = scratch.slots.as_mut_ptr();
            for (index, pointer) in scratch.pointers.iter_mut().enumerate() {
                *pointer = unsafe { base.add(index).cast() };
            }
            let mut result = Slot { u64_: 0 };
            // The CIF and argument slots have matching C layouts; the registration guarantees the symbol's ABI.
            unsafe {
                libffi::raw::ffi_call(
                    self.cif.as_raw_ptr(),
                    Some(*self.code.as_fun()),
                    (&mut result as *mut Slot).cast(),
                    scratch.pointers.as_mut_ptr(),
                );
            }
            // C strings may point into an argument copy, so decode before those copies are released.
            unsafe { decode(self.ret, result, vm) }
        })();
        for string in scratch.strings.iter_mut() {
            *string = None;
        }
        result
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

unsafe fn decode(ty: Scalar, result: Slot, vm: &mut Vm) -> Result<Value, String> {
    // libffi widens integral results smaller than ffi_arg, unlike floating-point and pointer results.
    Ok(unsafe {
        match ty {
            Scalar::Void => Value::Nil,
            Scalar::I8 => Value::Int(result.size as i8 as i64),
            Scalar::I16 => Value::Int(result.size as i16 as i64),
            Scalar::I32 => Value::Int(result.size as i32 as i64),
            Scalar::I64 => Value::Int(result.i64_),
            Scalar::U8 => Value::Int(result.size as u8 as i64),
            Scalar::U16 => Value::Int(result.size as u16 as i64),
            Scalar::U32 => Value::Int(result.size as u32 as i64),
            Scalar::U64 => Value::Int(result.u64_ as i64),
            Scalar::Size => Value::Int(result.size as i64),
            Scalar::Bool => Value::Bool(result.size != 0),
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
