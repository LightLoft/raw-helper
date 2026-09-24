//! Seatbelt sandbox of the helper process (profile: `sandbox.sb`).
//!
//! The helper enters it itself, once its decoders are initialised and before it reads any
//! request. It cannot be left: a compromised decoder stays confined.

use std::ffi::{c_char, c_int, CStr, CString};

const PROFILE: &str = include_str!("sandbox.sb");

extern "C" {
    // libsandbox (part of libSystem). `flags = 0`: `profile` is the profile text itself.
    fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> c_int;
    fn sandbox_free_error(errorbuf: *mut c_char);
}

pub fn enter() -> Result<(), String> {
    let profile = CString::new(PROFILE).map_err(|e| e.to_string())?;
    let mut error: *mut c_char = std::ptr::null_mut();
    // SAFETY: `profile` is a valid NUL-terminated string that outlives the call; on failure the
    // library sets `error` to a string that we copy, then free with the matching function.
    let status = unsafe { sandbox_init(profile.as_ptr(), 0, &mut error) };
    if status == 0 {
        return Ok(());
    }
    let message = if error.is_null() {
        "sandbox_init failed".to_owned()
    } else {
        // SAFETY: non-null error string returned by sandbox_init, freed right after copying.
        let text = unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned();
        unsafe { sandbox_free_error(error) };
        text
    };
    Err(message)
}
