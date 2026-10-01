//! C ABI of the verifier, linked into the C runtime.
//!
//! Declared for C in `runtime/include/calyx_verify.h`. Strings returned by
//! this module are owned by Rust and must be released with
//! [`calyx_string_free`].

use std::ffi::{CStr, CString, c_char};

/// Checks `len` bytes of UTF-8 source at `src`.
///
/// Returns a newly allocated, NUL-terminated JSON array of diagnostics
/// (`[]` when the program is valid), or NULL if `src` is NULL or not UTF-8.
///
/// # Safety
/// `src` must point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn calyx_verify(src: *const u8, len: usize) -> *mut c_char {
    if src.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller guarantees `src` points to `len` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(src, len) };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return std::ptr::null_mut();
    };
    let json = calyx_check::check("<runtime>", text).to_json();
    // JSON output escapes control characters, so it never contains NUL.
    CString::new(json).map_or(std::ptr::null_mut(), CString::into_raw)
}

/// Releases a string returned by this library. NULL is ignored.
///
/// # Safety
/// `s` must be NULL or a pointer returned by this library, not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn calyx_string_free(s: *mut c_char) {
    if !s.is_null() {
        // SAFETY: `s` came from `CString::into_raw` in this library.
        drop(unsafe { CString::from_raw(s) });
    }
}

/// Version of the verifier, as a static NUL-terminated string.
#[unsafe(no_mangle)]
pub extern "C" fn calyx_verifier_version() -> *const c_char {
    static VERSION: &CStr =
        match CStr::from_bytes_with_nul(concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes()) {
            Ok(v) => v,
            Err(_) => panic!("version contains NUL"),
        };
    VERSION.as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_through_the_c_abi() {
        let src = "node x = f(a);";
        // SAFETY: `src` is a valid buffer of `src.len()` bytes.
        let out = unsafe { calyx_verify(src.as_ptr(), src.len()) };
        assert!(!out.is_null());
        // SAFETY: `out` is a NUL-terminated string returned above.
        let json = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_owned();
        // SAFETY: `out` was returned by `calyx_verify` and is freed once.
        unsafe { calyx_string_free(out) };
        assert!(json.contains("\"code\":\"E0004\""), "{json}");
    }

    #[test]
    fn null_and_invalid_utf8_return_null() {
        // SAFETY: a NULL pointer is explicitly handled.
        assert!(unsafe { calyx_verify(std::ptr::null(), 0) }.is_null());
        let bad = [0xff_u8, 0xfe];
        // SAFETY: `bad` is a valid buffer of 2 bytes.
        assert!(unsafe { calyx_verify(bad.as_ptr(), bad.len()) }.is_null());
    }
}
