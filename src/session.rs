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
//! Bootstrap also needs the Android `Activity`, the Unity SDK's `unityActivity`, which a companion
//! publishes into `ndk_context` from the Java side (see `src/jni_bridge.rs` and
//! `XrealBridge.java`). That can arrive slightly after the first node `ready()`, so [`shared`] is
//! **retry-friendly**: while the Activity is missing it returns `None` quietly and tries again on
//! the next frame. A missing-library failure is terminal and logged once, which is the desktop and
//! editor case.
//!
//! Bootstrap order, mirroring the Unity loader and the RE of the native libs:
//!   1. `InitUserDefinedSettings(settings, Activity included)`, then `CreateSession(directPresent)`.
//!   2. `XREALLoadAPI()`, which constructs and wires the session-manager perception singleton.
//!      It is REQUIRED before any pose or `IsSessionStarted` call.

use std::sync::{
    atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering},
    Mutex, OnceLock,
};

use godot::builtin::Quaternion;

use crate::ffi::{NrPose, TrackingType, UserDefinedSettings};
use crate::native::XrealNative;

/// Explicit head-tracking-mode override set from GDScript through `XrealSystem.set_tracking_type`.
/// `-1` means unset, falling through to the system property and then the default. It must be set
/// **before** the session bootstraps, for instance in an autoload `_ready`, before the XR rig
/// enters the tree.
static TRACKING_MODE_OVERRIDE: AtomicI32 = AtomicI32::new(-1);

/// Set the tracking-mode override from GDScript. See [`tracking_mode`].
pub fn set_tracking_mode_override(mode: i32) {
    TRACKING_MODE_OVERRIDE.store(mode, Ordering::Relaxed);
}

/// The current tracking-mode override, or `-1` when unset.
#[allow(dead_code)] // kept public API; `tracking_mode` reads the static directly
pub fn tracking_mode_override() -> i32 {
    TRACKING_MODE_OVERRIDE.load(Ordering::Relaxed)
}

/// Explicit input-source override set from GDScript through `XrealSystem.set_input_source`. `-1`
/// means unset, falling through to the system property and then the controller-only default. It
/// must be set **before** the session bootstraps, because it is read once at
/// `InitUserDefinedSettings`.
static INPUT_SOURCE_OVERRIDE: AtomicI32 = AtomicI32::new(-1);

/// Set the input-source override from GDScript. See [`input_source`].
pub fn set_input_source_override(source: i32) {
    INPUT_SOURCE_OVERRIDE.store(source, Ordering::Relaxed);
}

/// `InitUserDefinedSettings`'s `inputSource`: 1 = Controller, 2 = Hands, 3 = ControllerAndHands.
/// Bit 1 (Hands) is the gate `InputManager::UpdateHandPose` checks, so hand tracking needs 2 or 3.
///
/// **It defaults to 1, controller only, because asking for hands costs about 878 ms of cold
/// start.** Measured on the X4000 with a One Pro on 2026-07-22: with the Hands bit set,
/// `InputManager::InputStart @ 0x78794` calls `NativePerception::SetHandTrackingEnabled(true) @
/// 0x97174` synchronously, and that single call is the entire gap between the SDK reporting input
/// device 2 and device 3, 878 ms of the 1.39 s input start. The reference Unity app ships
/// `inputSource=0`. See `docs/archive/codex-input-start-analysis.md`.
///
/// Hand tracking is Air 2 Ultra only, so on every other headset that 878 ms bought nothing at all.
/// Opt back in with the `xreal/input_source` project setting, mirroring Unity, which exposes Input
/// Source the same way: the demo reads it in `demo/main.gd` and passes it to
/// `XrealSystem.set_input_source` before the rig starts. `xreal_hands.tscn` warns when it is not
/// set, because dropping that scene in cannot turn it on for you. The choice is read once at
/// bootstrap, before any feature could react to it.
fn input_source() -> i32 {
    let ovr = INPUT_SOURCE_OVERRIDE.load(Ordering::Relaxed);
    if ovr >= 0 {
        return ovr;
    }
    android_prop_i32(b"debug.xreal.input_source\0").unwrap_or(1)
}

/// Explicit stereo-rendering-mode override set from GDScript through `XrealSystem.set_stereo_mode`.
/// `-1` means unset, falling through to the system property and then the Multipass default. It must
/// be set **before** the session bootstraps, because it is read once at
/// `InitUserDefinedSettings`.
static STEREO_MODE_OVERRIDE: AtomicI32 = AtomicI32::new(-1);

/// Set the stereo-mode override from GDScript. See [`stereo_rendering_mode`].
pub fn set_stereo_mode_override(mode: i32) {
    STEREO_MODE_OVERRIDE.store(mode, Ordering::Relaxed);
}

/// Read a NUL-terminated Android system property as an `i32`. It returns `None` off Android, and
/// when the property is unset or unparseable.
pub fn android_prop_i32(key: &[u8]) -> Option<i32> {
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

/// Stereo rendering mode for `InitUserDefinedSettings`, resolved **once at bootstrap** in priority
/// order: the GDScript override `XrealSystem.set_stereo_mode`, which is how the ProjectSetting
/// `xreal/stereo_mode` is applied, then `adb shell setprop debug.xreal.stereo_mode 2`, then the
/// default. That default is **Multipass** (`0`), the complete shipping path. **Multiview** (`2`,
/// single-pass-instanced) is a working option, and any other value falls back to Multipass.
///
/// Multiview **now renders correctly**, both eyes, as of 2026-07-17. The long-standing black right
/// eye was NOT the NR compositor: the old "libnr_api can't sample layer 1" conclusion was wrong,
/// and a solid-colour layer probe proved the compositor presents layer 1 fine. The real causes were
/// two Adreno GLES driver quirks in how we filled the array layers, both fixed in
/// `src/gl.rs::blit_texture_to_layer`. First, `glBlitFramebuffer` into a layer > 0 attachment is a
/// silent no-op, which gave the black right eye. Second, a direct `glCopyImageSubData` from the
/// non-RGBA8 SubViewport scrambles colours. The fix blits into an RGBA8 scratch, correcting the
/// format, then `glCopyImageSubData`s that into the layer.
///
/// The default stays Multipass only because Multiview buys **zero** GPU here: our rig draws two
/// Godot SubViewports every frame in both modes, and the single-pass-instanced win needs the
/// *engine* to draw both eyes in one pass. See `docs/archive/multiview-investigation.md`.
fn stereo_rendering_mode() -> i32 {
    // 1) The explicit override from the GDScript API, which carries the `xreal/stereo_mode`
    // ProjectSetting.
    let ovr = STEREO_MODE_OVERRIDE.load(Ordering::Relaxed);
    if ovr >= 0 {
        return if ovr == 2 { 2 } else { 0 };
    }
    // 2) The debug property, otherwise the default. Opt into Multiview only on an explicit 2:
    // everything else, whether unset, off Android or any other value, stays on the default Multipass
    // path.
    match android_prop_i32(b"debug.xreal.stereo_mode\0") {
        Some(2) => 2,
        _ => 0,
    }
}

/// Head-tracking mode for `InitUserDefinedSettings`, resolved **once at session bootstrap** in
/// priority order:
///   1. the GDScript override `XrealSystem.set_tracking_type`, which is also how the ProjectSetting
///      `xreal/tracking_type` is applied: read it in GDScript and pass it to the override before
///      the XR rig starts, as `demo/main.gd` does,
///   2. the Android system property `debug.xreal.tracking_type`, set with
///      `adb shell setprop debug.xreal.tracking_type 1`,
///   3. the default.
///
/// `0` is MODE_6DOF, SLAM position and orientation, the recommended mode: the DISP pose the eye
/// cameras use carries the full orientation, roll included, and does not drift. `1` is MODE_3DOF,
/// pure IMU with no position, and `2` is MODE_0DOF. It defaults to MODE_6DOF. NOTE: the eye-camera
/// rotation comes from the XR-plugin DISP pose (`node.rs`), not from the compact session-manager
/// `NrPose`, which is horizon-stabilized in every mode
/// (`docs/archive/roll-tracking-investigation.md`).
fn tracking_mode() -> i32 {
    const DEFAULT: i32 = TrackingType::Mode6Dof as i32;

    // 1) The explicit override from the GDScript API, which also carries a ProjectSetting value.
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
/// Terminal-failure latch, for instance when the libraries are absent on desktop. It stops further
/// attempts, and the warning is printed only once.
static DISABLED: OnceLock<String> = OnceLock::new();
/// Log the retryable CreateSession wait only once; it can happen every frame while the
/// glasses display / NR service is still not ready.
static WAITING_FOR_SESSION_READY_LOGGED: AtomicBool = AtomicBool::new(false);
/// `UnityPluginLoad` populates process-global Unity interface pointers inside
/// libXREALXRPlugin.so, so calling it once per process is enough.
static UNITY_PLUGIN_LOAD_DONE: AtomicBool = AtomicBool::new(false);
/// Master switch for glasses hardware-event delivery. When it is `true`,
/// `SetGlassesEventCallback` is registered once per process, at the site below, so the key,
/// wear-sensor, brightness, volume and EC events flow into the queue that
/// `XrealHeadTracker::process()` drains.
///
/// It is kept as a kill switch from an earlier crash hunt: an input build deterministically
/// SIGSEGV'd the render thread at `0x3f800000`, a `1.0f` bit pattern called as a function pointer,
/// on the first frame, and this callback was the initial suspect. On-device bisection cleared it.
/// The real trigger was an unrelated `XrealSystem::get_head_rotation` `#[func]` whose body
/// referenced `head_pose()`, since removed; see its note in `system.rs`. With that gone the
/// callback registers cleanly and full glasses input is device-verified on the One Pro, so this
/// stays `true`.
const ENABLE_GLASSES_EVENT_CALLBACK: bool = true;
/// Ensures the one-shot glasses-event registration runs at most once per process (see
/// [`ENABLE_GLASSES_EVENT_CALLBACK`]).
static GLASSES_EVENT_CALLBACK_REGISTERED: AtomicBool = AtomicBool::new(false);

/// Set once the native error callback (`SetNativeErrorCallback`) is registered.
static NATIVE_ERROR_CALLBACK_REGISTERED: AtomicBool = AtomicBool::new(false);
/// Retry runtime-dependent bootstrap while the glasses and the NR runtime are unavailable. Backing
/// off matters because every attempt re-runs `XrealNative::load()`, but the cadence has to start
/// tight. Measured on the X4000 on 2026-07-22, the Activity appears **about 145 ms** after our
/// first probe, and a flat 60-call wait, roughly 1 s at 60 fps, turned that into a 0.93 s stall on
/// every cold start, a fifth of the whole phone-screen-to-glasses gap. So double from one frame
/// instead, which finds it within about 100 ms while still converging to the old cadence when the
/// runtime really is far off.
static SHARED_CALLS: AtomicU64 = AtomicU64::new(0);
static NEXT_RUNTIME_RETRY_CALL: AtomicU64 = AtomicU64::new(0);
static RUNTIME_RETRY_BACKOFF: AtomicU64 = AtomicU64::new(1);
/// Ceiling for [`RUNTIME_RETRY_BACKOFF`], the old flat value, reached after six attempts, about
/// 1 s.
const RUNTIME_RETRY_BACKOFF_MAX: u64 = 60;

/// Initialize (once) and return the process-global XREAL session, or `None` when it is
/// not (yet) available.
///
/// Calling it every frame from any node is safe. Once created the session is cached; while the
/// Android Activity has not been published it returns `None` and retries on the next call; and on
/// a terminal failure, such as missing native libraries, it latches and stops trying.
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
            RUNTIME_RETRY_BACKOFF.store(1, Ordering::SeqCst);
            let _ = SESSION.set(*session);
            SESSION.get()
        }
        // Activity or the XREAL session target is not ready yet; retry soon.
        TryStart::WaitingForRuntime => {
            let backoff = RUNTIME_RETRY_BACKOFF.load(Ordering::SeqCst);
            NEXT_RUNTIME_RETRY_CALL.store(call.wrapping_add(backoff), Ordering::SeqCst);
            RUNTIME_RETRY_BACKOFF.store(
                backoff.saturating_mul(2).min(RUNTIME_RETRY_BACKOFF_MAX),
                Ordering::SeqCst,
            );
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
    /// The native libraries are present but the Android and XREAL runtime state is not ready yet.
    WaitingForRuntime,
    /// Terminal, so do not retry: the libraries are missing, or a bootstrap call failed.
    Disabled(String),
}

pub struct XrealSession {
    native: Mutex<XrealNative>,
}

impl XrealSession {
    /// One bootstrap attempt: load the libraries, then, once the Activity is available, create the
    /// session and wire the perception API.
    fn try_start() -> TryStart {
        let native = match XrealNative::load() {
            Ok(native) => native,
            Err(err) => return TryStart::Disabled(format!("native libraries unavailable: {err}")),
        };

        // This needs the host Activity, published into ndk_context from the Java side. Until then there
        // is nothing to create a session with, so retry on the next frame.
        let Some(activity) = crate::jni_bridge::activity_ptr() else {
            return TryStart::WaitingForRuntime;
        };

        // libXREALXRPlugin.so is a Unity native plugin, so hand it our fake IUnityInterfaces, which
        // reports OpenGL ES 3, the way Unity's engine would through UnityPluginLoad, and do it BEFORE
        // InitUserDefinedSettings, or its DisplayManager::LoadDisplay dereferences a null interface
        // pointer and segfaults. See crate::unity_plugin.
        if !UNITY_PLUGIN_LOAD_DONE.load(Ordering::SeqCst) {
            let loaded = native.unity_plugin_load(crate::unity_plugin::interfaces_ptr());
            if loaded {
                UNITY_PLUGIN_LOAD_DONE.store(true, Ordering::SeqCst);
            }
        }

        // Route the glasses hardware events, covering keys, the wear sensor and brightness, volume and
        // EC, into the process-wide queue; XrealHeadTracker::process() drains it on the main thread.
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
        // reference app; we follow the same path so the head-tracker gets both
        // rotation and position. Keep this aligned with Unity's InitUserDefinedSettings
        // log before falling back to narrower tracking modes.
        let stereo_mode = stereo_rendering_mode();
        let tracking_mode = tracking_mode();
        let input_src = input_source();
        godot::global::godot_print!(
            "[xreal] stereo_rendering_mode = {stereo_mode} (0=Multipass, 2=Multiview), \
             tracking_type = {tracking_mode} (0=6DoF, 1=3DoF, 2=0DoF),              input_source = {input_src} (1=Controller, 3=ControllerAndHands)"
        );
        let settings = UserDefinedSettings {
            color_space: 1,
            // Stereo mode, from stereo_rendering_mode(), defaulting to Multipass, with
            // `debug.xreal.stereo_mode 2` opting into Multiview. 0 is Multipass, per-eye 2D textures, and 2
            // is Multiview, or Single-Pass-Instanced, one 2-layer immutable array texture, which is the
            // reference app's StereoRendering: 2. See that function and
            // docs/archive/multiview-investigation.md.
            stereo_rendering_mode: stereo_mode,
            tracking_type: tracking_mode,
            support_mono_mode: 0,
            unity_activity: activity,
            // See `input_source()`: controller-only by default, because the Hands bit costs about 878 ms of
            // cold start, and the hands feature opts back in.
            input_source: input_src,
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

        // Unity owns this through the XR SDK provider callbacks. Our fake Unity interface stores the
        // callbacks during InitUserDefinedSettings, and we run initialize and start after CreateSession
        // so XREAL can construct NativeHMD and NativePerception.
        crate::unity_plugin::start_registered_providers();

        // GfxThreadStart is deferred to the rendering thread (see unity_plugin::run_render_thread_tick).
        // CreateSwapchainEx allocates GL textures and requires an active EGL context; the main
        // thread has none. The first call to run_render_thread_tick() from node::process() via
        // RenderingServer::call_on_render_thread will invoke GfxThreadStart on the rendering
        // thread (EGL context active), which then triggers SetSwapChainBuffers + AcquireFrame.

        // This constructs and wires the session-manager perception singleton. It is REQUIRED before any
        // pose or IsSessionStarted call, which dereference what it sets up.
        native.load_api();

        // A freshly created session is PAUSED: the server keeps it in pauseCount until the client
        // resumes, which Unity does on app resume. Without it IsSessionStarted stays false and no head
        // pose is delivered.
        native.resume_session();

        // SwitchTrackingType removed: it triggers action callbacks from the XREAL Nebula service
        // (via libnr_api.so) that race with NativeGlasses construction and cause SIGSEGV at
        // NativeGlasses::GetActionData+8 (null member deref) before the input subsystem is ready.
        // Head tracking works without this call once the 6DoF session starts via CreateSession.
        //
        // This is the "pre-construction" null window mapped in `crate::signal_guard`, where the action
        // lambda @0x84c28 lazily builds a zeroed SessionManager singleton with a null +0x60. Keeping
        // SwitchTrackingType out of bootstrap closes THIS window from our side, and the SDK's own
        // DestroySession-on-teardown race is async and stays covered by the code patch there.
        let initial_tracking_type = tracking_mode;

        // NOTE: display_manager_submit_frame_probe() was removed.
        //
        // Calling PopulateNextFrameDesc with (lib_base + 0xdb400) sets 0xdb410 = 0xa6, which switches the
        // XREAL SDK rendering thread's SubmitCurrentFrame path from the normal
        // "SetBufferViewport + NativeRendering::SubmitFrame" to "NativeRendering::DestroyFrame", the
        // cleanup and embedded-data mode. The DestroyFrame call then crashes because DisplayManager+0x120
        // holds a live SDK-managed frame handle, 0xb9a40998bac55c8a, MTE-tagged, which is the same
        // SIGABRT we saw before.
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

    /// Whether the native session reports it has started. It is safe, being reachable only through
    /// [`shared`], that is after the singleton has been constructed.
    pub fn is_session_started(&self) -> bool {
        self.native
            .lock()
            .expect("xreal native mutex")
            .is_session_started()
    }

    /// Native plugin version string, or `None` when unavailable.
    pub fn plugin_version(&self) -> Option<String> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .get_plugin_version()
    }

    /// Connected `XREALDeviceType` enum value, or `None` when unavailable.
    pub fn device_type(&self) -> Option<i32> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .get_device_type()
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

    /// The raw 16-float head pose from the **display** InputManager in libXREALXRPlugin.so, the exact
    /// source the compositor reprojects the glasses layer with. The caller decodes the layout, and it
    /// drives the eye cameras onto the compositor's pose for the peek window. It uses the display HMD
    /// clock, the same as [`head_pose`].
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
        self.native
            .lock()
            .expect("xreal native mutex")
            .rgb_camera_start()
    }

    /// Stop RGB-camera capture.
    pub fn rgb_camera_stop(&self, handle: u64) -> bool {
        self.native
            .lock()
            .expect("xreal native mutex")
            .rgb_camera_stop(handle)
    }

    /// Poll the latest RGB-camera frame's Y plane as `(bytes, width, height)`.
    #[allow(dead_code)] // Y-only grab; the demo uses rgb_camera_grab_yuv (colour)
    pub fn rgb_camera_grab_y(&self) -> Option<(Vec<u8>, i32, i32)> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .rgb_camera_grab_y()
    }

    /// Poll the latest RGB-camera frame as `(y, y_w, y_h, cbcr, c_w, c_h)`, the Y plane plus
    /// interleaved CbCr, for a YCbCr feed through `set_ycbcr_images` and shader conversion. It returns
    /// `None` when the SDK's latest frame is still the one `last_timestamp` names; see the native doc.
    pub fn rgb_camera_grab_yuv(
        &self,
        last_timestamp: &mut u64,
        timings: &mut crate::native::GrabTimings,
    ) -> Option<crate::native::YuvFrame> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .rgb_camera_grab_yuv(last_timestamp, timings)
    }

    /// Acquire the latest RGB frame and hand its planes to `consume` **without copying them**; see
    /// `XrealNative::rgb_camera_with_frame`. The borrow ends when `consume` returns.
    pub fn rgb_camera_with_frame<R>(
        &self,
        last_timestamp: &mut u64,
        timings: &mut crate::native::GrabTimings,
        consume: impl FnOnce(crate::native::RgbPlanes<'_>) -> Option<R>,
    ) -> Option<R> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .rgb_camera_with_frame(last_timestamp, timings, consume)
    }

    /// Re-center the view. It calls the SDK's input-provider recenter,
    /// `NativePerception::Recenter`, which resets the perception origin the compositor reprojects
    /// against and is the real fix for moving the glasses render to current-forward. It also calls the
    /// legacy `RecenterGlasses`, a harmless no-op on our pose, kept for completeness.
    pub fn recenter(&self) {
        self.native
            .lock()
            .expect("xreal native mutex")
            .recenter_glasses();
        crate::unity_plugin::call_input_recenter();
    }

    /// Keep the glasses display on by bypassing the proximity (wear) sensor auto-off. It returns the
    /// SDK status, or `None` when unsupported. Inside the SDK it no-ops until `NativeGlasses` is ready,
    /// so callers should invoke it a few times after the session goes live.
    pub fn set_display_bypass_psensor(&self, bypass: bool) -> Option<i32> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .set_display_bypass_psensor(bypass)
    }

    /// Set the glasses spatial display mode, an `NRGlassesSpaceMode`, whose values are RE'd and
    /// unverified.
    pub fn set_glasses_space_mode(&self, mode: i32) -> Option<i32> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .set_glasses_space_mode(mode)
    }

    /// XR-plugin tracking-state enum value, or `None` when the export is absent.
    pub fn tracking_state(&self) -> Option<i32> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .tracking_state()
    }

    /// XR-plugin tracking-reason enum value, or `None` when the export is absent.
    pub fn tracking_reason(&self) -> Option<i32> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .tracking_reason()
    }

    /// XR-plugin tracking-type enum value, a `TrackingType`, or `None` when the export is absent.
    pub fn tracking_type(&self) -> Option<i32> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .tracking_type()
    }

    /// Switch the tracking mode at runtime, with `TrackingType` 0 for 6DoF, 1 for 3DoF, 2 for 0DoF and
    /// 3 for 0DoF-stab. It is reachable only through [`shared`], that is after the session is live,
    /// because calling it during bootstrap races NativeGlasses construction; see `try_start`.
    pub fn switch_tracking_type(&self, tracking_type: i32) -> bool {
        self.native
            .lock()
            .expect("xreal native mutex")
            .switch_tracking_type(tracking_type)
    }

    /// Whether the connected glasses support an `ffi::hmd_feature`, through `IsHMDFeatureSupported`.
    pub fn hmd_feature_supported(&self, feature: i32) -> Option<bool> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .hmd_feature_supported(feature)
    }

    /// A `ffi::component` device's extrinsic relative to Head, as a Unity `Pose` of
    /// `[pos x,y,z, quat x,y,z,w]` in Unity space; see docs/plans/coordinate-systems-notes.md.
    pub fn device_pose_from_head(&self, component: i32) -> Option<[f32; 7]> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .device_pose_from_head(component)
    }

    /// A `ffi::component` device's pixel resolution `(width, height)`.
    pub fn device_resolution(&self, component: i32) -> Option<(i32, i32)> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .device_resolution(component)
    }

    /// A `ffi::component` camera's intrinsics `[fx, fy, cx, cy]` in pixels.
    pub fn camera_intrinsic(&self, component: i32) -> Option<[f32; 4]> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .camera_intrinsic(component)
    }

    /// A `ffi::component` camera's 4x4 projection matrix, 16 floats, for `[near, far]`.
    pub fn camera_projection_matrix(
        &self,
        component: i32,
        near: f32,
        far: f32,
    ) -> Option<[f32; 16]> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .camera_projection_matrix(component, near, far)
    }

    /// Current `PlaneDetectionMode` flags, or `None` when the export is absent.
    pub fn plane_detection_mode(&self) -> Option<i32> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .plane_detection_mode()
    }

    /// Enable horizontal and vertical plane detection; it needs a live 6DoF session.
    pub fn set_plane_detection_mode(&self, mode: i32) -> bool {
        self.native
            .lock()
            .expect("xreal native mutex")
            .set_plane_detection_mode(mode)
    }

    /// Poll the plane added/updated/removed changes since the last call.
    pub fn poll_plane_changes(&self) -> Option<crate::native::PlaneChanges> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .poll_plane_changes()
    }

    /// Boundary polygon of a detected plane, as plane-local `Vector2`s.
    pub fn plane_boundary(&self, id: crate::ffi::TrackableId) -> Vec<[f32; 2]> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .plane_boundary(id)
    }

    // --- Spatial anchors; see docs/plans/ar-features-plan.md. They need a live 6DoF session and
    //     the nr_spatial_anchor.aar backend. ---

    /// Enable or disable the anchor subsystem; call it before use. It returns whether the export was
    /// present.
    pub fn set_anchor_enabled(&self, enabled: bool) -> bool {
        self.native
            .lock()
            .expect("xreal native mutex")
            .set_anchor_enabled(enabled)
    }

    /// Point the anchor subsystem at a writable directory for its saved-anchor map files.
    pub fn set_anchor_mapping_dir(&self, dir: &str) -> bool {
        self.native
            .lock()
            .expect("xreal native mutex")
            .set_anchor_mapping_dir(dir)
    }

    /// Create a new anchor at `pose`, in Unity space.
    pub fn acquire_anchor(
        &self,
        pose: crate::ffi::UnityPose,
    ) -> Option<crate::native::AnchorSample> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .acquire_anchor(pose)
    }

    /// Poll the anchor added/updated/removed changes since the last call.
    pub fn poll_anchor_changes(&self) -> Option<crate::native::AnchorChanges> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .poll_anchor_changes()
    }

    /// Persist an anchor and return its `Guid` key.
    pub fn save_anchor(&self, id: crate::ffi::TrackableId) -> Option<crate::ffi::Guid> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .save_anchor(id)
    }

    /// Restore a saved anchor by its `Guid`.
    pub fn load_anchor(&self, guid: crate::ffi::Guid) -> Option<crate::native::AnchorSample> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .load_anchor(guid)
    }

    /// Drop a tracked anchor.
    pub fn remove_anchor(&self, id: crate::ffi::TrackableId) -> bool {
        self.native
            .lock()
            .expect("xreal native mutex")
            .remove_anchor(id)
    }

    /// Re-localize an anchor into the current map.
    pub fn remap_anchor(&self, id: crate::ffi::TrackableId) -> bool {
        self.native
            .lock()
            .expect("xreal native mutex")
            .remap_anchor(id)
    }

    /// Estimate an anchor's save quality, an `ffi::anchor_quality`, at `pose`.
    pub fn estimate_anchor_quality(
        &self,
        id: crate::ffi::TrackableId,
        pose: crate::ffi::UnityPose,
    ) -> Option<i32> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .estimate_anchor_quality(id, pose)
    }

    // --- Image tracking; see docs/plans/ar-features-plan.md. It needs a live 6DoF session, the
    //     nr_image_tracking.aar backend, assets/nr_plugins.json and a DB blob. ---

    /// Build a tracking database from a blob plus per-image metadata, returning the DB handle.
    pub fn init_image_database(
        &self,
        blob: &[u8],
        refs: &[crate::ffi::ManagedReferenceImage],
    ) -> Option<u64> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .init_image_database(blob, refs)
    }

    /// Activate a database; `0` disables image tracking.
    pub fn set_image_database(&self, handle: u64) {
        self.native
            .lock()
            .expect("xreal native mutex")
            .set_image_database(handle)
    }

    /// Number of reference images in a database.
    pub fn image_reference_count(&self, handle: u64) -> i32 {
        self.native
            .lock()
            .expect("xreal native mutex")
            .image_reference_count(handle)
    }

    /// Free a database.
    pub fn release_image_database(&self, handle: u64) {
        self.native
            .lock()
            .expect("xreal native mutex")
            .release_image_database(handle)
    }

    /// Poll the tracked-image added/updated/removed changes since the last call.
    pub fn poll_image_changes(&self) -> Option<crate::native::ImageChanges> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .poll_image_changes()
    }

    /// Current HMD clock in nanoseconds, or `None` while the perception pipe is down.
    pub fn hmd_time_nanos(&self) -> Option<u64> {
        self.native
            .lock()
            .expect("xreal native mutex")
            .hmd_time_nanos()
    }

    /// One-line diagnostic of the perception pipeline, logged in throttled form when no pose arrives,
    /// so we can see WHERE it breaks: whether the session started, whether the HMD clock ticks,
    /// whether the pose query succeeds, and whether the values are non-zero.
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
