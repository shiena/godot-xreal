//! Depth mesh (spatial meshing) through internal `libXREALXRPlugin.so` functions called by
//! `LIB_BASE + offset`, the same mechanism [`crate::hand_tracking`] uses, since dlsym cannot reach
//! these non-exported symbols. See `docs/develop/plans/ar-features-plan.md` section 4 for the codex RE.
//!
//! Unity surfaces meshing through the engine `XRMeshSubsystem` and a native provider whose
//! `GetMeshInfos` and `AcquireMesh` take engine-supplied allocators. The raw geometry, though, lives
//! in plain C++ `std::vector`s inside each `MeshBlockInfo`, produced by
//! `NativePerception::GetMeshBlockInfo()` **before** any allocator is involved. Path B here bypasses
//! the engine entirely: enable meshing, poll the block vector each frame, copy the vertices, normals
//! and indices out, and free the SDK's C++ vectors with libc++ `operator delete`.
//!
//! **Air 2 Ultra only.** The coordinate signs are still pending on-device verification, like the
//! other trackables. Device gating for meshing, plane, image and anchor lives in
//! `XrealSystem::is_ar_perception_available`, which asks for 6DoF and no RGB camera.
//! `NativePerception::GetSupportedFeatures()` is NOT used: it returns a device-INDEPENDENT `0x1f`
//! mask and the SDK's own C# never calls it, so it cannot gate by device.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;

use libloading::Library;

// --- Internal symbol offsets in libXREALXRPlugin.so, verified against the vendored .so with llvm-nm ---
const OFF_GET_INPUT_MANAGER: usize = 0x47a10; // TSingleton<InputManager>::GetInstance()
const OFF_SET_MESHING_ENABLED: usize = 0x9a4a8; // NativePerception::SetMeshingEnabled(bool)
const OFF_GET_MESH_BLOCK_INFO: usize = 0x9a664; // NativePerception::GetMeshBlockInfo() -> vector<MeshBlockInfo>

const IM_PERCEPTION_PTR: usize = 0x48; // InputManager + 0x48 = NativePerception*
const NP_STARTED: usize = 0x18; // NativePerception + 0x18 (non-zero once start succeeded)
const NP_SESSION: usize = 0x28; // NativePerception + 0x28 (NR session handle)
const NP_CONFIG: usize = 0x38; // NativePerception + 0x38 (NR config handle)

// --- MeshBlockInfo layout, a 128-byte block, confirmed from BOTH ends. The producer is
//     `NativePerception::GetMeshBlockInfo`, with `vector<NRVector3f>::__append` on `block_end-0x60`
//     and `-0x48` and `vector<u32>::__append` on `-0x30`. The consumer is
//     `InputManager::AcquireMesh`, whose `x21` is the *hash node*, so its `+0x38`, `+0x50` and
//     `+0x68` are block `+0x20`, `+0x38` and `+0x50`, because the libc++ node header puts the
//     mapped `MeshBlockInfo` at node+0x18. `MeshBlockInfo::operator=` shows a 28-byte POD head then
//     exactly four vectors. Each std::vector is {begin, end, cap}, 24 B, and the count is
//     (end-begin)/elem_size. ---
const MB_ID: usize = 0x00; // u64 (TrackableId.subId2)
const MB_STATE: usize = 0x08; // i32 Unity MeshChangeState (Added0/Updated1/Removed2/Unchanged3)
const MB_VERTICES: usize = 0x20; // vector<NRVector3f> (12 B/elem)
const MB_NORMALS: usize = 0x38; // vector<NRVector3f> (12 B/elem)
const MB_INDICES: usize = 0x50; // vector<u32> (4 B/elem)
const MB_LABELS: usize = 0x68; // vector<u8> NRMeshingVertexSemanticLabel, one per vertex
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
/// `NativePerception::GetMeshBlockInfo(this) -> std::vector<MeshBlockInfo>`. The 24-byte vector
/// comes back through the x8 sret, which Rust models as a struct return.
type FnGetMeshBlockInfo = unsafe extern "C" fn(*mut u8) -> CppVec;

/// A libc++ `std::vector<T>` header: `{ begin, end, capacity }`.
#[repr(C)]
struct CppVec {
    begin: *mut u8,
    end: *mut u8,
    _cap: *mut u8,
}

/// `NRMeshingVertexSemanticLabel` (a `u8`), the class the meshing backend assigns to each **vertex**,
/// not to a face or a whole block, so one block's triangles can straddle two classes. The values are
/// the SDK's own and are deliberately non-contiguous, since 3 and 9 are absent: this is a subset of
/// an outdoor-first segmentation taxonomy, which is why `HIGHWAY`, `SIDEWALK` and `GRASS` sit beside
/// the indoor classes. `BACKGROUND` is the catch-all for everything the classifier did not place.
pub mod semantic_label {
    pub const BACKGROUND: u8 = 0;
    pub const WALL: u8 = 1;
    pub const BUILDING: u8 = 2;
    pub const FLOOR: u8 = 4;
    pub const CEILING: u8 = 5;
    pub const HIGHWAY: u8 = 6;
    pub const SIDEWALK: u8 = 7;
    pub const GRASS: u8 = 8;
    pub const DOOR: u8 = 10;
    pub const TABLE: u8 = 11;
}

/// One meshing block copied out of the SDK. `vertices` and `normals` are in **raw NR backend
/// space**, that is *before* the SDK's Unity conversion: `AcquireMesh` negates Z on its way into
/// Unity's buffers, so raw is right-handed and Unity space is `(x, y, -z)` of these values. The
/// Godot side applies the flip. `state == 2` means the block was removed.
///
/// `labels` holds one [`semantic_label`] per vertex, so it parallels `vertices` when the backend
/// classified the block. Consumers must not assume it does: it comes back empty when meshing is
/// running without classification, so check its length against `vertices` before indexing.
pub struct MeshBlock {
    pub id: u64,
    pub state: i32,
    pub vertices: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    pub labels: Vec<u8>,
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
        return None; // perception not fully brought up yet, so retry next frame
    }
    Some((lib_base, np))
}

/// Enable or disable meshing through `NativePerception::SetMeshingEnabled`. It returns whether the
/// call was made, which needs perception up. It is idempotent on the cached flag, so calling it
/// each frame during bring-up is safe.
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

/// Poll the current mesh blocks. It copies each block's vertices, normals and indices out of the
/// SDK's transient C++ vectors and frees them. The result is empty while meshing is off,
/// unsupported, or not yet producing.
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
        // Iterate over EVERY block so each one's inner libc++ vector storages get freed, even the
        // blocks past MAX_BLOCKS that we do not copy out, or their storage leaks before the outer
        // array free below.
        for i in 0..total {
            let block = vec.begin.add(i * MESH_BLOCK_STRIDE);
            if i < count {
                let (vertices, v_begin) = read_vec3(block, MB_VERTICES);
                let (normals, n_begin) = read_vec3(block, MB_NORMALS);
                let (indices, i_begin) = read_u32(block, MB_INDICES);
                // The label storage is freed by the shared MB_LABELS free below, which every block goes
                // through, so this reads the bytes without taking the pointer back.
                let labels = read_u8(block, MB_LABELS);
                out.push(MeshBlock {
                    id: (block.add(MB_ID) as *const u64).read_unaligned(),
                    state: (block.add(MB_STATE) as *const i32).read_unaligned(),
                    vertices,
                    normals,
                    indices,
                    labels,
                });
                // Free the block's three geometry vector storages.
                free_op(v_begin);
                free_op(n_begin);
                free_op(i_begin);
            } else {
                // Beyond MAX_BLOCKS: still free the three geometry vector storages, and only skip copying the
                // geometry out.
                free_op((block.add(MB_VERTICES) as *const *mut u8).read_unaligned());
                free_op((block.add(MB_NORMALS) as *const *mut u8).read_unaligned());
                free_op((block.add(MB_INDICES) as *const *mut u8).read_unaligned());
            }
            free_op((block.add(MB_LABELS) as *const *mut u8).read_unaligned());
        }
        // Free the block array itself.
        free_op(vec.begin);
        out
    }
}

/// Read a `std::vector<Vector3>` at `block + off`, returning the copied points and the storage
/// `begin` pointer so it can be freed. `Vector3` here is 12 bytes, three f32, read verbatim; the
/// Godot side flips the signs.
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

/// Read the `std::vector<u8>` of [`semantic_label`]s at `block + off`. Unlike the two above it does
/// not hand back the storage pointer, because the caller frees that vector on every block, copied
/// out or not. An empty result means the backend produced no classification for this block.
unsafe fn read_u8(block: *const u8, off: usize) -> Vec<u8> {
    let begin = (block.add(off) as *const *mut u8).read_unaligned();
    let end = (block.add(off + 8) as *const *mut u8).read_unaligned();
    if begin.is_null() || end < begin {
        return Vec::new();
    }
    let count = (end as usize - begin as usize).min(MAX_VERTS);
    std::slice::from_raw_parts(begin as *const u8, count).to_vec()
}

/// libc++ `operator delete(void*)` on a non-null pointer, freeing SDK vector storage. It resolves
/// `_ZdlPv` from libc++_shared.so once and does nothing when that cannot be found, because a leak
/// beats a wrong-allocator crash.
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

/// dlsym libc++'s `operator delete(void*)`, `_ZdlPv`. The handle is leaked on purpose, since
/// libc++_shared.so is a process-global dependency, and this returns 0 when it is unavailable.
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
