//! [`XrealSystem`] — a small read-only handle onto the XREAL SDK for GDScript.
//!
//! Instantiate it (`XrealSystem.new()`) to query device/session info. It reads from the
//! process-global [`crate::session`] (shared with the head-tracker node) and reports
//! `is_available() == false` on desktop/editor or when the session failed to start.

use godot::classes::IRefCounted;
use godot::prelude::*;

use crate::session::{self, XrealSession};

#[derive(GodotClass)]
#[class(base = RefCounted)]
pub struct XrealSystem {
    base: Base<RefCounted>,
}

#[godot_api]
impl IRefCounted for XrealSystem {
    fn init(base: Base<RefCounted>) -> Self {
        Self { base }
    }
}

#[godot_api]
impl XrealSystem {
    /// Whether the native session is up (libraries loaded + session created). `false` on
    /// desktop/editor, or while waiting for the Android Activity / after a failed bootstrap.
    #[func]
    fn is_available(&self) -> bool {
        session::shared().is_some()
    }

    /// Whether a native session is currently running.
    #[func]
    fn is_session_started(&self) -> bool {
        session::shared()
            .map(XrealSession::is_session_started)
            .unwrap_or(false)
    }

    /// Native plugin version string (`"n/a"` when unavailable).
    #[func]
    fn get_plugin_version(&self) -> GString {
        session::shared()
            .and_then(XrealSession::plugin_version)
            .map(|version| GString::from(version.as_str()))
            .unwrap_or_else(|| GString::from("n/a"))
    }

    /// Connected `XREALDeviceType` enum value (`0` = invalid/unavailable).
    #[func]
    fn get_device_type(&self) -> i64 {
        session::shared()
            .and_then(XrealSession::device_type)
            .unwrap_or(0) as i64
    }
}
