//! C layouts and views over owned or borrowed struct storage.

use super::*;
use std::{
    alloc::{Layout, alloc_zeroed, dealloc},
    collections::HashSet,
    ptr::NonNull,
};

#[derive(Clone)]
pub(super) struct StructType {
    group: Rc<Group>,
    index: usize,
}

struct Group {
    spec: String,
    _library: Rc<Library>,
    layouts: Vec<Record>,
}

#[derive(Clone)]
enum Target {
    Local(usize),
    External(StructType),
}

#[derive(Clone)]
enum FieldType {
    Scalar(Scalar),
    Ptr,
    Handle(Rc<HandleType>),
    Callback(Rc<callback::CallbackType>),
    Inline(Target),
    Pointer(Target),
}

#[derive(Clone)]
struct Field {
    name: String,
    offset: usize,
    ty: FieldType,
}

#[derive(Clone)]
struct Record {
    name: String,
    layout: Layout,
    fields: Vec<Field>,
}

impl StructType {
    fn record(&self) -> &Record {
        &self.group.layouts[self.index]
    }
    pub(super) fn same(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.group, &other.group) && self.index == other.index
    }
    fn target(&self, target: &Target) -> Self {
        match target {
            Target::Local(index) => Self {
                group: self.group.clone(),
                index: *index,
            },
            Target::External(ty) => ty.clone(),
        }
    }
}

pub(super) fn bind(vm: &mut Vm, library: &str, symbol: &str, spec: &str) -> Result<Value, String> {
    let key = (library.into(), symbol.into());
    let mut all_names = HashSet::new();
    let mut definitions = Vec::new();
    for line in spec.lines() {
        let definition: Vec<_> = line.split('\t').collect();
        let name = definition[0];
        if name.is_empty() || Scalar::parse(name).is_ok() || ["Ptr", "Bytes", "Out"].contains(&name)
        {
            return Err(format!("invalid struct name `{name}`"));
        }
        if vm
            .foreign
            .callbacks
            .contains_key(&(library.into(), name.into()))
        {
            return Err(format!("duplicate native type `{name}`"));
        }
        if !all_names.insert(name)
            || vm
                .foreign
                .types
                .contains_key(&(library.into(), name.into()))
        {
            return Err(format!("duplicate native type `{name}`"));
        }
        if let Some(existing) = vm.foreign.structures.get(&(library.into(), name.into())) {
            let previous = existing
                .group
                .spec
                .lines()
                .find(|line| line.split('\t').next() == Some(name));
            if previous != Some(line) {
                return Err(format!(
                    "struct `{name}` already declared with a different definition"
                ));
            }
        } else {
            definitions.push(definition);
        }
    }
    if let Some(ty) = vm.foreign.structures.get(&key) {
        return Ok(vm.new_host(0, Box::new(Foreign::Type(NativeType::Struct(ty.clone())))));
    }
    let names: HashMap<_, _> = definitions
        .iter()
        .enumerate()
        .map(|(i, d)| (d[0], i))
        .collect();
    let mut fields = Vec::new();
    for definition in &definitions {
        let mut seen = HashSet::new();
        let mut record = Vec::new();
        for field in &definition[1..] {
            let (name, ty) = field
                .split_once(':')
                .ok_or("invalid struct field descriptor")?;
            if name.is_empty() || !seen.insert(name) {
                return Err(format!("duplicate struct field `{name}`"));
            }
            let target = |name: &str| -> Option<Target> {
                names
                    .get(name)
                    .map(|index| Target::Local(*index))
                    .or_else(|| {
                        vm.foreign
                            .structures
                            .get(&(library.into(), name.into()))
                            .cloned()
                            .map(Target::External)
                    })
            };
            let ty = if let Some(inner) = ty.strip_prefix("Ptr<").and_then(|s| s.strip_suffix('>'))
            {
                FieldType::Pointer(
                    target(inner)
                        .ok_or_else(|| format!("unknown struct `{inner}` in pointer field"))?,
                )
            } else if let Some(target) = target(ty) {
                FieldType::Inline(target)
            } else if ty == "Ptr" {
                FieldType::Ptr
            } else if let Some(callback) = vm.foreign.callbacks.get(&(library.into(), ty.into())) {
                FieldType::Callback(callback.clone())
            } else if let Some(handle) = vm.foreign.types.get(&(library.into(), ty.into())) {
                FieldType::Handle(handle.clone())
            } else {
                let scalar = Scalar::parse(ty)?;
                if scalar == Scalar::Void {
                    return Err("Void cannot be a struct field".into());
                }
                FieldType::Scalar(scalar)
            };
            record.push(Field {
                name: name.into(),
                offset: 0,
                ty,
            });
        }
        if record.is_empty() {
            return Err("empty C structs are not supported".into());
        }
        fields.push(record);
    }
    fn layout(
        index: usize,
        fields: &[Vec<Field>],
        names: &[Vec<&str>],
        states: &mut [u8],
        records: &mut [Option<Record>],
        depth: usize,
    ) -> Result<Layout, String> {
        if depth > 128 {
            return Err("native struct nesting exceeds 128 levels".into());
        }
        if states[index] == 1 {
            return Err(format!(
                "recursive inline struct `{}`; use Ptr<T> for links",
                names[index][0]
            ));
        }
        if let Some(record) = &records[index] {
            return Ok(record.layout);
        }
        states[index] = 1;
        let mut result = Layout::from_size_align(0, 1).unwrap();
        let mut resolved = fields[index].clone();
        for field in &mut resolved {
            let member = match &field.ty {
                FieldType::Inline(Target::Local(n)) => {
                    layout(*n, fields, names, states, records, depth + 1)?
                }
                FieldType::Inline(Target::External(ty)) => ty.record().layout,
                FieldType::Scalar(ty) => scalar_layout(*ty),
                _ => Layout::new::<*mut c_void>(),
            };
            let (next, offset) = result
                .extend(member)
                .map_err(|_| "struct layout overflows address space")?;
            field.offset = offset;
            result = next;
        }
        let result = result.pad_to_align();
        records[index] = Some(Record {
            name: names[index][0].into(),
            layout: result,
            fields: resolved,
        });
        states[index] = 2;
        Ok(result)
    }
    let mut records = vec![None; definitions.len()];
    let mut states = vec![0; definitions.len()];
    for index in 0..definitions.len() {
        layout(index, &fields, &definitions, &mut states, &mut records, 0)?;
    }
    let group = Rc::new(Group {
        spec: spec.into(),
        _library: vm.native_library(library)?,
        layouts: records.into_iter().map(Option::unwrap).collect(),
    });
    for (index, record) in group.layouts.iter().enumerate() {
        vm.foreign.structures.insert(
            (library.into(), record.name.clone()),
            StructType {
                group: group.clone(),
                index,
            },
        );
    }
    let ty = vm
        .foreign
        .structures
        .get(&key)
        .ok_or("struct not found in its descriptor")?
        .clone();
    Ok(vm.new_host(0, Box::new(Foreign::Type(NativeType::Struct(ty)))))
}

fn scalar_layout(ty: Scalar) -> Layout {
    match ty {
        Scalar::I8 | Scalar::U8 | Scalar::Bool => Layout::new::<u8>(),
        Scalar::I16 | Scalar::U16 => Layout::new::<u16>(),
        Scalar::I32 | Scalar::U32 => Layout::new::<u32>(),
        Scalar::Float => Layout::new::<f32>(),
        Scalar::I64 | Scalar::U64 => Layout::new::<u64>(),
        Scalar::Double => Layout::new::<f64>(),
        Scalar::Size | Scalar::CString => Layout::new::<usize>(),
        Scalar::Void => unreachable!(),
    }
}

struct Allocation {
    ptr: NonNull<u8>,
    layout: Layout,
    bytes: Rc<Cell<usize>>,
}
impl Drop for Allocation {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr.as_ptr(), self.layout) };
        self.bytes.set(self.bytes.get() - self.layout.size());
    }
}

#[derive(Clone)]
struct Retained {
    ptr: *mut c_void,
    value: Value,
}

pub(crate) struct Backing {
    pub(super) busy: Cell<Option<u64>>,
    allocation: Option<Allocation>,
    expired: Option<Rc<Cell<bool>>>,
    owner: Option<Value>,
    guard: Option<Rc<Resource>>,
    strings: RefCell<HashMap<usize, Rc<CString>>>,
    references: RefCell<HashMap<usize, Retained>>,
    copies: RefCell<HashMap<usize, Value>>,
}

impl Backing {
    fn new(
        allocation: Option<Allocation>,
        owner: Option<Value>,
        guard: Option<Rc<Resource>>,
    ) -> Rc<Self> {
        Rc::new(Self {
            busy: Cell::new(None),
            allocation,
            expired: None,
            owner,
            guard,
            strings: RefCell::new(HashMap::new()),
            references: RefCell::new(HashMap::new()),
            copies: RefCell::new(HashMap::new()),
        })
    }
    fn storage_closed(&self, seen: &mut HashSet<usize>) -> bool {
        if self.expired.as_ref().is_some_and(|s| s.get()) {
            return true;
        }
        if self.guard.as_ref().is_some_and(|r| r.ptr.get().is_null()) {
            return true;
        }
        if !seen.insert(self as *const Self as usize) {
            return false;
        }
        self.owner.is_some_and(|v| storage_closed_value(v, seen))
    }
    fn check_storage(&self) -> Result<(), String> {
        worker::check(self.busy.get())?;
        if self.storage_closed(&mut HashSet::new()) {
            Err("struct storage is closed".into())
        } else {
            Ok(())
        }
    }
    pub(super) fn check(&self) -> Result<(), String> {
        worker::check(self.busy.get())?;
        if self.closed(&mut HashSet::new()) {
            Err("struct storage or a referenced native handle is closed".into())
        } else {
            Ok(())
        }
    }
    fn closed(&self, seen: &mut HashSet<usize>) -> bool {
        if self.expired.as_ref().is_some_and(|s| s.get()) {
            return true;
        }
        if self.guard.as_ref().is_some_and(|r| r.ptr.get().is_null()) {
            return true;
        }
        if !seen.insert(self as *const Self as usize) {
            return false;
        }
        if self.owner.is_some_and(|v| closed_value(v, seen)) {
            return true;
        }
        if self
            .copies
            .borrow()
            .values()
            .any(|v| closed_value(*v, seen))
        {
            return true;
        }
        self.references.borrow().iter().any(|(address, retained)| {
            let current = unsafe { (*address as *const *mut c_void).read_unaligned() };
            current == retained.ptr && closed_value(retained.value, seen)
        })
    }
    pub(super) fn pin_strings(&self, strings: &mut Vec<Rc<CString>>) {
        strings.extend(self.strings.borrow().values().cloned());
    }
    pub(super) fn trace(&self, f: &mut dyn FnMut(Value)) {
        if let Some(owner) = self.owner {
            f(owner);
        }
        for retained in self.references.borrow().values() {
            f(retained.value);
        }
        for value in self.copies.borrow().values() {
            f(*value);
        }
    }
}

pub(super) fn storage_closed_value(value: Value, seen: &mut HashSet<usize>) -> bool {
    match foreign(value) {
        Some(Foreign::Handle(r)) => r.ptr.get().is_null(),
        Some(Foreign::Pointer {
            owner, structure, ..
        }) => {
            owner.as_ref().is_some_and(|r| r.ptr.get().is_null())
                || structure.as_ref().is_some_and(|b| b.storage_closed(seen))
        }
        Some(Foreign::Struct(view)) => {
            view.guard
                .as_ref()
                .is_some_and(|(_, r)| r.ptr.get().is_null())
                || view.backing.storage_closed(seen)
        }
        Some(Foreign::BorrowedHandle { backing, guard, .. }) => {
            guard.as_ref().is_some_and(|(_, r)| r.ptr.get().is_null())
                || backing.storage_closed(seen)
        }
        _ => false,
    }
}

pub(super) fn closed_value(value: Value, seen: &mut HashSet<usize>) -> bool {
    match foreign(value) {
        Some(Foreign::Handle(r)) => r.ptr.get().is_null(),
        Some(Foreign::Pointer {
            owner, structure, ..
        }) => {
            owner.as_ref().is_some_and(|r| r.ptr.get().is_null())
                || structure.as_ref().is_some_and(|b| b.closed(seen))
        }
        Some(Foreign::Struct(view)) => {
            view.guard
                .as_ref()
                .is_some_and(|(_, r)| r.ptr.get().is_null())
                || view.backing.closed(seen)
        }
        Some(Foreign::BorrowedHandle { backing, guard, .. }) => {
            guard.as_ref().is_some_and(|(_, r)| r.ptr.get().is_null()) || backing.closed(seen)
        }
        _ => false,
    }
}

#[derive(Clone)]
pub(super) struct View {
    pub(super) ty: StructType,
    pub(super) ptr: *mut u8,
    pub(super) backing: Rc<Backing>,
    count: usize,
    array: bool,
    guard: Option<(Value, Rc<Resource>)>,
    indirect: bool,
}

impl View {
    pub(super) fn address(&self) -> Result<*mut c_void, String> {
        self.check_storage()?;
        self.backing.check()?;
        Ok(self.ptr.cast())
    }
    fn check_storage(&self) -> Result<(), String> {
        if self
            .guard
            .as_ref()
            .is_some_and(|(_, r)| r.ptr.get().is_null())
        {
            return Err("struct storage is closed".into());
        }
        if self.indirect {
            self.backing.check()
        } else {
            self.backing.check_storage()
        }
    }
    fn element(&self, index: i64) -> Result<Self, String> {
        self.check_storage()?;
        let index = usize::try_from(index)
            .ok()
            .filter(|n| *n < self.count)
            .ok_or("struct array index out of range")?;
        Ok(Self {
            ptr: unsafe { self.ptr.add(index * self.ty.record().layout.size()) },
            count: 1,
            array: false,
            ..self.clone()
        })
    }
}

fn count(args: &[Value], at: usize) -> Result<usize, String> {
    match args.get(at) {
        None => Ok(1),
        Some(Value::Int(n)) => {
            usize::try_from(*n).map_err(|_| "struct count must be nonnegative".into())
        }
        _ => Err("struct count needs an Int".into()),
    }
}

fn type_arg(value: Value) -> Result<StructType, String> {
    match foreign(value) {
        Some(Foreign::Type(NativeType::Struct(ty))) => Ok(ty.clone()),
        _ => Err("expected a native struct type".into()),
    }
}

pub(super) fn install(vm: &mut Vm) {
    vm.define_native("alloc", |vm, args| {
        let result = (|| {
            if !(1..=2).contains(&args.len()) {
                return Err("alloc needs a struct type and optional count".into());
            }
            let ty = type_arg(args[0])?;
            let count = count(args, 1)?;
            let size = ty
                .record()
                .layout
                .size()
                .checked_mul(count)
                .ok_or("struct allocation size overflows")?;
            let layout = Layout::from_size_align(size.max(1), ty.record().layout.align())
                .map_err(|_| "struct allocation size overflows")?;
            let ptr =
                NonNull::new(unsafe { alloc_zeroed(layout) }).ok_or("struct allocation failed")?;
            let bytes = vm.foreign.struct_bytes.clone();
            bytes.set(bytes.get() + layout.size());
            vm.sync_native_storage();
            let backing = Backing::new(Some(Allocation { ptr, layout, bytes }), None, None);
            register(vm, &backing);
            Ok(vm.new_host(
                0,
                Box::new(Foreign::Struct(View {
                    ty,
                    ptr: ptr.as_ptr(),
                    backing,
                    count,
                    array: args.len() == 2,
                    guard: None,
                    indirect: false,
                })),
            ))
        })();
        result.map_err(|e: String| vm.err(e))
    });
    vm.define_native("borrow", |vm, args| {
        let result = (|| {
            if !(2..=3).contains(&args.len()) {
                return Err("borrow needs a struct type, pointer and optional count".into());
            }
            let ty = type_arg(args[0])?;
            let count = count(args, 2)?;
            let ptr = pointer(args[1])?.cast::<u8>();
            if ptr.is_null() {
                return Err("cannot borrow a null struct pointer".into());
            }
            check_alignment(&ty, ptr)?;
            let size = ty
                .record()
                .layout
                .size()
                .checked_mul(count)
                .filter(|n| *n <= isize::MAX as usize)
                .ok_or("struct view size overflows")?;
            known(vm, ptr, size)?;
            let guard = match foreign(args[1]) {
                Some(Foreign::Struct(v)) => v.guard.clone(),
                Some(Foreign::Pointer { owner: Some(r), .. }) => Some((args[1], r.clone())),
                _ => None,
            };
            let indirect = match foreign(args[1]) {
                Some(Foreign::Struct(v)) => v.indirect,
                Some(
                    Foreign::Pointer {
                        structure: Some(_), ..
                    }
                    | Foreign::BorrowedHandle { .. },
                ) => true,
                _ => false,
            };
            let backing = match foreign(args[1]) {
                Some(Foreign::Struct(v)) => v.backing.clone(),
                Some(Foreign::Pointer {
                    structure: Some(backing),
                    ..
                })
                | Some(Foreign::BorrowedHandle { backing, .. }) => backing.clone(),
                Some(Foreign::Handle(r)) => Backing::new(None, Some(args[1]), Some(r.clone())),
                Some(Foreign::Pointer { owner, .. }) => {
                    Backing::new(None, Some(args[1]), owner.clone())
                }
                _ => {
                    if let Some(obj) = args[1].as_obj().filter(|o| o.kind() == Kind::Bytes) {
                        if size > obj.bytes().len() {
                            return Err("struct view exceeds Bytes source buffer".into());
                        }
                        obj.bytes()
                            .native_backing
                            .borrow()
                            .as_ref()
                            .unwrap()
                            .clone()
                    } else {
                        Backing::new(None, Some(args[1]), None)
                    }
                }
            };
            if let Some(allocation) = &backing.allocation {
                let base = allocation.ptr.as_ptr() as usize;
                let address = ptr as usize;
                if address >= base
                    && address <= base + allocation.layout.size()
                    && size > base + allocation.layout.size() - address
                {
                    return Err("struct view exceeds allocated storage".into());
                }
            }
            register(vm, &backing);
            Ok(vm.new_host(
                0,
                Box::new(Foreign::Struct(View {
                    ty,
                    ptr,
                    backing,
                    count,
                    array: args.len() == 3,
                    guard,
                    indirect,
                })),
            ))
        })();
        result.map_err(|e: String| vm.err(e))
    });
}

fn check_alignment(ty: &StructType, ptr: *mut u8) -> Result<(), String> {
    if !(ptr as usize).is_multiple_of(ty.record().layout.align()) {
        Err("unaligned native struct pointer".into())
    } else {
        Ok(())
    }
}

pub(super) fn returned(
    vm: &mut Vm,
    ty: &StructType,
    ptr: *mut c_void,
    owner: Option<&(Value, Rc<Resource>)>,
) -> Result<Value, String> {
    if ptr.is_null() {
        return Ok(Value::Nil);
    }
    check_alignment(ty, ptr.cast())?;
    let backing = match known(vm, ptr.cast(), ty.record().layout.size())? {
        Some(backing) => backing,
        None => Backing::new(None, owner.map(|(v, _)| *v), owner.map(|(_, r)| r.clone())),
    };
    Ok(vm.new_host(
        0,
        Box::new(Foreign::Struct(View {
            ty: ty.clone(),
            ptr: ptr.cast(),
            backing,
            count: 1,
            array: false,
            guard: owner.cloned(),
            indirect: false,
        })),
    ))
}

pub(super) fn trace(value: Value, f: &mut dyn FnMut(Value)) {
    match foreign(value) {
        Some(Foreign::Struct(v)) => {
            if let Some((value, _)) = &v.guard {
                f(*value);
            }
            v.backing.trace(f);
        }
        Some(Foreign::BorrowedHandle { backing, guard, .. }) => {
            if let Some((value, _)) = guard {
                f(*value);
            }
            backing.trace(f);
        }
        Some(Foreign::Pointer {
            structure: Some(backing),
            ..
        }) => backing.trace(f),
        _ => {}
    }
}

pub(super) fn get_field(
    vm: &mut Vm,
    obj: crate::value::Obj,
    name: &str,
) -> Option<Result<Value, String>> {
    match foreign(Value::Obj(obj))? {
        Foreign::Type(NativeType::Struct(ty)) => match name {
            "size" => Some(Ok(Value::Int(ty.record().layout.size() as i64))),
            "alignment" => Some(Ok(Value::Int(ty.record().layout.align() as i64))),
            _ => None,
        },
        Foreign::Struct(view) => Some((|| {
            view.check_storage()?;
            if view.array {
                return if name == "length" {
                    Ok(Value::Int(view.count as i64))
                } else {
                    Err("index a struct array before reading fields".into())
                };
            }
            let field = view
                .ty
                .record()
                .fields
                .iter()
                .find(|f| f.name == name)
                .ok_or_else(|| format!("struct {} has no field `{name}`", view.ty.record().name))?;
            let ptr = unsafe { view.ptr.add(field.offset) };
            read_field(vm, view, field, ptr)
        })()),
        _ => None,
    }
}

fn cached(view: &View, address: *mut u8, ptr: *mut c_void) -> Option<Value> {
    view.backing
        .references
        .borrow()
        .get(&(address as usize))
        .filter(|r| r.ptr == ptr)
        .map(|r| r.value)
}

fn read_field(vm: &mut Vm, view: &View, field: &Field, address: *mut u8) -> Result<Value, String> {
    match &field.ty {
        FieldType::Scalar(s) => {
            if *s == Scalar::CString {
                view.backing.check()?;
            }
            unsafe { decode(*s, read_slot(*s, address), vm, false) }
        }
        FieldType::Inline(target) => Ok(vm.new_host(
            0,
            Box::new(Foreign::Struct(View {
                ty: view.ty.target(target),
                ptr: address,
                count: 1,
                array: false,
                ..view.clone()
            })),
        )),
        ty => {
            let ptr = unsafe { address.cast::<*mut c_void>().read() };
            if ptr.is_null() {
                return Ok(Value::Nil);
            }
            let retained = cached(view, address, ptr);
            let foreign = match ty {
                FieldType::Callback(_) => {
                    return retained.ok_or_else(|| {
                        "callback field contains an unmanaged C function pointer".into()
                    });
                }
                FieldType::Handle(ty) => {
                    if let Some(value) = retained {
                        return Ok(value);
                    }
                    Foreign::BorrowedHandle {
                        ty: ty.clone(),
                        ptr,
                        backing: view.backing.clone(),
                        guard: view.guard.clone(),
                    }
                }
                FieldType::Pointer(target) => {
                    let ty = view.ty.target(target);
                    check_alignment(&ty, ptr.cast())?;
                    let (backing, indirect, guard) = match retained.and_then(foreign) {
                        Some(Foreign::Struct(v)) => {
                            (v.backing.clone(), v.indirect, v.guard.clone())
                        }
                        _ => {
                            view.backing.check()?;
                            match known(vm, ptr.cast(), ty.record().layout.size())? {
                                Some(backing) => (backing, false, view.guard.clone()),
                                None => (view.backing.clone(), true, view.guard.clone()),
                            }
                        }
                    };
                    Foreign::Struct(View {
                        ty,
                        ptr: ptr.cast(),
                        backing,
                        count: 1,
                        array: false,
                        guard,
                        indirect,
                    })
                }
                FieldType::Ptr => {
                    if let Some(value) = retained {
                        return Ok(value);
                    }
                    Foreign::Pointer {
                        ptr,
                        owner: view.guard.as_ref().map(|(_, r)| r.clone()),
                        structure: Some(view.backing.clone()),
                        _library: view.ty.group._library.clone(),
                    }
                }
                _ => unreachable!(),
            };
            Ok(vm.new_host(0, Box::new(foreign)))
        }
    }
}

pub(super) fn set_field(
    obj: crate::value::Obj,
    name: &str,
    value: Value,
) -> Option<Result<(), String>> {
    let Foreign::Struct(view) = foreign(Value::Obj(obj))? else {
        return None;
    };
    Some((|| {
        view.check_storage()?;
        if view.array {
            return Err("index a struct array before writing fields".into());
        }
        let field = view
            .ty
            .record()
            .fields
            .iter()
            .find(|f| f.name == name)
            .ok_or_else(|| format!("struct {} has no field `{name}`", view.ty.record().name))?;
        let address = unsafe { view.ptr.add(field.offset) };
        if !vm_owned(&view.backing)
            && !matches!(field.ty, FieldType::Scalar(s) if s != Scalar::CString)
            && !matches!(field.ty, FieldType::Inline(_))
            && !matches!(value, Value::Nil)
        {
            return Err("pointer-bearing fields of C-owned structs are read-only; use alloc(Type) for writable storage".into());
        }
        match &field.ty {
            FieldType::Inline(target) => copy_struct(view, address, &view.ty.target(target), value),
            FieldType::Scalar(Scalar::CString) => {
                let text = match value {
                    Value::Nil => None,
                    Value::Obj(o) if o.kind() == Kind::String => Some(Rc::new(
                        CString::new(o.str()).map_err(|_| "CString contains an interior NUL")?,
                    )),
                    _ => return Err("CString field needs String or null".into()),
                };
                let ptr = text.as_ref().map_or(std::ptr::null(), |s| s.as_ptr());
                unsafe {
                    address.cast::<*const c_char>().write(ptr);
                }
                match text {
                    Some(text) => {
                        view.backing
                            .strings
                            .borrow_mut()
                            .insert(address as usize, text);
                    }
                    None => {
                        view.backing
                            .strings
                            .borrow_mut()
                            .remove(&(address as usize));
                    }
                }
                Ok(())
            }
            FieldType::Scalar(s) => {
                let mut scratch = Arguments::new(1);
                encode(*s, value, &mut scratch, 0)?;
                unsafe {
                    write_slot(*s, address, scratch.slots[0]);
                }
                Ok(())
            }
            ty => {
                let ptr = match ty {
                    FieldType::Ptr => pointer(value)?,
                    FieldType::Callback(ty) => callback::pointer(value, ty)?,
                    FieldType::Handle(ty) => handle_pointer(value, ty)?,
                    FieldType::Pointer(target) => struct_pointer(value, &view.ty.target(target))?,
                    _ => unreachable!(),
                };
                unsafe {
                    address.cast::<*mut c_void>().write(ptr);
                }
                if matches!(value, Value::Nil) {
                    view.backing
                        .references
                        .borrow_mut()
                        .remove(&(address as usize));
                } else {
                    view.backing
                        .references
                        .borrow_mut()
                        .insert(address as usize, Retained { ptr, value });
                }
                Ok(())
            }
        }
    })())
}

pub(super) fn struct_pointer(value: Value, ty: &StructType) -> Result<*mut c_void, String> {
    if matches!(value, Value::Nil) {
        return Ok(std::ptr::null_mut());
    }
    match foreign(value) {
        Some(Foreign::Struct(v)) if v.ty.same(ty) => v.address(),
        _ => Err(format!("expected struct {} or null", ty.record().name)),
    }
}

pub(super) fn handle_pointer(value: Value, ty: &Rc<HandleType>) -> Result<*mut c_void, String> {
    if matches!(value, Value::Nil) {
        return Ok(std::ptr::null_mut());
    }
    match foreign(value) {
        Some(Foreign::Handle(r)) if Rc::ptr_eq(&r.ty, ty) => {
            worker::check(r.busy.get())?;
            if r.ptr.get().is_null() {
                Err("handle is closed".into())
            } else {
                Ok(r.ptr.get())
            }
        }
        Some(Foreign::BorrowedHandle {
            ty: actual,
            ptr,
            backing,
            guard,
        }) if Rc::ptr_eq(actual, ty) => {
            if guard.as_ref().is_some_and(|(_, r)| r.ptr.get().is_null()) {
                return Err("handle owner is closed".into());
            }
            backing.check()?;
            Ok(*ptr)
        }
        _ => Err(format!("expected handle {} or null", ty.name)),
    }
}

pub(super) fn get_index(
    vm: &mut Vm,
    obj: crate::value::Obj,
    index: Value,
) -> Option<Result<Value, String>> {
    let Foreign::Struct(view) = foreign(Value::Obj(obj))? else {
        return None;
    };
    Some(match index {
        Value::Int(n) => view
            .element(n)
            .map(|v| vm.new_host(0, Box::new(Foreign::Struct(v)))),
        _ => Err("struct array index needs an Int".into()),
    })
}

pub(super) fn set_index(
    obj: crate::value::Obj,
    index: Value,
    value: Value,
) -> Option<Result<(), String>> {
    let Foreign::Struct(view) = foreign(Value::Obj(obj))? else {
        return None;
    };
    Some((|| {
        let Value::Int(n) = index else {
            return Err("struct array index needs an Int".into());
        };
        let element = view.element(n)?;
        copy_struct(view, element.ptr, &view.ty, value)
    })())
}

fn copy_struct(
    destination: &View,
    ptr: *mut u8,
    ty: &StructType,
    source: Value,
) -> Result<(), String> {
    let Some(Foreign::Struct(view)) = foreign(source) else {
        return Err("struct assignment needs a matching struct value".into());
    };
    if view.array || !view.ty.same(ty) {
        return Err("struct assignment needs a matching scalar struct value".into());
    }
    view.check_storage()?;
    view.backing.check()?;
    if !vm_owned(&destination.backing) && has_pointers(ty) {
        return Err("cannot copy pointer-bearing structs into C-owned storage".into());
    }
    let size = ty.record().layout.size();
    if view.ptr == ptr {
        return Ok(());
    }
    let start = view.ptr as usize;
    let end = start + size;
    let strings: Vec<_> = view
        .backing
        .strings
        .borrow()
        .iter()
        .filter(|(p, _)| **p >= start && **p < end)
        .map(|(p, s)| (*p - start, s.clone()))
        .collect();
    let refs: Vec<_> = view
        .backing
        .references
        .borrow()
        .iter()
        .filter(|(p, _)| **p >= start && **p < end)
        .map(|(p, r)| (*p - start, r.clone()))
        .collect();
    // Snapshot retainers before an overlapping copy or mutations of the same backing maps.
    unsafe {
        std::ptr::copy(view.ptr, ptr, size);
    }
    let start = ptr as usize;
    let end = start + size;
    destination
        .backing
        .strings
        .borrow_mut()
        .retain(|p, _| *p < start || *p >= end);
    destination
        .backing
        .references
        .borrow_mut()
        .retain(|p, _| *p < start || *p >= end);
    destination
        .backing
        .copies
        .borrow_mut()
        .retain(|p, _| *p < start || *p >= end);
    for (offset, string) in strings {
        destination
            .backing
            .strings
            .borrow_mut()
            .insert(start + offset, string);
    }
    for (offset, retained) in refs {
        destination
            .backing
            .references
            .borrow_mut()
            .insert(start + offset, retained);
    }
    if has_pointers(ty) {
        destination
            .backing
            .copies
            .borrow_mut()
            .insert(start, source);
    }
    Ok(())
}

unsafe fn read_slot(ty: Scalar, address: *mut u8) -> Slot {
    let mut slot = Slot { u64_: 0 };
    unsafe {
        std::ptr::copy_nonoverlapping(
            address,
            (&mut slot as *mut Slot).cast::<u8>(),
            scalar_layout(ty).size(),
        );
    }
    slot
}

unsafe fn write_slot(ty: Scalar, address: *mut u8, slot: Slot) {
    unsafe {
        std::ptr::copy_nonoverlapping(
            (&slot as *const Slot).cast::<u8>(),
            address,
            scalar_layout(ty).size(),
        );
    }
}

fn vm_owned(backing: &Backing) -> bool {
    backing.allocation.is_some()
        || backing
            .owner
            .and_then(|v| v.as_obj())
            .is_some_and(|o| o.kind() == Kind::Bytes)
}

fn region(backing: &Backing) -> Option<(usize, usize)> {
    if let Some(allocation) = &backing.allocation {
        return Some((allocation.ptr.as_ptr() as usize, allocation.layout.size()));
    }
    backing
        .owner
        .and_then(|v| v.as_obj())
        .filter(|o| o.kind() == Kind::Bytes)
        .map(|o| (o.bytes().as_mut_ptr() as usize, o.bytes().len()))
}

fn register(vm: &mut Vm, backing: &Rc<Backing>) {
    if !vm_owned(backing) {
        return;
    }
    vm.foreign.struct_backings.retain(|b| b.strong_count() > 0);
    if !vm
        .foreign
        .struct_backings
        .iter()
        .any(|b| b.as_ptr() == Rc::as_ptr(backing))
    {
        vm.foreign.struct_backings.push(Rc::downgrade(backing));
    }
}

pub(super) fn known(vm: &mut Vm, ptr: *mut u8, size: usize) -> Result<Option<Rc<Backing>>, String> {
    vm.foreign.struct_backings.retain(|b| b.strong_count() > 0);
    let mut end_pointer = None;
    for backing in vm.foreign.struct_backings.iter().filter_map(Weak::upgrade) {
        let Some((base, length)) = region(&backing) else {
            continue;
        };
        let address = ptr as usize;
        if address >= base && address - base == length {
            end_pointer = Some(backing);
            continue;
        }
        if address >= base && address - base < length {
            if size > length - (address - base) {
                return Err("native struct pointer exceeds allocated storage".into());
            }
            return Ok(Some(backing));
        }
    }
    if end_pointer.is_some() && size > 0 {
        return Err("native struct pointer exceeds allocated storage".into());
    }
    Ok(end_pointer)
}

fn has_pointers(ty: &StructType) -> bool {
    ty.record().fields.iter().any(|f| match &f.ty {
        FieldType::Scalar(s) => *s == Scalar::CString,
        FieldType::Inline(t) => has_pointers(&ty.target(t)),
        _ => true,
    })
}

/// Capture VM-managed targets that C wrote into pointer fields before the next collection.
pub(super) fn refresh(vm: &mut Vm, args: &[Value], result: Option<Value>) -> Result<(), String> {
    fn visit(
        vm: &mut Vm,
        value: Value,
        seen: &mut HashSet<(usize, usize, usize)>,
    ) -> Result<(), String> {
        if storage_closed_value(value, &mut HashSet::new()) {
            return Ok(());
        }
        let view = match foreign(value) {
            Some(Foreign::Out { .. }) => {
                for child in value.as_obj().unwrap().host_ref().references.clone() {
                    visit(vm, child, seen)?;
                }
                return Ok(());
            }
            Some(Foreign::Struct(v)) => v.clone(),
            _ => return Ok(()),
        };
        for index in 0..view.count {
            let element = View {
                ptr: unsafe { view.ptr.add(index * view.ty.record().layout.size()) },
                count: 1,
                array: false,
                ..view.clone()
            };
            fields(vm, &element, seen)?;
        }
        Ok(())
    }
    fn fields(
        vm: &mut Vm,
        view: &View,
        seen: &mut HashSet<(usize, usize, usize)>,
    ) -> Result<(), String> {
        let key = (
            view.ptr as usize,
            Rc::as_ptr(&view.ty.group) as usize,
            view.ty.index,
        );
        if !seen.insert(key) {
            return Ok(());
        }
        view.check_storage()?;
        for field in &view.ty.record().fields {
            let address = unsafe { view.ptr.add(field.offset) };
            if let FieldType::Inline(target) = &field.ty {
                let nested = View {
                    ty: view.ty.target(target),
                    ptr: address,
                    ..view.clone()
                };
                fields(vm, &nested, seen)?;
                continue;
            }
            if matches!(field.ty, FieldType::Scalar(_)) {
                continue;
            }
            let ptr = unsafe { address.cast::<*mut c_void>().read() };
            if let Some(value) = cached(view, address, ptr) {
                visit(vm, value, seen)?;
                continue;
            }
            view.backing
                .references
                .borrow_mut()
                .remove(&(address as usize));
            if ptr.is_null() || !vm_owned(&view.backing) {
                continue;
            }
            let value = match &field.ty {
                FieldType::Pointer(target) => {
                    let ty = view.ty.target(target);
                    let Some(backing) = known(vm, ptr.cast(), ty.record().layout.size())? else {
                        continue;
                    };
                    check_alignment(&ty, ptr.cast())?;
                    vm.new_host(
                        0,
                        Box::new(Foreign::Struct(View {
                            ty,
                            ptr: ptr.cast(),
                            backing,
                            count: 1,
                            array: false,
                            guard: None,
                            indirect: false,
                        })),
                    )
                }
                FieldType::Ptr => {
                    let Some(backing) = known(vm, ptr.cast(), 0)? else {
                        continue;
                    };
                    vm.new_host(
                        0,
                        Box::new(Foreign::Pointer {
                            ptr,
                            owner: None,
                            structure: Some(backing),
                            _library: view.ty.group._library.clone(),
                        }),
                    )
                }
                FieldType::Handle(ty) => {
                    let Some(resource) = ty
                        .live
                        .borrow()
                        .get(&(ptr as usize))
                        .and_then(Weak::upgrade)
                    else {
                        continue;
                    };
                    vm.new_host(0, Box::new(Foreign::Handle(resource)))
                }
                _ => continue,
            };
            if value.is_nil() {
                return Err("cannot retain a native struct field after allocation failure".into());
            }
            view.backing
                .references
                .borrow_mut()
                .insert(address as usize, Retained { ptr, value });
            visit(vm, value, seen)?;
        }
        Ok(())
    }
    let mut seen = HashSet::new();
    for value in args.iter().copied().chain(result) {
        visit(vm, value, &mut seen)?;
    }
    Ok(())
}

pub(super) fn pin_strings(value: Value, strings: &mut Vec<Rc<CString>>) {
    let backing = match foreign(value) {
        Some(Foreign::Struct(v)) => Some(v.backing.clone()),
        Some(Foreign::BorrowedHandle { backing, .. }) => Some(backing.clone()),
        Some(Foreign::Pointer {
            structure: Some(backing),
            ..
        }) => Some(backing.clone()),
        _ => value
            .as_obj()
            .filter(|o| o.kind() == Kind::Bytes)
            .and_then(|o| o.bytes().native_backing.borrow().clone()),
    };
    if let Some(backing) = backing {
        strings.extend(backing.strings.borrow().values().cloned());
    }
}

pub(super) fn callback_argument(
    vm: &mut Vm,
    ty: &NativeType,
    ptr: *mut c_void,
    library: &Rc<Library>,
    expired: Rc<Cell<bool>>,
) -> Result<Value, String> {
    if ptr.is_null() {
        return Ok(Value::Nil);
    }
    let known = known(vm, ptr.cast(), 0)?;
    let keeper = known.map(|backing| {
        vm.new_host(
            0,
            Box::new(Foreign::Pointer {
                ptr,
                owner: None,
                structure: Some(backing),
                _library: library.clone(),
            }),
        )
    });
    let resource = match ty {
        NativeType::Handle(ty) => ty
            .live
            .borrow()
            .get(&(ptr as usize))
            .and_then(Weak::upgrade),
        _ => None,
    };
    let keeper = keeper.or_else(|| {
        resource
            .as_ref()
            .map(|r| vm.new_host(0, Box::new(Foreign::Handle(r.clone()))))
    });
    let backing = Rc::new(Backing {
        busy: Cell::new(None),
        allocation: None,
        expired: Some(expired),
        owner: keeper,
        guard: resource,
        strings: RefCell::new(HashMap::new()),
        references: RefCell::new(HashMap::new()),
        copies: RefCell::new(HashMap::new()),
    });
    let data = match ty {
        NativeType::Struct(ty) => {
            check_alignment(ty, ptr.cast())?;
            known_range(vm, ptr, ty.record().layout.size())?;
            Foreign::Struct(View {
                ty: ty.clone(),
                ptr: ptr.cast(),
                backing,
                count: 1,
                array: false,
                guard: None,
                indirect: false,
            })
        }
        NativeType::Handle(ty) => Foreign::BorrowedHandle {
            ty: ty.clone(),
            ptr,
            backing,
            guard: None,
        },
        NativeType::Ptr => Foreign::Pointer {
            ptr,
            owner: None,
            structure: Some(backing),
            _library: library.clone(),
        },
        _ => return Err("unsupported callback argument".into()),
    };
    Ok(vm.new_host(0, Box::new(data)))
}

fn known_range(vm: &mut Vm, ptr: *mut c_void, size: usize) -> Result<(), String> {
    known(vm, ptr.cast(), size).map(|_| ())
}

pub(super) fn register_bytes(vm: &mut Vm, value: Value) {
    let Some(obj) = value.as_obj() else {
        return;
    };
    let backing = Backing::new(None, Some(value), None);
    register(vm, &backing);
    *obj.bytes().native_backing.borrow_mut() = Some(backing);
}

pub(super) fn pin_cstring(vm: &Vm, ptr: *const c_char, strings: &mut Vec<Rc<CString>>) {
    if ptr.is_null() {
        return;
    }
    let address = ptr as usize;
    for backing in vm.foreign.struct_backings.iter().filter_map(Weak::upgrade) {
        for text in backing.strings.borrow().values() {
            let base = text.as_ptr() as usize;
            if address >= base && address - base < text.as_bytes_with_nul().len() {
                strings.push(text.clone());
            }
        }
    }
}

pub(super) fn lock(value: Value, id: u64, locks: &mut Vec<worker::Lock>) -> Result<(), String> {
    let backing = match foreign(value) {
        Some(Foreign::Struct(view)) => Some(view.backing.clone()),
        Some(
            Foreign::Pointer {
                structure: Some(backing),
                ..
            }
            | Foreign::BorrowedHandle { backing, .. },
        ) => Some(backing.clone()),
        _ => value
            .as_obj()
            .filter(|o| o.kind() == Kind::Bytes)
            .and_then(|o| o.bytes().native_backing.borrow().clone()),
    };
    if let Some(backing) = backing {
        let old = backing.busy.get();
        if old != Some(id) {
            worker::check(old)?;
            backing.busy.set(Some(id));
            locks.push(worker::Lock::Backing(backing, old));
        }
    }
    Ok(())
}
