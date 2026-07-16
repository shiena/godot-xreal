//! Raw FFI types for the XREAL native C ABI.
//!
//! Signatures here are **confirmed by reverse engineering** the binaries (C++ mangled
//! names + AArch64 disassembly of the C wrappers in `libXREALNativeSessionManager.so`),
//! cross-checked against the Unity SDK's C# `[DllImport]` declarations. See
//! `docs/reference/reverse-engineering.md` for the derivation. Items still flagged `RE` need
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
// RE basis (see docs/reference/reverse-engineering.md):
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

/// `int GetHeadPoseAtTime(uint64_t time_ns, float out[16])` in **libXREALXRPlugin.so**.
///
/// Distinct from the session-manager `XREALGetHeadPoseAtTime`: this exported wrapper
/// (@0x48cc8) tail-calls `InputManager::GetHeadPoseAtTime` @0x7f4a0, which copies a
/// **64-byte / 16-float** block straight from `NativePerception::GetHeadPose`'s struct
/// return — i.e. the *display* subsystem's HMD pose, the exact source the compositor
/// reprojects the glasses layer with (so driving the eye cameras from it should yield a
/// head-locked peek window). Returns 1 on success. Device-pinned layout of the 16 floats:
/// a **4×4 row-major transform** (rotation 3×3 upper-left, position in floats 12/13/14);
/// see the RE map in `docs/archive/multiview-investigation.md`.
pub type FnGetHeadPoseDisplay = unsafe extern "C" fn(u64, *mut [f32; 16]) -> i32;

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

// --- RGB camera (libXREALXRPlugin.so, flat C ABI; see docs/plans/camera-feed-plan.md) ---

/// `NRSize2i` / Unity `Vector2Int` — plane or frame dimensions (RGB camera).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct NrSize2i {
    pub width: i32,
    pub height: i32,
}

/// `StartRGBCameraDataCapture(callback, userData) -> callbackHandle`. Pass a **null** callback
/// (first arg) to drive the camera in poll mode via [`FnTryAcquireLatestImage`]. Returns a handle
/// for [`FnStopRgbCameraCapture`] (`0` on failure).
pub type FnStartRgbCameraCapture = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u64;
/// `StopRGBCameraDataCapture(callbackHandle) -> bool`.
pub type FnStopRgbCameraCapture = unsafe extern "C" fn(u64) -> bool;
/// `TryAcquireLatestImage(&frameHandle, &resolution, &timeStamp) -> bool`. On success `frameHandle`
/// must be released with [`FnDisposeRgbCameraDataHandle`].
pub type FnTryAcquireLatestImage = unsafe extern "C" fn(*mut i32, *mut NrSize2i, *mut u64) -> bool;
/// `TryGetRGBCameraDataPlane(frameHandle, planeIndex, &dataPtr, &size) -> bool`. Planes are I420:
/// 0 = Y (full-res), 1 = V, 2 = U (half-res); each is tightly packed 8-bit (`size.width*size.height`
/// bytes). The pointer is valid until the handle is disposed.
pub type FnTryGetRgbCameraDataPlane =
    unsafe extern "C" fn(i32, i32, *mut *mut c_void, *mut NrSize2i) -> bool;
/// `DisposeRGBCameraDataHandle(frameHandle)` — free a frame acquired by [`FnTryAcquireLatestImage`].
pub type FnDisposeRgbCameraDataHandle = unsafe extern "C" fn(i32);

/// `GlassesEventData` from `XREALCallbackHandler.cs`, delivered **by value** to the
/// callback registered with `SetGlassesEventCallback` (libXREALXRPlugin.so export,
/// C# `[DllImport] SetGlassesEventCallback(XREALGlassesEventCallback)`).
///
/// 16 bytes `{i32, u32, u32, f32}` — on AArch64 AAPCS a ≤16-byte composite is passed in
/// x0/x1, which Rust's `extern "C"` handles for a `#[repr(C)]` struct.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GlassesEventData {
    /// `XREALActionType` (see the `ACTION_TYPE_*` constants below).
    pub action_type: i32,
    pub para: u32,
    pub para2: u32,
    pub para3: f32,
}

// `XREALActionType` values dispatched in `node.rs` (full enum in `XREALCallbackHandler.cs`).
pub const ACTION_TYPE_CLICK: i32 = 1;
pub const ACTION_TYPE_DOUBLE_CLICK: i32 = 2;
pub const ACTION_TYPE_LONG_PRESS: i32 = 3;
pub const ACTION_TYPE_INCREASE_BRIGHTNESS: i32 = 6;
pub const ACTION_TYPE_DECREASE_BRIGHTNESS: i32 = 7;
pub const ACTION_TYPE_INCREASE_VOLUME: i32 = 8;
pub const ACTION_TYPE_DECREASE_VOLUME: i32 = 9;
pub const ACTION_TYPE_NEXT_EC_LEVEL: i32 = 12;
pub const ACTION_TYPE_KEY_STATE: i32 = 2023;
pub const ACTION_TYPE_PROXIMITY_WEARING_STATE: i32 = 2024;

// `XREALWearingStatus` values (para of ACTION_TYPE_PROXIMITY_WEARING_STATE).
pub const WEARING_STATUS_PUT_ON: u32 = 1;
pub const WEARING_STATUS_TAKE_OFF: u32 = 2;

/// The callback passed to `SetGlassesEventCallback`. Invoked from an SDK-owned thread —
/// implementations must not touch Godot objects; queue and drain on the main thread.
pub type FnGlassesEventCallback = extern "C" fn(GlassesEventData);

/// `void SetGlassesEventCallback(XREALGlassesEventCallback cb)` (libXREALXRPlugin.so).
pub type FnSetGlassesEventCallback = unsafe extern "C" fn(FnGlassesEventCallback);

/// The callback passed to `SetNativeErrorCallback`: `void(XREALErrorCode code, const char* msg)`
/// (`XREALCallbackHandler.cs`). `code` is the `XREALErrorCode` enum as an i32; `msg` is a UTF-8 C
/// string (may be null). Invoked from an SDK-owned thread — no Godot calls; cache and poll.
pub type FnNativeErrorCallback = extern "C" fn(i32, *const c_char);

/// `void SetNativeErrorCallback(XREALErrorCallback cb)` (libXREALXRPlugin.so).
pub type FnSetNativeErrorCallback = unsafe extern "C" fn(FnNativeErrorCallback);

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
pub type FnNrRenderingSetGraphicContext =
    unsafe extern "C" fn(NrHandle, *const NrGraphicContext) -> NrResult;
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
pub type FnNrFrameGetViewportCount = unsafe extern "C" fn(NrHandle, NrHandle, *mut u32) -> NrResult;
pub type FnNrFrameGetBufferViewport =
    unsafe extern "C" fn(NrHandle, NrHandle, u32, *mut NrHandle) -> NrResult;
pub type FnNrBufferViewportGetSwapchain =
    unsafe extern "C" fn(NrHandle, NrHandle, *mut NrHandle) -> NrResult;
pub type FnNrFrameNoArgs = unsafe extern "C" fn(NrHandle, NrHandle) -> NrResult;
// NRFrameCompose takes (rendering, frame, ..., ..., flags) — 5 args; pass 0s for unknown ones.
pub type FnNrFrameCompose = unsafe extern "C" fn(NrHandle, NrHandle, u64, u64, u32) -> NrResult;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn euler_deg(pose: NrPose) -> (f32, f32, f32) {
        let e = pose.to_godot_quaternion().get_euler();
        let k = 180.0 / std::f32::consts::PI;
        (e.x * k, e.y * k, e.z * k)
    }

    /// Locks the exact conversion formula: the 4 rotation floats are **w-first** (w, x, y, z) and
    /// the handedness flip is (x, y, z, w) -> (-x, -y, z, w). Regressing to (x, y, z, w) order or a
    /// different flip breaks this.
    #[test]
    fn field_order_is_w_first_with_z_flip() {
        let pose = NrPose {
            qx: 0.1,
            qy: 0.2,
            qz: 0.3,
            qw: 0.4,
            ..Default::default()
        };
        let q = pose.to_godot_quaternion();
        // (w, x, y, z) = (qx, qy, qz, qw) = (0.1, 0.2, 0.3, 0.4) -> Godot (-0.2, -0.3, 0.4, 0.1).
        let expected = Quaternion::new(-0.2, -0.3, 0.4, 0.1).normalized();
        assert!(
            (q.x - expected.x).abs() < 1e-5,
            "x: {} vs {}",
            q.x,
            expected.x
        );
        assert!(
            (q.y - expected.y).abs() < 1e-5,
            "y: {} vs {}",
            q.y,
            expected.y
        );
        assert!(
            (q.z - expected.z).abs() < 1e-5,
            "z: {} vs {}",
            q.z,
            expected.z
        );
        assert!(
            (q.w - expected.w).abs() < 1e-5,
            "w: {} vs {}",
            q.w,
            expected.w
        );
    }

    /// At rest the first float is the scalar w (~1), so the pose must be (near) identity — NOT the
    /// 180-degree-about-X rotation that reading it as (x, y, z, w) would produce. This is the exact
    /// bug the w-first fix corrected.
    #[test]
    fn rest_pose_is_identity_not_180() {
        let pose = NrPose {
            qx: 1.0,
            qy: 0.0,
            qz: 0.0,
            qw: 0.0,
            ..Default::default()
        };
        let (x, y, z) = euler_deg(pose);
        assert!(
            x.abs() < 0.5 && y.abs() < 0.5 && z.abs() < 0.5,
            "expected identity, got ({x},{y},{z})"
        );
    }

    /// Each NRSDK rotation axis (encoded w-first) maps to the matching Godot Euler axis.
    #[test]
    fn axis_mapping_pitch_yaw_roll() {
        // 30-degree rotation about each NRSDK axis: (w=cos15, <axis>=sin15).
        let (c, s) = (15f32.to_radians().cos(), 15f32.to_radians().sin());
        // NRSDK pitch = about x (float index 1 = qy) -> Godot Euler dominated by X.
        let (x, y, z) = euler_deg(NrPose {
            qx: c,
            qy: s,
            ..Default::default()
        });
        assert!(
            x.abs() > 25.0 && y.abs() < 2.0 && z.abs() < 2.0,
            "pitch -> ({x},{y},{z})"
        );
        // NRSDK yaw = about y (float index 2 = qz) -> Godot Euler dominated by Y.
        let (x, y, z) = euler_deg(NrPose {
            qx: c,
            qz: s,
            ..Default::default()
        });
        assert!(
            y.abs() > 25.0 && x.abs() < 2.0 && z.abs() < 2.0,
            "yaw -> ({x},{y},{z})"
        );
        // NRSDK roll = about z (float index 3 = qw) -> Godot Euler dominated by Z.
        let (x, y, z) = euler_deg(NrPose {
            qx: c,
            qw: s,
            ..Default::default()
        });
        assert!(
            z.abs() > 25.0 && x.abs() < 2.0 && y.abs() < 2.0,
            "roll -> ({x},{y},{z})"
        );
    }
}
