//! [`XrealSystem`] — the SDK façade onto the XREAL plugin for GDScript.
//!
//! Instantiate it (`XrealSystem.new()`) to query device/session info, switch the tracking
//! mode, and drive the AR subsystems (plane detection, spatial anchors, image tracking, depth
//! meshing), render metrics, capture and FPV streaming. It reads from the process-global
//! [`crate::session`] (shared with the head-tracker node) and reports `is_available() == false`
//! on desktop/editor or when the session failed to start.

use godot::builtin::{VarArray, VarDictionary};
use godot::classes::{INode, IRefCounted};
use godot::prelude::*;

use crate::session::{self, XrealSession};

/// SDK information and control surface for the XREAL glasses.
///
/// A `RefCounted` façade over the native XREAL plugin: query availability / version / tracking
/// state, switch the tracking mode, drive the AR subsystems (plane detection, spatial anchors,
/// image tracking, depth meshing), read compositor render metrics, and run FPV streaming.
/// Instantiate it once (e.g. from an autoload) and keep it. Its methods are safe to call before a
/// native session exists — they return defaults / error sentinels until the session is live, so
/// they no-op on desktop and while the glasses are still connecting.
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
    /// `TrackingType` for `switch_tracking_type` / `get_tracking_type` (from the Unity
    /// `XREALPlugin.cs` enum): 6DoF — SLAM position + orientation (the recommended mode).
    #[constant]
    const TRACKING_6DOF: i64 = 0;
    /// `TrackingType`: 3DoF — IMU orientation only, no position.
    #[constant]
    const TRACKING_3DOF: i64 = 1;
    /// `TrackingType`: 0DoF — no head tracking.
    #[constant]
    const TRACKING_0DOF: i64 = 2;
    /// `TrackingType`: 0DoF with stabilization.
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

    // --- Device capability query (IsHMDFeatureSupported) ---

    /// `XREALSupportedFeature` for [`Self::is_hmd_feature_supported`]: the RGB camera.
    #[constant]
    const FEATURE_RGB_CAMERA: i64 = crate::ffi::hmd_feature::RGB_CAMERA as i64;
    /// `XREALSupportedFeature`: the proximity (wear) sensor.
    #[constant]
    const FEATURE_WEARING_STATUS: i64 = crate::ffi::hmd_feature::WEARING_STATUS as i64;
    /// `XREALSupportedFeature`: a handheld controller.
    #[constant]
    const FEATURE_CONTROLLER: i64 = crate::ffi::hmd_feature::CONTROLLER as i64;
    /// `XREALSupportedFeature`: head-tracking rotation (3DoF).
    #[constant]
    const FEATURE_HEAD_TRACKING_ROTATION: i64 =
        crate::ffi::hmd_feature::HEAD_TRACKING_ROTATION as i64;
    /// `XREALSupportedFeature`: head-tracking position (6DoF).
    #[constant]
    const FEATURE_HEAD_TRACKING_POSITION: i64 =
        crate::ffi::hmd_feature::HEAD_TRACKING_POSITION as i64;

    /// Whether the connected glasses support a `FEATURE_*` capability (`IsHMDFeatureSupported`).
    /// `false` when unavailable. This is the device-accurate gate the SDK itself uses.
    #[func]
    fn is_hmd_feature_supported(&self, feature: i64) -> bool {
        session::shared()
            .and_then(|s| s.hmd_feature_supported(feature as i32))
            .unwrap_or(false)
    }

    /// Whether the connected glasses have an RGB camera (`IsHMDFeatureSupported(FEATURE_RGB_CAMERA)`).
    /// The Air 2 Ultra reports `false` — gate the camera on this so it is never opened there.
    #[func]
    fn is_camera_supported(&self) -> bool {
        session::shared()
            .and_then(|s| s.hmd_feature_supported(crate::ffi::hmd_feature::RGB_CAMERA))
            .unwrap_or(false)
    }

    /// AR-perception (plane / anchor / image) availability, following the XREAL SDK for Unity's own
    /// rule. That SDK has NO per-feature device gate for those subsystems — it registers them
    /// unconditionally and assumes perception is present whenever 6DoF (`HEAD_TRACKING_POSITION`) is,
    /// and its C# never calls `GetSupportedFeatures` (which returns a device-INDEPENDENT `0x1f` mask,
    /// so it cannot gate by device). We use "6DoF present AND no RGB camera" to pick the perception
    /// device (Air 2 Ultra: 6DoF, no RGB cam) apart from the One series (6DoF + RGB cam, no
    /// trackables). **This heuristic matches com.xreal.xr 3.1.0** — a later SDK / device may change the
    /// mapping, so revisit it on SDK bumps. (Hand tracking is already in beta for One + Eye, so a
    /// One-class device could gain perception, at which point the "no RGB camera" test would wrongly
    /// exclude it.)
    fn is_ar_perception_available(&self) -> bool {
        self.is_hmd_feature_supported(Self::FEATURE_HEAD_TRACKING_POSITION)
            && !self.is_camera_supported()
    }

    // --- Device / camera geometry (Unity space; docs/plans/coordinate-systems-notes.md). Poses are in
    // Unity's left-handed system — convert on the Godot side. Useful for aligning the AR to the RGB
    // camera view (FOV / offset) in the blend. ---

    /// `XREALComponent` id for the geometry getters: the RGB camera.
    #[constant]
    const COMPONENT_RGB_CAMERA: i64 = crate::ffi::component::RGB_CAMERA as i64;
    /// `XREALComponent` id: the left display.
    #[constant]
    const COMPONENT_DISPLAY_LEFT: i64 = crate::ffi::component::DISPLAY_LEFT as i64;
    /// `XREALComponent` id: the right display.
    #[constant]
    const COMPONENT_DISPLAY_RIGHT: i64 = crate::ffi::component::DISPLAY_RIGHT as i64;
    /// `XREALComponent` id: the left SLAM grayscale camera.
    #[constant]
    const COMPONENT_GRAYSCALE_LEFT: i64 = crate::ffi::component::GRAYSCALE_CAMERA_LEFT as i64;
    /// `XREALComponent` id: the right SLAM grayscale camera.
    #[constant]
    const COMPONENT_GRAYSCALE_RIGHT: i64 = crate::ffi::component::GRAYSCALE_CAMERA_RIGHT as i64;

    /// A `COMPONENT_*` device's pixel resolution (`Vector2i.ZERO` when unavailable).
    #[func]
    fn get_device_resolution(&self, component: i64) -> Vector2i {
        session::shared()
            .and_then(|s| s.device_resolution(component as i32))
            .map(|(w, h)| Vector2i::new(w, h))
            .unwrap_or(Vector2i::ZERO)
    }

    /// A `COMPONENT_*` camera's intrinsics as `[fx, fy, cx, cy]` in pixels (empty when unavailable).
    #[func]
    fn get_camera_intrinsics(&self, component: i64) -> PackedFloat32Array {
        session::shared()
            .and_then(|s| s.camera_intrinsic(component as i32))
            .map(|k| PackedFloat32Array::from(&k))
            .unwrap_or_default()
    }

    /// A `COMPONENT_*` device's extrinsic relative to Head as a raw Unity `Pose`:
    /// `[pos x,y,z, quat x,y,z,w]` (Unity left-handed — convert to Godot; see coordinate-systems-notes).
    /// Empty when unavailable.
    #[func]
    fn get_device_pose_from_head(&self, component: i64) -> PackedFloat32Array {
        session::shared()
            .and_then(|s| s.device_pose_from_head(component as i32))
            .map(|p| PackedFloat32Array::from(&p))
            .unwrap_or_default()
    }

    /// A `COMPONENT_*` camera's 4x4 projection matrix (16 floats, Unity column-major) for `[near, far]`.
    /// Empty when unavailable.
    #[func]
    fn get_camera_projection_matrix(
        &self,
        component: i64,
        near: f64,
        far: f64,
    ) -> PackedFloat32Array {
        session::shared()
            .and_then(|s| s.camera_projection_matrix(component as i32, near as f32, far as f32))
            .map(|m| PackedFloat32Array::from(&m))
            .unwrap_or_default()
    }

    // --- Plane detection (see docs/plans/ar-features-plan.md). Needs a live 6DoF session. ---

    /// `PlaneDetectionMode` flag for [`Self::set_plane_detection_mode`] / [`Self::poll_planes`]:
    /// detection off.
    #[constant]
    const PLANE_NONE: i64 = crate::ffi::plane_detection_mode::NONE as i64;
    /// `PlaneDetectionMode` flag: detect horizontal planes (floors, tables).
    #[constant]
    const PLANE_HORIZONTAL: i64 = crate::ffi::plane_detection_mode::HORIZONTAL as i64;
    /// `PlaneDetectionMode` flag: detect vertical planes (walls).
    #[constant]
    const PLANE_VERTICAL: i64 = crate::ffi::plane_detection_mode::VERTICAL as i64;
    /// `PlaneDetectionMode` flag: detect both horizontal and vertical planes.
    #[constant]
    const PLANE_BOTH: i64 = crate::ffi::plane_detection_mode::BOTH as i64;

    /// Whether the connected glasses support plane detection. Gated by
    /// [`Self::is_ar_perception_available`] (6DoF + no RGB camera = the Air 2 Ultra). `false` until the
    /// session is up, so query it at toggle time, not at startup.
    #[func]
    fn is_plane_detection_available(&self) -> bool {
        self.is_ar_perception_available()
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

    // --- Spatial anchors (see docs/plans/ar-features-plan.md). Needs a live 6DoF session + the
    //     vendored nr_spatial_anchor.aar backend. ---

    /// Anchor-quality level from [`Self::estimate_anchor_quality`]: insufficient — do not save here.
    #[constant]
    const ANCHOR_QUALITY_INSUFFICIENT: i64 = crate::ffi::anchor_quality::INSUFFICIENT as i64;
    /// Anchor-quality level: sufficient — the minimum recommended before saving.
    #[constant]
    const ANCHOR_QUALITY_SUFFICIENT: i64 = crate::ffi::anchor_quality::SUFFICIENT as i64;
    /// Anchor-quality level: good — a strong spot to save an anchor.
    #[constant]
    const ANCHOR_QUALITY_GOOD: i64 = crate::ffi::anchor_quality::GOOD as i64;

    /// Whether the connected glasses support spatial anchors. Gated by
    /// [`Self::is_ar_perception_available`] (6DoF + no RGB camera = the Air 2 Ultra). `false` until the
    /// session is up.
    #[func]
    fn is_anchor_available(&self) -> bool {
        self.is_ar_perception_available()
    }

    /// Enable/disable the anchor subsystem (call once before acquiring). Returns whether the export
    /// was present. Needs a live 6DoF session for anchors to actually track.
    #[func]
    fn set_anchor_enabled(&self, enabled: bool) -> bool {
        session::shared()
            .map(|s| s.set_anchor_enabled(enabled))
            .unwrap_or(false)
    }

    /// Point the anchor subsystem at a writable directory for its saved-anchor map files (e.g.
    /// `OS.get_user_data_dir()`). Call before saving/loading.
    #[func]
    fn set_anchor_mapping_dir(&self, dir: GString) -> bool {
        session::shared()
            .map(|s| s.set_anchor_mapping_dir(&dir.to_string()))
            .unwrap_or(false)
    }

    /// Create a new anchor at `pose` (a world `Transform3D`). Returns the anchor `Dictionary`
    /// (`{ id, transform, tracking_state, session_id }`) or an empty dict on failure.
    #[func]
    fn acquire_anchor(&self, pose: Transform3D) -> VarDictionary {
        let up = transform_to_unity_pose(&pose);
        session::shared()
            .and_then(|s| s.acquire_anchor(up))
            .map(|a| anchor_to_dict(&a))
            .unwrap_or_default()
    }

    /// Poll anchor changes since the last call: `{ "added": Array, "updated": Array, "removed": Array }`
    /// (each added/updated entry a `Dictionary { id, transform, tracking_state, session_id }`; `removed`
    /// an array of id strings). Call once per frame to drive the SDK's change queue.
    #[func]
    fn poll_anchors(&self) -> VarDictionary {
        let mut added = VarArray::new();
        let mut updated = VarArray::new();
        let mut removed = VarArray::new();
        if let Some(ch) = session::shared().and_then(|s| s.poll_anchor_changes()) {
            for a in &ch.added {
                added.push(&anchor_to_dict(a).to_variant());
            }
            for a in &ch.updated {
                updated.push(&anchor_to_dict(a).to_variant());
            }
            for id in &ch.removed {
                removed.push(&trackable_id_to_gstring(*id).to_variant());
            }
        }
        let mut d = VarDictionary::new();
        d.set(&"added".to_variant(), &added.to_variant());
        d.set(&"updated".to_variant(), &updated.to_variant());
        d.set(&"removed".to_variant(), &removed.to_variant());
        d
    }

    /// Persist an anchor (by its id from `poll_anchors`) and return its `Guid` key as a 32-hex string
    /// (empty on failure). Estimate quality ≥ SUFFICIENT first; enumerate saved keys yourself.
    #[func]
    fn save_anchor(&self, id: GString) -> GString {
        let tid = gstring_to_trackable_id(&id);
        session::shared()
            .and_then(|s| s.save_anchor(tid))
            .map(guid_to_gstring)
            .unwrap_or_default()
    }

    /// Restore a saved anchor by its `Guid` string. Returns the anchor `Dictionary` or an empty dict.
    #[func]
    fn load_anchor(&self, guid: GString) -> VarDictionary {
        let g = gstring_to_guid(&guid);
        session::shared()
            .and_then(|s| s.load_anchor(g))
            .map(|a| anchor_to_dict(&a))
            .unwrap_or_default()
    }

    /// Drop a tracked anchor by its id. Returns the SDK bool (or `false` when unavailable).
    #[func]
    fn remove_anchor(&self, id: GString) -> bool {
        let tid = gstring_to_trackable_id(&id);
        session::shared()
            .map(|s| s.remove_anchor(tid))
            .unwrap_or(false)
    }

    /// Re-localize an anchor into the current map. Returns the SDK bool (or `false` when unavailable).
    #[func]
    fn remap_anchor(&self, id: GString) -> bool {
        let tid = gstring_to_trackable_id(&id);
        session::shared()
            .map(|s| s.remap_anchor(tid))
            .unwrap_or(false)
    }

    /// Estimate an anchor's save quality (`ANCHOR_QUALITY_*`) at `pose`, or `-1` on failure.
    #[func]
    fn estimate_anchor_quality(&self, id: GString, pose: Transform3D) -> i64 {
        let tid = gstring_to_trackable_id(&id);
        let up = transform_to_unity_pose(&pose);
        session::shared()
            .and_then(|s| s.estimate_anchor_quality(tid, up))
            .map(i64::from)
            .unwrap_or(-1)
    }

    // --- Image tracking (see docs/plans/ar-features-plan.md). Needs a live 6DoF session + the
    //     vendored nr_image_tracking.aar backend + assets/nr_plugins.json + a DB blob. ---

    /// Whether the connected glasses support image tracking. Gated by
    /// [`Self::is_ar_perception_available`] (6DoF + no RGB camera = the Air 2 Ultra). `false` until the
    /// session is up.
    #[func]
    fn is_image_tracking_available(&self) -> bool {
        self.is_ar_perception_available()
    }

    /// Build a tracking database from a reference-image DB `blob` (produced by `trackableImageTools`)
    /// plus, per image, its baked `image_guids` (32-hex, aligned with `image_sizes`) and physical
    /// `image_sizes` (metres). Returns the DB handle (`0` on failure). Pass it to
    /// [`Self::set_image_database`] to activate, and keep it for [`Self::release_image_database`].
    #[func]
    fn init_image_database(
        &self,
        blob: PackedByteArray,
        image_guids: PackedStringArray,
        image_sizes: PackedVector2Array,
    ) -> i64 {
        let refs = build_managed_refs(&image_guids, &image_sizes);
        session::shared()
            .and_then(|s| s.init_image_database(blob.as_slice(), &refs))
            .map(|h| h as i64)
            .unwrap_or(0)
    }

    /// Activate a database from [`Self::init_image_database`] (pass `0` to disable image tracking).
    #[func]
    fn set_image_database(&self, handle: i64) {
        if let Some(s) = session::shared() {
            s.set_image_database(handle as u64);
        }
    }

    /// Number of reference images in a database, or `0` when unavailable.
    #[func]
    fn image_reference_count(&self, handle: i64) -> i64 {
        session::shared()
            .map(|s| s.image_reference_count(handle as u64) as i64)
            .unwrap_or(0)
    }

    /// Free a database from [`Self::init_image_database`].
    #[func]
    fn release_image_database(&self, handle: i64) {
        if let Some(s) = session::shared() {
            s.release_image_database(handle as u64);
        }
    }

    /// Poll tracked-image changes since the last call: `{ "added": Array, "updated": Array,
    /// "removed": Array }` (each added/updated entry a `Dictionary { id, source_image, transform,
    /// size, tracking_state }`; `removed` an array of id strings). Call once per frame.
    #[func]
    fn poll_images(&self) -> VarDictionary {
        let mut added = VarArray::new();
        let mut updated = VarArray::new();
        let mut removed = VarArray::new();
        if let Some(ch) = session::shared().and_then(|s| s.poll_image_changes()) {
            for im in &ch.added {
                added.push(&image_to_dict(im).to_variant());
            }
            for im in &ch.updated {
                updated.push(&image_to_dict(im).to_variant());
            }
            for id in &ch.removed {
                removed.push(&trackable_id_to_gstring(*id).to_variant());
            }
        }
        let mut d = VarDictionary::new();
        d.set(&"added".to_variant(), &added.to_variant());
        d.set(&"updated".to_variant(), &updated.to_variant());
        d.set(&"removed".to_variant(), &removed.to_variant());
        d
    }

    // --- Depth mesh (see docs/plans/ar-features-plan.md §4). Internal libXREALXRPlugin.so functions by
    //     LIB_BASE+offset (like hand tracking), NOT flat exports. Air 2 Ultra only. ---

    /// Whether the connected glasses support depth meshing. Gated by
    /// [`Self::is_ar_perception_available`] (6DoF + no RGB camera = the Air 2 Ultra). `false` until the
    /// session is up.
    #[func]
    fn is_meshing_supported(&self) -> bool {
        self.is_ar_perception_available()
    }

    /// Enable/disable depth meshing (`NativePerception::SetMeshingEnabled`). Returns whether the call
    /// reached the SDK (perception up); poll `poll_mesh_blocks` after enabling.
    #[func]
    fn set_meshing_enabled(&self, on: bool) -> bool {
        crate::depth_mesh::set_meshing_enabled(on)
    }

    /// Poll the current mesh blocks. Returns an `Array` of `Dictionary { id: String, state: int,
    /// vertices: PackedVector3Array, normals: PackedVector3Array, indices: PackedInt32Array }`.
    /// `state == 2` means the block was removed; build/update an `ArrayMesh` per block otherwise.
    #[func]
    fn poll_mesh_blocks(&self) -> VarArray {
        let mut arr = VarArray::new();
        for b in crate::depth_mesh::poll_mesh_blocks() {
            arr.push(&mesh_block_to_dict(&b).to_variant());
        }
        arr
    }

    // --- First-person-view streaming (libmedia_codec HW encoder; see docs/plans/fpv-streaming-plan.md).
    //     Streams a rendered view (any device) as H.264 to a local mp4 / RTMP / RTP URL. ---

    /// Start streaming the FPV to `output` — an `rtp://ip:port` / `rtmp://…` URL or a local file path
    /// (the URL scheme picks local/RTMP/RTP). Returns whether the encoder started. Then feed the view's
    /// GL texture each frame via [`Self::stream_push_frame`] from a `RenderingServer.call_on_render_thread`.
    #[func]
    #[allow(clippy::too_many_arguments)] // GDScript-facing signature (resolution + rate + audio flags)
    fn stream_start(
        &self,
        output: GString,
        width: i64,
        height: i64,
        bitrate: i64,
        fps: i64,
        with_mic: bool,
        with_internal_audio: bool,
        with_alpha: bool,
    ) -> bool {
        // The encoder writes the track at the *config* rate and does not resample what we push, so
        // when we are the audio source the two must agree — see video_encoder::AUDIO_SAMPLE_RATE for
        // the measurement. Godot's mixer is 44100 by default; the SDK's 48000 constant only works for
        // Unity because Android runs its mixer at 48000 as well.
        let app_audio_rate = with_internal_audio
            .then(|| godot::classes::AudioServer::singleton().get_mix_rate() as i32);
        crate::video_encoder::start(
            &output.to_string(),
            width as i32,
            height as i32,
            bitrate as i32,
            fps as i32,
            with_mic,
            with_internal_audio,
            with_alpha,
            app_audio_rate,
        )
    }

    /// Feed one frame to the running stream: `gl_texture_id` from `RenderingServer.texture_get_native_handle`
    /// on the view's texture, `timestamp_ns` in nanoseconds. **Call inside a
    /// `RenderingServer.call_on_render_thread` callback** (the encoder reads the GL texture on the render
    /// thread's EGL context). Returns the encoder status (`0` ok, `-1` if not streaming).
    #[func]
    fn stream_push_frame(&self, gl_texture_id: i64, timestamp_ns: i64) -> i64 {
        crate::video_encoder::submit_frame(gl_texture_id as usize, timestamp_ns as u64) as i64
    }

    /// Stop + tear down the FPV stream.
    #[func]
    fn stream_stop(&self) {
        crate::video_encoder::stop();
    }

    /// Whether an FPV stream is currently active.
    #[func]
    fn is_stream_active(&self) -> bool {
        crate::video_encoder::is_active()
    }

    /// Select the stereo rendering mode applied when the native session **bootstraps** (a startup
    /// selector): `0` = Multipass (both eyes — the default shipping path), `2` = Multiview
    /// (single-pass-instanced). **Call before the session starts** (e.g. an autoload `_ready`, before
    /// the XR rig enters the tree) — it is read once at `InitUserDefinedSettings`. Equivalent to the
    /// ProjectSetting `xreal/stereo_mode` or `adb shell setprop debug.xreal.stereo_mode <n>`. Multiview
    /// buys nothing on this two-SubViewport rig (see docs/archive/multiview-investigation.md), so
    /// Multipass stays the recommended default.
    #[func]
    fn set_stereo_mode(&self, mode: i64) {
        session::set_stereo_mode_override(mode as i32);
    }

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

    /// Which input sources `InitUserDefinedSettings` asks the SDK for: `1` = Controller (default),
    /// `2` = Hands, `3` = ControllerAndHands. Must be called **before** the XR rig starts the session
    /// — it is read once at bootstrap. Also settable with
    /// `adb shell setprop debug.xreal.input_source <n>`.
    ///
    /// **Only ask for Hands if you actually use hand tracking.** The Hands bit makes the SDK call
    /// `NativePerception::SetHandTrackingEnabled(true)` synchronously during input start, measured at
    /// **~878 ms of cold start** on an X4000 + One Pro — and hand tracking is Air 2 Ultra only, so on
    /// any other headset that is pure latency. `addons/godot_xreal/features/xreal_hands.tscn` sets
    /// this to `3` from its `_ready()`, so scenes that include the hands feature get it automatically
    /// and everyone else starts faster. See `docs/archive/codex-input-start-analysis.md`.
    #[func]
    fn set_input_source(&self, source: i64) {
        session::set_input_source_override(source as i32);
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
    Transform3D::new(
        Basis::from_quaternion(quat),
        Vector3::new(p[0], -p[1], -p[2]),
    )
}

/// Format two u64s as a stable 32-hex-char string — the wire form shared by `TrackableId` and `Guid`.
fn hex_pair(a: u64, b: u64) -> String {
    format!("{a:016x}{b:016x}")
}

/// Parse a 32-hex-char string back into two u64s; a missing/invalid half falls back to 0.
fn parse_hex_pair(s: &str) -> (u64, u64) {
    let half = |r: std::ops::Range<usize>| -> u64 {
        s.get(r)
            .and_then(|h| u64::from_str_radix(h, 16).ok())
            .unwrap_or(0)
    };
    (half(0..16), half(16..32))
}

/// 128-bit `TrackableId` → a stable 32-hex-char string (round-trips via [`gstring_to_trackable_id`]).
fn trackable_id_to_gstring(id: crate::ffi::TrackableId) -> GString {
    GString::from(hex_pair(id.sub_id_1, id.sub_id_2).as_str())
}

/// Parse a 32-hex-char id string (from `poll_planes`) back into a `TrackableId`.
fn gstring_to_trackable_id(s: &GString) -> crate::ffi::TrackableId {
    let (sub_id_1, sub_id_2) = parse_hex_pair(&s.to_string());
    crate::ffi::TrackableId { sub_id_1, sub_id_2 }
}

/// A detected plane → a GDScript `Dictionary`.
fn plane_to_dict(p: &crate::native::PlaneSample) -> VarDictionary {
    let mut d = VarDictionary::new();
    d.set(
        &"id".to_variant(),
        &trackable_id_to_gstring(p.id).to_variant(),
    );
    d.set(
        &"transform".to_variant(),
        &unity_pose_to_transform(&p.pose).to_variant(),
    );
    d.set(
        &"center".to_variant(),
        &Vector2::new(p.center[0], p.center[1]).to_variant(),
    );
    d.set(
        &"size".to_variant(),
        &Vector2::new(p.size[0], p.size[1]).to_variant(),
    );
    d.set(
        &"alignment".to_variant(),
        &(p.alignment as i64).to_variant(),
    );
    d
}

/// Inverse of [`unity_pose_to_transform`]: a Godot world `Transform3D` → a Unity-space `UnityPose`
/// (for anchor acquire/quality input). The position/quaternion sign flips are self-inverse, so the
/// same `(x, -y, -z)` / `(x, -y, -z, w)` pattern round-trips.
fn transform_to_unity_pose(t: &Transform3D) -> crate::ffi::UnityPose {
    let p = t.origin;
    let q = t.basis.get_quaternion();
    crate::ffi::UnityPose {
        position: [p.x, -p.y, -p.z],
        rotation: [q.x, -q.y, -q.z, q.w],
    }
}

/// 128-bit anchor persistence `Guid` → a stable 32-hex-char string (round-trips via [`gstring_to_guid`]).
fn guid_to_gstring(g: crate::ffi::Guid) -> GString {
    GString::from(hex_pair(g.lo, g.hi).as_str())
}

/// Parse a 32-hex-char `Guid` string (from `save_anchor`) back into a [`crate::ffi::Guid`].
fn gstring_to_guid(s: &GString) -> crate::ffi::Guid {
    let (lo, hi) = parse_hex_pair(&s.to_string());
    crate::ffi::Guid { lo, hi }
}

/// A tracked spatial anchor → a GDScript `Dictionary`.
fn anchor_to_dict(a: &crate::native::AnchorSample) -> VarDictionary {
    let mut d = VarDictionary::new();
    d.set(
        &"id".to_variant(),
        &trackable_id_to_gstring(a.id).to_variant(),
    );
    d.set(
        &"transform".to_variant(),
        &unity_pose_to_transform(&a.pose).to_variant(),
    );
    d.set(
        &"tracking_state".to_variant(),
        &(a.tracking_state as i64).to_variant(),
    );
    d.set(
        &"session_id".to_variant(),
        &guid_to_gstring(a.session_id).to_variant(),
    );
    d
}

/// Build the `ManagedReferenceImage` metadata array from GDScript-side per-image guids + physical
/// sizes (metres). The guids must match the ones baked into the DB blob; `name`/`texture` are null.
fn build_managed_refs(
    guids: &PackedStringArray,
    sizes: &PackedVector2Array,
) -> Vec<crate::ffi::ManagedReferenceImage> {
    let g = guids.as_slice();
    let s = sizes.as_slice();
    let n = g.len().min(s.len());
    (0..n)
        .map(|i| crate::ffi::ManagedReferenceImage {
            guid: gstring_to_guid(&g[i]),
            texture_guid: crate::ffi::Guid::default(),
            size: [s[i].x, s[i].y],
            name: std::ptr::null(),
            texture: std::ptr::null(),
        })
        .collect()
}

/// A tracked reference image → a GDScript `Dictionary`.
fn image_to_dict(im: &crate::native::ImageSample) -> VarDictionary {
    let mut d = VarDictionary::new();
    d.set(
        &"id".to_variant(),
        &trackable_id_to_gstring(im.id).to_variant(),
    );
    d.set(
        &"source_image".to_variant(),
        &guid_to_gstring(im.source_image).to_variant(),
    );
    d.set(
        &"transform".to_variant(),
        &unity_pose_to_transform(&im.pose).to_variant(),
    );
    d.set(
        &"size".to_variant(),
        &Vector2::new(im.size[0], im.size[1]).to_variant(),
    );
    d.set(
        &"tracking_state".to_variant(),
        &(im.tracking_state as i64).to_variant(),
    );
    d
}

/// A depth-mesh block → a GDScript `Dictionary`.
///
/// Unlike the poses, mesh vertices come out of `MeshBlockInfo` in **raw NR space** (the SDK's
/// `AcquireMesh` is what negates Z on the way into Unity), so the flip is one step short of the pose
/// path: raw → Unity is `(x, y, -z)` and Unity → this port's Godot is `(x, -y, -z)`, which composes to
/// `(x, -y, z)`.
///
/// That leaves our space a pure 180°-about-X *rotation* of Unity's, so triangles keep Unity's
/// clockwise-front winding — the opposite of Godot's counter-clockwise-front rule. Emit each triangle
/// reversed so front faces stay front.
fn mesh_block_to_dict(b: &crate::depth_mesh::MeshBlock) -> VarDictionary {
    let mut verts = PackedVector3Array::new();
    for v in &b.vertices {
        verts.push(Vector3::new(v[0], -v[1], v[2]));
    }
    let mut norms = PackedVector3Array::new();
    for n in &b.normals {
        norms.push(Vector3::new(n[0], -n[1], n[2]));
    }
    let mut idx = PackedInt32Array::new();
    for t in b.indices.chunks_exact(3) {
        idx.push(t[0] as i32);
        idx.push(t[2] as i32);
        idx.push(t[1] as i32);
    }
    let mut d = VarDictionary::new();
    d.set(
        &"id".to_variant(),
        &GString::from(format!("{:016x}", b.id).as_str()).to_variant(),
    );
    d.set(&"state".to_variant(), &(b.state as i64).to_variant());
    d.set(&"vertices".to_variant(), &verts.to_variant());
    d.set(&"normals".to_variant(), &norms.to_variant());
    d.set(&"indices".to_variant(), &idx.to_variant());
    d
}

// --- XrealAR: a scene-placeable Node that polls the AR change streams each frame and re-emits them as
//     signals, so consumers connect in the editor instead of manually polling XrealSystem. Enable the
//     features via XrealSystem (set_plane_detection_mode / set_anchor_enabled / init_image_database /
//     set_meshing_enabled) — this node only surfaces the changes + the temperature / native-error events.
//     Drop it in the scene (a unique name helps) and connect the signals below.

/// Scene node that turns the AR change polls + glasses events into signals. See the module docs.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct XrealAR {
    base: Base<Node>,
    /// Master switch — poll each frame while `true`.
    #[export]
    active: bool,
    /// Poll plane-detection changes each frame and emit the `plane_*` signals (turn off the streams
    /// you don't use to skip their per-frame native poll).
    #[export]
    planes: bool,
    /// Poll spatial-anchor changes each frame and emit the `anchor_*` signals.
    #[export]
    anchors: bool,
    /// Poll image-tracking changes each frame and emit the `image_*` signals.
    #[export]
    images: bool,
    /// Poll depth-mesh changes each frame and emit the `mesh_block_*` signals.
    #[export]
    mesh: bool,
    /// Emit `temperature_changed` / `native_error` on change.
    #[export]
    glasses_events: bool,
    last_temperature: i64,
    last_error_code: i64,
}

#[godot_api]
impl INode for XrealAR {
    fn init(base: Base<Node>) -> Self {
        Self {
            base,
            active: true,
            planes: true,
            anchors: true,
            images: true,
            mesh: true,
            glasses_events: true,
            last_temperature: i64::MIN,
            last_error_code: i64::MIN,
        }
    }

    fn process(&mut self, _delta: f64) {
        if !self.active {
            return;
        }
        if self.planes {
            self.emit_plane_changes();
        }
        if self.anchors {
            self.emit_anchor_changes();
        }
        if self.images {
            self.emit_image_changes();
        }
        if self.mesh {
            self.emit_mesh_changes();
        }
        if self.glasses_events {
            self.emit_glasses_events();
        }
    }
}

#[godot_api]
impl XrealAR {
    /// A detected plane was added — `Dictionary { id, transform, center, size, alignment }`.
    #[signal]
    fn plane_added(plane: VarDictionary);
    /// A detected plane was updated (same `Dictionary` shape as `plane_added`).
    #[signal]
    fn plane_updated(plane: VarDictionary);
    /// A detected plane was removed (its id string).
    #[signal]
    fn plane_removed(id: GString);

    /// A tracked anchor was added — `Dictionary { id, transform, tracking_state, session_id }`.
    #[signal]
    fn anchor_added(anchor: VarDictionary);
    /// A tracked anchor was updated (same `Dictionary` shape as `anchor_added`).
    #[signal]
    fn anchor_updated(anchor: VarDictionary);
    /// A tracked anchor was removed (its id string).
    #[signal]
    fn anchor_removed(id: GString);

    /// A tracked image was added — `Dictionary { id, source_image, transform, size, tracking_state }`.
    #[signal]
    fn image_added(image: VarDictionary);
    /// A tracked image was updated (same `Dictionary` shape as `image_added`).
    #[signal]
    fn image_updated(image: VarDictionary);
    /// A tracked image was removed (its id string).
    #[signal]
    fn image_removed(id: GString);

    /// A mesh block was added / updated — `Dictionary { id, state, vertices, normals, indices }`.
    #[signal]
    fn mesh_block_changed(block: VarDictionary);
    /// A mesh block was removed (its id string).
    #[signal]
    fn mesh_block_removed(id: GString);

    /// The glasses temperature level changed (`0` NORMAL / `1` WARM / `2` HOT).
    #[signal]
    fn temperature_changed(level: i64);
    /// A native async error arrived (`XREALErrorCode` + its message).
    #[signal]
    fn native_error(code: i64, message: GString);

    fn emit_plane_changes(&mut self) {
        let Some(ch) = session::shared().and_then(|s| s.poll_plane_changes()) else {
            return;
        };
        for p in &ch.added {
            self.signals().plane_added().emit(&plane_to_dict(p));
        }
        for p in &ch.updated {
            self.signals().plane_updated().emit(&plane_to_dict(p));
        }
        for id in &ch.removed {
            self.signals()
                .plane_removed()
                .emit(&trackable_id_to_gstring(*id));
        }
    }

    fn emit_anchor_changes(&mut self) {
        let Some(ch) = session::shared().and_then(|s| s.poll_anchor_changes()) else {
            return;
        };
        for a in &ch.added {
            self.signals().anchor_added().emit(&anchor_to_dict(a));
        }
        for a in &ch.updated {
            self.signals().anchor_updated().emit(&anchor_to_dict(a));
        }
        for id in &ch.removed {
            self.signals()
                .anchor_removed()
                .emit(&trackable_id_to_gstring(*id));
        }
    }

    fn emit_image_changes(&mut self) {
        let Some(ch) = session::shared().and_then(|s| s.poll_image_changes()) else {
            return;
        };
        for im in &ch.added {
            self.signals().image_added().emit(&image_to_dict(im));
        }
        for im in &ch.updated {
            self.signals().image_updated().emit(&image_to_dict(im));
        }
        for id in &ch.removed {
            self.signals()
                .image_removed()
                .emit(&trackable_id_to_gstring(*id));
        }
    }

    fn emit_mesh_changes(&mut self) {
        for b in crate::depth_mesh::poll_mesh_blocks() {
            if b.state == 2 {
                self.signals()
                    .mesh_block_removed()
                    .emit(&GString::from(format!("{:016x}", b.id).as_str()));
            } else {
                self.signals()
                    .mesh_block_changed()
                    .emit(&mesh_block_to_dict(&b));
            }
        }
    }

    fn emit_glasses_events(&mut self) {
        let temp = crate::glasses_events::temperature_level() as i64;
        if temp != self.last_temperature {
            self.last_temperature = temp;
            self.signals().temperature_changed().emit(temp);
        }
        let code = crate::native_error::last_error_code() as i64;
        if code != self.last_error_code {
            self.last_error_code = code;
            let msg = crate::native_error::last_error_message();
            self.signals()
                .native_error()
                .emit(code, &GString::from(msg.as_str()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{hex_pair, parse_hex_pair, transform_to_unity_pose, unity_pose_to_transform};
    use crate::ffi::UnityPose;
    use godot::builtin::{Quaternion, Vector3};

    #[test]
    fn hex_pair_round_trips_both_halves() {
        let (a, b) = (0x0123_4567_89ab_cdef_u64, 0xfedc_ba98_7654_3210_u64);
        let s = hex_pair(a, b);
        assert_eq!(s, "0123456789abcdeffedcba9876543210");
        assert_eq!(s.len(), 32);
        assert_eq!(parse_hex_pair(&s), (a, b));
    }

    #[test]
    fn hex_pair_zero_pads_each_half_first_then_second() {
        // Each u64 is 16 zero-padded hex chars; first half is `a`, second is `b` (order matters).
        assert_eq!(hex_pair(1, 2), "00000000000000010000000000000002");
        assert_eq!(parse_hex_pair("00000000000000010000000000000002"), (1, 2));
    }

    #[test]
    fn parse_hex_pair_tolerates_short_or_garbage_input() {
        assert_eq!(parse_hex_pair(""), (0, 0));
        assert_eq!(parse_hex_pair("zzzz"), (0, 0));
        // valid first half, missing second → second is 0 (guards the OOB slice).
        assert_eq!(parse_hex_pair("0000000000000005"), (5, 0));
    }

    #[test]
    fn unity_pose_to_transform_flips_y_and_z_position() {
        let pose = UnityPose {
            position: [1.0, 2.0, 3.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
        };
        let t = unity_pose_to_transform(&pose);
        assert_eq!(t.origin, Vector3::new(1.0, -2.0, -3.0));
    }

    #[test]
    fn unity_pose_transform_round_trips() {
        // The (x, -y, -z) position and (x, -y, -z, w) quaternion flips are self-inverse.
        let q = Quaternion::new(0.1, 0.2, 0.3, 0.9).normalized();
        let pose = UnityPose {
            position: [1.0, -2.0, 3.0],
            rotation: [q.x, q.y, q.z, q.w],
        };
        let back = transform_to_unity_pose(&unity_pose_to_transform(&pose));
        for i in 0..3 {
            assert!(
                (back.position[i] - pose.position[i]).abs() < 1e-5,
                "position[{i}]"
            );
        }
        // Quaternion may return negated (double cover) but must represent the same rotation.
        let dot: f32 = (0..4).map(|i| back.rotation[i] * pose.rotation[i]).sum();
        assert!(dot.abs() > 0.999, "rotation round-trip dot={dot}");
    }
}
