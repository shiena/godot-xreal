//! Depth mesh (spatial meshing) via internal `libXREALXRPlugin.so` functions called by `LIB_BASE +
//! offset` — the same mechanism [`crate::hand_tracking`] uses (dlsym can't reach these non-exported
//! symbols). See `docs/plans/ar-features-plan.md` §4 for the codex RE.
//!
//! Unity surfaces meshing through the engine `XRMeshSubsystem` + a native provider whose
//! `GetMeshInfos`/`AcquireMesh` take engine-supplied allocators — but the raw geometry lives in plain
//! C++ `std::vector`s inside each `MeshBlockInfo`, produced by `NativePerception::GetMeshBlockInfo()`
//! **before** any allocator is involved. Path B here bypasses the engine entirely: enable meshing, poll
//! the block vector each frame, copy vertices/normals/indices out, and free the SDK's C++ vectors with
//! libc++ `operator delete`.
//!
//! **Air 2 Ultra only** (meshing = `GetSupportedFeatures() & (1<<3)`). Coordinate signs are
//! on-device-verify-pending, like the other trackables.
//!
//! This module also owns the shared `NativePerception::GetSupportedFeatures()` capability query and the
//! `*_supported()` gates it drives — plane / image / anchor / meshing (see [`feature_supported`]) — the
//! device-accurate replacement for the per-trackable dlsym symbol-presence checks.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;

use libloading::Library;

// --- Internal symbol offsets in libXREALXRPlugin.so (verified against the vendored .so via llvm-nm) ---
const OFF_GET_INPUT_MANAGER: usize = 0x47a10; // TSingleton<InputManager>::GetInstance()
const OFF_SET_MESHING_ENABLED: usize = 0x9a4a8; // NativePerception::SetMeshingEnabled(bool)
const OFF_GET_MESH_BLOCK_INFO: usize = 0x9a664; // NativePerception::GetMeshBlockInfo() -> vector<MeshBlockInfo>
const OFF_GET_SUPPORTED_FEATURES: usize = 0x96214; // NativePerception::GetSupportedFeatures() -> u64

const IM_PERCEPTION_PTR: usize = 0x48; // InputManager + 0x48 = NativePerception*
const NP_STARTED: usize = 0x18; // NativePerception + 0x18 (non-zero once start succeeded)
const NP_SESSION: usize = 0x28; // NativePerception + 0x28 (NR session handle)
const NP_CONFIG: usize = 0x38; // NativePerception + 0x38 (NR config handle)

// `NRPerceptionFeature` bits in `NativePerception::GetSupportedFeatures()`, RE'd from the SDK's own
// gates in libXREALXRPlugin.so — the bit index is the value each gate tests (plane / image / anchor via
// `InputManager::IsFeatureSupported(NRPerceptionFeature)`, meshing via `GetMeshInfos`, hand via
// `IsHandTrackingSupported`): bit 0 = plane, 1 = image tracking, 2 = anchor, 3 = meshing, 4 = hand.
// This is the device-accurate capability gate (only the Air 2 Ultra reports the trackables). Exported-
// symbol presence CANNOT gate by device — there is one shared libXREALXRPlugin.so whose exports are
// identical on every model — so these bits replace the old dlsym-presence checks.
const PLANE_FEATURE_BIT: u64 = 1 << 0;
const IMAGE_FEATURE_BIT: u64 = 1 << 1;
const ANCHOR_FEATURE_BIT: u64 = 1 << 2;
const MESHING_FEATURE_BIT: u64 = 1 << 3;

// --- MeshBlockInfo layout (128-byte block, device-confirmed from the AcquireMesh copy loop). Each
//     std::vector is {begin, end, cap} (24 B); count = (end-begin)/elem_size. ---
const MB_ID: usize = 0x00; // u64 (TrackableId.subId2)
const MB_STATE: usize = 0x08; // i32 NRMeshingBlockState (2 = removed)
const MB_VERTICES: usize = 0x38; // vector<Vector3> (12 B/elem)
const MB_NORMALS: usize = 0x50; // vector<Vector3> (12 B/elem)
const MB_INDICES: usize = 0x68; // vector<u32> (4 B/elem)
const MESH_BLOCK_STRIDE: usize = 0x80; // 128 bytes

/// Sanity caps against a garbage vector length driving an OOB read (the SDK vectors are transient).
const MAX_BLOCKS: usize = 4096;
const MAX_VERTS: usize = 4_000_000;
const MAX_INDICES: usize = 12_000_000;

static MESHING_ENABLED: AtomicBool = AtomicBool::new(false);
/// Cached libc++ `operator delete(void*)` (`_ZdlPv`) for freeing the SDK's vector storage.
static OP_DELETE: AtomicUsize = AtomicUsize::new(0);

type FnGetInputManager = unsafe extern "C" fn() -> *mut u8;
type FnSetMeshingEnabled = unsafe extern "C" fn(*mut u8, bool);
type FnGetSupportedFeatures = unsafe extern "C" fn(*mut u8) -> u64;
/// `NativePerception::GetMeshBlockInfo(this) -> std::vector<MeshBlockInfo>` — the 24-byte vector is
/// returned via x8 sret; Rust models that as a struct return.
type FnGetMeshBlockInfo = unsafe extern "C" fn(*mut u8) -> CppVec;

/// A libc++ `std::vector<T>` header: `{ begin, end, capacity }`.
#[repr(C)]
struct CppVec {
    begin: *mut u8,
    end: *mut u8,
    _cap: *mut u8,
}

/// One meshing block copied out of the SDK. `vertices`/`normals`/`indices` are **raw backend space**;
/// the Godot side applies the coordinate flip. `state == 2` means the block was removed.
pub struct MeshBlock {
    pub id: u64,
    pub state: i32,
    pub vertices: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

/// Resolve `(lib_base, NativePerception*)` once the SDK's perception is fully up; `None` otherwise.
unsafe fn perception() -> Option<(usize, *mut u8)> {
    let lib_base = crate::signal_guard::lib_base();
    if lib_base == 0 {
        return None;
    }
    let get_im: FnGetInputManager = std::mem::transmute(lib_base + OFF_GET_INPUT_MANAGER);
    let input_manager = get_im();
    if input_manager.is_null() {
        return None;
    }
    let np = (input_manager.add(IM_PERCEPTION_PTR) as *const *mut u8).read();
    if np.is_null() {
        return None;
    }
    let started = np.add(NP_STARTED).read();
    let session = (np.add(NP_SESSION) as *const u64).read();
    let config = (np.add(NP_CONFIG) as *const u64).read();
    if started == 0 || session == 0 || config == 0 {
        return None; // perception not fully brought up yet — retry next frame
    }
    Some((lib_base, np))
}

/// The `NativePerception::GetSupportedFeatures()` bitmask, or `None` until the perception is fully up.
fn supported_features() -> Option<u64> {
    unsafe {
        let (lib_base, np) = perception()?;
        let get_features: FnGetSupportedFeatures =
            std::mem::transmute(lib_base + OFF_GET_SUPPORTED_FEATURES);
        Some(get_features(np))
    }
}

/// Whether a `*_FEATURE_BIT` capability is reported by the connected glasses. Device-accurate, but
/// `false` until the perception is fully up (so query it at feature-toggle time, not at startup) and on
/// devices that don't support it (only the Air 2 Ultra reports the trackables).
fn feature_supported(bit: u64) -> bool {
    supported_features().is_some_and(|f| f & bit != 0)
}

/// Whether the glasses support spatial meshing (`GetSupportedFeatures` bit 3).
pub fn meshing_supported() -> bool {
    feature_supported(MESHING_FEATURE_BIT)
}

/// Whether the glasses support plane detection (`GetSupportedFeatures` bit 0). Replaces the old dlsym
/// symbol-presence check, which couldn't tell models apart (one shared .so).
pub fn plane_detection_supported() -> bool {
    feature_supported(PLANE_FEATURE_BIT)
}

/// Whether the glasses support image tracking (`GetSupportedFeatures` bit 1).
pub fn image_tracking_supported() -> bool {
    feature_supported(IMAGE_FEATURE_BIT)
}

/// Whether the glasses support spatial anchors (`GetSupportedFeatures` bit 2).
pub fn anchor_supported() -> bool {
    feature_supported(ANCHOR_FEATURE_BIT)
}

/// Enable/disable meshing (`NativePerception::SetMeshingEnabled`). Returns whether the call was made
/// (perception up). Idempotent on the cached flag; safe to call each frame while bringing up.
pub fn set_meshing_enabled(on: bool) -> bool {
    unsafe {
        let Some((lib_base, np)) = perception() else {
            return false;
        };
        let set_enabled: FnSetMeshingEnabled =
            std::mem::transmute(lib_base + OFF_SET_MESHING_ENABLED);
        set_enabled(np, on);
        MESHING_ENABLED.store(on, Ordering::Relaxed);
        godot::global::godot_print!(
            "[xreal] meshing {}",
            if on { "enabled" } else { "disabled" }
        );
        true
    }
}

/// Poll the current mesh blocks. Copies each block's vertices/normals/indices out of the SDK's
/// transient C++ vectors and frees them. Empty when meshing is off / unsupported / not yet producing.
pub fn poll_mesh_blocks() -> Vec<MeshBlock> {
    if !MESHING_ENABLED.load(Ordering::Relaxed) {
        return Vec::new();
    }
    unsafe {
        let Some((lib_base, np)) = perception() else {
            return Vec::new();
        };
        let get_blocks: FnGetMeshBlockInfo =
            std::mem::transmute(lib_base + OFF_GET_MESH_BLOCK_INFO);
        let vec = get_blocks(np);
        if vec.begin.is_null() || vec.end < vec.begin {
            free_op(vec.begin);
            return Vec::new();
        }
        let total = (vec.end as usize - vec.begin as usize) / MESH_BLOCK_STRIDE;
        let count = total.min(MAX_BLOCKS);
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let block = vec.begin.add(i * MESH_BLOCK_STRIDE);
            let (vertices, v_begin) = read_vec3(block, MB_VERTICES);
            let (normals, n_begin) = read_vec3(block, MB_NORMALS);
            let (indices, i_begin) = read_u32(block, MB_INDICES);
            out.push(MeshBlock {
                id: (block.add(MB_ID) as *const u64).read_unaligned(),
                state: (block.add(MB_STATE) as *const i32).read_unaligned(),
                vertices,
                normals,
                indices,
            });
            // Free the block's three vector storages (libc++-allocated).
            free_op(v_begin);
            free_op(n_begin);
            free_op(i_begin);
        }
        // Free the block array itself.
        free_op(vec.begin);
        out
    }
}

/// Read a `std::vector<Vector3>` at `block + off`, returning the copied points and the storage `begin`
/// pointer (to free). `Vector3` here is 12 bytes (3× f32), read verbatim (Godot side flips signs).
unsafe fn read_vec3(block: *const u8, off: usize) -> (Vec<[f32; 3]>, *mut u8) {
    let begin = (block.add(off) as *const *mut u8).read_unaligned();
    let end = (block.add(off + 8) as *const *mut u8).read_unaligned();
    if begin.is_null() || end < begin {
        return (Vec::new(), std::ptr::null_mut());
    }
    let count = ((end as usize - begin as usize) / 12).min(MAX_VERTS);
    let p = begin as *const [f32; 3];
    let v = (0..count).map(|i| p.add(i).read_unaligned()).collect();
    (v, begin)
}

/// Read a `std::vector<u32>` at `block + off`, returning the copied indices and the storage `begin`.
unsafe fn read_u32(block: *const u8, off: usize) -> (Vec<u32>, *mut u8) {
    let begin = (block.add(off) as *const *mut u8).read_unaligned();
    let end = (block.add(off + 8) as *const *mut u8).read_unaligned();
    if begin.is_null() || end < begin {
        return (Vec::new(), std::ptr::null_mut());
    }
    let count = ((end as usize - begin as usize) / 4).min(MAX_INDICES);
    let p = begin as *const u32;
    let v = (0..count).map(|i| p.add(i).read_unaligned()).collect();
    (v, begin)
}

/// libc++ `operator delete(void*)` on a non-null pointer (frees SDK vector storage). Resolves `_ZdlPv`
/// from libc++_shared.so once; no-op if it can't be found (leak beats a wrong-allocator crash).
unsafe fn free_op(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    let f = OP_DELETE.load(Ordering::Relaxed);
    let f = if f != 0 {
        f
    } else {
        let resolved = resolve_op_delete();
        OP_DELETE.store(resolved, Ordering::Relaxed);
        resolved
    };
    if f != 0 {
        let del: unsafe extern "C" fn(*mut u8) = std::mem::transmute(f);
        del(ptr);
    }
}

/// dlsym libc++'s `operator delete(void*)` (`_ZdlPv`). The handle is leaked (libc++_shared.so is a
/// process-global dependency); returns 0 if unavailable.
fn resolve_op_delete() -> usize {
    static LIB: OnceLock<Option<Library>> = OnceLock::new();
    let lib = LIB.get_or_init(|| unsafe { Library::new("libc++_shared.so").ok() });
    let Some(lib) = lib else {
        return 0;
    };
    unsafe {
        lib.get::<unsafe extern "C" fn(*mut u8)>(b"_ZdlPv\0")
            .map(|s| *s as usize)
            .unwrap_or(0)
    }
}
