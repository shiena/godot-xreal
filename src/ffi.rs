//! Raw FFI types for the XREAL native C ABI.
//!
//! Signatures here are **confirmed by reverse engineering** the binaries (C++ mangled
//! names + AArch64 disassembly of the C wrappers in `libXREALNativeSessionManager.so`),
//! cross-checked against the Unity SDK's C# `[DllImport]` declarations. See
//! `docs/develop/reference/reverse-engineering.md` for the derivation. Items still flagged `RE` need
//! on-device confirmation.

use std::ffi::{c_char, c_void};

use godot::builtin::Quaternion;

/// Native head pose written by `XREALGetHeadPoseAtTime`.
///
/// The internal method is `GetHeadPoseAtTime(unsigned long, float*)`, so the output
/// is a flat `float` array. It maps to the NRSDK `NRPose`, whose documented layout puts
/// **rotation first**, `NRRotation{x,y,z,w}`, then **position**, `NRPosition{x,y,z}`,
/// the opposite order from Unity's `Pose`.
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
    /// The sign convention is device-confirmed below. Mirroring the Z axis between the two
    /// coordinate systems flips the X/Y quaternion components; if look-around ever inverts on
    /// one axis, try the other variants (`(x,y,-z,-w)`, `(x,-y,z,-w)`, `(-x,y,-z,w)`).
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
// RE basis (see docs/develop/reference/reverse-engineering.md):
//   - mangled `XREALNativeSessionManager::GetHeadPoseAtTime(unsigned long, float*)`
//   - mangled `XREALNativeSessionManager::GetHMDTimeNanos(unsigned long*)`  <- out-param!
//   - C wrappers tail-call the methods, so the C export return == the method return
//     (NRSDK uniformly returns `NRResult` = i32, 0 on success).

/// `int XREALGetHMDTimeNanos(uint64_t* out_time_ns)` writes the HMD clock through an
/// out-pointer. From the RE: SessionManager-style wrappers appear to use `0` as success, while
/// libXREALXRPlugin.so's InputManager export returns a bool-style `1` on success.
pub type FnHmdTimeNanos = unsafe extern "C" fn(*mut u64) -> i32;

/// `int XREALGetHeadPoseAtTime(uint64_t time_ns, NrPose* out)` writes the pose to `out`.
/// From the RE: use this compact 7-float layout only with libXREALNativeSessionManager.so. The
/// libXREALXRPlugin.so export of the same name writes a larger Unity-facing pose block.
pub type FnGetHeadPoseAtTime = unsafe extern "C" fn(u64, *mut NrPose) -> i32;

/// `int GetHeadPoseAtTime(uint64_t time_ns, float out[16])` in **libXREALXRPlugin.so**.
///
/// This is distinct from the session-manager `XREALGetHeadPoseAtTime`: the exported wrapper
/// @0x48cc8 tail-calls `InputManager::GetHeadPoseAtTime` @0x7f4a0, which copies a
/// **64-byte, 16-float** block straight from `NativePerception::GetHeadPose`'s struct
/// return. That is the *display* subsystem's HMD pose, the exact source the compositor
/// reprojects the glasses layer with, so driving the eye cameras from it should yield a
/// head-locked peek window. It returns 1 on success. The device-pinned layout of the 16 floats is
/// a **4x4 row-major transform**, with the rotation in the upper-left 3x3 and the position in
/// floats 12, 13 and 14; see the RE map in `docs/develop/archive/multiview-investigation.md`.
pub type FnGetHeadPoseDisplay = unsafe extern "C" fn(u64, *mut [f32; 16]) -> i32;

/// `void XREALLoadAPI(void)` wires the session-manager perception delegate and has to run
/// before any pose query. Its return value, if it has one, is ignored.
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

/// `IntPtr GetPluginVersion(void)`, a C# DllImport, returning a NUL-terminated C string.
pub type FnGetPluginVersion = unsafe extern "C" fn() -> *const c_char;

/// `XREALDeviceType GetDeviceType(void)`, a C# DllImport, returning the enum value as an `int`.
pub type FnGetDeviceType = unsafe extern "C" fn() -> i32;

/// `int GetTrackingState()` / `int GetTrackingReason()` / `int GetTrackingType()`
/// (libXREALXRPlugin.so). Read-only enum getters, used for diagnostics.
pub type FnQueryInt = unsafe extern "C" fn() -> i32;

/// `bool SwitchTrackingType(TrackingType type)` (libXREALXRPlugin.so, from
/// `XREALPlugin.cs`). The Unity input-subsystem's perception start calls this; we probe it
/// directly to try to kick perception without the full XR-subsystem host.
pub type FnSwitchTrackingType = unsafe extern "C" fn(i32) -> bool;

// --- RGB camera (libXREALXRPlugin.so, flat C ABI; see docs/develop/plans/camera-feed-plan.md) ---

/// `NRSize2i`, Unity's `Vector2Int`: plane or frame dimensions for the RGB camera.
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
/// `DisposeRGBCameraDataHandle(frameHandle)` frees a frame acquired by
/// [`FnTryAcquireLatestImage`].
pub type FnDisposeRgbCameraDataHandle = unsafe extern "C" fn(i32);

// --- Plane detection (libXREALXRPlugin.so, flat C ABI; see docs/develop/plans/ar-features-plan.md) ---
//
// Source: `XREALPlaneSubsystem.cs` `[DllImport]` + demangled `InputManager::*` internals. Needs a
// 6DoF session. Poses are in **Unity space** (left-handed) and need the same conversion the head/hand
// poses use (`(x, -y, -z)` / quaternion `(-x, -y, z, w)`).

/// AR Foundation's `TrackableId`, a 128-bit id of `m_SubId1` and `m_SubId2`. It is passed **by
/// value**, 16 bytes in AArch64 x0 and x1, into the boundary and anchor calls.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug)]
pub struct TrackableId {
    pub sub_id_1: u64,
    pub sub_id_2: u64,
}

/// Unity's `Pose`: a position `Vector3` then a rotation `Quaternion` of x, y, z, w. It is 28 bytes,
/// with no padding.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct UnityPose {
    pub position: [f32; 3],
    pub rotation: [f32; 4],
}

/// `UnityXRVector3`, three floats passed **by value**. AArch64 classifies it as a homogeneous
/// floating-point aggregate, so two of them arrive in `s0-s2` and `s3-s5` rather than through
/// memory. The components are named rather than an `[f32; 3]` so the call sites read clearly; the
/// ABI classification is the same either way.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct UnityVector3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

/// `void SetFocusPlane(UnityXRVector3 point, UnityXRVector3 normal)`: the plane the compositor
/// reprojects the rendered frame against, in **head-local Unity space**. Both arguments are by
/// value; the export tail-calls `DisplayManager::SetFocusPlane(UnityXRVector3, UnityXRVector3)`,
/// which is where the argument count comes from (the disassembly saves exactly `v0`-`v5`).
///
/// Note it takes **two** vectors, where Unity's own `XRDisplaySubsystem.SetFocusPlane` takes a
/// third `velocity`. The SDK's C# goes through Unity's wrapper, which drops it before this point.
/// Without a call the compositor holds its default plane at 1.4 m, which is what makes content at
/// other distances judder.
pub type FnSetFocusPlane = unsafe extern "C" fn(UnityVector3, UnityVector3);

/// `ARSubsystemChanges`, from `XREALPlaneSubsystem.cs:86`: the added, updated and removed poll shape
/// shared by planes, images and anchors. The pointers index native arrays of `element_size`-byte
/// elements and stay valid only until the next poll, so copy out immediately.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ArSubsystemChanges {
    pub added_ptr: *const c_void,
    pub added_count: i32,
    pub updated_ptr: *const c_void,
    pub updated_count: i32,
    pub removed_ptr: *const c_void,
    pub removed_count: i32,
    /// Native element stride, used to walk the arrays by offset. It stays robust against AR Foundation
    /// version differences in the trailing struct fields.
    pub element_size: i32,
}

impl Default for ArSubsystemChanges {
    fn default() -> Self {
        Self {
            added_ptr: std::ptr::null(),
            added_count: 0,
            updated_ptr: std::ptr::null(),
            updated_count: 0,
            removed_ptr: std::ptr::null(),
            removed_count: 0,
            element_size: 0,
        }
    }
}

/// Field offsets within this SDK build's `BoundedPlane` element (**device-confirmed write offsets**,
/// element_size = **104**; note `center` precedes `pose` here, unlike stock AR Foundation):
/// `[trackableId:16][subsumedById:16][center:8][pose:28][size:8][alignment:4][trackingState:4]…`
/// `alignment` is `100` (horizontal) / `200` (vertical). See `docs/develop/plans/ar-features-plan.md`.
pub mod bounded_plane {
    pub const TRACKABLE_ID: usize = 0x00;
    pub const CENTER: usize = 0x20;
    pub const POSE: usize = 0x28;
    pub const SIZE: usize = 0x44;
    pub const ALIGNMENT: usize = 0x4c;
    pub const TRACKING_STATE: usize = 0x50;
    /// Expected `ArSubsystemChanges::element_size` for a `BoundedPlane` (assert at runtime).
    pub const ELEMENT_SIZE: i32 = 104;
}

/// `PlaneDetectionMode` (AR Foundation, `[Flags]`): bit 0 = horizontal, bit 1 = vertical.
pub mod plane_detection_mode {
    pub const NONE: i32 = 0;
    pub const HORIZONTAL: i32 = 1;
    pub const VERTICAL: i32 = 2;
    pub const BOTH: i32 = 3;
}

/// `PlaneDetectionMode GetPlaneDetectionMode()` returns the active detection-mode flags.
pub type FnGetPlaneDetectionMode = unsafe extern "C" fn() -> i32;
/// `bool SetPlaneDetectionMode(PlaneDetectionMode)` enables horizontal and vertical detection.
pub type FnSetPlaneDetectionMode = unsafe extern "C" fn(i32) -> bool;
/// `void GetPlaneDetectionChanges(out ARSubsystemChanges)` returns the added, updated and removed
/// `BoundedPlane`s.
pub type FnGetPlaneDetectionChanges = unsafe extern "C" fn(*mut ArSubsystemChanges);
/// `int GetPlaneBoundaryVertexCount(TrackableId)` returns the boundary-polygon vertex count.
pub type FnGetPlaneBoundaryVertexCount = unsafe extern "C" fn(TrackableId) -> i32;
/// `void GetPlaneBoundaryVertexData(TrackableId, void* out)` writes `count` plane-local `Vector2`s.
pub type FnGetPlaneBoundaryVertexData = unsafe extern "C" fn(TrackableId, *mut c_void);

/// `XREALSupportedFeature`, from `XREALPlugin.cs`: the per-device capability queried by
/// [`FnIsHmdFeatureSupported`]. The SDK uses `RGB_CAMERA` to gate the camera pipeline, so the Air 2
/// Ultra, which has no RGB camera, reports `false` and never opens it; see
/// `XREALCameraInitializer.cs`.
pub mod hmd_feature {
    pub const RGB_CAMERA: i32 = 1;
    pub const WEARING_STATUS: i32 = 2;
    pub const CONTROLLER: i32 = 3;
    pub const HEAD_TRACKING_ROTATION: i32 = 4;
    pub const HEAD_TRACKING_POSITION: i32 = 5;
}
/// `bool IsHMDFeatureSupported(XREALSupportedFeature)` reports whether the connected glasses
/// support an `hmd_feature`. It is the correct, device-accurate gate before opening the RGB camera
/// and the like.
pub type FnIsHmdFeatureSupported = unsafe extern "C" fn(i32) -> bool;

/// `XREALComponent` device ids for the geometry APIs below. They are distinct from
/// [`hmd_feature`]: here the RGB camera is `2`, not `1`. See
/// docs/develop/plans/coordinate-systems-notes.md.
pub mod component {
    pub const DISPLAY_LEFT: i32 = 0;
    pub const DISPLAY_RIGHT: i32 = 1;
    pub const RGB_CAMERA: i32 = 2;
    pub const GRAYSCALE_CAMERA_LEFT: i32 = 3;
    pub const GRAYSCALE_CAMERA_RIGHT: i32 = 4;
    /// Completes the `XREALComponent` enum for reference; no geometry getter targets it, so it is unused.
    #[allow(dead_code)]
    pub const MAGNETIC: i32 = 5;
}
// --- Device and camera geometry, the libXREALXRPlugin.so C exports, in Unity space. The export
// symbols were confirmed with `llvm-objdump -T`. See docs/develop/plans/coordinate-systems-notes.md. ---
// `GetDevicePoseFromHead(component, &pose) -> bool`. `pose` is a Unity `Pose`: the position
// `[x,y,z]` then the rotation quaternion `[x,y,z,w]`, 7 floats in all, giving the device's
// extrinsic relative to Head in Unity space.
pub type FnGetDevicePoseFromHead = unsafe extern "C" fn(i32, *mut [f32; 7]) -> bool;
/// `GetDeviceResolution(component, &size) -> bool` returns the pixel resolution as an `NrSize2i`,
/// which is Unity's `Vector2Int`.
pub type FnGetDeviceResolution = unsafe extern "C" fn(i32, *mut NrSize2i) -> bool;
/// `GetCameraIntrinsic(component, &focalLength, &principalPoint) -> bool` returns
/// `focalLength=(fx,fy)` and `principalPoint=(cx,cy)` in pixels, each a Unity `Vector2` of 2
/// floats.
pub type FnGetCameraIntrinsic = unsafe extern "C" fn(i32, *mut [f32; 2], *mut [f32; 2]) -> bool;
/// `GetCameraProjectionMatrix(component, z_near, z_far, &mat) -> bool` returns a 4x4 projection
/// matrix, 16 floats in Unity's column-major `Matrix4x4`.
pub type FnGetCameraProjectionMatrix = unsafe extern "C" fn(i32, f32, f32, *mut [f32; 16]) -> bool;

// --- Spatial anchors, the libXREALXRPlugin.so flat C exports; see
// docs/develop/plans/ar-features-plan.md --------
// The sources are `XREALAnchorSubsystem.cs`'s `[DllImport]` plus the demangled `InputManager::*`
// internals. They need a 6DoF session AND the vendored `nr_spatial_anchor.aar` backend `.so`. The
// poses are in Unity space, so convert them like the plane and hand poses. The changes-poll reuses
// [`ArSubsystemChanges`], and `removed` is a `TrackableId[]`.

/// .NET's `System.Guid`, a 128-bit persistence key for a saved anchor. It is an opaque 16-byte
/// blob, passed by value in 2 GPRs into [`FnLoadTrackableAnchor`] and written out by
/// [`FnSaveTrackableAnchor`].
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug)]
pub struct Guid {
    pub lo: u64,
    pub hi: u64,
}

/// Field offsets within this SDK build's `XRTrackedAnchor` element (`element_size = 72`). Layout
/// derived from the disassembly of `InputManager::AcquireNewTrackableAnchor` / `LoadTrackableAnchor`:
/// `[trackableId:16][pose:28][trackingState:4][nativePtr:8][sessionId(Guid):16]`. See
/// `docs/develop/plans/ar-features-plan.md`.
pub mod xr_anchor {
    pub const TRACKABLE_ID: usize = 0x00;
    pub const POSE: usize = 0x10;
    pub const TRACKING_STATE: usize = 0x2c;
    pub const SESSION_ID: usize = 0x38;
    /// Expected `ArSubsystemChanges::element_size` for an `XRTrackedAnchor` (assert at runtime).
    pub const ELEMENT_SIZE: i32 = 72;
}

/// Anchor-quality estimate returned by [`FnEstimateTrackableAnchorQuality`] (require ≥ `SUFFICIENT`
/// before saving). `XREALAnchorSubsystem.cs` `NRTrackableAnchorQuality`.
pub mod anchor_quality {
    pub const INSUFFICIENT: i32 = 0;
    pub const SUFFICIENT: i32 = 1;
    pub const GOOD: i32 = 2;
}

/// `void SetAnchorMappingFileDirectory(const char* dir)` sets where saved-anchor maps are
/// persisted.
pub type FnSetAnchorMappingFileDirectory = unsafe extern "C" fn(*const c_char);
/// `void SetTrackableAnchorEnabled(bool)` turns the anchor subsystem on or off; call it before use.
pub type FnSetTrackableAnchorEnabled = unsafe extern "C" fn(bool);
/// `bool AcquireNewTrackableAnchor(UnityPose pose, XRTrackedAnchor* out)` creates an anchor at a
/// pose. `UnityPose`, 28 bytes and not an HFA, is passed **indirectly** by the ABI, so declare it by
/// value and Rust matches.
pub type FnAcquireNewTrackableAnchor = unsafe extern "C" fn(UnityPose, *mut c_void) -> bool;
/// `void GetTrackableAnchorChanges(out ARSubsystemChanges)` returns the added, updated and removed
/// `XRTrackedAnchor`s.
pub type FnGetTrackableAnchorChanges = unsafe extern "C" fn(*mut ArSubsystemChanges);
/// `bool SaveTrackableAnchor(TrackableId, Guid* out)` persists an anchor and writes its `Guid` key.
pub type FnSaveTrackableAnchor = unsafe extern "C" fn(TrackableId, *mut Guid) -> bool;
/// `bool LoadTrackableAnchor(Guid, XRTrackedAnchor* out)` restores a saved anchor by its `Guid`.
pub type FnLoadTrackableAnchor = unsafe extern "C" fn(Guid, *mut c_void) -> bool;
/// `bool RemoveTrackableAnchor(TrackableId)` drops a tracked anchor.
pub type FnRemoveTrackableAnchor = unsafe extern "C" fn(TrackableId) -> bool;
/// `bool RemapTrackableAnchor(TrackableId)` re-localizes an anchor into the current map.
pub type FnRemapTrackableAnchor = unsafe extern "C" fn(TrackableId) -> bool;
/// `bool EstimateTrackableAnchorQuality(TrackableId, UnityPose, i32* out)` returns a save-quality
/// estimate, an `anchor_quality`.
pub type FnEstimateTrackableAnchorQuality =
    unsafe extern "C" fn(TrackableId, UnityPose, *mut i32) -> bool;

// --- Image tracking (libXREALXRPlugin.so flat C exports; see docs/develop/plans/ar-features-plan.md) --------
// Source: `XREALImageTrackingSubsystem.cs` / `XREALImageDatabase.cs` + disassembly. Needs a 6DoF
// session AND the vendored `nr_image_tracking.aar` backend + an `assets/nr_plugins.json` entry + a
// reference-image DB blob built by the `trackableImageTools` CLI. The changes-poll reuses
// [`ArSubsystemChanges`]; `removed` is `TrackableId[]`.

/// `NativeView { void* data; int count; }`, 16 bytes: a pointer and length view over a managed
/// buffer, passed **by value** in 2 GPRs, with `data` in the first and `count` in the low 32 bits of
/// the second.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NativeView {
    pub data: *const c_void,
    pub count: i32,
}

/// `ManagedReferenceImage`, 56 bytes with `StructLayout.Sequential`: one entry of the second
/// `NativeView` into [`FnInitImageTrackingDatabase`], mapping a baked image `guid` to its metadata.
/// `name` and `texture` are Unity `GCHandle` and pointer fields, so pass null from Godot, and `guid`
/// has to match the blob's baked guid.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ManagedReferenceImage {
    pub guid: Guid,
    pub texture_guid: Guid,
    pub size: [f32; 2],
    pub name: *const c_void,
    pub texture: *const c_void,
}

/// Field offsets within this SDK build's `XRTrackedImage` element (`element_size = 80`), DEVICE-CONFIRMED
/// by disassembling `TrackableChanges<…XRTrackedImage>::GetTrackableChanges`' element writer:
/// `[trackableId:16][sourceImageId(Guid):16][pose:28 (pos@0x20, rot@0x2c)][size:8][trackingState:4][ptr:8]`.
pub mod xr_tracked_image {
    pub const TRACKABLE_ID: usize = 0x00;
    pub const SOURCE_IMAGE_ID: usize = 0x10;
    pub const POSE: usize = 0x20;
    pub const SIZE: usize = 0x3c;
    pub const TRACKING_STATE: usize = 0x44;
    /// Expected `ArSubsystemChanges::element_size` for an `XRTrackedImage` (assert at runtime).
    pub const ELEMENT_SIZE: i32 = 80;
}

/// `void SetImageTrackingDatabase(u64 handle)` activates the database from
/// [`FnInitImageTrackingDatabase`]; pass `0` to disable image tracking.
pub type FnSetImageTrackingDatabase = unsafe extern "C" fn(u64);
/// `void GetImageTrackingChanges(out ARSubsystemChanges)` returns the added, updated and removed
/// `XRTrackedImage`s.
pub type FnGetImageTrackingChanges = unsafe extern "C" fn(*mut ArSubsystemChanges);
/// `u64 InitImageTrackingDatabase(NativeView database, NativeView managedReferenceImages)` builds a
/// tracking DB from the blob and its metadata, returning an opaque handle for the calls below. The
/// two 16-byte `NativeView`s are passed by value in GPR pairs, confirmed on device.
pub type FnInitImageTrackingDatabase = unsafe extern "C" fn(NativeView, NativeView) -> u64;
/// `int GetReferenceImageCount(u64 handle)` returns the number of reference images in a database.
pub type FnGetReferenceImageCount = unsafe extern "C" fn(u64) -> i32;
/// `void ReleaseImageTrackingDatabase(u64 handle)` frees a database.
pub type FnReleaseImageTrackingDatabase = unsafe extern "C" fn(u64);

/// `GlassesEventData` from `XREALCallbackHandler.cs`, delivered **by value** to the
/// callback registered with `SetGlassesEventCallback` (libXREALXRPlugin.so export,
/// C# `[DllImport] SetGlassesEventCallback(XREALGlassesEventCallback)`).
///
/// It is 16 bytes of `{i32, u32, u32, f32}`, and on AArch64 AAPCS a composite of 16 bytes or less
/// is passed in x0 and x1, which Rust's `extern "C"` handles for a `#[repr(C)]` struct.
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

/// The callback passed to `SetGlassesEventCallback`. It is invoked from an SDK-owned thread, so an
/// implementation must not touch Godot objects: queue there and drain on the main thread.
pub type FnGlassesEventCallback = extern "C" fn(GlassesEventData);

/// `void SetGlassesEventCallback(XREALGlassesEventCallback cb)` (libXREALXRPlugin.so).
pub type FnSetGlassesEventCallback = unsafe extern "C" fn(FnGlassesEventCallback);

/// The callback passed to `SetNativeErrorCallback`: `void(XREALErrorCode code, const char* msg)`,
/// from `XREALCallbackHandler.cs`. `code` is the `XREALErrorCode` enum as an i32, and `msg` is a
/// UTF-8 C string that may be null. It is invoked from an SDK-owned thread, so make no Godot calls:
/// cache the values and poll them.
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

    /// At rest the first float is the scalar w, near 1, so the pose has to be near identity, and NOT
    /// the 180-degree rotation about X that reading it as (x, y, z, w) would produce. That is the exact
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
