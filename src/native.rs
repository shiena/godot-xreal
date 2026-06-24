//! Runtime binding to the vendored XREAL `.so` libraries via `dlopen`/`dlsym`.
//!
//! We deliberately avoid linking the XREAL libraries at build time: they only exist for
//! Android arm64, and keeping them out of the link line lets the same GDExtension load
//! in a desktop editor (where [`XrealNative::load`] simply returns `Err` and the node
//! no-ops).
//!
//! Symbols are resolved once and the owning [`libloading::Library`] handles are kept
//! alive in the struct for the lifetime of the resolved function pointers.

use libloading::Library;

use std::ffi::c_void;

use crate::ffi::{
    FnCreateSession, FnGetDeviceType, FnGetHeadPoseAtTime, FnGetPluginVersion, FnHmdTimeNanos,
    FnInitUserDefinedSettings, FnIsSessionStarted, FnLoadApi, FnQueryInt, FnSwitchTrackingType,
    FnUnityPluginLoad, FnVoid, NrPose, UserDefinedSettings,
};

const SESSION_LIB: &str = "libXREALNativeSessionManager.so";
const PLUGIN_LIB: &str = "libXREALXRPlugin.so";

pub struct XrealNative {
    // Keep the libraries loaded; the function pointers below borrow from them.
    _session_lib: Library,
    _plugin_lib: Option<Library>,

    // Perception (libXREALNativeSessionManager.so) — RE-confirmed signatures.
    hmd_time_nanos: FnHmdTimeNanos,
    get_head_pose_at_time: FnGetHeadPoseAtTime,
    load_api: Option<FnLoadApi>,
    is_session_started: Option<FnIsSessionStarted>,

    // Perception via libXREALXRPlugin.so (`InputManager::GetHeadPoseAtTime(unsigned long,
    // float*)` etc., same ABI). This is the layer that actually RUNS the session
    // (`CreateSession`/`ResumeSession` → "NRGlasses RUN!"), whereas the SessionManager lib
    // above returned no data on device — so we PREFER these when present.
    xp_hmd_time_nanos: Option<FnHmdTimeNanos>,
    xp_get_head_pose_at_time: Option<FnGetHeadPoseAtTime>,
    xp_is_session_started: Option<FnIsSessionStarted>,
    get_tracking_state: Option<FnQueryInt>,
    get_tracking_reason: Option<FnQueryInt>,
    get_tracking_type: Option<FnQueryInt>,
    switch_tracking_type: Option<FnSwitchTrackingType>,

    // Session / control (libXREALXRPlugin.so) — optional, used for full bootstrap.
    unity_plugin_load: Option<FnUnityPluginLoad>,
    init_user_defined_settings: Option<FnInitUserDefinedSettings>,
    create_session: Option<FnCreateSession>,
    resume_session: Option<FnVoid>,
    recenter_glasses: Option<FnVoid>,

    // Read-only device info (libXREALXRPlugin.so) — exposed via XrealSystem.
    get_plugin_version: Option<FnGetPluginVersion>,
    get_device_type: Option<FnGetDeviceType>,
}

impl XrealNative {
    /// `dlopen` the XREAL libraries and resolve the symbols needed for 3DoF.
    ///
    /// Returns `Err` (without panicking) when the libraries are missing — the expected
    /// case on desktop/editor builds.
    pub fn load() -> Result<Self, String> {
        unsafe {
            let session_lib =
                Library::new(SESSION_LIB).map_err(|e| format!("dlopen {SESSION_LIB}: {e}"))?;
            let plugin_lib = Library::new(PLUGIN_LIB).ok();

            let hmd_time_nanos: FnHmdTimeNanos = *session_lib
                .get(b"XREALGetHMDTimeNanos\0")
                .map_err(|e| format!("dlsym XREALGetHMDTimeNanos: {e}"))?;
            let get_head_pose_at_time: FnGetHeadPoseAtTime = *session_lib
                .get(b"XREALGetHeadPoseAtTime\0")
                .map_err(|e| format!("dlsym XREALGetHeadPoseAtTime: {e}"))?;

            let load_api: Option<FnLoadApi> = session_lib.get(b"XREALLoadAPI\0").ok().map(|s| *s);
            let is_session_started: Option<FnIsSessionStarted> =
                session_lib.get(b"XREALIsSessionStarted\0").ok().map(|s| *s);

            // Same-named flat-C perception exports in the XR plugin (the running session).
            let xp_hmd_time_nanos: Option<FnHmdTimeNanos> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetHMDTimeNanos\0").ok().map(|s| *s));
            let xp_get_head_pose_at_time: Option<FnGetHeadPoseAtTime> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetHeadPoseAtTime\0").ok().map(|s| *s));
            let xp_is_session_started: Option<FnIsSessionStarted> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"IsSessionStarted\0").ok().map(|s| *s));
            let get_tracking_state: Option<FnQueryInt> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetTrackingState\0").ok().map(|s| *s));
            let get_tracking_reason: Option<FnQueryInt> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetTrackingReason\0").ok().map(|s| *s));
            let get_tracking_type: Option<FnQueryInt> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetTrackingType\0").ok().map(|s| *s));
            let switch_tracking_type: Option<FnSwitchTrackingType> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"SwitchTrackingType\0").ok().map(|s| *s));

            let unity_plugin_load: Option<FnUnityPluginLoad> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"UnityPluginLoad\0").ok().map(|s| *s));
            let init_user_defined_settings: Option<FnInitUserDefinedSettings> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"InitUserDefinedSettings\0").ok().map(|s| *s));
            let create_session: Option<FnCreateSession> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"CreateSession\0").ok().map(|s| *s));
            let resume_session: Option<FnVoid> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"ResumeSession\0").ok().map(|s| *s));
            let recenter_glasses: Option<FnVoid> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"RecenterGlasses\0").ok().map(|s| *s));

            let get_plugin_version: Option<FnGetPluginVersion> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetPluginVersion\0").ok().map(|s| *s));
            let get_device_type: Option<FnGetDeviceType> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetDeviceType\0").ok().map(|s| *s));

            Ok(Self {
                _session_lib: session_lib,
                _plugin_lib: plugin_lib,
                hmd_time_nanos,
                get_head_pose_at_time,
                load_api,
                is_session_started,
                xp_hmd_time_nanos,
                xp_get_head_pose_at_time,
                xp_is_session_started,
                get_tracking_state,
                get_tracking_reason,
                get_tracking_type,
                switch_tracking_type,
                unity_plugin_load,
                init_user_defined_settings,
                create_session,
                resume_session,
                recenter_glasses,
                get_plugin_version,
                get_device_type,
            })
        }
    }

    /// `true` once the native session reports it has started. Prefers the XR-plugin layer
    /// (the one running the session); falls back to the SessionManager layer.
    pub fn is_session_started(&self) -> bool {
        match self.xp_is_session_started.or(self.is_session_started) {
            Some(f) => unsafe { f() },
            None => false,
        }
    }

    /// Hand the plugin a (fake) Unity `IUnityInterfaces`, mirroring Unity's startup
    /// `UnityPluginLoad`. Must run before `init_user_defined_settings`, whose
    /// `DisplayManager::LoadDisplay` dereferences the stored interface pointer. Returns
    /// `false` if the symbol was unavailable.
    pub fn unity_plugin_load(&self, interfaces: *mut c_void) -> bool {
        match self.unity_plugin_load {
            Some(f) => {
                unsafe { f(interfaces) };
                true
            }
            None => false,
        }
    }

    /// Configure the native plugin (color space, stereo mode, tracking type, Activity).
    /// Returns `false` if the symbol was unavailable.
    pub fn init_user_defined_settings(&self, settings: UserDefinedSettings) -> bool {
        match self.init_user_defined_settings {
            Some(f) => {
                unsafe { f(settings) };
                true
            }
            None => false,
        }
    }

    /// Create the native session. `direct_present` mirrors the Unity flag.
    pub fn create_session(&self, direct_present: bool) -> bool {
        match self.create_session {
            Some(f) => unsafe { f(direct_present) },
            None => false,
        }
    }

    /// Resume the session — Unity calls this on app resume; it activates the perception
    /// subsystem (a freshly `CreateSession`'d session stays paused, so `IsSessionStarted`
    /// is false and no pose flows until this runs). No-op if the symbol is unavailable.
    pub fn resume_session(&self) {
        if let Some(f) = self.resume_session {
            unsafe { f() }
        }
    }

    /// Wire the session-manager perception delegate. Must run before pose queries.
    pub fn load_api(&self) {
        if let Some(f) = self.load_api {
            unsafe { f() }
        }
    }

    /// Current HMD clock in nanoseconds via the out-pointer ABI, or `None` on failure.
    /// Prefers the XR-plugin layer (running session), falls back to the SessionManager.
    pub fn hmd_time_nanos(&self) -> Option<u64> {
        let f = self.xp_hmd_time_nanos.unwrap_or(self.hmd_time_nanos);
        let mut time_ns: u64 = 0;
        let status = unsafe { f(&mut time_ns) };
        (status == 0 && time_ns != 0).then_some(time_ns)
    }

    /// Fetch the head pose predicted for `time_ns`. Returns `true` on success. Prefers the
    /// XR-plugin layer (running session), falls back to the SessionManager.
    pub fn get_head_pose_at_time(&self, time_ns: u64, out: &mut NrPose) -> bool {
        let f = self.xp_get_head_pose_at_time.unwrap_or(self.get_head_pose_at_time);
        // NRResult: 0 == success.
        unsafe { f(time_ns, out as *mut NrPose) == 0 }
    }

    /// Diagnostic: the XR-plugin tracking state / reason enums (`None` if unavailable).
    pub fn tracking_state(&self) -> Option<i32> {
        self.get_tracking_state.map(|f| unsafe { f() })
    }
    pub fn tracking_reason(&self) -> Option<i32> {
        self.get_tracking_reason.map(|f| unsafe { f() })
    }
    pub fn tracking_type(&self) -> Option<i32> {
        self.get_tracking_type.map(|f| unsafe { f() })
    }

    /// Select the tracking mode (the Unity input subsystem calls this during perception
    /// start). `false` if the symbol is unavailable. Experiment to see if it kicks
    /// perception without the full XR-subsystem host.
    pub fn switch_tracking_type(&self, tracking_type: i32) -> bool {
        match self.switch_tracking_type {
            Some(f) => unsafe { f(tracking_type) },
            None => false,
        }
    }

    /// Diagnostic: raw HMD clock from each layer (SessionManager, XR-plugin), to see which
    /// one is actually delivering data.
    pub fn hmd_time_probe(&self) -> (Option<u64>, Option<u64>) {
        let probe = |f: Option<FnHmdTimeNanos>| {
            f.and_then(|f| {
                let mut t = 0u64;
                let status = unsafe { f(&mut t) };
                (status == 0 && t != 0).then_some(t)
            })
        };
        (probe(Some(self.hmd_time_nanos)), probe(self.xp_hmd_time_nanos))
    }

    /// Reset the 3DoF forward direction (no-op if the plugin/symbol is unavailable).
    pub fn recenter_glasses(&self) {
        if let Some(f) = self.recenter_glasses {
            unsafe { f() }
        }
    }

    /// Native plugin version string, or `None` if unavailable.
    pub fn get_plugin_version(&self) -> Option<String> {
        let f = self.get_plugin_version?;
        let ptr = unsafe { f() };
        if ptr.is_null() {
            return None;
        }
        Some(
            unsafe { std::ffi::CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    /// Connected device type (`XREALDeviceType` enum value), or `None` if unavailable.
    pub fn get_device_type(&self) -> Option<i32> {
        self.get_device_type.map(|f| unsafe { f() })
    }
}
