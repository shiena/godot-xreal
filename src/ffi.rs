//! Raw FFI types for the XREAL native C ABI.
//!
//! Signatures here are **confirmed by reverse engineering** the binaries (C++ mangled
//! names + AArch64 disassembly of the C wrappers in `libXREALNativeSessionManager.so`),
//! cross-checked against the Unity SDK's C# `[DllImport]` declarations. See
//! `docs/reverse-engineering.md` for the derivation. Items still flagged `RE` need
//! on-device confirmation.

use std::ffi::{c_char, c_void};

use godot::builtin::Quaternion;

/// Native head pose written by `XREALGetHeadPoseAtTime`.
///
/// The internal method is `GetHeadPoseAtTime(unsigned long, float*)`, so the output
/// is a flat `float` array. It maps to the NRSDK `NRPose`, whose documented layout is
/// **rotation first** (`NRRotation{x,y,z,w}`) then **position** (`NRPosition{x,y,z}`)
/// — the opposite order from Unity's `Pose`. For the 3DoF MVP only the rotation is used.
///
/// RE: confirm the field order on hardware (log the 7 floats and check which 4 form a
/// unit quaternion).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct NrPose {
    pub qx: f32,
    pub qy: f32,
    pub qz: f32,
    pub qw: f32,
    pub px: f32,
    pub py: f32,
    pub pz: f32,
}

impl NrPose {
    /// Convert the native (Unity/NRSDK, left-handed, Y-up) rotation into a Godot
    /// (right-handed, Y-up) quaternion.
    ///
    /// RE: the exact sign convention must be verified on hardware. Mirroring the Z
    /// axis between the two coordinate systems flips the X/Y quaternion components; if
    /// look-around is inverted on one axis, try the other variants (`(x,y,-z,-w)`,
    /// `(x,-y,z,-w)`, `(-x,y,-z,w)`).
    pub fn to_godot_quaternion(self) -> Quaternion {
        Quaternion::new(-self.qx, -self.qy, self.qz, self.qw)
    }
}

/// `TrackingType` from `XREALPlugin.cs`.
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum TrackingType {
    Mode6Dof = 0,
    Mode3Dof = 1,
    Mode0Dof = 2,
    Mode0DofStab = 3,
}

/// `XREALComponent` from `XREALPlugin.cs` (subset used here).
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum XrealComponent {
    DisplayLeft = 0,
    DisplayRight = 1,
    Head = 6,
    Imu = 7,
}

/// Mirror of the Unity SDK's `UserDefinedSettings` (`XREALXRLoader.cs`), passed by
/// value to `InitUserDefinedSettings`.
///
/// `supportMonoMode` is a C# `bool`; the default P/Invoke struct marshaling promotes it
/// to a 4-byte `BOOL`, so it is an `i32` here to keep the 32-byte layout
/// (`{i32,i32,i32,i32, ptr, i32}`, pointer 8-byte aligned at offset 16).
///
/// RE: verify the bool width / overall size on device if init misbehaves.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UserDefinedSettings {
    pub color_space: i32,
    pub stereo_rendering_mode: i32,
    pub tracking_type: i32,
    pub support_mono_mode: i32,
    pub unity_activity: *mut c_void,
    pub input_source: i32,
}

// ---- Resolved function-pointer types -------------------------------------------------
//
// RE basis (see docs/reverse-engineering.md):
//   - mangled `XREALNativeSessionManager::GetHeadPoseAtTime(unsigned long, float*)`
//   - mangled `XREALNativeSessionManager::GetHMDTimeNanos(unsigned long*)`  <- out-param!
//   - C wrappers tail-call the methods, so the C export return == the method return
//     (NRSDK uniformly returns `NRResult` = i32, 0 on success).

/// `int XREALGetHMDTimeNanos(uint64_t* out_time_ns)` — writes the HMD clock through an
/// out-pointer and returns an NRResult status (`0` = success). NOT a value-returning fn.
pub type FnHmdTimeNanos = unsafe extern "C" fn(*mut u64) -> i32;

/// `int XREALGetHeadPoseAtTime(uint64_t time_ns, NrPose* out)` — NRResult, `0` = success.
pub type FnGetHeadPoseAtTime = unsafe extern "C" fn(u64, *mut NrPose) -> i32;

/// `void XREALLoadAPI(void)` — wires the session-manager perception delegate; must run
/// before pose queries. (Return value, if any, is ignored.)
pub type FnLoadApi = unsafe extern "C" fn();

/// `bool XREALIsSessionStarted(void)`.
pub type FnIsSessionStarted = unsafe extern "C" fn() -> bool;

/// `void UnityPluginLoad(IUnityInterfaces*)` (in `libXREALXRPlugin.so`). Unity's engine
/// calls this at startup; we call it ourselves with a minimal fake `IUnityInterfaces`
/// (see `crate::unity_plugin`) so the plugin's stored interface pointer is non-null before
/// `InitUserDefinedSettings` dereferences it in `DisplayManager::LoadDisplay`.
pub type FnUnityPluginLoad = unsafe extern "C" fn(*mut c_void);

/// `void InitUserDefinedSettings(UserDefinedSettings)` (in `libXREALXRPlugin.so`).
pub type FnInitUserDefinedSettings = unsafe extern "C" fn(UserDefinedSettings);

/// `bool CreateSession(bool directPresent)` (in `libXREALXRPlugin.so`).
pub type FnCreateSession = unsafe extern "C" fn(bool) -> bool;

/// `void RecenterGlasses(void)` (in `libXREALXRPlugin.so`).
pub type FnVoid = unsafe extern "C" fn();

/// `IntPtr GetPluginVersion(void)` (C# DllImport) — a NUL-terminated C string.
pub type FnGetPluginVersion = unsafe extern "C" fn() -> *const c_char;

/// `XREALDeviceType GetDeviceType(void)` (C# DllImport) — enum value as `int`.
pub type FnGetDeviceType = unsafe extern "C" fn() -> i32;

/// `int GetTrackingState()` / `int GetTrackingReason()` / `int GetTrackingType()`
/// (libXREALXRPlugin.so). Read-only enum getters, used for diagnostics.
pub type FnQueryInt = unsafe extern "C" fn() -> i32;

/// `bool SwitchTrackingType(TrackingType type)` (libXREALXRPlugin.so, from
/// `XREALPlugin.cs`). The Unity input-subsystem's perception start calls this; we probe it
/// directly to try to kick perception without the full XR-subsystem host.
pub type FnSwitchTrackingType = unsafe extern "C" fn(i32) -> bool;
