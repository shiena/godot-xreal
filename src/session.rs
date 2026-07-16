//! Safe lifecycle wrapper over [`crate::native::XrealNative`].
//!
//! Keeps the Godot nodes free of `unsafe`, owns the session bootstrap, and centralises
//! the coordinate-system conversion.
//!
//! The XREAL native **SessionManager is a process-global singleton**. Querying it
//! (`IsSessionStarted`, head-pose) *before* it has been constructed dereferences a null
//! `this` and segfaults. So this module exposes a single, lazily-created [`shared`]
//! session and every node goes through it; the singleton is built once, in the correct
//! order, before any query.
//!
//! Bootstrap also needs the Android `Activity` (the Unity SDK's `unityActivity`), which a
//! companion publishes into `ndk_context` from the Java side (see `src/jni_bridge.rs` and
//! `XrealBridge.java`). That can arrive slightly after the first node `ready()`, so
//! [`shared`] is **retry-friendly**: while the Activity is missing it returns `None`
//! quietly and tries again next frame; a missing-library failure is terminal and logged
//! once (the desktop/editor case).
//!
//! Bootstrap order (mirrors the Unity loader / RE of the native libs):
//!   1. `InitUserDefinedSettings(settings incl. Activity)` → `CreateSession(directPresent)`.
//!   2. `XREALLoadAPI()` — construct/wire the session-manager perception singleton.
//!      REQUIRED before any pose / `IsSessionStarted` call.

use std::sync::{
    atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering},
    Mutex, OnceLock,
};

use godot::builtin::Quaternion;

use crate::ffi::{NrPose, TrackingType, UserDefinedSettings};
use crate::native::XrealNative;

/// Explicit head-tracking-mode override set from GDScript (`XrealSystem.set_tracking_type`).
/// `-1` = unset (fall through to system property / default). Must be set **before** the session
/// bootstraps (e.g. in an autoload `_ready`, before the XR rig enters the tree).
static TRACKING_MODE_OVERRIDE: AtomicI32 = AtomicI32::new(-1);

/// Set the tracking-mode override from GDScript. See [`tracking_mode`].
pub fn set_tracking_mode_override(mode: i32) {
    TRACKING_MODE_OVERRIDE.store(mode, Ordering::Relaxed);
}

/// The current tracking-mode override (`-1` if unset).
pub fn tracking_mode_override() -> i32 {
    TRACKING_MODE_OVERRIDE.load(Ordering::Relaxed)
}

/// Read a NUL-terminated Android system property as `i32` (`None` off-Android or if unset/unparseable).
fn android_prop_i32(key: &[u8]) -> Option<i32> {
    #[cfg(target_os = "android")]
    {
        let mut buf = [0u8; 92]; // PROP_VALUE_MAX
        let n = unsafe {
            libc::__system_property_get(
                key.as_ptr() as *const libc::c_char,
                buf.as_mut_ptr() as *mut libc::c_char,
            )
        };
        if n > 0 {
            if let Ok(s) = std::str::from_utf8(&buf[..n as usize]) {
                if let Ok(v) = s.trim().parse::<i32>() {
                    return Some(v);
                }
            }
        }
    }
    #[cfg(not(target_os = "android"))]
    let _ = key;
    None
}

/// Stereo rendering mode for `InitUserDefinedSettings`. **The port is Multipass-only** (`0`) — the
/// complete path (both eyes + camera + tracking). Multiview (`2`, single-pass-instanced) is **shelved
/// and no longer reachable** (the `debug.xreal.force_multiview` escape was removed).
///
/// Full investigation: `docs/archive/multiview-investigation.md`. Summary of why it's not worth it:
/// (1) the right eye actually renders *correctly* — the earlier "gray right eye" was an alpha-channel
/// measurement artifact, confirmed on-device; (2) our two-SubViewport rig draws both eyes every frame
/// regardless, so single-pass-instanced buys **zero** GPU savings here; (3) the shared
/// `SubmitCurrentFrame → UpdateMetrics` path hits a cross-thread SIGBUS (see the doc) that is not
/// Multiview-specific. So enabling Multiview gains nothing.
fn stereo_rendering_mode() -> i32 {
    // Multipass is the only supported/working stereo mode. Multiview's right eye is blocked inside
    // libnr_api (it imports the client swapchain GL name as GL_TEXTURE_2D, never GL_TEXTURE_2D_ARRAY,
    // so layer 1 samples a cleared resource) and it gives no perf benefit for our two-SubViewport rig.
    // The 2026-07-16 re-attempt confirmed the UpdateMetrics fix lets forced Multiview run *stably*, but
    // the right eye stays black and the stubbed Unity callbacks are not the cause. See
    // docs/archive/multiview-investigation.md and docs/archive/codex-stub-callbacks-analysis.md.
    // To re-exercise Multiview for future libnr_api RE, return 2 here (add a debug prop if desired).
    0
}

/// Head-tracking mode for `InitUserDefinedSettings`, resolved **once at session bootstrap** from, in
/// priority order:
///   1. the GDScript override (`XrealSystem.set_tracking_type`, also how the ProjectSetting
///      `xreal/tracking_type` is applied — read it in GDScript and pass it to the override before the
///      XR rig starts, see `demo/main.gd`),
///   2. the Android system property `debug.xreal.tracking_type`
///      (`adb shell setprop debug.xreal.tracking_type 1`),
///   3. the default.
///
/// `0` = MODE_6DOF (SLAM position + orientation — the DISP pose the eye cameras use carries full
/// orientation incl. roll and has no drift; the recommended mode), `1` = MODE_3DOF (pure IMU, no
/// position), `2` = MODE_0DOF. Defaults to MODE_6DOF. NOTE: the eye-camera rotation comes from the
/// XR-plugin DISP pose (`node.rs`), not the compact session-manager `NrPose`, which is
/// horizon-stabilized in every mode (`docs/archive/roll-tracking-investigation.md`).
fn tracking_mode() -> i32 {
    const DEFAULT: i32 = TrackingType::Mode6Dof as i32;

    // 1) Explicit override (GDScript API — also carries a ProjectSetting value).
    let ovr = TRACKING_MODE_OVERRIDE.load(Ordering::Relaxed);
    if ovr >= 0 {
        return ovr;
    }
    // 2) Android system property, else the default.
    android_prop_i32(b"debug.xreal.tracking_type\0").unwrap_or(DEFAULT)
}

/// The created session (success latch). `XrealNative` is `Send + Sync` (the `libloading`
/// handles and the resolved `extern "C"` function pointers all are), so it lives in a
/// `static`.
static SESSION: OnceLock<XrealSession> = OnceLock::new();
/// Terminal-failure latch (e.g. libraries absent on desktop). Stops further attempts and
/// the warning is printed only once.
static DISABLED: OnceLock<String> = OnceLock::new();
/// Log the retryable CreateSession wait only once; it can happen every frame while the
/// glasses display / NR service is still not ready.
static WAITING_FOR_SESSION_READY_LOGGED: AtomicBool = AtomicBool::new(false);
/// `UnityPluginLoad` populates process-global Unity interface pointers inside
/// libXREALXRPlugin.so, so calling it once per process is enough.
static UNITY_PLUGIN_LOAD_DONE: AtomicBool = AtomicBool::new(false);
/// Master switch for glasses hardware-event delivery. **Off**: `SetGlassesEventCallback`'s
/// first call runs an InputManager singleton init that clobbers the `lib_base + 0xdb400`
/// DisplayManager frame descriptor (writes `1.0f`/`0x3f800000`), and the SDK render thread then
/// SIGSEGVs calling `0x3f800000` on the first frame. Ordering does not help — our display path
/// never repopulates the clobbered offsets. Flip to `true` only once that conflict is resolved
/// (see the registration site in `try_start`); the rest of the input plumbing stays wired.
const ENABLE_GLASSES_EVENT_CALLBACK: bool = true;
/// Ensures the one-shot glasses-event registration runs at most once per process (see
/// [`ENABLE_GLASSES_EVENT_CALLBACK`]).
static GLASSES_EVENT_CALLBACK_REGISTERED: AtomicBool = AtomicBool::new(false);

/// Set once the native error callback (`SetNativeErrorCallback`) is registered.
static NATIVE_ERROR_CALLBACK_REGISTERED: AtomicBool = AtomicBool::new(false);
/// Retry runtime-dependent bootstrap at a modest cadence while the glasses / NR runtime
/// are unavailable. This avoids spamming UnityPluginLoad/provider registration every frame.
static SHARED_CALLS: AtomicU64 = AtomicU64::new(0);
static NEXT_RUNTIME_RETRY_CALL: AtomicU64 = AtomicU64::new(0);

/// Initialize (once) and return the process-global XREAL session, or `None` when it is
/// not (yet) available.
///
/// Safe to call every frame from any node: once created the session is cached; while the
/// Android Activity has not been published it returns `None` and retries on the next
/// call; on a terminal failure (no native libraries) it latches and stops trying.
pub fn shared() -> Option<&'static XrealSession> {
    if let Some(session) = SESSION.get() {
        return Some(session);
    }
    if DISABLED.get().is_some() {
        return None;
    }

    let call = SHARED_CALLS.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
    if call < NEXT_RUNTIME_RETRY_CALL.load(Ordering::SeqCst) {
        return None;
    }

    match XrealSession::try_start() {
        TryStart::Ready(session) => {
            NEXT_RUNTIME_RETRY_CALL.store(0, Ordering::SeqCst);
            let _ = SESSION.set(*session);
            SESSION.get()
        }
        // Activity or the XREAL session target is not ready yet; retry soon.
        TryStart::WaitingForRuntime => {
            NEXT_RUNTIME_RETRY_CALL.store(call.wrapping_add(60), Ordering::SeqCst);
            None
        }
        TryStart::Disabled(reason) => {
            godot::global::godot_warn!(
                "[xreal] head tracking disabled: {reason} (expected on desktop/editor)"
            );
            let _ = DISABLED.set(reason);
            None
        }
    }
}

/// Outcome of a single bootstrap attempt.
enum TryStart {
    Ready(Box<XrealSession>),
    /// Native libraries are present but Android/XREAL runtime state is not ready yet.
    WaitingForRuntime,
    /// Terminal: do not retry (libraries missing, or a bootstrap call failed).
    Disabled(String),
}

pub struct XrealSession {
    native: Mutex<XrealNative>,
}

impl XrealSession {
    /// One bootstrap attempt: load the libraries, then (once the Activity is available)
    /// create the session and wire the perception API.
    fn try_start() -> TryStart {
        let mut native = match XrealNative::load() {
            Ok(native) => native,
            Err(err) => return TryStart::Disabled(format!("native libraries unavailable: {err}")),
        };

        // Needs the host Activity (published into ndk_context from the Java side). Until
        // then there is nothing to create a session with — retry next frame.
        let Some(activity) = crate::jni_bridge::activity_ptr() else {
            return TryStart::WaitingForRuntime;
        };

        // libXREALXRPlugin.so is a Unity native plugin: hand it our fake IUnityInterfaces
        // (reporting OpenGL ES 3) the way Unity's engine would via UnityPluginLoad, BEFORE
        // InitUserDefinedSettings — otherwise its DisplayManager::LoadDisplay dereferences a
        // null interface pointer and segfaults. See crate::unity_plugin.
        if !UNITY_PLUGIN_LOAD_DONE.load(Ordering::SeqCst) {
            let loaded = native.unity_plugin_load(crate::unity_plugin::interfaces_ptr());
            if loaded {
                UNITY_PLUGIN_LOAD_DONE.store(true, Ordering::SeqCst);
            }
        }

        // Route glasses hardware events (keys, wear sensor, brightness/volume/EC…) into the
        // process-wide queue; XrealHeadTracker::process() drains it on the main thread.
        //
        // Gated off (ENABLE_GLASSES_EVENT_CALLBACK) while a startup crash is investigated.
        if ENABLE_GLASSES_EVENT_CALLBACK
            && !GLASSES_EVENT_CALLBACK_REGISTERED.load(Ordering::SeqCst)
            && native.set_glasses_event_callback(crate::glasses_events::on_glasses_event)
        {
            GLASSES_EVENT_CALLBACK_REGISTERED.store(true, Ordering::SeqCst);
            godot::global::godot_print!("[xreal] glasses event callback registered");
        }

        // Cache the plugin's asynchronous native errors (XREALErrorCode) for polling via
        // XrealSystem.get_last_native_error_code/message. Same funnel shape as above.
        if !NATIVE_ERROR_CALLBACK_REGISTERED.load(Ordering::SeqCst)
            && native.set_native_error_callback(crate::native_error::on_native_error)
        {
            NATIVE_ERROR_CALLBACK_REGISTERED.store(true, Ordering::SeqCst);
            godot::global::godot_print!("[xreal] native error callback registered");
        }

        // Color space: Unity ColorSpace.Linear == 1; stereo/input default to 0.
        //
        // XREAL One/One Pro starts perception through the 6DoF path in the Unity
        // reference app even when the MVP only consumes head rotation. Keep this
        // aligned with Unity's InitUserDefinedSettings log before falling back to
        // narrower tracking modes.
        let stereo_mode = stereo_rendering_mode();
        let tracking_mode = tracking_mode();
        godot::global::godot_print!(
            "[xreal] stereo_rendering_mode = {stereo_mode} (0=Multipass, 2=Multiview), \
             tracking_type = {tracking_mode} (0=6DoF, 1=3DoF, 2=0DoF)"
        );
        let settings = UserDefinedSettings {
            color_space: 1,
            // Selectable at startup (see stereo_rendering_mode()): 0 = Multipass (per-eye 2D
            // textures; renders but the layer is world-anchored), 2 = Multiview / Single-Pass-
            // Instanced (one 2-layer array texture, matches the reference app's StereoRendering: 2 —
            // the reference app's head-locked peek window; still WIP: NR swapchain registration).
            stereo_rendering_mode: stereo_mode,
            tracking_type: tracking_mode,
            support_mono_mode: 0,
            unity_activity: activity,
            input_source: 0,
        };
        if !native.init_user_defined_settings(settings) {
            return TryStart::Disabled(
                "InitUserDefinedSettings unavailable (libXREALXRPlugin.so?)".into(),
            );
        }
        if !native.create_session(false) {
            if !WAITING_FOR_SESSION_READY_LOGGED.swap(true, Ordering::SeqCst) {
                godot::global::godot_warn!(
                    "[xreal] CreateSession returned false; waiting for XREAL display/session readiness"
                );
            }
            return TryStart::WaitingForRuntime;
        }

        // Unity owns this through XR SDK provider callbacks. Our fake Unity interface stores
        // the callbacks during InitUserDefinedSettings; run initialize/start after
        // CreateSession so XREAL can construct NativeHMD / NativePerception.
        crate::unity_plugin::start_registered_providers();

        // GfxThreadStart is deferred to the rendering thread (see unity_plugin::run_render_thread_tick).
        // CreateSwapchainEx allocates GL textures and requires an active EGL context; the main
        // thread has none. The first call to run_render_thread_tick() from node::process() via
        // RenderingServer::call_on_render_thread will invoke GfxThreadStart on the rendering
        // thread (EGL context active), which then triggers SetSwapChainBuffers + AcquireFrame.

        // Constructs/wires the session-manager perception singleton. REQUIRED before any
        // pose or IsSessionStarted call — those dereference what this sets up.
        native.load_api();

        // A freshly created session is PAUSED: the server keeps it in pauseCount until the
        // client resumes (Unity calls this on app resume). Without it IsSessionStarted stays
        // false and no head pose is delivered.
        native.resume_session();

        // SwitchTrackingType removed: it triggers action callbacks from the XREAL Nebula service
        // (via libnr_api.so) that race with NativeGlasses construction and cause SIGSEGV at
        // NativeGlasses::GetActionData+8 (null member deref) before the input subsystem is ready.
        // Head tracking works without this call once the 6DoF session starts via CreateSession.
        let initial_tracking_type = tracking_mode;

        // NOTE: display_manager_submit_frame_probe() was removed.
        //
        // Calling PopulateNextFrameDesc with (lib_base + 0xdb400) sets 0xdb410 = 0xa6,
        // which switches the XREAL SDK rendering thread's SubmitCurrentFrame path from
        // "SetBufferViewport + NativeRendering::SubmitFrame" (normal) to
        // "NativeRendering::DestroyFrame" (cleanup/embedded-data mode). The DestroyFrame
        // call then crashes because DisplayManager+0x120 holds a live SDK-managed frame
        // handle (0xb9a40998bac55c8a, MTE-tagged) — the same SIGABRT we saw before.
        //
        // The SDK's own rendering thread (GLThread) is already submitting frames via
        // SubmitCurrentFrame with 0xdb410 == 0 (the SetBufferViewport+SubmitFrame path),
        // which is what makes the Android home screen appear on the XREAL display.
        // We must NOT interfere with that by setting 0xdb410.
        //
        // To get Godot content on the display: register a Godot texture as an overlay
        // or swapchain that SetBufferViewport picks up. This requires CreateDisplayLayer
        // or a similar compositor registration, not a PopulateNextFrameDesc call.
        let display_submit_result = "deferred to rendering thread (run_render_thread_tick)";

        let (gfx_start_registered, gfx_submit_registered, gfx_populate_registered) =
            crate::unity_plugin::display_gfx_callback_status();
        godot::global::godot_print!(
            "[xreal] native session created (tracking_type_request={initial_tracking_type}); \
             session_started={}, tracking_type={:?}, \
             tracking_state={:?}, tracking_reason={:?}, \
             display_submit={display_submit_result:?}, gfx_callbacks=({}, {}, {})",
            native.is_session_started(),
            native.tracking_type(),
            native.tracking_state(),
            native.tracking_reason(),
            gfx_start_registered,
            gfx_submit_registered,
            gfx_populate_registered
        );
        TryStart::Ready(Box::new(Self {
            native: Mutex::new(native),
        }))
    }

    /// Whether the native session reports it has started. Safe: only reachable via
    /// [`shared`], i.e. after the singleton has been constructed.
    pub fn is_session_started(&self) -> bool {
        self.native
            .lock()
            .expect("xreal native mutex")
            .is_session_started()
    }

    /// Native plugin version string, or `None` if unavailable.
    pub fn plugin_version(&self) -> Option<String> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .get_plugin_version()
    }

    /// Connected `XREALDeviceType` enum value, or `None` if unavailable.
    pub fn device_type(&self) -> Option<i32> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .get_device_type()
    }

    /// Start the lower NRRendering pipeline (swapchain + GL textures + viewports).
    /// Must be called on the rendering thread (EGL context required for GL texture allocation).
    pub fn start_nr_rendering(&self) -> Result<(), i32> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .nr_rendering_start_persistent()
    }

    /// Submit one frame to the NR compositor.
    /// Returns the swapchain buffer index (maps to gl_texture_ids[index]).
    pub fn submit_nr_frame(&self) -> Result<u32, i32> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .nr_frame_submit()
    }

    /// Whether the direct NR rendering/compositor API was resolved from libnr_loader.so.
    pub fn nr_rendering_available(&self) -> bool {
        self.native
            .lock()
            .expect("xreal native mutex")
            .nr_rendering_available()
    }

    /// Number of direct NR rendering symbols resolved from libnr_loader.so.
    pub fn nr_rendering_symbol_count(&self) -> usize {
        self.native
            .lock()
            .expect("xreal native mutex")
            .nr_rendering_symbol_count()
    }

    /// RE probe for the lower compositor path. Creates and immediately destroys an
    /// NRRendering handle; it does not start presentation or submit frames.
    pub fn nr_rendering_smoke_create_destroy(&self) -> i32 {
        match self
            .native
            .lock()
            .expect("xreal native mutex")
            .nr_rendering_smoke_create_destroy()
        {
            Ok(()) => 0,
            Err(status) => status,
        }
    }

    /// RE probe for the lower compositor path. Creates, starts, stops, and destroys an
    /// NRRendering handle without submitting frames.
    pub fn nr_rendering_smoke_start_stop(&self) -> i32 {
        match self
            .native
            .lock()
            .expect("xreal native mutex")
            .nr_rendering_smoke_start_stop()
        {
            Ok(()) => 0,
            Err(status) => status,
        }
    }

    /// The current native pose and Godot rotation, or `None` when no fresh pose is
    /// available this frame.
    pub fn head_pose(&self) -> Option<(NrPose, Quaternion)> {
        let native = self.native.lock().expect("xreal native mutex");
        let time_ns = native.hmd_time_nanos()?;
        let mut pose = NrPose::default();
        native
            .get_head_pose_at_time(time_ns, &mut pose)
            .then(|| (pose, pose.to_godot_quaternion()))
    }

    /// The raw 16-float head pose from the **display** InputManager (libXREALXRPlugin.so) — the
    /// exact source the compositor reprojects the glasses layer with. Layout is decoded by the
    /// caller; used to drive the eye cameras onto the compositor's pose (peek window). Uses the
    /// display HMD clock (same as [`head_pose`]).
    pub fn head_pose_display(&self) -> Option<[f32; 16]> {
        let native = self.native.lock().expect("xreal native mutex");
        let time_ns = native.hmd_time_nanos()?;
        native.head_pose_display(time_ns)
    }

    /// Whether the RGB-camera C ABI resolved (see `docs/plans/camera-feed-plan.md`).
    pub fn rgb_camera_available(&self) -> bool {
        self.native
            .lock()
            .expect("xreal native mutex")
            .rgb_camera_available()
    }

    /// Start RGB-camera capture in poll mode; returns the capture handle (or `None`).
    pub fn rgb_camera_start(&self) -> Option<u64> {
        self.native.lock().expect("xreal native mutex").rgb_camera_start()
    }

    /// Stop RGB-camera capture.
    pub fn rgb_camera_stop(&self, handle: u64) -> bool {
        self.native
            .lock()
            .expect("xreal native mutex")
            .rgb_camera_stop(handle)
    }

    /// Poll the latest RGB-camera frame's Y plane as `(bytes, width, height)`.
    pub fn rgb_camera_grab_y(&self) -> Option<(Vec<u8>, i32, i32)> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .rgb_camera_grab_y()
    }

    /// Poll the latest RGB-camera frame as `(y, y_w, y_h, cbcr, c_w, c_h)` — Y plane + interleaved
    /// CbCr — for a YCbCr feed (`set_ycbcr_images`) + shader conversion.
    pub fn rgb_camera_grab_yuv(&self) -> Option<(Vec<u8>, i32, i32, Vec<u8>, i32, i32)> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .rgb_camera_grab_yuv()
    }

    /// Re-center the view. Calls the SDK's input-provider recenter (`NativePerception::Recenter`,
    /// which resets the perception origin the compositor reprojects against — the real fix for
    /// "move the glasses render to current-forward"), plus the legacy `RecenterGlasses` (harmless
    /// no-op on our pose, kept for completeness).
    pub fn recenter(&self) {
        self.native
            .lock()
            .expect("xreal native mutex")
            .recenter_glasses();
        crate::unity_plugin::call_input_recenter();
    }

    /// Keep the glasses display on by bypassing the proximity (wear) sensor auto-off. Returns the
    /// SDK status (or `None` if unsupported). No-ops inside the SDK until `NativeGlasses` is ready,
    /// so callers should invoke it a few times after the session goes live.
    pub fn set_display_bypass_psensor(&self, bypass: bool) -> Option<i32> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .set_display_bypass_psensor(bypass)
    }

    /// Set the glasses spatial display mode (`NRGlassesSpaceMode`; RE / unverified values).
    pub fn set_glasses_space_mode(&self, mode: i32) -> Option<i32> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .set_glasses_space_mode(mode)
    }

    ///

    /// XR-plugin tracking-state enum value, or `None` if the export is absent.
    pub fn tracking_state(&self) -> Option<i32> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .tracking_state()
    }

    /// XR-plugin tracking-reason enum value, or `None` if the export is absent.
    pub fn tracking_reason(&self) -> Option<i32> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .tracking_reason()
    }

    /// XR-plugin tracking-type enum value (`TrackingType`), or `None` if the export is absent.
    pub fn tracking_type(&self) -> Option<i32> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .tracking_type()
    }

    /// Switch the tracking mode at runtime (`TrackingType`: 0=6DoF, 1=3DoF, 2=0DoF,
    /// 3=0DoF-stab). Only reachable via [`shared`], i.e. after the session is live —
    /// calling it during bootstrap races NativeGlasses construction (see `try_start`).
    pub fn switch_tracking_type(&self, tracking_type: i32) -> bool {
        self.native
            .lock()
            .expect("xreal native mutex")
            .switch_tracking_type(tracking_type)
    }

    /// Current HMD clock in nanoseconds, or `None` while the perception pipe is down.
    pub fn hmd_time_nanos(&self) -> Option<u64> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .hmd_time_nanos()
    }

    /// One-line diagnostic of the perception pipeline, logged (throttled) when no pose
    /// arrives, so we can see WHERE it breaks: is the session started, does the HMD clock
    /// tick, does the pose query succeed, and are the values non-zero.
    pub fn diagnostics(&self) -> String {
        let native = self.native.lock().expect("xreal native mutex");
        let started = native.is_session_started();
        let (sm_time, xp_time) = native.hmd_time_probe();
        let time = native.hmd_time_nanos();
        let mut pose = NrPose::default();
        let pose_ok = match time {
            Some(t) => native.get_head_pose_at_time(t, &mut pose),
            None => false,
        };
        format!(
            "session_started={started}, hmd_time(sm)={sm_time:?}, hmd_time(xrplugin)={xp_time:?}, \
             pose_ok={pose_ok}, track_state={:?}, track_reason={:?}, pose=[{:.3}, {:.3}, {:.3}, {:.3}]",
            native.tracking_state(),
            native.tracking_reason(),
            pose.qx,
            pose.qy,
            pose.qz,
            pose.qw
        )
    }
}
