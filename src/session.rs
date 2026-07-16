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

use std::sync::OnceLock;

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

    match XrealSession::try_start() {
        TryStart::Ready(session) => {
            let _ = SESSION.set(session);
            SESSION.get()
        }
        // Activity not published yet — stay quiet and try again next frame.
        TryStart::WaitingForActivity => None,
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
    Ready(XrealSession),
    /// Native libraries are present but no Android Activity has been published yet.
    WaitingForActivity,
    /// Terminal: do not retry (libraries missing, or a bootstrap call failed).
    Disabled(String),
}

pub struct XrealSession {
    native: XrealNative,
}

impl XrealSession {
    /// One bootstrap attempt: load the libraries, then (once the Activity is available)
    /// create the session and wire the perception API.
    fn try_start() -> TryStart {
        let native = match XrealNative::load() {
            Ok(native) => native,
            Err(err) => return TryStart::Disabled(format!("native libraries unavailable: {err}")),
        };

        // Needs the host Activity (published into ndk_context from the Java side). Until
        // then there is nothing to create a session with — retry next frame.
        let Some(activity) = crate::jni_bridge::activity_ptr() else {
            return TryStart::WaitingForActivity;
        };

        // libXREALXRPlugin.so is a Unity native plugin: hand it our fake IUnityInterfaces
        // (reporting OpenGL ES 3) the way Unity's engine would via UnityPluginLoad, BEFORE
        // InitUserDefinedSettings — otherwise its DisplayManager::LoadDisplay dereferences a
        // null interface pointer and segfaults. See crate::unity_plugin.
        native.unity_plugin_load(crate::unity_plugin::interfaces_ptr());

        // Color space: Unity ColorSpace.Linear == 1; stereo/input default to 0.
        let settings = UserDefinedSettings {
            color_space: 1,
            stereo_rendering_mode: 0,
            tracking_type: TrackingType::Mode3Dof as i32,
            support_mono_mode: 0,
            unity_activity: activity,
            input_source: 0,
        };
        if !native.init_user_defined_settings(settings) {
            return TryStart::Disabled("InitUserDefinedSettings unavailable (libXREALXRPlugin.so?)".into());
        }
        if !native.create_session(false) {
            return TryStart::Disabled("CreateSession returned false".into());
        }

        // Constructs/wires the session-manager perception singleton. REQUIRED before any
        // pose or IsSessionStarted call — those dereference what this sets up.
        native.load_api();

        // A freshly created session is PAUSED: the server keeps it in pauseCount until the
        // client resumes (Unity calls this on app resume). Without it IsSessionStarted stays
        // false and no head pose is delivered.
        native.resume_session();

        // Experiment: Unity's input subsystem kicks perception via SwitchTrackingType. Try
        // it directly (1 = 3DoF) to see if perception starts without the full XR-subsystem
        // host. Log the tracking enums so we can read the outcome.
        let switched = native.switch_tracking_type(TrackingType::Mode3Dof as i32);
        godot::global::godot_print!(
            "[xreal] native session created (3DoF); session_started={}, \
             switch_tracking_type={switched}, tracking_type={:?}, tracking_state={:?}, tracking_reason={:?}",
            native.is_session_started(),
            native.tracking_type(),
            native.tracking_state(),
            native.tracking_reason()
        );
        TryStart::Ready(Self { native })
    }

    /// Whether the native session reports it has started. Safe: only reachable via
    /// [`shared`], i.e. after the singleton has been constructed.
    pub fn is_session_started(&self) -> bool {
        self.native.is_session_started()
    }

    /// Native plugin version string, or `None` if unavailable.
    pub fn plugin_version(&self) -> Option<String> {
        self.native.get_plugin_version()
    }

    /// Connected `XREALDeviceType` enum value, or `None` if unavailable.
    pub fn device_type(&self) -> Option<i32> {
        self.native.get_device_type()
    }

    /// The current head rotation as a Godot quaternion, or `None` when no fresh pose is
    /// available this frame.
    pub fn head_rotation(&self) -> Option<Quaternion> {
        let time_ns = self.native.hmd_time_nanos()?;
        let mut pose = NrPose::default();
        self.native
            .get_head_pose_at_time(time_ns, &mut pose)
            .then(|| pose.to_godot_quaternion())
    }

    /// Re-center the 3DoF view.
    pub fn recenter(&self) {
        self.native.recenter_glasses();
    }

    /// One-line diagnostic of the perception pipeline, logged (throttled) when no pose
    /// arrives, so we can see WHERE it breaks: is the session started, does the HMD clock
    /// tick, does the pose query succeed, and are the values non-zero.
    pub fn diagnostics(&self) -> String {
        let started = self.native.is_session_started();
        let (sm_time, xp_time) = self.native.hmd_time_probe();
        let time = self.native.hmd_time_nanos();
        let mut pose = NrPose::default();
        let pose_ok = match time {
            Some(t) => self.native.get_head_pose_at_time(t, &mut pose),
            None => false,
        };
        format!(
            "session_started={started}, hmd_time(sm)={sm_time:?}, hmd_time(xrplugin)={xp_time:?}, \
             pose_ok={pose_ok}, track_state={:?}, track_reason={:?}, pose=[{:.3}, {:.3}, {:.3}, {:.3}]",
            self.native.tracking_state(),
            self.native.tracking_reason(),
            pose.qx, pose.qy, pose.qz, pose.qw
        )
    }
}
