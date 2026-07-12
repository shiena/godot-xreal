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
    atomic::{AtomicBool, AtomicU64, Ordering},
    Mutex, OnceLock,
};

use godot::builtin::Quaternion;

use crate::ffi::{NrPose, TrackingType, UserDefinedSettings};
use crate::native::XrealNative;

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

        // Color space: Unity ColorSpace.Linear == 1; stereo/input default to 0.
        //
        // XREAL One/One Pro starts perception through the 6DoF path in the Unity
        // reference app even when the MVP only consumes head rotation. Keep this
        // aligned with Unity's InitUserDefinedSettings log before falling back to
        // narrower tracking modes.
        let settings = UserDefinedSettings {
            color_space: 1,
            // EXPERIMENT: Multi-pass (0) instead of Single Pass Instanced / Multiview (2).
            // With Multiview (2) the SDK runs "single_buffer:1" and CreateTexture asks for ONE
            // 2-layer array texture (arraylen=2), but SetSwapChainBuffers never calls our
            // QueryTextureDesc, so our GL texture is never registered → black. Multi-pass should
            // create two separate 2D textures and a normal multi-buffer swapchain, which is the
            // path our SetSwapChainBuffers analysis assumes and may actually trigger QueryTextureDesc.
            // Unity uses 2; revert if 0 fails to create the render texture.
            stereo_rendering_mode: 0,
            tracking_type: TrackingType::Mode6Dof as i32,
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
        let initial_tracking_type = TrackingType::Mode6Dof as i32;

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

    /// Re-center the 3DoF view.
    pub fn recenter(&self) {
        self.native
            .lock()
            .expect("xreal native mutex")
            .recenter_glasses();
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
        self.native.lock().expect("xreal native mutex").tracking_state()
    }

    /// XR-plugin tracking-reason enum value, or `None` if the export is absent.
    pub fn tracking_reason(&self) -> Option<i32> {
        self.native.lock().expect("xreal native mutex").tracking_reason()
    }

    /// XR-plugin tracking-type enum value (`TrackingType`), or `None` if the export is absent.
    pub fn tracking_type(&self) -> Option<i32> {
        self.native.lock().expect("xreal native mutex").tracking_type()
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
        self.native.lock().expect("xreal native mutex").hmd_time_nanos()
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
