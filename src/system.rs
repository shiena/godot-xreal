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
    /// `TrackingType` values accepted by `switch_tracking_type` and returned by
    /// `get_tracking_type` (from the Unity `XREALPlugin.cs` enum).
    #[constant]
    const TRACKING_6DOF: i64 = 0;
    #[constant]
    const TRACKING_3DOF: i64 = 1;
    #[constant]
    const TRACKING_0DOF: i64 = 2;
    #[constant]
    const TRACKING_0DOF_STAB: i64 = 3;

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

    /// Whether the direct NR rendering/compositor API was resolved from libnr_loader.so.
    #[func]
    fn is_nr_rendering_available(&self) -> bool {
        session::shared()
            .map(XrealSession::nr_rendering_available)
            .unwrap_or(false)
    }

    /// Number of direct NR rendering symbols resolved from libnr_loader.so.
    #[func]
    fn get_nr_rendering_symbol_count(&self) -> i64 {
        session::shared()
            .map(XrealSession::nr_rendering_symbol_count)
            .unwrap_or(0) as i64
    }

    /// RE probe: create and immediately destroy an NRRendering handle.
    ///
    /// Returns 0 on success, -1 when libnr_loader.so was not resolved, or the native
    /// NRResult status on failure.
    #[func]
    fn smoke_test_nr_rendering_create_destroy(&self) -> i64 {
        session::shared()
            .map(XrealSession::nr_rendering_smoke_create_destroy)
            .unwrap_or(-1) as i64
    }

    /// RE probe: create, start, stop, and destroy an NRRendering handle.
    ///
    /// Returns 0 on success, -1 when libnr_loader.so was not resolved, or the native
    /// NRResult status on failure.
    #[func]
    fn smoke_test_nr_rendering_start_stop(&self) -> i64 {
        session::shared()
            .map(XrealSession::nr_rendering_smoke_start_stop)
            .unwrap_or(-1) as i64
    }

    /// XR-plugin tracking-state enum value (`-1` when unavailable).
    #[func]
    fn get_tracking_state(&self) -> i64 {
        session::shared()
            .and_then(XrealSession::tracking_state)
            .unwrap_or(-1) as i64
    }

    /// XR-plugin tracking-reason enum value (`-1` when unavailable).
    #[func]
    fn get_tracking_reason(&self) -> i64 {
        session::shared()
            .and_then(XrealSession::tracking_reason)
            .unwrap_or(-1) as i64
    }

    /// Current `TrackingType` enum value — see the `TRACKING_*` constants (`-1` when
    /// unavailable).
    #[func]
    fn get_tracking_type(&self) -> i64 {
        session::shared()
            .and_then(XrealSession::tracking_type)
            .unwrap_or(-1) as i64
    }

    /// Switch the tracking mode at runtime (`TRACKING_6DOF` / `TRACKING_3DOF` /
    /// `TRACKING_0DOF` / `TRACKING_0DOF_STAB`). Returns the SDK's bool result; `false`
    /// also when the session is not up yet.
    #[func]
    fn switch_tracking_type(&self, tracking_type: i64) -> bool {
        session::shared()
            .map(|s| s.switch_tracking_type(tracking_type as i32))
            .unwrap_or(false)
    }

    /// Keep the glasses display on by bypassing the proximity (wear) sensor auto-off.
    /// Returns the SDK status (0 = success), or `-1` when unavailable. The SDK no-ops
    /// until the session is live, so retry after `is_session_started()` turns true.
    #[func]
    fn set_display_bypass_psensor(&self, bypass: bool) -> i64 {
        session::shared()
            .and_then(|s| s.set_display_bypass_psensor(bypass))
            .unwrap_or(-1) as i64
    }

    /// Set the glasses spatial display mode (`SetGlassesSpaceMode`, One Pro X1 chip).
    /// `mode` is the `NRGlassesSpaceMode` enum (RE / unverified — probe 0/1/2/… on device to
    /// find follow vs world-anchor). Returns the SDK status, or `-1` when unavailable. Call
    /// after `is_session_started()` is true (the SDK no-ops until NativeGlasses is ready).
    #[func]
    fn set_glasses_space_mode(&self, mode: i64) -> i64 {
        session::shared()
            .and_then(|s| s.set_glasses_space_mode(mode as i32))
            .unwrap_or(-1) as i64
    }

    /// Select the stereo rendering mode applied when the native session bootstraps:
    /// `0` = Multipass (per-eye 2D textures — renders; the glasses layer is world-anchored),
    /// `2` = Multiview / Single-Pass-Instanced (matches LayeredClient; still WIP). **Call before the
    /// session starts** (e.g. an autoload `_ready`, before the XR rig enters the tree) — it is read
    /// once at `InitUserDefinedSettings`. Equivalent to the ProjectSetting
    /// `xreal/stereo_rendering_mode` or `adb shell setprop debug.xreal.stereo_mode <n>`.
    #[func]
    fn set_stereo_rendering_mode(&self, mode: i64) {
        session::set_stereo_mode_override(mode as i32);
    }

    /// The current stereo-mode override (`-1` if unset; the effective mode is resolved at bootstrap
    /// from the override, the ProjectSetting, the system property, then the default).
    #[func]
    fn get_stereo_rendering_mode(&self) -> i64 {
        session::stereo_mode_override() as i64
    }

    /// Current HMD clock in nanoseconds (`0` while the perception pipe is down).
    #[func]
    fn get_hmd_time_nanos(&self) -> i64 {
        session::shared()
            .and_then(XrealSession::hmd_time_nanos)
            .unwrap_or(0) as i64
    }

    // REMOVED (2026-07-12): `get_head_rotation(&self) -> Quaternion` calling `head_pose()`.
    // Isolated by controlled on-device bisection as the *sole* trigger of a deterministic render
    // -thread SIGSEGV (GLThread, addr 0x3f800000) at the first frame submit — present in the class
    // = crash, absent = runs, independent of return type (Quaternion / PackedFloat32Array / i64 all
    // crash) and method count. The trigger is this #[func] body referencing `XrealSession::head_pose`
    // (a #[func] whose body constructs only a Quaternion, or calls hmd_time_nanos instead, is fine).
    // Suspected rustc/gdext codegen interaction (the method is never actually called). Read head
    // rotation from `XrealHeadTracker` instead; reintroduce here only via a path that does not pull
    // `head_pose` into an `XrealSystem` #[func] thunk. See docs / memory input-feature-glthread-crash.

    /// One-line diagnostic of the perception pipeline (session/clock/pose state).
    #[func]
    fn get_diagnostics(&self) -> GString {
        session::shared()
            .map(|s| GString::from(s.diagnostics().as_str()))
            .unwrap_or_else(|| GString::from("session unavailable"))
    }
}
