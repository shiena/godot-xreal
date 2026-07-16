//! Latest native error from the XREAL plugin, cached for polling.
//!
//! The plugin reports asynchronous native errors through the callback registered with
//! `SetNativeErrorCallback` (`XREALCallbackHandler.SetNativeErrorCallback` in the Unity SDK,
//! feeding `OnXREALError`). The callback fires on an SDK-owned thread, so — like the glasses
//! event funnel — it must not touch Godot: it only stores the latest code/message here, and
//! `XrealSystem` exposes them as plain getters (no signal).

use std::ffi::{c_char, CStr};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;

/// Latest `XREALErrorCode` reported by the native plugin, or `-1` until one arrives.
/// (All real codes are `>= 0`, with `0` = `Success`, so `-1` is an unambiguous "none yet".)
static LAST_ERROR_CODE: AtomicI32 = AtomicI32::new(-1);

/// Optional message that accompanied the latest error (the plugin may pass null).
static LAST_ERROR_MESSAGE: Mutex<Option<String>> = Mutex::new(None);

/// The `extern "C"` callback handed to `SetNativeErrorCallback`. Runs on an SDK thread: no
/// Godot calls, no logging — just cache the code and copy the message out of the C string.
pub extern "C" fn on_native_error(code: i32, message: *const c_char) {
    LAST_ERROR_CODE.store(code, Ordering::Relaxed);
    // Copy the message while the pointer is still valid (it belongs to the caller's frame).
    let text = if message.is_null() {
        None
    } else {
        Some(
            unsafe { CStr::from_ptr(message) }
                .to_string_lossy()
                .into_owned(),
        )
    };
    if let Ok(mut slot) = LAST_ERROR_MESSAGE.lock() {
        *slot = text;
    }
}

/// Latest cached native error code (`-1` if none has arrived). A plain poll — no signal.
pub fn last_error_code() -> i32 {
    LAST_ERROR_CODE.load(Ordering::Relaxed)
}

/// Message that came with the latest native error (empty if none / null).
pub fn last_error_message() -> String {
    match LAST_ERROR_MESSAGE.lock() {
        Ok(slot) => slot.clone().unwrap_or_default(),
        Err(poisoned) => poisoned.into_inner().clone().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn caches_code_and_message_and_tolerates_null() {
        let _guard = TEST_LOCK.lock().unwrap();
        let msg = CString::new("control channel init fail").unwrap();
        on_native_error(101, msg.as_ptr());
        assert_eq!(last_error_code(), 101);
        assert_eq!(last_error_message(), "control channel init fail");

        // A later error with a null message keeps the new code and clears the text.
        on_native_error(13, std::ptr::null());
        assert_eq!(last_error_code(), 13);
        assert_eq!(last_error_message(), "");
    }
}
