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
        // DEVICE-CONFIRMED field order: the 4 rotation floats are **w-first** (w, x, y, z), NOT
        // (x, y, z, w). At rest the first float ≈ 1.0 (the scalar w) and the rest ≈ 0. So the
        // struct slots map: w=qx, x=qy, y=qz, z=qw.
        let (w, x, y, z) = (self.qx, self.qy, self.qz, self.qw);
        // Unity/NRSDK left-handed Z-forward → Godot right-handed -Z-forward: flip the Z basis,
        // (x, y, z, w) → (-x, -y, z, w). If an axis still reads inverted on device, flip that
        // component's sign (the calibration log prints the raw quaternion + converted Euler).
        Quaternion::new(-x, -y, z, w).normalized()
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
/// out-pointer. RE: SessionManager-style wrappers appear to use `0` as success, while
/// libXREALXRPlugin.so's InputManager export returns bool-style `1` on success.
pub type FnHmdTimeNanos = unsafe extern "C" fn(*mut u64) -> i32;

/// `int XREALGetHeadPoseAtTime(uint64_t time_ns, NrPose* out)` — writes pose to `out`.
/// RE: use this compact 7-float layout only with libXREALNativeSessionManager.so. The
/// libXREALXRPlugin.so export of the same name writes a larger Unity-facing pose block.
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

/// `bool CreateFrame(void)` (in `libXREALXRPlugin.so`).
///
/// RE / unverified: the export is a no-argument trampoline to
/// `DisplayManager::CreateFrame()` and returns `w0` as a boolean success flag.
pub type FnCreateFrame = unsafe extern "C" fn() -> bool;

/// `GetFrameMetaData(void)` (in `libXREALXRPlugin.so`).
///
/// RE / unverified: `DisplayManager::GetFrameMetaData()` returns two register values:
/// metadata pointer and byte count. The data appears to be RGB triplets expanded to RGBA.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XrealFrameMetaData {
    pub ptr: *const c_void,
    pub size: usize,
}
pub type FnGetFrameMetaData = unsafe extern "C" fn() -> XrealFrameMetaData;

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

/// `int ControlSetDisplayBypassPsensorFlag(int flag)` (libXREALXRPlugin.so).
/// RE-confirmed by disassembly: the C wrapper tail-calls
/// `NativeGlasses::ControlSetDisplayBypassPsensorFlag(int)` once `NativeGlasses` is ready
/// (`[NativeGlasses+0x18] != 0`), else no-ops. Setting flag=1 keeps the glasses display on when
/// the proximity (wear) sensor would otherwise power it off after idle.
pub type FnControlSetI32 = unsafe extern "C" fn(i32) -> i32;

// ---- libnr_loader.so rendering path -------------------------------------------------
//
// RE / unverified. These are resolved from libnr_loader.so, based on
// NRRenderingWrapper::InitWrapper in libXREALXRPlugin.so. Keep all direct NR calls behind
// crate::native until the struct and enum layouts are confirmed on hardware.

pub type NrHandle = u64;
pub type NrResult = i32;

pub type FnNrRenderingCreate = unsafe extern "C" fn(*mut NrHandle) -> NrResult;
pub type FnNrRenderingOneHandle = unsafe extern "C" fn(NrHandle) -> NrResult;
pub type FnNrRenderingSetI32 = unsafe extern "C" fn(NrHandle, i32) -> NrResult;
pub type FnNrRenderingGetI32 = unsafe extern "C" fn(NrHandle, *mut i32) -> NrResult;
/// NRGraphicContext: { type: i32 (5=OpenGL ES), _pad: [u8;4], context: *mut c_void (EGLContext) }
#[repr(C)]
pub struct NrGraphicContext {
    pub gfx_type: i32,
    pub _pad: [u8; 4],
    pub context: *mut c_void,
}
pub type FnNrRenderingSetGraphicContext = unsafe extern "C" fn(NrHandle, *const NrGraphicContext) -> NrResult;
pub type FnNrRenderingSetU64 = unsafe extern "C" fn(NrHandle, u64) -> NrResult;
// NRRenderingAcquireFrame: uses NRRendering vtable (0x4904f0) not NRFrame vtable (0x490580).
pub type FnNrRenderingAcquireFrame = unsafe extern "C" fn(NrHandle, *mut NrHandle) -> NrResult;

pub type FnNrBufferSpecCreate = unsafe extern "C" fn(NrHandle, *mut NrHandle) -> NrResult;
pub type FnNrHandleDestroy = unsafe extern "C" fn(NrHandle, NrHandle) -> NrResult;
pub type FnNrBufferSpecSetSize = unsafe extern "C" fn(NrHandle, NrHandle, u32, u32) -> NrResult;
pub type FnNrBufferSpecSetI32 = unsafe extern "C" fn(NrHandle, NrHandle, i32) -> NrResult;
pub type FnNrBufferSpecSetU32 = unsafe extern "C" fn(NrHandle, NrHandle, u32) -> NrResult;
pub type FnNrBufferSpecSetU64 = unsafe extern "C" fn(NrHandle, NrHandle, u64) -> NrResult;

pub type FnNrSwapchainCreate = unsafe extern "C" fn(NrHandle, NrHandle, *mut NrHandle) -> NrResult;
pub type FnNrSwapchainCreateAndroidSurface =
    unsafe extern "C" fn(NrHandle, NrHandle, *mut *mut c_void, *mut *mut c_void) -> NrResult;
pub type FnNrSwapchainSetBuffers =
    unsafe extern "C" fn(NrHandle, NrHandle, u32, *mut *mut c_void) -> NrResult;
pub type FnNrSwapchainGetRecommendBufferCount =
    unsafe extern "C" fn(NrHandle, NrHandle, *mut u32) -> NrResult;

pub type FnNrViewportCreate = unsafe extern "C" fn(NrHandle, *mut NrHandle) -> NrResult;
pub type FnNrViewportSetI32 = unsafe extern "C" fn(NrHandle, NrHandle, i32) -> NrResult;
pub type FnNrViewportSetU32 = unsafe extern "C" fn(NrHandle, NrHandle, u32) -> NrResult;
pub type FnNrViewportSetU64 = unsafe extern "C" fn(NrHandle, NrHandle, u64) -> NrResult;
pub type FnNrViewportSetF32x2 = unsafe extern "C" fn(NrHandle, NrHandle, f32, f32) -> NrResult;
pub type FnNrViewportSetPtr = unsafe extern "C" fn(NrHandle, NrHandle, *const c_void) -> NrResult;
pub type FnNrViewportSetNearFar = unsafe extern "C" fn(NrHandle, NrHandle, f32, f32) -> NrResult;

pub type FnNrFrameCreate = unsafe extern "C" fn(NrHandle, *mut NrHandle) -> NrResult;
pub type FnNrFrameSetBufferViewport =
    unsafe extern "C" fn(NrHandle, NrHandle, u32, NrHandle) -> NrResult;
// 3-arg variant: (rendering, frame, viewport) — no index parameter
pub type FnNrFrameSetBufferViewport3 =
    unsafe extern "C" fn(NrHandle, NrHandle, NrHandle) -> NrResult;
pub type FnNrFrameGetViewportCount =
    unsafe extern "C" fn(NrHandle, NrHandle, *mut u32) -> NrResult;
pub type FnNrFrameGetBufferViewport =
    unsafe extern "C" fn(NrHandle, NrHandle, u32, *mut NrHandle) -> NrResult;
pub type FnNrBufferViewportGetSwapchain =
    unsafe extern "C" fn(NrHandle, NrHandle, *mut NrHandle) -> NrResult;
pub type FnNrFrameNoArgs = unsafe extern "C" fn(NrHandle, NrHandle) -> NrResult;
// NRFrameCompose takes (rendering, frame, ..., ..., flags) — 5 args; pass 0s for unknown ones.
pub type FnNrFrameCompose =
    unsafe extern "C" fn(NrHandle, NrHandle, u64, u64, u32) -> NrResult;
pub type FnNrFrameAcquireBuffers =
    unsafe extern "C" fn(NrHandle, NrHandle, *mut NrHandle, *mut u32) -> NrResult;
pub type FnNrFrameSetColorTextures =
    unsafe extern "C" fn(NrHandle, NrHandle, *const *const c_void, u32) -> NrResult;
pub type FnNrFrameSendMetaData = unsafe extern "C" fn(
    NrHandle,
    NrHandle,
    NrHandle,
    NrHandle,
    *const *const c_void,
    *mut u32,
) -> NrResult;
