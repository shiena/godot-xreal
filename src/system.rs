//! [`XrealSystem`] — a small read-only handle onto the XREAL SDK for GDScript.
//!
//! Instantiate it (`XrealSystem.new()`) to query device/session info. It reads from the
//! process-global [`crate::session`] (shared with the head-tracker node) and reports
//! `is_available() == false` on desktop/editor or when the session failed to start.

use godot::builtin::{VarArray, VarDictionary};
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

    /// Latest glasses temperature level from the hardware event funnel: `0` NORMAL /
    /// `1` WARM / `2` HOT (mirrors the SDK's `XREALTemperatureLevel`), or `-1` until the
    /// glasses first report one. A cached poll — no signal. This is the data source behind
    /// the SDK's over-temperature notification. (The SDK's low-battery notification reads
    /// the Android *host* battery, not a glasses API — poll it from the platform; SLAM-state
    /// is `get_tracking_state` / `get_tracking_reason`.)
    #[func]
    fn get_glasses_temperature_level(&self) -> i64 {
        crate::glasses_events::temperature_level() as i64
    }

    /// Latest asynchronous native error reported by the plugin, as the `XREALErrorCode`
    /// enum value (`0` Success / `1` Failure / … `-1` until one arrives). A cached poll —
    /// no signal — mirroring the SDK's native-error notification. Pair with
    /// `get_last_native_error_message()` for the accompanying text.
    #[func]
    fn get_last_native_error_code(&self) -> i64 {
        crate::native_error::last_error_code() as i64
    }

    /// Message that accompanied the latest native error (empty string if none / not provided).
    #[func]
    fn get_last_native_error_message(&self) -> GString {
        crate::native_error::last_error_message().as_str().into()
    }

    /// Discover + create + start the NRController subsystem (`libnr_loader.so`) and keep it alive for
    /// `poll_controller`. Returns a one-line diagnostic (count / id / connected & handheld type).
    /// The phone-as-3D-pointer source (docs/plans/input-plan.md Phase C).
    #[func]
    fn start_controller(&self) -> GString {
        crate::controller_probe::start().as_str().into()
    }

    /// One-frame read of the live controller's raw sensors (call each frame after
    /// `start_controller`). Returns a flat `PackedFloat32Array`, layout:
    /// `[ok, accel.xyz(1..4), gyro.xyz(4..7), mag.xyz(7..10), touch(10), touch_xy(11..13), buttons(13)]`.
    /// The phone IMU (`accel` = gravity dir via `-accel.normalized()`) feeds the GDScript pointer
    /// fusion, since the NRController fused pose isn't available on this host.
    #[func]
    fn poll_controller(&self) -> PackedFloat32Array {
        let r = crate::controller_probe::poll_raw();
        PackedFloat32Array::from(&[
            if r.ok { 1.0 } else { 0.0 },
            r.accel[0],
            r.accel[1],
            r.accel[2],
            r.gyro[0],
            r.gyro[1],
            r.gyro[2],
            r.mag[0],
            r.mag[1],
            r.mag[2],
            r.touch as f32,
            r.touch_xy[0],
            r.touch_xy[1],
            r.buttons as f32,
        ])
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

    // --- Plane detection (see docs/plans/ar-features-plan.md). Needs a live 6DoF session. ---

    /// `PlaneDetectionMode` flags for [`Self::set_plane_detection_mode`] / [`Self::poll_planes`].
    #[constant]
    const PLANE_NONE: i64 = crate::ffi::plane_detection_mode::NONE as i64;
    #[constant]
    const PLANE_HORIZONTAL: i64 = crate::ffi::plane_detection_mode::HORIZONTAL as i64;
    #[constant]
    const PLANE_VERTICAL: i64 = crate::ffi::plane_detection_mode::VERTICAL as i64;
    #[constant]
    const PLANE_BOTH: i64 = crate::ffi::plane_detection_mode::BOTH as i64;

    /// Whether the plane-detection C ABI resolved (false on desktop / older devices).
    #[func]
    fn is_plane_detection_available(&self) -> bool {
        session::shared()
            .map(XrealSession::plane_detection_available)
            .unwrap_or(false)
    }

    /// Enable plane detection (`PLANE_HORIZONTAL | PLANE_VERTICAL` flags). Needs a live 6DoF session;
    /// returns the SDK bool (false when unavailable). Call after `is_session_started()`.
    #[func]
    fn set_plane_detection_mode(&self, mode: i64) -> bool {
        session::shared()
            .map(|s| s.set_plane_detection_mode(mode as i32))
            .unwrap_or(false)
    }

    /// Current `PlaneDetectionMode` flags, or `-1` when unavailable.
    #[func]
    fn get_plane_detection_mode(&self) -> i64 {
        session::shared()
            .and_then(XrealSession::plane_detection_mode)
            .unwrap_or(-1) as i64
    }

    /// Poll detected-plane changes since the last call. Returns
    /// `{ "added": Array, "updated": Array, "removed": Array }` where each added/updated entry is a
    /// `Dictionary { id: String, transform: Transform3D, center: Vector2, size: Vector2, alignment: int }`
    /// and `removed` is an array of id strings. Call once per frame (drives the SDK's change queue).
    #[func]
    fn poll_planes(&self) -> VarDictionary {
        let mut added = VarArray::new();
        let mut updated = VarArray::new();
        let mut removed = VarArray::new();
        if let Some(changes) = session::shared().and_then(XrealSession::poll_plane_changes) {
            for p in &changes.added {
                added.push(&plane_to_dict(p).to_variant());
            }
            for p in &changes.updated {
                updated.push(&plane_to_dict(p).to_variant());
            }
            for id in &changes.removed {
                removed.push(&trackable_id_to_gstring(*id).to_variant());
            }
        }
        let mut d = VarDictionary::new();
        d.set(&"added".to_variant(), &added.to_variant());
        d.set(&"updated".to_variant(), &updated.to_variant());
        d.set(&"removed".to_variant(), &removed.to_variant());
        d
    }

    /// Boundary polygon (plane-local points) of a detected plane, by its id string from `poll_planes`.
    #[func]
    fn get_plane_boundary(&self, id: GString) -> PackedVector2Array {
        let tid = gstring_to_trackable_id(&id);
        let verts = session::shared()
            .map(|s| s.plane_boundary(tid))
            .unwrap_or_default();
        let mut out = PackedVector2Array::new();
        for v in &verts {
            out.push(Vector2::new(v[0], v[1]));
        }
        out
    }

    // NOTE: there is no stereo-mode selector — the port is Multipass-only. Multiview is shelved and
    // no longer reachable (the force_multiview escape was removed). It renders correctly but gains
    // nothing here and shares a cross-thread crash path; see docs/archive/multiview-investigation.md.

    /// Select the head-tracking mode applied when the native session **bootstraps** (a startup
    /// selector): `0` = 6DoF (SLAM position + orientation, no drift — the recommended mode),
    /// `1` = 3DoF (IMU orientation only, no position), `2` = 0DoF.
    /// **Call before the session starts** (e.g. an autoload `_ready`, before the XR rig enters the
    /// tree) — it is read once at `InitUserDefinedSettings`. Equivalent to the ProjectSetting
    /// `xreal/tracking_type` or `adb shell setprop debug.xreal.tracking_type <n>`. Use
    /// `get_tracking_type()` for the mode actually active on the running session, and
    /// `switch_tracking_type()` to change it at runtime (SDK call; may be unavailable mid-session).
    #[func]
    fn set_tracking_type(&self, mode: i64) {
        session::set_tracking_mode_override(mode as i32);
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

    // --- Render metrics (XREAL SDK NRMetrics, queried directly — see src/metrics.rs) ---------------
    //
    // These read the process-global NR compositor metrics service (the same numbers the SDK's own
    // `DisplayManager::UpdateMetrics` reports to Unity's stat sink; we neuter that sink and query NR
    // directly). The handle is created + started lazily on first read, so the first calls after launch
    // may return the "unavailable" sentinel until the NR runtime is up. Poll each frame or on a timer.

    /// Present rate in frames/second (compositor, integer ~60). `-1.0` while the metrics handle is
    /// not up yet.
    #[func]
    fn get_present_fps(&self) -> f64 {
        crate::metrics::present_fps().map(f64::from).unwrap_or(-1.0)
    }

    /// Frames dropped by the compositor. `-1` while the metrics handle is not up yet.
    #[func]
    fn get_dropped_frame_count(&self) -> i64 {
        crate::metrics::dropped_frame_count()
            .map(i64::from)
            .unwrap_or(-1)
    }

    /// Frames presented early. `-1` while the metrics handle is not up yet.
    #[func]
    fn get_early_frame_count(&self) -> i64 {
        crate::metrics::early_frame_count()
            .map(i64::from)
            .unwrap_or(-1)
    }

    /// Present count for the current frame (FPC). `-1` while the metrics handle is not up yet.
    #[func]
    fn get_frame_present_count(&self) -> i64 {
        crate::metrics::frame_present_count()
            .map(i64::from)
            .unwrap_or(-1)
    }

    /// Extended (re-projected/stale) frame count (EFC). `-1` while the metrics handle is not up yet.
    #[func]
    fn get_extended_frame_count(&self) -> i64 {
        crate::metrics::extended_frame_count()
            .map(i64::from)
            .unwrap_or(-1)
    }

    /// Teared frame count. `-1` when unavailable (also the SDK's own "not tracked" sentinel).
    #[func]
    fn get_teared_frame_count(&self) -> i64 {
        crate::metrics::teared_frame_count()
            .map(i64::from)
            .unwrap_or(-1)
    }

    /// Compositor frame composite time in milliseconds. `-1.0` while the metrics handle is not up yet.
    #[func]
    fn get_frame_composite_time_ms(&self) -> f64 {
        crate::metrics::frame_composite_time_ns()
            .map(|ns| ns as f64 * 1e-6)
            .unwrap_or(-1.0)
    }

    /// App frame latency (motion-to-photon input) in milliseconds. `-1.0` while unavailable.
    #[func]
    fn get_app_frame_latency_ms(&self) -> f64 {
        crate::metrics::app_frame_latency_ns()
            .map(|ns| ns as f64 * 1e-6)
            .unwrap_or(-1.0)
    }

    /// One-line diagnostic / start status of the render-metrics handle.
    #[func]
    fn get_render_metrics_diagnostics(&self) -> GString {
        GString::from(crate::metrics::diagnostics().as_str())
    }
}

// --- Plane-detection conversions (Unity → Godot) ---

/// Convert a Unity-space plane pose to a Godot `Transform3D`: position `(x, -y, -z)`, quaternion
/// `(-x, -y, z, w)` — the same convention as the head/hand poses (`src/hand_tracking.rs`). The exact
/// axis signs are pending on-device verification with real planes.
fn unity_pose_to_transform(pose: &crate::ffi::UnityPose) -> Transform3D {
    let p = pose.position;
    let r = pose.rotation;
    let quat = Quaternion::new(r[0], -r[1], -r[2], r[3]);
    Transform3D::new(Basis::from_quaternion(quat), Vector3::new(p[0], -p[1], -p[2]))
}

/// 128-bit `TrackableId` → a stable 32-hex-char string (round-trips via [`gstring_to_trackable_id`]).
fn trackable_id_to_gstring(id: crate::ffi::TrackableId) -> GString {
    GString::from(format!("{:016x}{:016x}", id.sub_id_1, id.sub_id_2).as_str())
}

/// Parse a 32-hex-char id string (from `poll_planes`) back into a `TrackableId`.
fn gstring_to_trackable_id(s: &GString) -> crate::ffi::TrackableId {
    let s = s.to_string();
    crate::ffi::TrackableId {
        sub_id_1: s.get(0..16).and_then(|h| u64::from_str_radix(h, 16).ok()).unwrap_or(0),
        sub_id_2: s.get(16..32).and_then(|h| u64::from_str_radix(h, 16).ok()).unwrap_or(0),
    }
}

/// A detected plane → a GDScript `Dictionary`.
fn plane_to_dict(p: &crate::native::PlaneSample) -> VarDictionary {
    let mut d = VarDictionary::new();
    d.set(&"id".to_variant(), &trackable_id_to_gstring(p.id).to_variant());
    d.set(&"transform".to_variant(), &unity_pose_to_transform(&p.pose).to_variant());
    d.set(&"center".to_variant(), &Vector2::new(p.center[0], p.center[1]).to_variant());
    d.set(&"size".to_variant(), &Vector2::new(p.size[0], p.size[1]).to_variant());
    d.set(&"alignment".to_variant(), &(p.alignment as i64).to_variant());
    d
}
