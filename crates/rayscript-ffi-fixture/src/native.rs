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
