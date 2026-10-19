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
