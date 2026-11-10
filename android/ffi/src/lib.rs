//! C interface of the gdirect mobile engine, loaded by the Flutter app
//! through `dart:ffi`. Two functions: [`gdm_call`] runs a method and
//! [`gdm_free`] releases its answer. See `engine.rs` for the methods.
mod engine;
mod serve;

use std::ffi::{CStr, CString, c_char};

pub use engine::call;

/// Runs `method` with JSON `args` and returns a JSON reply, `{"ok": …}` or
/// `{"error": …}`, as a string to release with [`gdm_free`].
///
/// # Safety
/// `method` and `args` must be valid NUL-terminated strings (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gdm_call(method: *const c_char, args: *const c_char) -> *mut c_char {
    let read = |p: *const c_char| {
        if p.is_null() {
            String::new()
        } else {
            // SAFETY: the caller passes a NUL-terminated string.
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    };
    let (method, args) = (read(method), read(args));
    let reply = engine::call(&method, &args);
    CString::new(reply)
        .unwrap_or_else(|_| CString::new(r#"{"error":"bad reply"}"#).unwrap())
        .into_raw()
}

/// Releases a string returned by [`gdm_call`].
///
/// # Safety
/// `ptr` must come from [`gdm_call`] and not have been freed already.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gdm_free(ptr: *mut c_char) {
    if !ptr.is_null() {
        // SAFETY: allocated by CString::into_raw in gdm_call.
        drop(unsafe { CString::from_raw(ptr) });
    }
}
