use std::ffi::{CStr, c_char};
use std::sync::atomic::{AtomicI64, Ordering};

macro_rules! identity {
    ($name:ident, $ty:ty) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn $name(value: $ty) -> $ty {
            value
        }
    };
}
identity!(identity_i8, i8);
identity!(identity_i16, i16);
identity!(identity_i32, i32);
identity!(identity_i64, i64);
identity!(identity_u8, u8);
identity!(identity_u16, u16);
identity!(identity_u32, u32);
identity!(identity_u64, u64);
identity!(identity_size, usize);
identity!(identity_bool, bool);
identity!(identity_float, f32);
identity!(identity_double, f64);

#[unsafe(no_mangle)]
pub extern "C" fn add(a: i64, b: i64) -> i64 {
    a.wrapping_add(b)
}
#[unsafe(no_mangle)]
pub extern "C" fn mixed(a: i8, b: u16, c: i64, d: f32, e: f64) -> f64 {
    a as f64 + b as f64 + c as f64 + d as f64 + e
}
#[unsafe(no_mangle)]
pub extern "C" fn identity_cstring(s: *const c_char) -> *const c_char {
    s
}
#[unsafe(no_mangle)]
pub extern "C" fn greeting() -> *const c_char {
    c"h\u{e9}llo".as_ptr()
}
#[unsafe(no_mangle)]
pub extern "C" fn null_string() -> *const c_char {
    std::ptr::null()
}
#[unsafe(no_mangle)]
pub extern "C" fn invalid_utf8() -> *const c_char {
    b"\xff\0".as_ptr().cast()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn string_length(s: *const c_char) -> usize {
    if s.is_null() {
        0
    } else {
        unsafe { CStr::from_ptr(s) }.to_bytes().len()
    }
}
static NUMBER: AtomicI64 = AtomicI64::new(0);
#[unsafe(no_mangle)]
pub extern "C" fn store(n: i64) {
    NUMBER.store(n, Ordering::SeqCst);
}
#[unsafe(no_mangle)]
pub extern "C" fn stored() -> i64 {
    NUMBER.load(Ordering::SeqCst)
}

// Thread-local counters keep independent VM tests from sharing fixture state.
static ALL_FREED: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());
thread_local! {
    static FREED: std::cell::RefCell<Vec<i64>> = const { std::cell::RefCell::new(Vec::new()) };
}
#[repr(C)]
pub struct Session {
    id: i64,
}
#[repr(C)]
pub struct Record {
    session: *mut Session,
    id: i64,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn open_session(out: *mut *mut Session, id: i64) -> i32 {
    unsafe {
        *out = if id < 0 {
            std::ptr::null_mut()
        } else {
            Box::into_raw(Box::new(Session { id }))
        };
    }
    if id < 0 { -1 } else { 0 }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn close_session(session: *mut Session) {
    if !session.is_null() {
        let session = unsafe { Box::from_raw(session) };
        FREED.with(|f| f.borrow_mut().push(session.id));
        ALL_FREED.lock().unwrap().push(session.id);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_record(out: *mut *mut Record, session: *mut Session, id: i64) -> i32 {
    unsafe {
        *out = Box::into_raw(Box::new(Record { session, id }));
    }
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn make_record(session: *mut Session, id: i64) -> *mut Record {
    Box::into_raw(Box::new(Record { session, id }))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_record(record: *mut Record) {
    if !record.is_null() {
        let record = unsafe { Box::from_raw(record) };
        // Reading the parent verifies that a record is destroyed before its session.
        let parent = unsafe { (*record.session).id };
        FREED.with(|f| f.borrow_mut().push(parent * 100 + record.id));
        ALL_FREED.lock().unwrap().push(parent * 100 + record.id);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn record_value(record: *mut Record) -> i64 {
    unsafe { (*(*record).session).id * 100 + (*record).id }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn record_alias(record: *mut Record) -> *mut Record {
    record
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn session_id(session: *mut Session) -> i64 {
    unsafe { (*session).id }
}
#[unsafe(no_mangle)]
pub extern "C" fn freed_count() -> usize {
    FREED.with(|f| f.borrow().len())
}
#[unsafe(no_mangle)]
pub extern "C" fn freed_at(index: usize) -> i64 {
    FREED.with(|f| f.borrow().get(index).copied().unwrap_or(i64::MIN))
}
#[unsafe(no_mangle)]
pub extern "C" fn reset_freed() {
    FREED.with(|f| f.borrow_mut().clear());
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn identity_ptr(p: *mut std::ffi::c_void) -> *mut std::ffi::c_void {
    p
}
#[unsafe(no_mangle)]
pub extern "C" fn static_ptr() -> *const i64 {
    &12345
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn read_ptr(p: *const i64) -> i64 {
    unsafe { *p }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn out_ptr(out: *mut *const i64) {
    unsafe {
        *out = static_ptr();
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn session_ptr(session: *mut Session) -> *mut std::ffi::c_void {
    session.cast()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn out_string(out: *mut *const c_char) {
    unsafe {
        *out = c"output".as_ptr();
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn out_bad_string(out: *mut *const c_char) {
    unsafe {
        *out = invalid_utf8();
    }
}
macro_rules! output {
    ($name:ident, $ty:ty) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(out: *mut $ty, value: $ty) {
            unsafe {
                *out = value;
            }
        }
    };
}
output!(out_i8, i8);
output!(out_i16, i16);
output!(out_i32, i32);
output!(out_i64, i64);
output!(out_u8, u8);
output!(out_u16, u16);
output!(out_u32, u32);
output!(out_u64, u64);
output!(out_size, usize);
output!(out_bool, bool);
output!(out_float, f32);
output!(out_double, f64);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fill_bytes(bytes: *mut u8, len: usize, value: u8) {
    unsafe {
        std::ptr::write_bytes(bytes, value, len);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sum_bytes(bytes: *const u8, len: usize) -> u64 {
    unsafe { std::slice::from_raw_parts(bytes, len) }
        .iter()
        .map(|b| *b as u64)
        .sum()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn copy_bytes(out: *mut u8, source: *const u8, len: usize) {
    unsafe {
        std::ptr::copy(source, out, len);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn two_outputs(a: *mut i32, b: *mut i32) {
    unsafe {
        *a = 1;
        *b = 2;
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn handle_and_bad_string(text: *mut *const c_char, out: *mut *mut Session) {
    unsafe {
        *text = invalid_utf8();
        *out = Box::into_raw(Box::new(Session { id: 15 }));
    }
}

#[repr(C)]
pub struct Pair {
    pub x: i32,
    pub y: f64,
}
#[repr(C)]
pub struct Nested {
    pub tag: u8,
    pub pair: Pair,
    pub tail: u16,
}
#[repr(C)]
pub struct Node {
    pub value: i64,
    pub next: *mut Node,
    pub name: *const c_char,
}
#[repr(C)]
pub struct Fields {
    pub i8_: i8,
    pub u8_: u8,
    pub i16_: i16,
    pub u16_: u16,
    pub i32_: i32,
    pub u32_: u32,
    pub i64_: i64,
    pub u64_: u64,
    pub size: usize,
    pub flag: u8,
    pub float: f32,
    pub double: f64,
    pub text: *const c_char,
    pub data: *mut std::ffi::c_void,
}
#[repr(C)]
pub struct HandleHeader {
    pub session: *mut Session,
}
#[repr(C)]
pub struct BoxedPair {
    pub session: Session,
    pub pair: Pair,
    pub header: HandleHeader,
}

#[unsafe(no_mangle)]
pub extern "C" fn pair_size() -> usize {
    std::mem::size_of::<Pair>()
}
#[unsafe(no_mangle)]
pub extern "C" fn pair_align() -> usize {
    std::mem::align_of::<Pair>()
}
#[unsafe(no_mangle)]
pub extern "C" fn nested_size() -> usize {
    std::mem::size_of::<Nested>()
}
#[unsafe(no_mangle)]
pub extern "C" fn nested_align() -> usize {
    std::mem::align_of::<Nested>()
}
#[unsafe(no_mangle)]
pub extern "C" fn fields_size() -> usize {
    std::mem::size_of::<Fields>()
}
#[unsafe(no_mangle)]
pub extern "C" fn fields_align() -> usize {
    std::mem::align_of::<Fields>()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pair_value(pair: *const Pair) -> f64 {
    unsafe { (*pair).x as f64 + (*pair).y }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fill_pair(pair: *mut Pair, x: i32, y: f64) {
    unsafe {
        *pair = Pair { x, y };
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fill_nested(n: *mut Nested) {
    unsafe {
        *n = Nested {
            tag: 2,
            pair: Pair { x: 3, y: 4.5 },
            tail: 6,
        };
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nested_value(n: *const Nested) -> f64 {
    unsafe { (*n).tag as f64 + (*n).pair.x as f64 + (*n).pair.y + (*n).tail as f64 }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fill_pairs(pairs: *mut Pair, count: usize) {
    for n in 0..count {
        unsafe {
            pairs.add(n).write(Pair {
                x: n as i32,
                y: n as f64 + 0.25,
            });
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pairs_value(pairs: *const Pair, count: usize) -> f64 {
    (0..count)
        .map(|n| unsafe { (*pairs.add(n)).x as f64 + (*pairs.add(n)).y })
        .sum()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fill_fields(f: *mut Fields) {
    unsafe {
        *f = Fields {
            i8_: -128,
            u8_: 255,
            i16_: -32768,
            u16_: 65535,
            i32_: i32::MIN,
            u32_: u32::MAX,
            i64_: i64::MIN + 1,
            u64_: u64::MAX,
            size: usize::MAX,
            flag: 1,
            float: 1.25,
            double: -2.5,
            text: c"fixture".as_ptr(),
            data: static_ptr().cast_mut().cast(),
        };
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn check_fields(f: *const Fields) -> bool {
    let f = unsafe { &*f };
    f.i8_ == -128
        && f.u8_ == 255
        && f.i16_ == -32768
        && f.u16_ == 65535
        && f.i32_ == i32::MIN
        && f.u32_ == u32::MAX
        && f.i64_ == i64::MIN + 1
        && f.u64_ == u64::MAX
        && f.size == usize::MAX
        && f.flag == 1
        && f.float == 1.25
        && f.double == -2.5
        && unsafe { CStr::from_ptr(f.text) }.to_bytes() == b"assigned"
        && unsafe { read_ptr(f.data.cast()) } == 12345
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn node_value(node: *const Node) -> i64 {
    unsafe {
        (*node).value
            + if (*node).next.is_null() {
                0
            } else {
                (*(*node).next).value
            }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn node_name(node: *const Node) -> *const c_char {
    unsafe { (*node).name }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn set_node_name(node: *mut Node) {
    unsafe {
        (*node).name = c"native".as_ptr();
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn static_pair() -> *mut Pair {
    // Each test mutates a separate cell; C pointers are used only on the invoking VM thread.
    thread_local! { static PAIR: std::cell::UnsafeCell<Pair> = const { std::cell::UnsafeCell::new(Pair { x: 10, y: 0.5 }) }; }
    PAIR.with(|p| p.get())
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn out_pair(out: *mut *mut Pair) {
    unsafe {
        *out = static_pair();
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn unaligned_pair() -> *mut Pair {
    1usize as *mut Pair
}
#[unsafe(no_mangle)]
pub extern "C" fn open_box(id: i64) -> *mut BoxedPair {
    let mut value = Box::new(BoxedPair {
        session: Session { id },
        pair: Pair {
            x: id as i32,
            y: 0.5,
        },
        header: HandleHeader {
            session: std::ptr::null_mut(),
        },
    });
    value.header.session = &raw mut value.session;
    Box::into_raw(value)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn close_box(value: *mut BoxedPair) {
    let value = unsafe { Box::from_raw(value) };
    FREED.with(|f| f.borrow_mut().push(value.session.id));
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn box_pair(value: *mut BoxedPair) -> *mut Pair {
    unsafe { &raw mut (*value).pair }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn box_session(value: *mut BoxedPair) -> *mut Session {
    value.cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn box_header(value: *mut BoxedPair) -> *mut HandleHeader {
    unsafe { &raw mut (*value).header }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn identity_pair(pair: *mut Pair) -> *mut Pair {
    pair
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn identity_node(node: *mut Node) -> *mut Node {
    node
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn one_past_pair(pair: *mut Pair, count: usize) -> *mut Pair {
    unsafe { pair.add(count) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn link_nodes(node: *mut Node, next: *mut Node) {
    unsafe {
        (*node).next = next;
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn put_session(header: *mut HandleHeader, session: *mut Session) {
    unsafe {
        (*header).session = session;
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn static_node() -> *mut Node {
    thread_local! { static NODE: std::cell::UnsafeCell<Node> = const { std::cell::UnsafeCell::new(Node { value: 0, next: std::ptr::null_mut(), name: c"static".as_ptr() }) }; }
    NODE.with(|n| n.get())
}

type Visit = unsafe extern "C" fn(i32, *mut std::ffi::c_void) -> i32;
thread_local! { static CALLBACK_RESULTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }

#[unsafe(no_mangle)]
pub unsafe extern "C" fn visit(
    callback: Option<Visit>,
    data: *mut std::ffi::c_void,
    count: i32,
) -> i32 {
    let mut result = 0;
    for index in 0..count {
        let value = callback.map_or(0, |f| unsafe { f(index, data) });
        CALLBACK_RESULTS.with(|v| v.borrow_mut().push(value));
        result += value;
    }
    result
}
#[unsafe(no_mangle)]
pub extern "C" fn callback_result(index: usize) -> i32 {
    CALLBACK_RESULTS.with(|v| v.borrow()[index])
}

thread_local! { static CALLBACK_BITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
#[unsafe(no_mangle)]
pub extern "C" fn callback_bits() -> u64 {
    CALLBACK_BITS.with(|v| v.get())
}

macro_rules! callback_identity {
    ($name:ident, $ty:ty) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(
            callback: unsafe extern "C" fn($ty) -> $ty,
            value: $ty,
        ) -> $ty {
            let result = unsafe { callback(value) };
            let mut bytes = [0u8; 8];
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (&result as *const $ty).cast::<u8>(),
                    bytes.as_mut_ptr(),
                    std::mem::size_of::<$ty>(),
                )
            };
            CALLBACK_BITS.with(|v| v.set(u64::from_ne_bytes(bytes)));
            result
        }
    };
}
callback_identity!(callback_i8, i8);
callback_identity!(callback_i16, i16);
callback_identity!(callback_i32, i32);
callback_identity!(callback_i64, i64);
callback_identity!(callback_u8, u8);
callback_identity!(callback_u16, u16);
callback_identity!(callback_u32, u32);
callback_identity!(callback_u64, u64);
callback_identity!(callback_size, usize);
callback_identity!(callback_bool, bool);
callback_identity!(callback_float, f32);
callback_identity!(callback_double, f64);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn callback_mixed(
    callback: unsafe extern "C" fn(i8, u16, i64, f32, f64, *const c_char) -> f64,
) -> f64 {
    unsafe { callback(-12, 65000, -400, 1.25, 2.5, c"h\u{e9}llo".as_ptr()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn callback_strings(
    callback: unsafe extern "C" fn(*const c_char) -> *const c_char,
) -> *const c_char {
    let first = unsafe { callback(c"first".as_ptr()) };
    unsafe { callback(c"second".as_ptr()) };
    first
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn callback_void(callback: unsafe extern "C" fn()) {
    unsafe { callback() };
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn callback_pointer(callback: unsafe extern "C" fn() -> *const u8) -> u8 {
    unsafe { *callback() }
}

#[repr(C)]
pub struct VisitOptions {
    callback: Option<Visit>,
    data: *mut std::ffi::c_void,
    label: *const c_char,
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn visit_options(options: *mut VisitOptions) -> i32 {
    let callback = unsafe { (*options).callback.unwrap() };
    let data = unsafe { (*options).data };
    let label = unsafe { (*options).label };
    let a = unsafe { callback(1, data) };
    let b = unsafe { callback(2, data) };
    a + b + unsafe { CStr::from_ptr(label) }.to_bytes().len() as i32
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn visit_session(
    session: *mut Session,
    callback: Visit,
    data: *mut std::ffi::c_void,
) -> i64 {
    let before = unsafe { (*session).id };
    unsafe { callback(0, data) };
    before + unsafe { (*session).id }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn visit_record(
    out: *mut *mut Record,
    session: *mut Session,
    callback: Visit,
) {
    unsafe { callback(0, std::ptr::null_mut()) };
    unsafe { *out = make_record(session, 3) };
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn visit_output(out: *mut i32, callback: Visit) {
    unsafe {
        *out = 1;
        callback(0, std::ptr::null_mut());
        *out += 10;
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn callback_pair(callback: unsafe extern "C" fn(*mut Pair) -> i32) -> i32 {
    let mut pair = Pair { x: 9, y: 2.5 };
    unsafe { callback(&mut pair) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn callback_session(
    callback: unsafe extern "C" fn(*mut Session) -> i64,
) -> i64 {
    let mut session = Session { id: 19 };
    unsafe { callback(&mut session) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn callback_worker(callback: Visit) -> i32 {
    std::thread::spawn(move || unsafe { callback(7, std::ptr::null_mut()) })
        .join()
        .unwrap()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn callback_owned_session(
    session: *mut Session,
    callback: unsafe extern "C" fn(*mut Session) -> i64,
) -> i64 {
    unsafe { callback(session) + (*session).id }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn callback_closed_alias(
    session: *mut Session,
    callback: Visit,
) -> *mut Session {
    unsafe { callback(0, std::ptr::null_mut()) };
    session
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn callback_owned_pair(pair: *mut Pair, callback: Visit) -> i32 {
    unsafe {
        callback(0, std::ptr::null_mut());
        (*pair).x
    }
}

thread_local! { static REGISTERED_VISIT: std::cell::Cell<Option<(Visit, *mut std::ffi::c_void)>> = const { std::cell::Cell::new(None) }; }
#[unsafe(no_mangle)]
pub unsafe extern "C" fn register_visit(callback: Visit, data: *mut std::ffi::c_void) {
    REGISTERED_VISIT.with(|slot| slot.set(Some((callback, data))));
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn run_registered_visit() -> i32 {
    let (callback, data) = REGISTERED_VISIT.with(|slot| slot.get().unwrap());
    unsafe { callback(1, data) + *data.cast::<u8>() as i32 }
}
#[unsafe(no_mangle)]
pub extern "C" fn unregister_visit() {
    REGISTERED_VISIT.with(|slot| slot.set(None));
}

static GATES: [std::sync::atomic::AtomicBool; 16] =
    [const { std::sync::atomic::AtomicBool::new(false) }; 16];
#[unsafe(no_mangle)]
pub extern "C" fn release_gate(gate: usize) {
    GATES[gate].store(true, Ordering::Release);
}
#[unsafe(no_mangle)]
pub extern "C" fn gated_value(gate: usize, value: i64) -> i64 {
    while !GATES[gate].load(Ordering::Acquire) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    value
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gated_fill(bytes: *mut u8, count: usize, gate: usize) {
    for index in 0..count {
        unsafe { bytes.add(index).write(66) };
    }
    gated_value(gate, 0);
}
#[unsafe(no_mangle)]
pub extern "C" fn sleep_value(ms: usize, value: i64) -> i64 {
    std::thread::sleep(std::time::Duration::from_millis(ms as u64));
    value
}
static NEXT_THREAD: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
thread_local! { static NATIVE_THREAD: usize = NEXT_THREAD.fetch_add(1, Ordering::SeqCst); }
#[unsafe(no_mangle)]
pub extern "C" fn native_thread_id() -> usize {
    NATIVE_THREAD.with(|id| *id)
}

#[unsafe(no_mangle)]
pub extern "C" fn freed_globally(id: i64) -> bool {
    ALL_FREED.lock().unwrap().contains(&id)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn callback_gated_pointer(
    callback: unsafe extern "C" fn() -> *const u8,
    gate: usize,
) -> u8 {
    let pointer = unsafe { callback() };
    gated_value(gate, 0);
    unsafe { *pointer }
}
thread_local! { static SCRATCH_TEXT: std::cell::RefCell<std::ffi::CString> = std::cell::RefCell::new(std::ffi::CString::new("").unwrap()); }
#[unsafe(no_mangle)]
pub extern "C" fn scratch_text(value: i64) -> *const c_char {
    SCRATCH_TEXT.with(|text| {
        *text.borrow_mut() = std::ffi::CString::new(value.to_string()).unwrap();
        text.borrow().as_ptr()
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn callback_workers(callback: Visit) -> i32 {
    let a = std::thread::spawn(move || unsafe { callback(1, std::ptr::null_mut()) });
    let b = std::thread::spawn(move || unsafe { callback(2, std::ptr::null_mut()) });
    a.join().unwrap() + b.join().unwrap()
}

#[repr(C)]
pub struct Lease { references: usize }
thread_local! { static LEASE_FREED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[unsafe(no_mangle)]
pub extern "C" fn lease_new() -> *mut Lease { Box::into_raw(Box::new(Lease { references: 1 })) }
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lease_retain(value: *mut Lease) -> *mut Lease {
    unsafe { (*value).references += 1; } value
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lease_out(out: *mut *mut Lease, value: *mut Lease) {
    unsafe { *out = lease_retain(value); }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lease_release(value: *mut Lease) {
    unsafe {
        (*value).references -= 1;
        if (*value).references == 0 {
            drop(Box::from_raw(value));
            LEASE_FREED.with(|n| n.set(n.get() + 1));
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lease_count(value: *mut Lease) -> usize { unsafe { (*value).references } }
#[unsafe(no_mangle)]
pub extern "C" fn lease_freed() -> usize { LEASE_FREED.with(|n| n.get()) }
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lease_callback(value: *mut Lease, cb: unsafe extern "C" fn(i64, i64) -> i64) -> *mut Lease {
    unsafe { cb(0,0); lease_retain(value) }
}
