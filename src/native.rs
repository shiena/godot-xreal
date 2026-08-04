//! Runtime binding to the vendored XREAL `.so` libraries via `dlopen`/`dlsym`.
//!
//! We deliberately avoid linking the XREAL libraries at build time: they only exist for
//! Android arm64, and keeping them out of the link line lets the same GDExtension load
//! in a desktop editor (where [`XrealNative::load`] simply returns `Err` and the node
//! no-ops).
//!
//! Symbols are resolved once and the owning [`libloading::Library`] handles are kept
//! alive in the struct for the lifetime of the resolved function pointers.

// FFI module: retains RE bindings/probes kept for completeness (unused on the active path), and
// desktop never loads the `.so`. Allow dead code on both targets.
#![allow(dead_code)]

use libloading::Library;

use std::ffi::c_void;

use crate::ffi::{
    bounded_plane, xr_anchor, xr_tracked_image, ArSubsystemChanges, FnAcquireNewTrackableAnchor,
    FnEstimateTrackableAnchorQuality, FnGetImageTrackingChanges, FnGetPlaneBoundaryVertexCount,
    FnGetPlaneBoundaryVertexData, FnGetPlaneDetectionChanges, FnGetPlaneDetectionMode,
    FnGetReferenceImageCount, FnGetTrackableAnchorChanges, FnInitImageTrackingDatabase,
    FnIsHmdFeatureSupported, FnLoadTrackableAnchor, FnReleaseImageTrackingDatabase,
    FnRemapTrackableAnchor, FnRemoveTrackableAnchor, FnSaveTrackableAnchor,
    FnSetAnchorMappingFileDirectory, FnSetFocusPlane, FnSetImageTrackingDatabase,
    FnSetPlaneDetectionMode, FnSetTrackableAnchorEnabled, Guid, ManagedReferenceImage, NativeView,
    TrackableId, UnityPose, UnityVector3,
};
use crate::ffi::{
    FnControlSetI32, FnCreateFrame, FnCreateSession, FnGetCameraIntrinsic,
    FnGetCameraProjectionMatrix, FnGetDevicePoseFromHead, FnGetDeviceResolution, FnGetDeviceType,
    FnGetFrameMetaData, FnGetHeadPoseAtTime, FnGetHeadPoseDisplay, FnGetPluginVersion,
    FnGlassesEventCallback, FnHmdTimeNanos, FnInitUserDefinedSettings, FnIsSessionStarted,
    FnLoadApi, FnNrRenderingCreate, FnNrRenderingOneHandle, FnQueryInt, FnSetGlassesEventCallback,
    FnSetNativeErrorCallback, FnSwitchTrackingType, FnUnityPluginLoad, FnVoid, NrHandle, NrPose,
    UserDefinedSettings,
};
use crate::ffi::{
    FnDisposeRgbCameraDataHandle, FnStartRgbCameraCapture, FnStopRgbCameraCapture,
    FnTryAcquireLatestImage, FnTryGetRgbCameraDataPlane, NrSize2i,
};

const SESSION_LIB: &str = "libXREALNativeSessionManager.so";
const PLUGIN_LIB: &str = "libXREALXRPlugin.so";
const NR_LOADER_LIB: &str = "libnr_loader.so";

/// Upper bound on a single AR change-array count. The SDK's change pointers alias internal vectors;
/// a stale/garbage count (e.g. read during an internal update) would otherwise drive an out-of-bounds
/// read. Real scenes have at most a handful of planes/anchors, so anything past this is treated as 0.
const MAX_TRACKABLES: i32 = 1024;

/// Upper bound on a plane's boundary vertex count. The count and the data write are two separate SDK
/// calls, so an out-of-range count is refused rather than driving a huge alloc / letting the write
/// overrun. Real boundary polygons have at most a handful of vertices.
const MAX_BOUNDARY_VERTS: i32 = 1 << 16;

/// Clamp a change-array count to a sane range (`0..=MAX_TRACKABLES`); negative/oversized → 0 + warn.
fn sane_count(count: i32, what: &str) -> i32 {
    if (0..=MAX_TRACKABLES).contains(&count) {
        count
    } else {
        godot::global::godot_warn!("[xreal] {what} change count {count} out of range; skipping");
        0
    }
}

/// A detected plane sampled from the plane-detection changes. The `pose` is in **Unity space**, so
/// convert on the Godot side, to `(x, -y, -z)` and quaternion `(-x, -y, z, w)`. `center` and `size`
/// are plane-local.
#[derive(Clone, Copy, Debug)]
pub struct PlaneSample {
    pub id: TrackableId,
    pub pose: UnityPose,
    pub center: [f32; 2],
    pub size: [f32; 2],
    pub alignment: i32,
}

/// Added / updated / removed planes from one [`XrealNative::poll_plane_changes`] call.
pub struct PlaneChanges {
    pub added: Vec<PlaneSample>,
    pub updated: Vec<PlaneSample>,
    pub removed: Vec<TrackableId>,
}

/// Read `count` `BoundedPlane`s from a native array of `stride`-byte elements, pulling the stable
/// leading fields at the [`bounded_plane`] offsets. `ptr` must be valid for `count * stride` bytes.
fn read_planes(ptr: *const c_void, count: i32, stride: usize) -> Vec<PlaneSample> {
    if ptr.is_null() || count <= 0 {
        return Vec::new();
    }
    let base = ptr as *const u8;
    (0..count as usize)
        .map(|i| unsafe {
            let e = base.add(i * stride);
            PlaneSample {
                id: std::ptr::read_unaligned(
                    e.add(bounded_plane::TRACKABLE_ID) as *const TrackableId
                ),
                pose: std::ptr::read_unaligned(e.add(bounded_plane::POSE) as *const UnityPose),
                center: std::ptr::read_unaligned(e.add(bounded_plane::CENTER) as *const [f32; 2]),
                size: std::ptr::read_unaligned(e.add(bounded_plane::SIZE) as *const [f32; 2]),
                alignment: std::ptr::read_unaligned(e.add(bounded_plane::ALIGNMENT) as *const i32),
            }
        })
        .collect()
}

/// Read `count` removed `TrackableId`s. AR Foundation packs the removed array as a `TrackableId[]`
/// of 16 bytes each, not as full `BoundedPlane`s.
fn read_removed_ids(ptr: *const c_void, count: i32) -> Vec<TrackableId> {
    if ptr.is_null() || count <= 0 {
        return Vec::new();
    }
    let base = ptr as *const u8;
    let stride = std::mem::size_of::<TrackableId>();
    (0..count as usize)
        .map(|i| unsafe { std::ptr::read_unaligned(base.add(i * stride) as *const TrackableId) })
        .collect()
}

/// A tracked spatial anchor sampled from the anchor changes, or from an acquire or load call.
/// `pose` is in **Unity space**, so convert on the Godot side, to `(x, -y, -z)` and quaternion
/// `(-x, -y, z, w)`. `session_id` is the map session it belongs to, and stays zero until saved.
#[derive(Clone, Copy, Debug)]
pub struct AnchorSample {
    pub id: TrackableId,
    pub pose: UnityPose,
    pub tracking_state: i32,
    pub session_id: Guid,
}

/// Added / updated / removed anchors from one [`XrealNative::poll_anchor_changes`] call.
pub struct AnchorChanges {
    pub added: Vec<AnchorSample>,
    pub updated: Vec<AnchorSample>,
    pub removed: Vec<TrackableId>,
}

/// Read one `XRTrackedAnchor` element at `e` (a pointer to `>= xr_anchor::ELEMENT_SIZE` bytes), pulling
/// the stable fields at the [`xr_anchor`] offsets.
unsafe fn read_anchor_at(e: *const u8) -> AnchorSample {
    AnchorSample {
        id: std::ptr::read_unaligned(e.add(xr_anchor::TRACKABLE_ID) as *const TrackableId),
        pose: std::ptr::read_unaligned(e.add(xr_anchor::POSE) as *const UnityPose),
        tracking_state: std::ptr::read_unaligned(e.add(xr_anchor::TRACKING_STATE) as *const i32),
        session_id: std::ptr::read_unaligned(e.add(xr_anchor::SESSION_ID) as *const Guid),
    }
}

/// Read `count` `XRTrackedAnchor`s from a native array of `stride`-byte elements. `ptr` must be valid
/// for `count * stride` bytes.
fn read_anchors(ptr: *const c_void, count: i32, stride: usize) -> Vec<AnchorSample> {
    if ptr.is_null() || count <= 0 {
        return Vec::new();
    }
    let base = ptr as *const u8;
    (0..count as usize)
        .map(|i| unsafe { read_anchor_at(base.add(i * stride)) })
        .collect()
}

/// A tracked reference image sampled from the image-tracking changes. `pose` is **Unity space** (convert
/// like planes/anchors). `source_image` is the reference image's `Guid` (matches the baked DB entry).
#[derive(Clone, Copy, Debug)]
pub struct ImageSample {
    pub id: TrackableId,
    pub source_image: Guid,
    pub pose: UnityPose,
    pub size: [f32; 2],
    pub tracking_state: i32,
}

/// Added / updated / removed tracked images from one [`XrealNative::poll_image_changes`] call.
pub struct ImageChanges {
    pub added: Vec<ImageSample>,
    pub updated: Vec<ImageSample>,
    pub removed: Vec<TrackableId>,
}

/// Read one `XRTrackedImage` element at `e` (a pointer to `>= xr_tracked_image::ELEMENT_SIZE` bytes).
unsafe fn read_image_at(e: *const u8) -> ImageSample {
    ImageSample {
        id: std::ptr::read_unaligned(e.add(xr_tracked_image::TRACKABLE_ID) as *const TrackableId),
        source_image: std::ptr::read_unaligned(
            e.add(xr_tracked_image::SOURCE_IMAGE_ID) as *const Guid
        ),
        pose: std::ptr::read_unaligned(e.add(xr_tracked_image::POSE) as *const UnityPose),
        size: std::ptr::read_unaligned(e.add(xr_tracked_image::SIZE) as *const [f32; 2]),
        tracking_state: std::ptr::read_unaligned(
            e.add(xr_tracked_image::TRACKING_STATE) as *const i32
        ),
    }
}

/// Read `count` `XRTrackedImage`s from a native array of `stride`-byte elements.
fn read_images(ptr: *const c_void, count: i32, stride: usize) -> Vec<ImageSample> {
    if ptr.is_null() || count <= 0 {
        return Vec::new();
    }
    let base = ptr as *const u8;
    (0..count as usize)
        .map(|i| unsafe { read_image_at(base.add(i * stride)) })
        .collect()
}

pub struct XrealNative {
    // Keep the libraries loaded; the function pointers below borrow from them.
    _session_lib: Library,
    _plugin_lib: Option<Library>,

    // Perception, from libXREALNativeSessionManager.so, with RE-confirmed signatures.
    hmd_time_nanos: FnHmdTimeNanos,
    get_head_pose_at_time: FnGetHeadPoseAtTime,
    load_api: Option<FnLoadApi>,
    is_session_started: Option<FnIsSessionStarted>,

    // Perception via libXREALXRPlugin.so. This is the layer that actually RUNS the
    // session (`CreateSession`/`ResumeSession` → "NRGlasses RUN!"). We only use its HMD
    // clock export here: its pose export writes a larger Unity-facing block, not NrPose.
    xp_hmd_time_nanos: Option<FnHmdTimeNanos>,
    /// Head pose from the **display** InputManager (libXREALXRPlugin.so `GetHeadPoseAtTime`).
    /// This is the pose the compositor reprojects the glasses layer with, so aligning the Godot
    /// eye cameras to it should make the render a head-locked peek window. Writes a 64-byte /
    /// 16-float block (from `NativePerception::GetHeadPose`), not the 7-float `NrPose`.
    xp_get_head_pose: Option<FnGetHeadPoseDisplay>,
    xp_is_session_started: Option<FnIsSessionStarted>,
    get_tracking_state: Option<FnQueryInt>,
    get_tracking_reason: Option<FnQueryInt>,
    get_tracking_type: Option<FnQueryInt>,
    switch_tracking_type: Option<FnSwitchTrackingType>,
    /// Per-device capability query, `IsHMDFeatureSupported`. The RGB camera, for instance, is absent on
    /// the Air 2 Ultra, so the camera path has to gate on this and never open a nonexistent camera.
    is_hmd_feature_supported: Option<FnIsHmdFeatureSupported>,
    /// `SetFocusPlane`: the plane the compositor reprojects against, left at its 1.4 m default
    /// unless something calls this each frame. See [`FnSetFocusPlane`].
    set_focus_plane: Option<FnSetFocusPlane>,

    // Plane detection (libXREALXRPlugin.so, flat C ABI; see docs/develop/plans/ar-features-plan.md). Needs 6DoF.
    get_plane_detection_mode: Option<FnGetPlaneDetectionMode>,
    set_plane_detection_mode: Option<FnSetPlaneDetectionMode>,
    get_plane_detection_changes: Option<FnGetPlaneDetectionChanges>,
    get_plane_boundary_vertex_count: Option<FnGetPlaneBoundaryVertexCount>,
    get_plane_boundary_vertex_data: Option<FnGetPlaneBoundaryVertexData>,

    // Spatial anchors (libXREALXRPlugin.so, flat C ABI; see docs/develop/plans/ar-features-plan.md). Needs
    // 6DoF + the vendored nr_spatial_anchor.aar backend.
    set_anchor_mapping_dir: Option<FnSetAnchorMappingFileDirectory>,
    set_anchor_enabled: Option<FnSetTrackableAnchorEnabled>,
    acquire_anchor: Option<FnAcquireNewTrackableAnchor>,
    get_anchor_changes: Option<FnGetTrackableAnchorChanges>,
    save_anchor: Option<FnSaveTrackableAnchor>,
    load_anchor: Option<FnLoadTrackableAnchor>,
    remove_anchor: Option<FnRemoveTrackableAnchor>,
    remap_anchor: Option<FnRemapTrackableAnchor>,
    estimate_anchor_quality: Option<FnEstimateTrackableAnchorQuality>,

    // Image tracking (libXREALXRPlugin.so, flat C ABI; see docs/develop/plans/ar-features-plan.md). Needs
    // 6DoF + the vendored nr_image_tracking.aar backend + assets/nr_plugins.json + a DB blob.
    init_image_db: Option<FnInitImageTrackingDatabase>,
    set_image_db: Option<FnSetImageTrackingDatabase>,
    get_image_changes: Option<FnGetImageTrackingChanges>,
    get_reference_image_count: Option<FnGetReferenceImageCount>,
    release_image_db: Option<FnReleaseImageTrackingDatabase>,

    // RGB camera (libXREALXRPlugin.so, flat C ABI; see docs/develop/plans/camera-feed-plan.md). Poll path.
    rgb_start_capture: Option<FnStartRgbCameraCapture>,
    rgb_stop_capture: Option<FnStopRgbCameraCapture>,
    rgb_try_acquire_latest: Option<FnTryAcquireLatestImage>,
    rgb_get_data_plane: Option<FnTryGetRgbCameraDataPlane>,
    rgb_dispose_handle: Option<FnDisposeRgbCameraDataHandle>,

    // Session and control, from libXREALXRPlugin.so: optional, and used for the full bootstrap.
    unity_plugin_load: Option<FnUnityPluginLoad>,
    init_user_defined_settings: Option<FnInitUserDefinedSettings>,
    create_session: Option<FnCreateSession>,
    resume_session: Option<FnVoid>,
    recenter_glasses: Option<FnVoid>,
    set_display_bypass_psensor: Option<FnControlSetI32>,
    set_glasses_space_mode: Option<FnControlSetI32>,
    set_glasses_event_callback: Option<FnSetGlassesEventCallback>,
    set_native_error_callback: Option<FnSetNativeErrorCallback>,
    #[allow(dead_code)]
    initialize_rendering: Option<FnVoid>,
    #[allow(dead_code)]
    create_frame: Option<FnCreateFrame>,
    get_frame_metadata: Option<FnGetFrameMetaData>,
    deinitialize_rendering: Option<FnVoid>,

    // Read-only device info, from libXREALXRPlugin.so, exposed through XrealSystem.
    get_plugin_version: Option<FnGetPluginVersion>,
    get_device_type: Option<FnGetDeviceType>,

    // Device / camera geometry (libXREALXRPlugin.so, Unity space; docs/develop/plans/coordinate-systems-notes.md).
    get_device_pose_from_head: Option<FnGetDevicePoseFromHead>,
    get_device_resolution: Option<FnGetDeviceResolution>,
    get_camera_intrinsic: Option<FnGetCameraIntrinsic>,
    get_camera_projection_matrix: Option<FnGetCameraProjectionMatrix>,

    // The direct NR compositor and rendering API, from libnr_loader.so. RE'd and unverified.
    nr_rendering: Option<NrRenderingApi>,
    display_manager_rendering_initialized: bool,

    // Runtime address of DisplayManager's function-local UnityXRNextFrameDesc static.
    //
    // RE: `CreateFrame()` / `SubmitCurrentFrame()` gate on the byte at static+0x10
    // (`ldrb w8, [0xdb410]`), which starts as 0 after the lazy init. Calling
    // `PopulateNextFrameDesc` with this pointer causes XREAL to write a non-zero
    // render-pass count there, unblocking both functions.
    //
    // The static is at compile-time offset 0xdb400 in libXREALXRPlugin.so.
    // We recover the runtime base by subtracting CreateFrame's compile-time offset
    // (0x53bd8) from its runtime address. See docs/develop/reference/reverse-engineering.md.
    display_manager_desc_ptr: Option<*mut c_void>,
}

#[allow(dead_code)]
struct NrRenderingApi {
    // Keep the loader alive; all function pointers below are borrowed from it.
    _lib: Library,

    rendering_create: FnNrRenderingCreate,
    rendering_start: FnNrRenderingOneHandle,
    rendering_stop: FnNrRenderingOneHandle,
    rendering_destroy: FnNrRenderingOneHandle,
}

/// One RGB-camera frame as planar YCbCr: `(y, y_w, y_h, cbcr, c_w, c_h)`, meaning the Y plane in
/// full-res R8 plus an interleaved CbCr buffer in half-res RG8, the layout `set_ycbcr_images` and a
/// YCbCr shader expect.
pub type YuvFrame = (Vec<u8>, i32, i32, Vec<u8>, i32, i32);

/// Borrowed view of one acquired RGB frame. The slices point **into the SDK's own frame buffer**,
/// so they stay valid only while the frame handle is alive, that is only for the body of the
/// [`XrealNative::rgb_camera_with_frame`] closure, which the lifetime enforces.
pub struct RgbPlanes<'a> {
    /// Luma, full-res, tightly packed (`y_width * y_height` bytes).
    pub y: &'a [u8],
    pub y_width: i32,
    pub y_height: i32,
    /// I420 plane 2: U, or Cb, at half-res.
    pub u: &'a [u8],
    /// I420 plane 1: V, or Cr, at half-res.
    pub v: &'a [u8],
    pub chroma_width: i32,
    pub chroma_height: i32,
}

/// Borrow one plane of an acquired frame in place, without copying.
///
/// # Safety
/// `handle` must be a live frame handle, and the returned slice must not outlive it. The lifetime
/// `'a` is unconstrained, so the caller alone guarantees that. Only
/// [`XrealNative::rgb_camera_with_frame`] should call it.
unsafe fn borrow_plane<'a>(
    get_plane: FnTryGetRgbCameraDataPlane,
    handle: i32,
    idx: i32,
) -> Option<(&'a [u8], i32, i32)> {
    let mut ptr: *mut c_void = std::ptr::null_mut();
    let mut sz = NrSize2i::default();
    let ok = get_plane(handle, idx, &mut ptr, &mut sz);
    if !ok || ptr.is_null() || sz.width <= 0 || sz.height <= 0 {
        return None;
    }
    let len = (sz.width as usize) * (sz.height as usize);
    Some((
        std::slice::from_raw_parts(ptr as *const u8, len),
        sz.width,
        sz.height,
    ))
}

/// Address of the SDK's RGB "latest frame" `std::mutex`, which is layout-compatible with
/// `pthread_mutex_t` and guards the `shared_ptr<RGBCameraDataFrame>` holder inside the
/// `SessionManager` singleton. It is `None` until `lib_base` is published. The two constants come
/// from disassembling `libXREALXRPlugin.so`: the singleton is the fixed global
/// `lib_base + 0xDB400`, the same base the other REs in this crate use, and the receive, start and
/// stop paths, `GetRGBCameraData`, `StartRGBCameraDataCapture` and `StopRGBCameraDataCapture`, all
/// lock the same mutex at `singleton + 0x1C0`. It is the single lock guarding the RGB camera state,
/// so holding it serialises us against every writer. `TryGetRGBCameraFrame`, the destructive
/// new-frame gate, also reads `+0x188` and `+0x140` unlocked; we do not use it, but adopting it
/// would need this same lock.
///
/// # Why this exists: the RGB-camera double-free crash
/// `TryAcquireLatestImage`, the poll API we call from the GL thread, reads that holder **without
/// taking any lock**, bumps the `shared_ptr` control block's refcount, and copies it into its
/// handle map. Meanwhile the SDK's camera receive thread runs
/// `SessionManager::GetRGBCameraData`, which, *under this mutex*, swaps the holder to a new frame
/// and releases the old `shared_ptr`, dropping the refcount to 0 and deleting it. Racing the two
/// lets us latch a control block the receive thread is freeing, and the later
/// `DisposeRGBCameraDataHandle` then double-frees it, so Scudo aborts the GL thread with "invalid
/// chunk state when deallocating" (tombstone_29 and tombstone_30, both the same stack). Holding
/// this mutex across the acquire serialises us against the receive thread: the `shared_ptr` we
/// latch is valid, and from then on its refcount keeps the frame alive. A frame we still reference
/// is not returned to the `ObjectPool`, so its planes stay stable too, which also removes tearing.
/// That is why only the acquire needs the lock, and neither the plane reads nor the dispose do.
///
/// This is a **stopgap** that borrows an SDK-internal `std::mutex`, so the offsets are hard-coded
/// and have to be re-checked on an SDK update. The root-cause fix is **callback mode**: call
/// `StartRGBCameraDataCapture` with a non-null callback, receive the `RGBCameraDataFrameToUnity` on
/// the receive thread, and never touch the poll API off-thread, which is what Unity does. That was
/// not done here because it needs the callback struct's ABI reversed and the zero-copy
/// direct-upload path redesigned, a mid-sized change with a wider on-device test surface, whereas
/// this stopgap is local and reuses existing REs. So the stopgap ships first and the callback
/// migration is tracked as separate work.
#[cfg(target_os = "android")]
fn rgb_holder_mutex() -> Option<*mut libc::pthread_mutex_t> {
    let base = crate::signal_guard::lib_base();
    (base != 0).then(|| (base + 0xDB400 + 0x1C0) as *mut libc::pthread_mutex_t)
}

/// Call `TryAcquireLatestImage` serialised against the SDK camera receive thread; see
/// [`rgb_holder_mutex`] for the race this closes. It falls back to an unguarded call only while
/// `lib_base` is unpublished, which cannot happen once the camera has started.
///
/// # Safety
/// `acquire` must be the live `TryAcquireLatestImage` export and the three out-pointers valid.
#[cfg(target_os = "android")]
unsafe fn rgb_acquire_latest_locked(
    acquire: FnTryAcquireLatestImage,
    frame_handle: &mut i32,
    resolution: &mut NrSize2i,
    timestamp: &mut u64,
) -> bool {
    match rgb_holder_mutex() {
        Some(m) => {
            libc::pthread_mutex_lock(m);
            let ok = acquire(frame_handle, resolution, timestamp);
            libc::pthread_mutex_unlock(m);
            ok
        }
        None => acquire(frame_handle, resolution, timestamp),
    }
}

/// Desktop fallback: no SDK library, no receive thread, nothing to serialise against.
///
/// # Safety
/// As [`rgb_acquire_latest_locked`] on Android.
#[cfg(not(target_os = "android"))]
unsafe fn rgb_acquire_latest_locked(
    acquire: FnTryAcquireLatestImage,
    frame_handle: &mut i32,
    resolution: &mut NrSize2i,
    timestamp: &mut u64,
) -> bool {
    acquire(frame_handle, resolution, timestamp)
}

/// Interleave the I420 chroma planes into the `[Cb, Cr, Cb, Cr, …]` RG8 layout a YCbCr shader
/// samples, reusing `out`'s allocation.
///
/// Fixed-width stores into a pre-sized buffer, deliberately: the obvious `push` loop carries a
/// capacity check per byte, cannot vectorise, and measured 903 us per frame on the X4000, at
/// 0.51 GB/s, the slowest stage of the whole grab. This shape is what LLVM lowers to a NEON
/// two-channel interleaving store, which does the same work in 167 us.
pub fn interleave_cbcr(u: &[u8], v: &[u8], width: i32, height: i32, out: &mut Vec<u8>) {
    let n = (width.max(0) as usize) * (height.max(0) as usize);
    let m = n.min(u.len()).min(v.len());
    // Only ever resizes on the first frame / a resolution change; steady state reuses the buffer
    // and every byte below is overwritten, so no clear or memset is needed.
    if out.len() != m * 2 {
        out.resize(m * 2, 0);
    }
    for (dst, (&cb, &cr)) in out.chunks_exact_mut(2).zip(u[..m].iter().zip(&v[..m])) {
        dst[0] = cb; // Cb = U
        dst[1] = cr; // Cr = V
    }
}

/// OPT2 (RGBA8 experiment): pack (Cb, Cr, 0, 255) per texel, so a `Texture2DRD` wrapping an
/// `R8G8B8A8_UNORM` texture (which maps identity, unlike `R8G8_UNORM -> LA8`) exposes `.rg` as
/// (Cb, Cr) with no shader change. Costs 4 bytes/texel instead of 2 (double the chroma upload).
pub fn interleave_cbcr_rgba(u: &[u8], v: &[u8], width: i32, height: i32, out: &mut Vec<u8>) {
    let n = (width.max(0) as usize) * (height.max(0) as usize);
    let m = n.min(u.len()).min(v.len());
    if out.len() != m * 4 {
        out.resize(m * 4, 0);
    }
    for (dst, (&cb, &cr)) in out.chunks_exact_mut(4).zip(u[..m].iter().zip(&v[..m])) {
        dst[0] = cb; // Cb = U -> .r
        dst[1] = cr; // Cr = V -> .g
        dst[2] = 0;
        dst[3] = 255;
    }
}

/// Per-stage cost of one grab, in microseconds, so the SDK's own calls can be told apart from the
/// client-side work around them. `XrealCameraFeed` reports it when `debug.xreal.camera_timing` is
/// set.
///
/// This settled the open question in `docs/develop/archive/codex-camera-acquire-analysis.md`: the
/// disassembly said the SDK getters were hash lookups and pointer arithmetic, and the measurement
/// agreed, with `acquire` at about 4 us and `planes` at 0. Every microsecond of the old 3.5 ms per
/// frame was on our side. It is kept because it is how any future regression here gets found.
#[derive(Clone, Copy, Default, Debug)]
pub struct GrabTimings {
    /// `TryAcquireLatestImage`.
    pub acquire_us: u32,
    /// 3x `TryGetRGBCameraDataPlane` + our `to_vec` of each plane (1,382,400 bytes total).
    pub planes_us: u32,
    /// Our byte-wise U/V -> CbCr interleave.
    pub interleave_us: u32,
    /// `DisposeRGBCameraDataHandle`.
    pub dispose_us: u32,
}

impl NrRenderingApi {
    fn load() -> Result<Self, String> {
        unsafe {
            let lib =
                Library::new(NR_LOADER_LIB).map_err(|e| format!("dlopen {NR_LOADER_LIB}: {e}"))?;

            macro_rules! sym {
                ($name:literal, $ty:ty) => {
                    *lib.get::<$ty>(concat!($name, "\0").as_bytes())
                        .map_err(|e| format!("dlsym {}: {e}", $name))?
                };
            }

            Ok(Self {
                rendering_create: sym!("NRRenderingCreate", FnNrRenderingCreate),
                rendering_start: sym!("NRRenderingStart", FnNrRenderingOneHandle),
                rendering_stop: sym!("NRRenderingStop", FnNrRenderingOneHandle),
                rendering_destroy: sym!("NRRenderingDestroy", FnNrRenderingOneHandle),
                _lib: lib,
            })
        }
    }

    fn resolved_symbol_count(&self) -> usize {
        4
    }

    fn smoke_create_destroy(&self) -> Result<(), i32> {
        let mut rendering: NrHandle = 0;
        let status = unsafe { (self.rendering_create)(&mut rendering) };
        if status != 0 {
            return Err(status);
        }
        if rendering != 0 {
            let destroy_status = unsafe { (self.rendering_destroy)(rendering) };
            if destroy_status != 0 {
                return Err(destroy_status);
            }
        }
        Ok(())
    }

    fn smoke_start_stop(&self) -> Result<(), i32> {
        let mut rendering: NrHandle = 0;
        let status = unsafe { (self.rendering_create)(&mut rendering) };
        if status != 0 {
            return Err(status);
        }
        if rendering == 0 {
            return Err(-2);
        }

        let start_status = unsafe { (self.rendering_start)(rendering) };
        let stop_status = if start_status == 0 {
            unsafe { (self.rendering_stop)(rendering) }
        } else {
            0
        };
        let destroy_status = unsafe { (self.rendering_destroy)(rendering) };

        if start_status != 0 {
            return Err(start_status);
        }
        if stop_status != 0 {
            return Err(stop_status);
        }
        if destroy_status != 0 {
            return Err(destroy_status);
        }
        Ok(())
    }
}

// SAFETY: `display_manager_desc_ptr` points into `libXREALXRPlugin.so`'s read-write data
// section (the `UnityXRNextFrameDesc` function-local static). It is written only from the
// session-init thread (before the `OnceLock` is populated) and then treated as read-only.
// All other raw pointers in XrealNative are function pointers or Library handles, which are
// inherently `Send`.
unsafe impl Send for XrealNative {}

impl XrealNative {
    /// `dlopen` the XREAL libraries and resolve the symbols the extension needs.
    ///
    /// It returns `Err`, without panicking, when the libraries are missing, which is the expected case
    /// on desktop and editor builds.
    pub fn load() -> Result<Self, String> {
        unsafe {
            let session_lib =
                Library::new(SESSION_LIB).map_err(|e| format!("dlopen {SESSION_LIB}: {e}"))?;
            let plugin_lib = Library::new(PLUGIN_LIB).ok();
            // Pin both XREAL libs for the process lifetime with RTLD_NODELETE. `load()` runs on every session
            // bring-up retry, and a FAILED attempt drops this XrealNative, which dlcloses the libraries. That
            // opens an unload window in which `signal_guard::lib_base()`, published on load, the code patches,
            // and every callback pointer the SDK stored all dangle. Scudo reuses the address range and the
            // next `blr lib_base+offset`, from hand_tracking::ensure_enabled for instance, executes heap
            // memory. Observed on device: a SIGSEGV with SEGV_ACCERR on the GLThread at exactly
            // lib_base+0x47a10. The XREAL runtime is a process-global singleton, and unloading it is never
            // useful, so pin it.
            #[cfg(target_os = "android")]
            for name in [SESSION_LIB, PLUGIN_LIB] {
                let cname = std::ffi::CString::new(name).unwrap();
                if libc::dlopen(cname.as_ptr(), libc::RTLD_NOW | libc::RTLD_NODELETE).is_null() {
                    godot::global::godot_warn!("[xreal] RTLD_NODELETE pin failed for {name}");
                }
            }

            let hmd_time_nanos: FnHmdTimeNanos = *session_lib
                .get(b"XREALGetHMDTimeNanos\0")
                .map_err(|e| format!("dlsym XREALGetHMDTimeNanos: {e}"))?;
            let get_head_pose_at_time: FnGetHeadPoseAtTime = *session_lib
                .get(b"XREALGetHeadPoseAtTime\0")
                .map_err(|e| format!("dlsym XREALGetHeadPoseAtTime: {e}"))?;

            let load_api: Option<FnLoadApi> = session_lib.get(b"XREALLoadAPI\0").ok().map(|s| *s);
            let is_session_started: Option<FnIsSessionStarted> =
                session_lib.get(b"XREALIsSessionStarted\0").ok().map(|s| *s);

            // Same-named flat-C HMD clock export in the XR plugin (the running session).
            let xp_hmd_time_nanos: Option<FnHmdTimeNanos> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetHMDTimeNanos\0").ok().map(|s| *s));
            // The XR plugin's own head-pose export (@0x48cc8 → InputManager::GetHeadPoseAtTime):
            // the compositor's pose source. Note it shares the name `GetHeadPoseAtTime` with the
            // session-manager export but writes a 16-float block, so it needs its own fn type.
            let xp_get_head_pose: Option<FnGetHeadPoseDisplay> = plugin_lib
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
            let is_hmd_feature_supported: Option<FnIsHmdFeatureSupported> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"IsHMDFeatureSupported\0").ok().map(|s| *s));

            // Plane detection exports (libXREALXRPlugin.so). See docs/develop/plans/ar-features-plan.md.
            let set_focus_plane: Option<FnSetFocusPlane> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"SetFocusPlane\0").ok().map(|s| *s));

            let get_plane_detection_mode: Option<FnGetPlaneDetectionMode> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetPlaneDetectionMode\0").ok().map(|s| *s));
            let set_plane_detection_mode: Option<FnSetPlaneDetectionMode> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"SetPlaneDetectionMode\0").ok().map(|s| *s));
            let get_plane_detection_changes: Option<FnGetPlaneDetectionChanges> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetPlaneDetectionChanges\0").ok().map(|s| *s));
            let get_plane_boundary_vertex_count: Option<FnGetPlaneBoundaryVertexCount> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetPlaneBoundaryVertexCount\0").ok().map(|s| *s));
            let get_plane_boundary_vertex_data: Option<FnGetPlaneBoundaryVertexData> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetPlaneBoundaryVertexData\0").ok().map(|s| *s));

            // Spatial-anchor exports (libXREALXRPlugin.so). See docs/develop/plans/ar-features-plan.md.
            let set_anchor_mapping_dir: Option<FnSetAnchorMappingFileDirectory> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"SetAnchorMappingFileDirectory\0").ok().map(|s| *s));
            let set_anchor_enabled: Option<FnSetTrackableAnchorEnabled> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"SetTrackableAnchorEnabled\0").ok().map(|s| *s));
            let acquire_anchor: Option<FnAcquireNewTrackableAnchor> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"AcquireNewTrackableAnchor\0").ok().map(|s| *s));
            let get_anchor_changes: Option<FnGetTrackableAnchorChanges> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetTrackableAnchorChanges\0").ok().map(|s| *s));
            let save_anchor: Option<FnSaveTrackableAnchor> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"SaveTrackableAnchor\0").ok().map(|s| *s));
            let load_anchor: Option<FnLoadTrackableAnchor> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"LoadTrackableAnchor\0").ok().map(|s| *s));
            let remove_anchor: Option<FnRemoveTrackableAnchor> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"RemoveTrackableAnchor\0").ok().map(|s| *s));
            let remap_anchor: Option<FnRemapTrackableAnchor> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"RemapTrackableAnchor\0").ok().map(|s| *s));
            let estimate_anchor_quality: Option<FnEstimateTrackableAnchorQuality> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"EstimateTrackableAnchorQuality\0").ok().map(|s| *s));

            // Image-tracking exports (libXREALXRPlugin.so). See docs/develop/plans/ar-features-plan.md.
            let init_image_db: Option<FnInitImageTrackingDatabase> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"InitImageTrackingDatabase\0").ok().map(|s| *s));
            let set_image_db: Option<FnSetImageTrackingDatabase> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"SetImageTrackingDatabase\0").ok().map(|s| *s));
            let get_image_changes: Option<FnGetImageTrackingChanges> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetImageTrackingChanges\0").ok().map(|s| *s));
            let get_reference_image_count: Option<FnGetReferenceImageCount> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetReferenceImageCount\0").ok().map(|s| *s));
            let release_image_db: Option<FnReleaseImageTrackingDatabase> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"ReleaseImageTrackingDatabase\0").ok().map(|s| *s));

            // RGB camera exports (libXREALXRPlugin.so). See docs/develop/plans/camera-feed-plan.md.
            let rgb_start_capture: Option<FnStartRgbCameraCapture> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"StartRGBCameraDataCapture\0").ok().map(|s| *s));
            let rgb_stop_capture: Option<FnStopRgbCameraCapture> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"StopRGBCameraDataCapture\0").ok().map(|s| *s));
            let rgb_try_acquire_latest: Option<FnTryAcquireLatestImage> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"TryAcquireLatestImage\0").ok().map(|s| *s));
            let rgb_get_data_plane: Option<FnTryGetRgbCameraDataPlane> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"TryGetRGBCameraDataPlane\0").ok().map(|s| *s));
            let rgb_dispose_handle: Option<FnDisposeRgbCameraDataHandle> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"DisposeRGBCameraDataHandle\0").ok().map(|s| *s));

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
            let set_display_bypass_psensor: Option<FnControlSetI32> =
                plugin_lib.as_ref().and_then(|l| {
                    l.get(b"ControlSetDisplayBypassPsensorFlag\0")
                        .ok()
                        .map(|s| *s)
                });
            let set_glasses_space_mode: Option<FnControlSetI32> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"SetGlassesSpaceMode\0").ok().map(|s| *s));
            let set_glasses_event_callback: Option<FnSetGlassesEventCallback> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"SetGlassesEventCallback\0").ok().map(|s| *s));
            let set_native_error_callback: Option<FnSetNativeErrorCallback> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"SetNativeErrorCallback\0").ok().map(|s| *s));
            let initialize_rendering: Option<FnVoid> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"InitializeRendering\0").ok().map(|s| *s));
            let create_frame: Option<FnCreateFrame> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"CreateFrame\0").ok().map(|s| *s));
            let get_frame_metadata: Option<FnGetFrameMetaData> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetFrameMetaData\0").ok().map(|s| *s));
            let deinitialize_rendering: Option<FnVoid> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"DeinitializeRendering\0").ok().map(|s| *s));

            let get_plugin_version: Option<FnGetPluginVersion> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetPluginVersion\0").ok().map(|s| *s));
            let get_device_type: Option<FnGetDeviceType> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetDeviceType\0").ok().map(|s| *s));

            // Device / camera geometry (Unity space; docs/develop/plans/coordinate-systems-notes.md).
            let get_device_pose_from_head: Option<FnGetDevicePoseFromHead> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetDevicePoseFromHead\0").ok().map(|s| *s));
            let get_device_resolution: Option<FnGetDeviceResolution> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetDeviceResolution\0").ok().map(|s| *s));
            let get_camera_intrinsic: Option<FnGetCameraIntrinsic> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetCameraIntrinsic\0").ok().map(|s| *s));
            let get_camera_projection_matrix: Option<FnGetCameraProjectionMatrix> = plugin_lib
                .as_ref()
                .and_then(|l| l.get(b"GetCameraProjectionMatrix\0").ok().map(|s| *s));

            let nr_rendering = NrRenderingApi::load().ok();

            // Compute runtime address of libXREALXRPlugin.so's UnityXRNextFrameDesc static.
            // CreateFrame compile-time offset: 0x53bd8 (confirmed with llvm-nm).
            // Static compile-time offset: 0xdb400.
            let display_manager_desc_ptr = plugin_lib.as_ref().and_then(|l| {
                l.get::<FnCreateFrame>(b"CreateFrame\0").ok().map(|s| {
                    let fn_runtime_addr: usize = *s as usize;
                    let lib_base = fn_runtime_addr.wrapping_sub(0x53bd8);
                    // Code-patch HandleActionCallback+28 to add a null-NativeGlasses check.
                    // The SIGSEGV handler approach doesn't work because Android libsigchain
                    // intercepts SIGSEGV before user sigaction handlers on ART-managed threads.
                    // Apply once per process (the OnceLock ensures a single call even though
                    // XrealNative::load() may be called repeatedly during session retries).
                    #[cfg(target_os = "android")]
                    {
                        use std::sync::OnceLock;
                        static PATCHED: OnceLock<()> = OnceLock::new();
                        PATCHED.get_or_init(|| {
                            crate::signal_guard::patch_handle_action_callback(lib_base);
                            // Force CreateDisplayLayer to always create real DisplayOverlay.
                            // Without this, it creates DummyDisplayOverlay (no textures) because
                            // 0xdb410 is 0 at the time GfxThreadStart runs.
                            crate::signal_guard::patch_create_display_layer(lib_base);
                            // Neuter UpdateMetrics' null metrics-callback so SubmitCurrentFrame
                            // (which presents our registered buffers) doesn't SIGBUS.
                            crate::signal_guard::patch_update_metrics(lib_base);
                        });
                    }
                    // Publish the library base so LIB_BASE readers, such as
                    // reassert_update_metrics_on_render_thread, work. On Android publish it WITHOUT installing the
                    // SIGSEGV sigaction, which is a no-op there, since libsigchain wins, and destabilised the process.
                    // Off Android, use install().
                    #[cfg(target_os = "android")]
                    crate::signal_guard::publish_lib_base(lib_base);
                    #[cfg(not(target_os = "android"))]
                    crate::signal_guard::install(lib_base);
                    (lib_base + 0xdb400) as *mut c_void
                })
            });
            godot::global::godot_print!(
                "[xreal] libXREALXRPlugin.so desc_ptr={display_manager_desc_ptr:?}"
            );

            Ok(Self {
                _session_lib: session_lib,
                _plugin_lib: plugin_lib,
                hmd_time_nanos,
                get_head_pose_at_time,
                load_api,
                is_session_started,
                xp_hmd_time_nanos,
                xp_get_head_pose,
                xp_is_session_started,
                get_tracking_state,
                get_tracking_reason,
                get_tracking_type,
                switch_tracking_type,
                is_hmd_feature_supported,
                set_focus_plane,
                get_plane_detection_mode,
                set_plane_detection_mode,
                get_plane_detection_changes,
                get_plane_boundary_vertex_count,
                get_plane_boundary_vertex_data,
                set_anchor_mapping_dir,
                set_anchor_enabled,
                acquire_anchor,
                get_anchor_changes,
                save_anchor,
                load_anchor,
                remove_anchor,
                remap_anchor,
                estimate_anchor_quality,
                init_image_db,
                set_image_db,
                get_image_changes,
                get_reference_image_count,
                release_image_db,
                rgb_start_capture,
                rgb_stop_capture,
                rgb_try_acquire_latest,
                rgb_get_data_plane,
                rgb_dispose_handle,
                set_display_bypass_psensor,
                set_glasses_space_mode,
                set_glasses_event_callback,
                set_native_error_callback,
                unity_plugin_load,
                init_user_defined_settings,
                create_session,
                resume_session,
                recenter_glasses,
                initialize_rendering,
                create_frame,
                get_frame_metadata,
                deinitialize_rendering,
                get_plugin_version,
                get_device_type,
                get_device_pose_from_head,
                get_device_resolution,
                get_camera_intrinsic,
                get_camera_projection_matrix,
                nr_rendering,
                display_manager_rendering_initialized: false,
                display_manager_desc_ptr,
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

    /// Resume the session. Unity calls this on app resume, and it activates the perception subsystem: a
    /// freshly `CreateSession`'d session stays paused, so `IsSessionStarted` is false and no pose flows
    /// until this runs. It does nothing when the symbol is unavailable.
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
        ((status == 0 || status == 1) && time_ns != 0).then_some(time_ns)
    }

    /// Fetch the head pose predicted for `time_ns`. Returns `true` on success.
    ///
    /// Use the SessionManager export here: libXREALXRPlugin.so's InputManager wrapper writes
    /// a larger 64-byte Unity-facing pose struct, not the compact 7-float `NrPose`.
    pub fn get_head_pose_at_time(&self, time_ns: u64, out: &mut NrPose) -> bool {
        let status = unsafe { (self.get_head_pose_at_time)(time_ns, out as *mut NrPose) };
        // RE: native exports across XREAL libraries use both NRResult-style 0 and bool-style 1.
        matches!(status, 0 | 1)
    }

    /// Fetch the **display** subsystem head pose, libXREALXRPlugin.so's `GetHeadPoseAtTime`, as the raw
    /// 16-float block it writes, which is the pose the compositor reprojects with. It returns `None`
    /// when the export is absent or the query fails. The caller decodes the 16-float layout, a
    /// device-pinned 4x4 row-major transform; see the RE map in
    /// `docs/develop/archive/multiview-investigation.md`.
    pub fn head_pose_display(&self, time_ns: u64) -> Option<[f32; 16]> {
        let f = self.xp_get_head_pose?;
        let mut raw = [0.0_f32; 16];
        let status = unsafe { f(time_ns, &mut raw) };
        matches!(status, 0 | 1).then_some(raw)
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

    // --- Plane detection (libXREALXRPlugin.so; see docs/develop/plans/ar-features-plan.md). Needs a 6DoF session. ---

    /// Whether the connected glasses support an [`crate::ffi::hmd_feature`] (`IsHMDFeatureSupported`).
    /// `None` if the export is absent. The device-accurate camera/6DoF gate (the Air 2 Ultra has no
    /// RGB camera, so `hmd_feature::RGB_CAMERA` returns `Some(false)` there).
    pub fn hmd_feature_supported(&self, feature: i32) -> Option<bool> {
        self.is_hmd_feature_supported.map(|f| unsafe { f(feature) })
    }

    // --- Device / camera geometry (Unity space; docs/develop/plans/coordinate-systems-notes.md). `component`
    // is a `crate::ffi::component` id (RGB_CAMERA = 2). All return `None` if the export is absent or the
    // SDK returns false (e.g. the device lacks that component, or the session isn't ready). ---

    /// A device's extrinsic relative to Head as a Unity `Pose`: `[pos x,y,z, quat x,y,z,w]` (Unity LH).
    pub fn device_pose_from_head(&self, component: i32) -> Option<[f32; 7]> {
        let f = self.get_device_pose_from_head?;
        let mut pose = [0.0f32; 7];
        unsafe { f(component, &mut pose) }.then_some(pose)
    }

    /// A device's pixel resolution `(width, height)`.
    pub fn device_resolution(&self, component: i32) -> Option<(i32, i32)> {
        let f = self.get_device_resolution?;
        let mut size = NrSize2i::default();
        unsafe { f(component, &mut size) }.then_some((size.width, size.height))
    }

    /// A camera's intrinsics `[fx, fy, cx, cy]` in pixels.
    pub fn camera_intrinsic(&self, component: i32) -> Option<[f32; 4]> {
        let f = self.get_camera_intrinsic?;
        let (mut focal, mut principal) = ([0.0f32; 2], [0.0f32; 2]);
        unsafe { f(component, &mut focal, &mut principal) }.then_some([
            focal[0],
            focal[1],
            principal[0],
            principal[1],
        ])
    }

    /// A camera's 4x4 projection matrix (16 floats, Unity `Matrix4x4` column-major) for `[near, far]`.
    pub fn camera_projection_matrix(
        &self,
        component: i32,
        near: f32,
        far: f32,
    ) -> Option<[f32; 16]> {
        let f = self.get_camera_projection_matrix?;
        let mut mat = [0.0f32; 16];
        unsafe { f(component, near, far, &mut mat) }.then_some(mat)
    }

    /// Point the compositor's reprojection plane at `point` with surface normal `normal`, both in
    /// **head-local Unity space**. Returns whether the export was there to call.
    ///
    /// This is a per-frame setting, not a mode: the compositor uses whatever it was last given, so
    /// a caller that stops calling leaves the plane wherever it stopped, and one that never calls
    /// gets the SDK's fixed 1.4 m default.
    pub fn set_focus_plane(&self, point: [f32; 3], normal: [f32; 3]) -> bool {
        let Some(f) = self.set_focus_plane else {
            return false;
        };
        unsafe {
            f(
                UnityVector3 {
                    x: point[0],
                    y: point[1],
                    z: point[2],
                },
                UnityVector3 {
                    x: normal[0],
                    y: normal[1],
                    z: normal[2],
                },
            )
        };
        true
    }

    /// Current `PlaneDetectionMode` flags (`ffi::plane_detection_mode`), or `None` if the export is absent.
    pub fn plane_detection_mode(&self) -> Option<i32> {
        self.get_plane_detection_mode.map(|f| unsafe { f() })
    }

    /// Enable horizontal/vertical plane detection (`ffi::plane_detection_mode` flags). Returns the SDK
    /// bool (or `false` if the export is absent). Detection needs a live 6DoF session.
    pub fn set_plane_detection_mode(&self, mode: i32) -> bool {
        match self.set_plane_detection_mode {
            Some(f) => unsafe { f(mode) },
            None => false,
        }
    }

    /// Poll the plane added/updated/removed changes since the last call. Copies the data out of the
    /// SDK's (transient) arrays immediately. `None` when the export is absent.
    pub fn poll_plane_changes(&self) -> Option<PlaneChanges> {
        let f = self.get_plane_detection_changes?;
        let mut changes = ArSubsystemChanges::default();
        unsafe { f(&mut changes) };
        let stride = changes.element_size as usize;
        // A stride smaller than the fields we read means an unexpected layout, so bail rather than read
        // out of bounds; the expected element_size is `bounded_plane::ELEMENT_SIZE`.
        if stride < bounded_plane::TRACKING_STATE + 4 {
            if changes.added_count != 0 || changes.updated_count != 0 {
                godot::global::godot_warn!(
                    "[xreal] plane changes: element_size={stride} < expected {}; skipping parse",
                    bounded_plane::ELEMENT_SIZE
                );
            }
            // Removed ids are just TrackableIds (16 B), still parseable -- but clamp the count with
            // sane_count() like the normal path, so a corrupt removed_count cannot read out of bounds.
            return Some(PlaneChanges {
                added: Vec::new(),
                updated: Vec::new(),
                removed: read_removed_ids(
                    changes.removed_ptr,
                    sane_count(changes.removed_count, "plane removed"),
                ),
            });
        }
        Some(PlaneChanges {
            added: read_planes(
                changes.added_ptr,
                sane_count(changes.added_count, "plane added"),
                stride,
            ),
            updated: read_planes(
                changes.updated_ptr,
                sane_count(changes.updated_count, "plane updated"),
                stride,
            ),
            removed: read_removed_ids(
                changes.removed_ptr,
                sane_count(changes.removed_count, "plane removed"),
            ),
        })
    }

    /// The boundary polygon (plane-local `Vector2`s) of a detected plane, or empty if unavailable.
    pub fn plane_boundary(&self, id: TrackableId) -> Vec<[f32; 2]> {
        let (Some(count_fn), Some(data_fn)) = (
            self.get_plane_boundary_vertex_count,
            self.get_plane_boundary_vertex_data,
        ) else {
            return Vec::new();
        };
        let n = unsafe { count_fn(id) };
        if n <= 0 {
            return Vec::new();
        }
        // Refuse an out-of-range count rather than clamping: count-fetch and data-write are separate
        // SDK calls, so a partial buffer would just let data_fn overrun.
        if n > MAX_BOUNDARY_VERTS {
            godot::global::godot_warn!(
                "[xreal] plane boundary vertex count {n} out of range; skipping"
            );
            return Vec::new();
        }
        let mut verts = vec![[0.0_f32; 2]; n as usize];
        unsafe { data_fn(id, verts.as_mut_ptr() as *mut c_void) };
        verts
    }

    // --- Spatial anchors (libXREALXRPlugin.so; see docs/develop/plans/ar-features-plan.md). Needs 6DoF +
    //     the nr_spatial_anchor.aar backend. ---

    /// Enable/disable the anchor subsystem. Returns whether the export was present (call before use).
    pub fn set_anchor_enabled(&self, enabled: bool) -> bool {
        match self.set_anchor_enabled {
            Some(f) => {
                unsafe { f(enabled) };
                true
            }
            None => false,
        }
    }

    /// Point the anchor subsystem at a writable directory for its saved-anchor map files.
    pub fn set_anchor_mapping_dir(&self, dir: &str) -> bool {
        let (Some(f), Ok(c)) = (self.set_anchor_mapping_dir, std::ffi::CString::new(dir)) else {
            return false;
        };
        unsafe { f(c.as_ptr()) };
        true
    }

    /// Create a new anchor at `pose` (Unity space). `None` if the export is absent or the SDK fails.
    pub fn acquire_anchor(&self, pose: UnityPose) -> Option<AnchorSample> {
        let f = self.acquire_anchor?;
        let mut buf = [0u8; 128]; // >= xr_anchor::ELEMENT_SIZE; the SDK writes the element into it
        let ok = unsafe { f(pose, buf.as_mut_ptr() as *mut c_void) };
        if !ok {
            return None;
        }
        Some(unsafe { read_anchor_at(buf.as_ptr()) })
    }

    /// Poll the anchor added/updated/removed changes since the last call. Copies out of the SDK's
    /// (transient) arrays immediately. `None` when the export is absent.
    pub fn poll_anchor_changes(&self) -> Option<AnchorChanges> {
        let f = self.get_anchor_changes?;
        let mut changes = ArSubsystemChanges::default();
        unsafe { f(&mut changes) };
        let stride = changes.element_size as usize;
        // A stride smaller than the fields we read means an unexpected layout, so bail rather than read
        // out of bounds; the expected element_size is `xr_anchor::ELEMENT_SIZE`.
        if stride < xr_anchor::SESSION_ID + std::mem::size_of::<Guid>() {
            if changes.added_count != 0 || changes.updated_count != 0 {
                godot::global::godot_warn!(
                    "[xreal] anchor changes: element_size={stride} < expected {}; skipping parse",
                    xr_anchor::ELEMENT_SIZE
                );
            }
            return Some(AnchorChanges {
                added: Vec::new(),
                updated: Vec::new(),
                removed: read_removed_ids(
                    changes.removed_ptr,
                    sane_count(changes.removed_count, "anchor removed"),
                ),
            });
        }
        Some(AnchorChanges {
            added: read_anchors(
                changes.added_ptr,
                sane_count(changes.added_count, "anchor added"),
                stride,
            ),
            updated: read_anchors(
                changes.updated_ptr,
                sane_count(changes.updated_count, "anchor updated"),
                stride,
            ),
            removed: read_removed_ids(
                changes.removed_ptr,
                sane_count(changes.removed_count, "anchor removed"),
            ),
        })
    }

    /// Persist an anchor and return its `Guid` key. `None` if the export is absent or the SDK fails
    /// (estimate quality ≥ SUFFICIENT first).
    pub fn save_anchor(&self, id: TrackableId) -> Option<Guid> {
        let f = self.save_anchor?;
        let mut guid = Guid::default();
        let ok = unsafe { f(id, &mut guid) };
        ok.then_some(guid)
    }

    /// Restore a saved anchor by its `Guid`. `None` if the export is absent or the SDK fails.
    pub fn load_anchor(&self, guid: Guid) -> Option<AnchorSample> {
        let f = self.load_anchor?;
        let mut buf = [0u8; 128];
        let ok = unsafe { f(guid, buf.as_mut_ptr() as *mut c_void) };
        if !ok {
            return None;
        }
        Some(unsafe { read_anchor_at(buf.as_ptr()) })
    }

    /// Drop a tracked anchor. Returns the SDK bool (or `false` if the export is absent).
    pub fn remove_anchor(&self, id: TrackableId) -> bool {
        match self.remove_anchor {
            Some(f) => unsafe { f(id) },
            None => false,
        }
    }

    /// Re-localize an anchor into the current map. Returns the SDK bool (or `false` if absent).
    pub fn remap_anchor(&self, id: TrackableId) -> bool {
        match self.remap_anchor {
            Some(f) => unsafe { f(id) },
            None => false,
        }
    }

    /// Estimate an anchor's save quality (`ffi::anchor_quality`) at `pose`. `None` if the export is
    /// absent or the SDK fails.
    pub fn estimate_anchor_quality(&self, id: TrackableId, pose: UnityPose) -> Option<i32> {
        let f = self.estimate_anchor_quality?;
        let mut quality = -1_i32;
        let ok = unsafe { f(id, pose, &mut quality) };
        ok.then_some(quality)
    }

    // --- Image tracking (libXREALXRPlugin.so; see docs/develop/plans/ar-features-plan.md). Needs 6DoF +
    //     the nr_image_tracking.aar backend + assets/nr_plugins.json + a DB blob. ---

    /// Build a tracking database from a blob (from `trackableImageTools`) + its per-image metadata.
    /// Returns the DB handle (`None` if the export is absent or the SDK returns a 0 handle). The two
    /// slices must outlive the call only (the SDK copies the data it needs).
    pub fn init_image_database(&self, blob: &[u8], refs: &[ManagedReferenceImage]) -> Option<u64> {
        let f = self.init_image_db?;
        let db = NativeView {
            data: blob.as_ptr() as *const c_void,
            count: blob.len() as i32,
        };
        let managed = NativeView {
            data: refs.as_ptr() as *const c_void,
            count: refs.len() as i32,
        };
        let handle = unsafe { f(db, managed) };
        (handle != 0).then_some(handle)
    }

    /// Activate a database (pass `0` to disable image tracking). No-op if the export is absent.
    pub fn set_image_database(&self, handle: u64) {
        if let Some(f) = self.set_image_db {
            unsafe { f(handle) };
        }
    }

    /// Number of reference images in a database, or `0` if the export is absent.
    pub fn image_reference_count(&self, handle: u64) -> i32 {
        self.get_reference_image_count
            .map(|f| unsafe { f(handle) })
            .unwrap_or(0)
    }

    /// Free a database. No-op if the export is absent.
    pub fn release_image_database(&self, handle: u64) {
        if let Some(f) = self.release_image_db {
            unsafe { f(handle) };
        }
    }

    /// Poll the tracked-image added/updated/removed changes since the last call. `None` when the
    /// export is absent.
    pub fn poll_image_changes(&self) -> Option<ImageChanges> {
        let f = self.get_image_changes?;
        let mut changes = ArSubsystemChanges::default();
        unsafe { f(&mut changes) };
        let stride = changes.element_size as usize;
        if stride < xr_tracked_image::TRACKING_STATE + 4 {
            if changes.added_count != 0 || changes.updated_count != 0 {
                godot::global::godot_warn!(
                    "[xreal] image changes: element_size={stride} < expected {}; skipping parse",
                    xr_tracked_image::ELEMENT_SIZE
                );
            }
            return Some(ImageChanges {
                added: Vec::new(),
                updated: Vec::new(),
                removed: read_removed_ids(
                    changes.removed_ptr,
                    sane_count(changes.removed_count, "image removed"),
                ),
            });
        }
        Some(ImageChanges {
            added: read_images(
                changes.added_ptr,
                sane_count(changes.added_count, "image added"),
                stride,
            ),
            updated: read_images(
                changes.updated_ptr,
                sane_count(changes.updated_count, "image updated"),
                stride,
            ),
            removed: read_removed_ids(
                changes.removed_ptr,
                sane_count(changes.removed_count, "image removed"),
            ),
        })
    }

    /// Whether the RGB-camera C ABI is available (libXREALXRPlugin.so present + symbols resolved).
    pub fn rgb_camera_available(&self) -> bool {
        // Require stop and dispose too: without them a partially-resolved build could start a capture
        // it cannot stop, and would leak every acquired frame handle (the grab paths only dispose
        // `if let Some(d) = dispose`).
        self.rgb_start_capture.is_some()
            && self.rgb_stop_capture.is_some()
            && self.rgb_try_acquire_latest.is_some()
            && self.rgb_get_data_plane.is_some()
            && self.rgb_dispose_handle.is_some()
    }

    /// Start RGB-camera capture in **poll mode**, with a null callback. It returns the capture handle
    /// for [`Self::rgb_camera_stop`], or `None` when the export is unavailable or the SDK reports
    /// failure. NOTE: in poll mode a successful start returns a `0` handle, since there is no callback
    /// registration to track, and that is **not** a failure: capture is enabled and
    /// [`Self::rgb_camera_grab_y`] then works, as confirmed on device. A wedged glasses camera, where
    /// an unclean prior exit left it holding the connection so NRSDK rejects the new one with
    /// "RgbCamera Recv Frame, -99" or "Plugin Start failed", returns the `u64::MAX`, that is -1, error
    /// sentinel instead. Surface that as `None`, so the caller never caches a dead handle and drives an
    /// unfed, pink panel.
    pub fn rgb_camera_start(&self) -> Option<u64> {
        let f = self.rgb_start_capture?;
        let handle = unsafe { f(std::ptr::null_mut(), std::ptr::null_mut()) };
        if handle == u64::MAX {
            return None;
        }
        Some(handle)
    }

    /// Stop RGB-camera capture (`false` if unavailable).
    pub fn rgb_camera_stop(&self, handle: u64) -> bool {
        match self.rgb_stop_capture {
            Some(f) => unsafe { f(handle) },
            None => false,
        }
    }

    /// Poll the latest RGB-camera frame and copy its **Y plane** (full-res 8-bit luma) into a
    /// freshly-allocated buffer. Returns `(bytes, width, height)`, or `None` if no fresh frame /
    /// unavailable. The SDK frame handle is disposed before returning, so nothing is left pinned.
    pub fn rgb_camera_grab_y(&self) -> Option<(Vec<u8>, i32, i32)> {
        let acquire = self.rgb_try_acquire_latest?;
        let get_plane = self.rgb_get_data_plane?;
        unsafe {
            let mut frame_handle: i32 = 0;
            let mut resolution = NrSize2i::default();
            let mut timestamp: u64 = 0;
            // Serialise the acquire against the SDK camera receive thread; see `rgb_holder_mutex`.
            if !rgb_acquire_latest_locked(
                acquire,
                &mut frame_handle,
                &mut resolution,
                &mut timestamp,
            ) {
                return None;
            }
            // Best-effort dispose on every exit path once we hold a valid handle.
            let dispose = self.rgb_dispose_handle;
            let mut data_ptr: *mut c_void = std::ptr::null_mut();
            let mut size = NrSize2i::default();
            let ok = get_plane(frame_handle, 0, &mut data_ptr, &mut size);
            let result = if ok && !data_ptr.is_null() && size.width > 0 && size.height > 0 {
                let len = (size.width as usize) * (size.height as usize);
                let bytes = std::slice::from_raw_parts(data_ptr as *const u8, len).to_vec();
                Some((bytes, size.width, size.height))
            } else {
                None
            };
            if let Some(d) = dispose {
                d(frame_handle);
            }
            result
        }
    }

    /// Poll the latest RGB-camera frame and copy its planes as **Y**, full-res 8-bit, plus a **CbCr**
    /// buffer interleaved from the chroma planes; in I420 plane 1 is V/Cr and plane 2 is U/Cb, both
    /// half-res. It returns `(y, y_w, y_h, cbcr, c_w, c_h)`, where `cbcr` is `[Cb, Cr, Cb, Cr, …]` with
    /// `Cb = U` and `Cr = V`, the RG8 layout Godot's `set_ycbcr_images` and a YCbCr shader expect.
    /// The frame handle is disposed before returning.
    ///
    /// `last_timestamp` gates the copy. `TryAcquireLatestImage` hands out a fresh handle to the
    /// *same* latest frame when nothing new has been published, so polling at 60 Hz over a 30 Hz
    /// camera re-copies and re-uploads an image we already have on roughly every other call. When
    /// the acquired timestamp still equals `*last_timestamp` the handle is disposed immediately and
    /// `None` is returned; on a new frame `*last_timestamp` advances. A timestamp of `0` never
    /// gates, so an SDK build that leaves the field untouched keeps working.
    ///
    /// Comparing timestamps is deliberate. The SDK also exports `TryGetRGBCameraFrame`, a cheaper
    /// "new frame?" flag, but reading it is a *destructive*, unlocked read-and-clear of shared state:
    /// only one caller in the process may use it, and a publish landing between its load and store is
    /// lost. The timestamp is already an out-parameter of the acquire we do anyway, and the extra cost
    /// over the flag is one hash-map insert and erase. See
    /// `docs/develop/archive/codex-camera-acquire-analysis.md`.
    pub fn rgb_camera_grab_yuv(
        &self,
        last_timestamp: &mut u64,
        timings: &mut GrabTimings,
    ) -> Option<YuvFrame> {
        let interleave_us = std::cell::Cell::new(0u32);
        let out = self.rgb_camera_with_frame(last_timestamp, timings, |p| {
            let t = std::time::Instant::now();
            let y = p.y.to_vec();
            let mut cbcr = Vec::new();
            interleave_cbcr(p.u, p.v, p.chroma_width, p.chroma_height, &mut cbcr);
            interleave_us.set(t.elapsed().as_micros() as u32);
            Some((
                y,
                p.y_width,
                p.y_height,
                cbcr,
                p.chroma_width,
                p.chroma_height,
            ))
        });
        timings.interleave_us = interleave_us.get();
        out
    }

    /// Acquire the latest RGB frame and hand its planes to `consume` **without copying them**: the
    /// slices point straight into the SDK's frame buffer. The handle is disposed as soon as `consume`
    /// returns, which is why [`RgbPlanes`] borrows, since the lifetime stops the slices from outliving
    /// the frame. When `consume` returns `None` the `last_timestamp` stays unadvanced, so the same
    /// frame is retried on the next poll.
    ///
    /// The gating and timestamp semantics are as described on [`Self::rgb_camera_grab_yuv`], which is
    /// implemented on top of this. `timings` receives the acquire, plane-fetch and dispose costs, and
    /// whatever `consume` does with the pixels is the caller's to measure. Note that `planes_us` here
    /// covers the three `TryGetRGBCameraDataPlane` calls *only*, with no copy, so it should be a
    /// handful of microseconds.
    pub fn rgb_camera_with_frame<R>(
        &self,
        last_timestamp: &mut u64,
        timings: &mut GrabTimings,
        consume: impl FnOnce(RgbPlanes<'_>) -> Option<R>,
    ) -> Option<R> {
        let acquire = self.rgb_try_acquire_latest?;
        let get_plane = self.rgb_get_data_plane?;
        let dispose = self.rgb_dispose_handle;

        let mut frame_handle: i32 = 0;
        let mut resolution = NrSize2i::default();
        let mut timestamp: u64 = 0;
        let t_acquire = std::time::Instant::now();
        // Serialise the acquire against the SDK camera receive thread; see `rgb_holder_mutex`.
        if !unsafe {
            rgb_acquire_latest_locked(acquire, &mut frame_handle, &mut resolution, &mut timestamp)
        } {
            return None;
        }
        timings.acquire_us = t_acquire.elapsed().as_micros() as u32;
        // Same frame as the last poll, so drop the handle without touching the planes.
        if timestamp != 0 && timestamp == *last_timestamp {
            if let Some(d) = dispose {
                unsafe { d(frame_handle) };
            }
            return None;
        }

        let t = std::time::Instant::now();
        // SAFETY: the borrows live only until `dispose` below, and `consume` cannot leak them out.
        let planes = unsafe {
            (|| {
                let (y, yw, yh) = borrow_plane(get_plane, frame_handle, 0)?; // Y, full-res
                let (v, _, _) = borrow_plane(get_plane, frame_handle, 1)?; // plane 1 = V (Cr)
                let (u, cw, ch) = borrow_plane(get_plane, frame_handle, 2)?; // plane 2 = U (Cb)
                Some(RgbPlanes {
                    y,
                    y_width: yw,
                    y_height: yh,
                    u,
                    v,
                    chroma_width: cw,
                    chroma_height: ch,
                })
            })()
        };
        timings.planes_us = t.elapsed().as_micros() as u32;

        let out = planes.and_then(consume);
        // Only advance on success, so a transient plane-read failure is retried on the next poll.
        if out.is_some() {
            *last_timestamp = timestamp;
        }
        let t_dispose = std::time::Instant::now();
        if let Some(d) = dispose {
            unsafe { d(frame_handle) };
        }
        timings.dispose_us = t_dispose.elapsed().as_micros() as u32;
        out
    }

    /// Diagnostic: raw HMD clock from each layer (SessionManager, XR-plugin), to see which
    /// one is actually delivering data.
    pub fn hmd_time_probe(&self) -> (Option<u64>, Option<u64>) {
        let probe = |f: Option<FnHmdTimeNanos>| {
            f.and_then(|f| {
                let mut t = 0u64;
                let status = unsafe { f(&mut t) };
                ((status == 0 || status == 1) && t != 0).then_some(t)
            })
        };
        (
            probe(Some(self.hmd_time_nanos)),
            probe(self.xp_hmd_time_nanos),
        )
    }

    /// Reset the forward direction (no-op if the plugin/symbol is unavailable).
    pub fn recenter_glasses(&self) {
        if let Some(f) = self.recenter_glasses {
            unsafe { f() }
        }
    }

    /// Set the display proximity-sensor bypass. `bypass=true` stops the glasses from powering the
    /// display off after idle (the wear/proximity sensor). Returns the SDK status, or `None` if
    /// the symbol is absent. The underlying C wrapper no-ops until `NativeGlasses` is ready
    /// (post session start), so this may need to be called again after the session is live.
    pub fn set_display_bypass_psensor(&self, bypass: bool) -> Option<i32> {
        self.set_display_bypass_psensor
            .map(|f| unsafe { f(bypass as i32) })
    }

    /// `SetGlassesSpaceMode(NRGlassesSpaceMode)`, from libXREALXRPlugin.so: how the glasses' X1 chip
    /// anchors the virtual screen in space, whether follow, world-anchor or another mode. The enum
    /// values are RE'd and unverified, and it is exposed so the mode can be probed at runtime from
    /// GDScript. The C wrapper safely returns 0 until NativeGlasses is ready, and this returns `None`
    /// when the symbol is absent.
    pub fn set_glasses_space_mode(&self, mode: i32) -> Option<i32> {
        self.set_glasses_space_mode.map(|f| unsafe { f(mode) })
    }

    /// Register the process-wide glasses hardware event callback (keys, wear sensor,
    /// brightness/volume/EC changes…). The callback is invoked on an SDK-owned thread with
    /// a 16-byte `GlassesEventData` by value (ABI from the Unity C# `[DllImport]`
    /// `SetGlassesEventCallback`). Returns `false` if the symbol is unavailable.
    pub fn set_glasses_event_callback(&self, callback: FnGlassesEventCallback) -> bool {
        match self.set_glasses_event_callback {
            Some(f) => {
                unsafe { f(callback) };
                true
            }
            None => false,
        }
    }

    pub fn set_native_error_callback(&self, callback: crate::ffi::FnNativeErrorCallback) -> bool {
        match self.set_native_error_callback {
            Some(f) => {
                unsafe { f(callback) };
                true
            }
            None => false,
        }
    }

    /// PopulateNextFrameDesc → CreateFrame → SubmitCurrentFrame probe via the DisplayManager path.
    ///
    /// RE: `CreateFrame()` checks `libXREALXRPlugin.so + 0xdb410` (first byte of the
    /// `UnityXRNextFrameDesc` function-local static at +0x10). That byte is initialised to 0
    /// and only becomes non-zero once `PopulateNextFrameDesc` is called with the static's
    /// address as `desc`. Calling with a temporary buffer (the previous diagnostic) left the
    /// static untouched. This method passes `display_manager_desc_ptr` (= lib_base + 0xdb400)
    /// so the byte is set before `CreateFrame()` is invoked.
    ///
    /// `SubmitCurrentFrame` reads the same byte: non-zero → skips `UpdateMetrics` (which
    /// crashed before) and goes directly to `WaitForTargetFrameRate` → safe path.
    pub fn display_manager_submit_frame_probe(&mut self) -> String {
        let desc = match self.display_manager_desc_ptr {
            Some(d) => d,
            None => return "no desc_ptr (plugin lib not loaded)".into(),
        };

        // Read the gate byte BEFORE populate to see its initial value.
        let gate_byte_before = unsafe { *(desc as *const u8).add(0x10) };

        // Call PopulateNextFrameDesc with the global UnityXRNextFrameDesc static.
        // This writes a non-zero render-pass indicator to desc+0x10 (gate byte = 0xa6
        // on XREAL One Pro) and populates render-pass / texture fields at various offsets.
        let populate_status =
            crate::unity_plugin::populate_registered_display_frame_desc_with_ptr(desc);

        let gate_byte = unsafe { *(desc as *const u8).add(0x10) };
        let read_u64_at = |off: usize| -> u64 { unsafe { *(desc as *const u64).byte_add(off) } };
        godot::global::godot_print!(
            "[xreal] DisplayManager desc gate_byte(+0x10): before={gate_byte_before:#04x} \
             after={gate_byte:#04x} (populate_status={populate_status})"
        );
        // Log key offsets from the desc that look like texture/swapchain handles or frame counts.
        godot::global::godot_print!(
            "[xreal] desc fields: +0x08={:#018x} +0x18={:#018x} +0x24={:#018x} +0x28={:#018x} \
             +0x30={:#018x} +0x38={:#018x} +0x3f0={:#018x} +0x410={:#018x} +0x450={:#018x} \
             +0x580={:#018x}",
            read_u64_at(0x08),
            read_u64_at(0x18),
            read_u64_at(0x24),
            read_u64_at(0x28),
            read_u64_at(0x30),
            read_u64_at(0x38),
            read_u64_at(0x3f0),
            read_u64_at(0x410),
            read_u64_at(0x450),
            read_u64_at(0x580)
        );

        // DO NOT call CreateFrame() or SubmitCurrentFrame() here.
        // The XREAL SDK's own rendering thread, the GLThread, manages DisplayManager+0x120.
        // Calling CreateFrame tries to destroy the SDK's live frame through NativeRendering::DestroyFrame,
        // which fails because the render thread holds the frame, and then crashes in LogHelper::Error at
        // fault address 0xb9a40998bac55c8a, a valid SDK frame handle passed to an fprintf-style log.
        // Device-confirmed: both CreateFrame and SubmitCurrentFrame crash with SIGSEGV on the DestroyFrame
        // path while the SDK's rendering thread owns DisplayManager+0x120.
        //
        // Next step: hook into the SDK's rendering loop properly by providing Godot textures
        // to the SetBufferViewport path BEFORE the SDK calls SubmitCurrentFrame on its own thread.

        format!(
            "gate_before={gate_byte_before:#x} populate={populate_status} gate_after={gate_byte:#x}"
        )
    }

    /// RE / unverified: probe the Unity-plugin DisplayManager path. This mirrors Unity's
    /// public native calls and avoids the direct `NRFrameCreate` export, whose frame
    /// wrapper table is currently uninitialized under Godot.
    #[allow(dead_code)]
    pub fn unity_display_manager_probe(&mut self) -> Result<bool, &'static str> {
        let initialize = self
            .initialize_rendering
            .ok_or("InitializeRendering missing")?;
        let create_frame = self.create_frame.ok_or("CreateFrame missing")?;

        unsafe { initialize() };
        self.display_manager_rendering_initialized = true;
        let created_frame = unsafe { create_frame() };
        godot::global::godot_print!(
            "[xreal] Unity DisplayManager probe: InitializeRendering -> CreateFrame = \
             {created_frame}"
        );
        Ok(created_frame)
    }

    /// RE / unverified: probe the XREAL XRDisplaySubsystem-backed frame path after the
    /// provider lifecycle has started. Unity normally drives this from XRDisplaySubsystem.
    pub fn unity_display_frame_probe(&mut self) -> Result<(bool, usize), &'static str> {
        let create_frame = self.create_frame.ok_or("CreateFrame missing")?;
        let get_frame_metadata = self.get_frame_metadata.ok_or("GetFrameMetaData missing")?;

        let created_frame = unsafe { create_frame() };
        let metadata = unsafe { get_frame_metadata() };
        let metadata_size = if metadata.ptr.is_null() {
            0
        } else {
            metadata.size
        };
        godot::global::godot_print!(
            "[xreal] Unity DisplayManager frame probe: CreateFrame={created_frame}, \
             metadata_ptr={:?}, metadata_size={metadata_size}",
            metadata.ptr
        );
        Ok((created_frame, metadata_size))
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

    /// Whether the direct NR rendering/compositor symbols were resolved from
    /// `libnr_loader.so`. This does not imply the compositor is initialized yet.
    pub fn nr_rendering_available(&self) -> bool {
        self.nr_rendering.is_some()
    }

    /// Number of direct NR rendering symbols resolved. Useful as a device-side sanity
    /// check before wiring texture handoff.
    pub fn nr_rendering_symbol_count(&self) -> usize {
        self.nr_rendering
            .as_ref()
            .map(NrRenderingApi::resolved_symbol_count)
            .unwrap_or(0)
    }

    /// RE probe: call only after session bootstrap. It creates and immediately destroys an
    /// NR rendering handle, without starting presentation or touching textures.
    pub fn nr_rendering_smoke_create_destroy(&self) -> Result<(), i32> {
        self.nr_rendering
            .as_ref()
            .ok_or(-1)
            .and_then(NrRenderingApi::smoke_create_destroy)
    }

    /// RE probe: call only after session bootstrap. It creates an NR rendering handle,
    /// starts/stops it, then destroys it without submitting frames.
    pub fn nr_rendering_smoke_start_stop(&self) -> Result<(), i32> {
        self.nr_rendering
            .as_ref()
            .ok_or(-1)
            .and_then(NrRenderingApi::smoke_start_stop)
    }
}

impl Drop for XrealNative {
    fn drop(&mut self) {
        if self.display_manager_rendering_initialized {
            if let Some(deinitialize) = self.deinitialize_rendering {
                unsafe { deinitialize() };
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::{sane_count, MAX_TRACKABLES};

    #[test]
    fn sane_count_passes_values_in_range() {
        assert_eq!(sane_count(0, "t"), 0);
        assert_eq!(sane_count(1, "t"), 1);
        assert_eq!(sane_count(MAX_TRACKABLES, "t"), MAX_TRACKABLES);
    }

    #[test]
    fn sane_count_clamps_out_of_range_to_zero() {
        // The guard that prevents OOB reads from a stale/garbage change count (device crash trap).
        assert_eq!(sane_count(-1, "t"), 0);
        assert_eq!(sane_count(MAX_TRACKABLES + 1, "t"), 0);
        assert_eq!(sane_count(i32::MAX, "t"), 0);
        assert_eq!(sane_count(i32::MIN, "t"), 0);
    }
}
