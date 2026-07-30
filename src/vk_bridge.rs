//! The Vulkan glasses bridge (vulkan-path-plan.md stage 2).
//!
//! Under the Vulkan renderer the SDK compositor still consumes client GL texture names, and this
//! module is the span between the two APIs. Each `IUnityXRDisplay::CreateTexture` slot becomes an
//! [`EyeBundle`]: one exportable RGBA8 `VkImage` + `VkDeviceMemory` on **Godot's own** Vulkan
//! device, whose memory is exported as an **OPAQUE_FD** (`vkGetMemoryFdKHR`) and imported into
//! the private EGL context (`egl_context.rs`) through `GL_EXT_memory_object_fd`
//! (`glImportMemoryFdEXT` + `glTexStorageMem2DEXT`); the resulting GL texture name is what the
//! SDK receives, exactly as under GL. Per frame, a bridge-owned command buffer copies each eye
//! SubViewport's `VkImage` into the acquired slot's `VkImage` (`vkCmdCopyImage`: raw texels, so
//! Godot's display-ready sRGB-encoded bytes arrive unaltered), bracketed by
//! `VK_QUEUE_FAMILY_EXTERNAL` acquire/release barriers, and the v1 sync is `vkQueueWaitIdle`
//! before `SubmitCurrentFrame`. The design and its alternatives (a fullscreen sampled pass as
//! fill v2, a SYNC_FD fence as sync v2) are recorded in `docs/plans/vulkan-path-plan.md` and
//! `docs/archive/codex-vulkan-stage2-design.md`.
//!
//! Why OPAQUE_FD and not the plan's original AHardwareBuffer: the AHB import needs
//! `VK_ANDROID_external_memory_android_hardware_buffer`, which Godot 4.7 never enables on its
//! device, and the device-verified failure modes were exactly the two spec outcomes -
//! `vkGetDeviceProcAddr` returns null, and the `vkGetInstanceProcAddr`-resolved stub "succeeds"
//! with `memoryTypeBits = 0` (2026-07-30, Beam Pro). `VK_KHR_external_memory_fd` IS enabled by
//! Godot (against validation noise, but enabled is enabled), `VK_KHR_external_memory` is core
//! 1.1, and the Adreno 710 GLES driver advertises `GL_EXT_memory_object_fd`, so the opaque-fd
//! route needs nothing Godot withholds. Same memory, same architecture; only the import
//! mechanics changed.
//!
//! Submission ordering is the load-bearing subtlety: the bridge submits on Godot's graphics
//! queue from the **frame-drawn callback**, after Godot submitted the frame's rendering, so
//! same-queue submission order puts the eye SubViewport writes before the bridge's copy with no
//! semaphore from Godot. (The main `RenderingDevice`'s own `texture_copy` cannot be used here:
//! it defers into Godot's next frame command buffer, which makes it unorderable against
//! `SubmitCurrentFrame` and unbracketable by the foreign-ownership barriers.)
//!
//! Kill switch: `adb shell setprop debug.xreal.vulkan_glasses 1` enables the bridge (default
//! OFF for the first landing). `debug.xreal.vk_solid 1` clears the eyes to solid red/blue and
//! `2` animates, replacing the copy - the bring-up ladder's steps 4 and 5. Every Vulkan failure
//! latches [`BROKEN`] and the app degrades to the stage-1 phone-only behavior.

#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use libloading::Library;

use crate::session::android_prop_i32;

// ---------------------------------------------------------------------------------------------
// Vulkan ABI: the minimal handful of types and entry points the bridge needs. Dispatchable
// handles (VkDevice, VkQueue, VkCommandBuffer, ...) are pointers; non-dispatchable ones
// (VkImage, VkDeviceMemory, ...) are u64.
// ---------------------------------------------------------------------------------------------

type VkHandle = u64; // non-dispatchable
type VkPtr = *mut c_void; // dispatchable

const VK_SUCCESS: i32 = 0;
const VK_STRUCTURE_TYPE_SUBMIT_INFO: u32 = 4;
const VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO: u32 = 5;
const VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO: u32 = 14;
const VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO: u32 = 39;
const VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO: u32 = 40;
const VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO: u32 = 42;
const VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER: u32 = 45;
const VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO: u32 = 1000072001;
const VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO: u32 = 1000072002;
const VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR: u32 = 1000074002;
const VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO: u32 = 1000127001;

const VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT: u32 = 0x0000_0001;
/// Queue-family for the ownership handoff to the GL/compositor side. `VK_QUEUE_FAMILY_EXTERNAL`
/// deliberately, not `FOREIGN_EXT`: EXTERNAL is core Vulkan 1.1 (via the promoted
/// VK_KHR_external_memory), while FOREIGN_EXT needs VK_EXT_queue_family_foreign, which Godot's
/// device does not enable. The GL sibling qualifies as an external queue for this handoff.
const VK_QUEUE_FAMILY_EXTERNAL: u32 = 0xFFFF_FFFE;

const VK_FORMAT_R8G8B8A8_UNORM: u32 = 37;
const VK_FORMAT_R8G8B8A8_SRGB: u32 = 43;

const VK_IMAGE_TYPE_2D: u32 = 1;
const VK_SAMPLE_COUNT_1_BIT: u32 = 1;
const VK_IMAGE_TILING_OPTIMAL: u32 = 0;
const VK_SHARING_MODE_EXCLUSIVE: u32 = 0;
const VK_IMAGE_USAGE_TRANSFER_SRC_BIT: u32 = 0x1;
const VK_IMAGE_USAGE_TRANSFER_DST_BIT: u32 = 0x2;
const VK_IMAGE_USAGE_SAMPLED_BIT: u32 = 0x4;
const VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT: u32 = 0x10;

const VK_IMAGE_LAYOUT_UNDEFINED: u32 = 0;
const VK_IMAGE_LAYOUT_GENERAL: u32 = 1;
const VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL: u32 = 5;
const VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL: u32 = 6;
const VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL: u32 = 7;

const VK_ACCESS_TRANSFER_READ_BIT: u32 = 0x800;
const VK_ACCESS_TRANSFER_WRITE_BIT: u32 = 0x1000;
const VK_ACCESS_MEMORY_READ_BIT: u32 = 0x8000;
const VK_ACCESS_MEMORY_WRITE_BIT: u32 = 0x10000;

const VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT: u32 = 0x1;
const VK_PIPELINE_STAGE_TRANSFER_BIT: u32 = 0x1000;
const VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT: u32 = 0x2000;
const VK_PIPELINE_STAGE_ALL_COMMANDS_BIT: u32 = 0x10000;

const VK_IMAGE_ASPECT_COLOR_BIT: u32 = 0x1;
const VK_STRUCTURE_TYPE_FENCE_CREATE_INFO: u32 = 8;
const VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT: u32 = 0x2;
const VK_COMMAND_BUFFER_LEVEL_PRIMARY: u32 = 0;
const VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT: u32 = 0x1;

#[repr(C)]
struct VkExtent3D {
    width: u32,
    height: u32,
    depth: u32,
}

#[repr(C)]
struct VkExternalMemoryImageCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    handle_types: u32,
}

#[repr(C)]
struct VkImageCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    image_type: u32,
    format: u32,
    extent: VkExtent3D,
    mip_levels: u32,
    array_layers: u32,
    samples: u32,
    tiling: u32,
    usage: u32,
    sharing_mode: u32,
    queue_family_index_count: u32,
    p_queue_family_indices: *const u32,
    initial_layout: u32,
}

#[repr(C)]
struct VkMemoryRequirements {
    size: u64,
    alignment: u64,
    memory_type_bits: u32,
}

#[repr(C)]
struct VkExportMemoryAllocateInfo {
    s_type: u32,
    p_next: *const c_void,
    handle_types: u32,
}

#[repr(C)]
struct VkMemoryGetFdInfoKHR {
    s_type: u32,
    p_next: *const c_void,
    memory: VkHandle,
    handle_type: u32,
}

#[repr(C)]
struct VkMemoryDedicatedAllocateInfo {
    s_type: u32,
    p_next: *const c_void,
    image: VkHandle,
    buffer: VkHandle,
}

#[repr(C)]
struct VkMemoryAllocateInfo {
    s_type: u32,
    p_next: *const c_void,
    allocation_size: u64,
    memory_type_index: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkImageSubresourceRange {
    aspect_mask: u32,
    base_mip_level: u32,
    level_count: u32,
    base_array_layer: u32,
    layer_count: u32,
}

const COLOR_RANGE: VkImageSubresourceRange = VkImageSubresourceRange {
    aspect_mask: VK_IMAGE_ASPECT_COLOR_BIT,
    base_mip_level: 0,
    level_count: 1,
    base_array_layer: 0,
    layer_count: 1,
};

#[repr(C)]
struct VkImageMemoryBarrier {
    s_type: u32,
    p_next: *const c_void,
    src_access_mask: u32,
    dst_access_mask: u32,
    old_layout: u32,
    new_layout: u32,
    src_queue_family_index: u32,
    dst_queue_family_index: u32,
    image: VkHandle,
    subresource_range: VkImageSubresourceRange,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkImageSubresourceLayers {
    aspect_mask: u32,
    mip_level: u32,
    base_array_layer: u32,
    layer_count: u32,
}

const COLOR_LAYERS: VkImageSubresourceLayers = VkImageSubresourceLayers {
    aspect_mask: VK_IMAGE_ASPECT_COLOR_BIT,
    mip_level: 0,
    base_array_layer: 0,
    layer_count: 1,
};

#[repr(C)]
struct VkOffset3D {
    x: i32,
    y: i32,
    z: i32,
}

#[repr(C)]
struct VkImageCopy {
    src_subresource: VkImageSubresourceLayers,
    src_offset: VkOffset3D,
    dst_subresource: VkImageSubresourceLayers,
    dst_offset: VkOffset3D,
    extent: VkExtent3D,
}

#[repr(C)]
struct VkFenceCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
}

#[repr(C)]
struct VkCommandPoolCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    queue_family_index: u32,
}

#[repr(C)]
struct VkCommandBufferAllocateInfo {
    s_type: u32,
    p_next: *const c_void,
    command_pool: VkHandle,
    level: u32,
    command_buffer_count: u32,
}

#[repr(C)]
struct VkCommandBufferBeginInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    p_inheritance_info: *const c_void,
}

#[repr(C)]
struct VkSubmitInfo {
    s_type: u32,
    p_next: *const c_void,
    wait_semaphore_count: u32,
    p_wait_semaphores: *const VkHandle,
    p_wait_dst_stage_mask: *const u32,
    command_buffer_count: u32,
    p_command_buffers: *const VkPtr,
    signal_semaphore_count: u32,
    p_signal_semaphores: *const VkHandle,
}

type FnVkGetInstanceProcAddr = unsafe extern "C" fn(VkPtr, *const u8) -> *mut c_void;
type FnVkGetDeviceProcAddr = unsafe extern "C" fn(VkPtr, *const u8) -> *mut c_void;
type FnVkCreateImage =
    unsafe extern "C" fn(VkPtr, *const VkImageCreateInfo, *const c_void, *mut VkHandle) -> i32;
type FnVkDestroyImage = unsafe extern "C" fn(VkPtr, VkHandle, *const c_void);
type FnVkGetImageMemoryRequirements =
    unsafe extern "C" fn(VkPtr, VkHandle, *mut VkMemoryRequirements);
type FnVkAllocateMemory =
    unsafe extern "C" fn(VkPtr, *const VkMemoryAllocateInfo, *const c_void, *mut VkHandle) -> i32;
type FnVkFreeMemory = unsafe extern "C" fn(VkPtr, VkHandle, *const c_void);
type FnVkBindImageMemory = unsafe extern "C" fn(VkPtr, VkHandle, VkHandle, u64) -> i32;
type FnVkGetMemoryFdKHR = unsafe extern "C" fn(VkPtr, *const VkMemoryGetFdInfoKHR, *mut i32) -> i32;
type FnVkCreateCommandPool = unsafe extern "C" fn(
    VkPtr,
    *const VkCommandPoolCreateInfo,
    *const c_void,
    *mut VkHandle,
) -> i32;
type FnVkDestroyCommandPool = unsafe extern "C" fn(VkPtr, VkHandle, *const c_void);
type FnVkAllocateCommandBuffers =
    unsafe extern "C" fn(VkPtr, *const VkCommandBufferAllocateInfo, *mut VkPtr) -> i32;
type FnVkBeginCommandBuffer = unsafe extern "C" fn(VkPtr, *const VkCommandBufferBeginInfo) -> i32;
type FnVkEndCommandBuffer = unsafe extern "C" fn(VkPtr) -> i32;
type FnVkResetCommandBuffer = unsafe extern "C" fn(VkPtr, u32) -> i32;
type FnVkCmdPipelineBarrier = unsafe extern "C" fn(
    VkPtr,
    u32,
    u32,
    u32,
    u32,
    *const c_void,
    u32,
    *const c_void,
    u32,
    *const VkImageMemoryBarrier,
);
type FnVkCmdCopyImage =
    unsafe extern "C" fn(VkPtr, VkHandle, u32, VkHandle, u32, u32, *const VkImageCopy);
type FnVkCmdClearColorImage = unsafe extern "C" fn(
    VkPtr,
    VkHandle,
    u32,
    *const [f32; 4],
    u32,
    *const VkImageSubresourceRange,
);
type FnVkQueueSubmit = unsafe extern "C" fn(VkPtr, u32, *const VkSubmitInfo, VkHandle) -> i32;
type FnVkQueueWaitIdle = unsafe extern "C" fn(VkPtr) -> i32;
type FnVkCreateFence =
    unsafe extern "C" fn(VkPtr, *const VkFenceCreateInfo, *const c_void, *mut VkHandle) -> i32;
type FnVkDestroyFence = unsafe extern "C" fn(VkPtr, VkHandle, *const c_void);
type FnVkWaitForFences = unsafe extern "C" fn(VkPtr, u32, *const VkHandle, u32, u64) -> i32;
type FnVkResetFences = unsafe extern "C" fn(VkPtr, u32, *const VkHandle) -> i32;

/// Godot's Vulkan handles plus the resolved device-level entry points and the bridge's command
/// pool/buffer. Everything is used only from the frame-drawn callback thread (asserted by the
/// tick); the raw pointers are process-global driver handles.
struct VkApi {
    device: VkPtr,
    queue: VkPtr,
    queue_family: u32,
    create_image: FnVkCreateImage,
    destroy_image: FnVkDestroyImage,
    get_image_memory_requirements: FnVkGetImageMemoryRequirements,
    allocate_memory: FnVkAllocateMemory,
    free_memory: FnVkFreeMemory,
    bind_image_memory: FnVkBindImageMemory,
    get_memory_fd: FnVkGetMemoryFdKHR,
    destroy_command_pool: FnVkDestroyCommandPool,
    begin_command_buffer: FnVkBeginCommandBuffer,
    end_command_buffer: FnVkEndCommandBuffer,
    reset_command_buffer: FnVkResetCommandBuffer,
    cmd_pipeline_barrier: FnVkCmdPipelineBarrier,
    cmd_copy_image: FnVkCmdCopyImage,
    cmd_clear_color_image: FnVkCmdClearColorImage,
    queue_submit: FnVkQueueSubmit,
    queue_wait_idle: FnVkQueueWaitIdle,
    create_fence: FnVkCreateFence,
    destroy_fence: FnVkDestroyFence,
    wait_for_fences: FnVkWaitForFences,
    reset_fences: FnVkResetFences,
    command_pool: VkHandle,
    command_buffer: VkPtr,
    /// Fence signaled by the fill submission; used by the pipelined sync mode (vk_sync=1).
    fence: VkHandle,
    _lib: Library,
}

unsafe impl Send for VkApi {}
unsafe impl Sync for VkApi {}

// ---------------------------------------------------------------------------------------------
// GL_EXT_memory_object_fd import (the GL half of a bundle): the exported OPAQUE_FD becomes a GL
// memory object backing an immutable-storage GL_TEXTURE_2D on the private EGL context.
// ---------------------------------------------------------------------------------------------

const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE_BINDING_2D: u32 = 0x8069;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_LINEAR: i32 = 0x2601;
const GL_CLAMP_TO_EDGE: i32 = 0x812F;
const GL_RGBA8: u32 = 0x8058;
// GL_EXT_memory_object / _fd tokens.
const GL_TEXTURE_TILING_EXT: u32 = 0x9580;
const GL_DEDICATED_MEMORY_OBJECT_EXT: u32 = 0x9581;
const GL_OPTIMAL_TILING_EXT: i32 = 0x9584;
const GL_HANDLE_TYPE_OPAQUE_FD_EXT: u32 = 0x9586;

type FnEglGetProcAddress = unsafe extern "C" fn(*const u8) -> *mut c_void;
type FnGenTextures = unsafe extern "C" fn(i32, *mut u32);
type FnDeleteTextures = unsafe extern "C" fn(i32, *const u32);
type FnBindTexture = unsafe extern "C" fn(u32, u32);
type FnTexParameteri = unsafe extern "C" fn(u32, u32, i32);
type FnGetIntegerv = unsafe extern "C" fn(u32, *mut i32);
type FnGetError = unsafe extern "C" fn() -> u32;
type FnCreateMemoryObjects = unsafe extern "C" fn(i32, *mut u32);
type FnDeleteMemoryObjects = unsafe extern "C" fn(i32, *const u32);
type FnMemoryObjectParameteriv = unsafe extern "C" fn(u32, u32, *const i32);
type FnImportMemoryFd = unsafe extern "C" fn(u32, u64, u32, i32);
type FnTexStorageMem2D = unsafe extern "C" fn(u32, i32, u32, i32, i32, u32, u64);

struct GlMem {
    gen_textures: FnGenTextures,
    delete_textures: FnDeleteTextures,
    bind_texture: FnBindTexture,
    tex_parameteri: FnTexParameteri,
    get_integerv: FnGetIntegerv,
    get_error: FnGetError,
    create_memory_objects: FnCreateMemoryObjects,
    delete_memory_objects: FnDeleteMemoryObjects,
    memory_object_parameteriv: FnMemoryObjectParameteriv,
    import_memory_fd: FnImportMemoryFd,
    tex_storage_mem_2d: FnTexStorageMem2D,
    _libs: Vec<Library>,
}

unsafe impl Send for GlMem {}
unsafe impl Sync for GlMem {}

impl GlMem {
    fn load() -> Result<Self, String> {
        unsafe {
            let gles = Library::new("libGLESv3.so").map_err(|e| format!("libGLESv3: {e}"))?;
            let egl = Library::new("libEGL.so").map_err(|e| format!("libEGL: {e}"))?;
            macro_rules! sym {
                ($lib:expr, $name:literal, $ty:ty) => {
                    *$lib
                        .get::<$ty>(concat!($name, "\0").as_bytes())
                        .map_err(|e| format!("dlsym {}: {e}", $name))?
                };
            }
            let egl_get_proc_address: FnEglGetProcAddress =
                sym!(egl, "eglGetProcAddress", FnEglGetProcAddress);
            // GL_EXT_memory_object entry points are extension-only: eglGetProcAddress is the
            // spec route, with a direct dlsym as the cheap first try.
            macro_rules! ext_sym {
                ($lib:expr, $name:literal, $ty:ty) => {{
                    match $lib.get::<$ty>(concat!($name, "\0").as_bytes()).map(|s| *s) {
                        Ok(f) => f,
                        Err(_) => {
                            let p = egl_get_proc_address(concat!($name, "\0").as_bytes().as_ptr());
                            if p.is_null() {
                                return Err(format!("{} unavailable", $name));
                            }
                            std::mem::transmute::<*mut c_void, $ty>(p)
                        }
                    }
                }};
            }
            Ok(GlMem {
                gen_textures: sym!(gles, "glGenTextures", FnGenTextures),
                delete_textures: sym!(gles, "glDeleteTextures", FnDeleteTextures),
                bind_texture: sym!(gles, "glBindTexture", FnBindTexture),
                tex_parameteri: sym!(gles, "glTexParameteri", FnTexParameteri),
                get_integerv: sym!(gles, "glGetIntegerv", FnGetIntegerv),
                get_error: sym!(gles, "glGetError", FnGetError),
                create_memory_objects: ext_sym!(
                    gles,
                    "glCreateMemoryObjectsEXT",
                    FnCreateMemoryObjects
                ),
                delete_memory_objects: ext_sym!(
                    gles,
                    "glDeleteMemoryObjectsEXT",
                    FnDeleteMemoryObjects
                ),
                memory_object_parameteriv: ext_sym!(
                    gles,
                    "glMemoryObjectParameterivEXT",
                    FnMemoryObjectParameteriv
                ),
                import_memory_fd: ext_sym!(gles, "glImportMemoryFdEXT", FnImportMemoryFd),
                tex_storage_mem_2d: ext_sym!(gles, "glTexStorageMem2DEXT", FnTexStorageMem2D),
                _libs: vec![gles, egl],
            })
        }
    }
}

// ---------------------------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------------------------

/// One `CreateTexture` slot: the exportable Vulkan image/memory and its GL import.
struct EyeBundle {
    gl_name: u32,
    /// The GL memory object wrapping the imported fd.
    gl_memobj: u32,
    vk_image: VkHandle,
    vk_memory: VkHandle,
    width: i32,
    height: i32,
    /// First bridge use acquires with `UNDEFINED` (contents discardable); later frames preserve.
    first_use: bool,
}

/// One eye SubViewport's Vulkan-side identity, published from the main thread each frame.
#[derive(Clone, Copy, Default)]
pub struct EyeSource {
    pub vk_image: u64,
    pub width: i32,
    pub height: i32,
    /// The RD data format is the sRGB-typed RGBA8 twin. Copy-compatible either way; logged only.
    pub srgb: bool,
    pub valid: bool,
}

static VK: OnceLock<Option<VkApi>> = OnceLock::new();
static GL_MEM: OnceLock<Option<GlMem>> = OnceLock::new();
static BUNDLES: Mutex<Vec<EyeBundle>> = Mutex::new(Vec::new());
/// GL names whose bundles were dropped by `xr_destroy_texture`; torn down on the next tick,
/// which runs with the private EGL context current and after a queue-wait.
static PENDING_DESTROY: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static EYE_SOURCES: Mutex<[EyeSource; 2]> = Mutex::new([EyeSource::ZERO; 2]);
/// Latched on the first unrecoverable Vulkan/EGL failure: the bridge stops driving frames and
/// the app degrades to the stage-1 phone-only behavior.
static BROKEN: AtomicBool = AtomicBool::new(false);
static FILL_LOG: AtomicU32 = AtomicU32::new(0);
/// Set while a fill submission's fence has not been waited on yet (pipelined sync, vk_sync=1).
static FENCE_PENDING: AtomicBool = AtomicBool::new(false);
/// One-shot eye-source format log gate.
static SRC_LOGGED: AtomicBool = AtomicBool::new(false);

impl EyeSource {
    const ZERO: EyeSource = EyeSource {
        vk_image: 0,
        width: 0,
        height: 0,
        srgb: false,
        valid: false,
    };
}

fn broken(reason: &str) {
    if !BROKEN.swap(true, Ordering::Relaxed) {
        godot::global::godot_warn!(
            "[xreal] vk_bridge BROKEN: {reason}; degrading to phone-only (stage-1 behavior)"
        );
    }
}

/// Is the bridge switched on for this process? Renderer must be Vulkan and the kill switch
/// (`debug.xreal.vulkan_glasses`, default OFF while stage 2 is in bring-up) must be set.
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        let on = !crate::gl::renderer_is_gl()
            && android_prop_i32(b"debug.xreal.vulkan_glasses\0") == Some(1);
        if on {
            godot::global::godot_print!(
                "[xreal] vk_bridge: enabled (debug.xreal.vulkan_glasses=1)"
            );
        }
        on
    })
}

/// [`enabled`] and initialized and not [`BROKEN`]: the per-frame gate.
pub fn active() -> bool {
    enabled() && !BROKEN.load(Ordering::Relaxed) && VK.get().is_some_and(|v| v.is_some())
}

/// Resolve Godot's Vulkan handles and the bridge's own command pool. Call once from the main
/// thread (node.rs) while `enabled()`; failures latch [`BROKEN`] and log the reason.
pub fn init_from_main_thread() {
    if VK.get().is_some() {
        return;
    }
    let api = VK.get_or_init(|| match load_vk() {
        Ok(api) => Some(api),
        Err(e) => {
            broken(&format!("init: {e}"));
            None
        }
    });
    if api.is_some() {
        godot::global::godot_print!("[xreal] vk_bridge: Vulkan side initialized");
    }
}

fn load_vk() -> Result<VkApi, String> {
    use godot::classes::rendering_device::DriverResource;
    use godot::classes::RenderingServer;
    use godot::obj::Singleton;

    let rs = RenderingServer::singleton();
    let Some(rd) = rs.get_rendering_device() else {
        return Err("no RenderingDevice (not a Vulkan renderer?)".into());
    };
    let instance = rd.get_driver_resource(
        DriverResource::TOPMOST_OBJECT,
        godot::builtin::Rid::Invalid,
        0,
    );
    let device = rd.get_driver_resource(
        DriverResource::LOGICAL_DEVICE,
        godot::builtin::Rid::Invalid,
        0,
    );
    let queue = rd.get_driver_resource(
        DriverResource::COMMAND_QUEUE,
        godot::builtin::Rid::Invalid,
        0,
    );
    let queue_family = rd.get_driver_resource(
        DriverResource::QUEUE_FAMILY,
        godot::builtin::Rid::Invalid,
        0,
    ) as u32;
    if instance == 0 || device == 0 || queue == 0 {
        return Err(format!(
            "driver resources missing (instance={instance:#x} device={device:#x} queue={queue:#x})"
        ));
    }

    unsafe {
        let lib = Library::new("libvulkan.so").map_err(|e| format!("dlopen libvulkan.so: {e}"))?;
        let gipa: FnVkGetInstanceProcAddr = *lib
            .get::<FnVkGetInstanceProcAddr>(b"vkGetInstanceProcAddr\0")
            .map_err(|e| format!("dlsym vkGetInstanceProcAddr: {e}"))?;
        let gdpa: FnVkGetDeviceProcAddr = {
            // .cast(): c_char is u8 on Android but i8 on the desktop stub build.
            let p = gipa(instance as VkPtr, c"vkGetDeviceProcAddr".as_ptr().cast());
            if p.is_null() {
                return Err("vkGetDeviceProcAddr unresolved".into());
            }
            std::mem::transmute::<*mut c_void, FnVkGetDeviceProcAddr>(p)
        };
        macro_rules! dev_fn {
            ($name:literal, $ty:ty) => {{
                let p = gdpa(device as VkPtr, concat!($name, "\0").as_bytes().as_ptr());
                if p.is_null() {
                    return Err(format!(
                        "{} unresolved (extension not enabled on Godot's device?)",
                        $name
                    ));
                }
                std::mem::transmute::<*mut c_void, $ty>(p)
            }};
        }
        let create_image: FnVkCreateImage = dev_fn!("vkCreateImage", FnVkCreateImage);
        let destroy_image: FnVkDestroyImage = dev_fn!("vkDestroyImage", FnVkDestroyImage);
        let get_image_memory_requirements: FnVkGetImageMemoryRequirements = dev_fn!(
            "vkGetImageMemoryRequirements",
            FnVkGetImageMemoryRequirements
        );
        let allocate_memory: FnVkAllocateMemory = dev_fn!("vkAllocateMemory", FnVkAllocateMemory);
        let free_memory: FnVkFreeMemory = dev_fn!("vkFreeMemory", FnVkFreeMemory);
        let bind_image_memory: FnVkBindImageMemory =
            dev_fn!("vkBindImageMemory", FnVkBindImageMemory);
        // The one extension entry point the export hinges on. Spec-clean: Godot 4.7 enables
        // VK_KHR_external_memory_fd on its device (rendering_device_driver_vulkan.cpp registers
        // it "against validation noise"), so vkGetDeviceProcAddr must resolve this.
        let get_memory_fd: FnVkGetMemoryFdKHR = dev_fn!("vkGetMemoryFdKHR", FnVkGetMemoryFdKHR);
        let create_command_pool: FnVkCreateCommandPool =
            dev_fn!("vkCreateCommandPool", FnVkCreateCommandPool);
        let destroy_command_pool: FnVkDestroyCommandPool =
            dev_fn!("vkDestroyCommandPool", FnVkDestroyCommandPool);
        let allocate_command_buffers: FnVkAllocateCommandBuffers =
            dev_fn!("vkAllocateCommandBuffers", FnVkAllocateCommandBuffers);
        let begin_command_buffer: FnVkBeginCommandBuffer =
            dev_fn!("vkBeginCommandBuffer", FnVkBeginCommandBuffer);
        let end_command_buffer: FnVkEndCommandBuffer =
            dev_fn!("vkEndCommandBuffer", FnVkEndCommandBuffer);
        let reset_command_buffer: FnVkResetCommandBuffer =
            dev_fn!("vkResetCommandBuffer", FnVkResetCommandBuffer);
        let cmd_pipeline_barrier: FnVkCmdPipelineBarrier =
            dev_fn!("vkCmdPipelineBarrier", FnVkCmdPipelineBarrier);
        let cmd_copy_image: FnVkCmdCopyImage = dev_fn!("vkCmdCopyImage", FnVkCmdCopyImage);
        let cmd_clear_color_image: FnVkCmdClearColorImage =
            dev_fn!("vkCmdClearColorImage", FnVkCmdClearColorImage);
        let queue_submit: FnVkQueueSubmit = dev_fn!("vkQueueSubmit", FnVkQueueSubmit);
        let queue_wait_idle: FnVkQueueWaitIdle = dev_fn!("vkQueueWaitIdle", FnVkQueueWaitIdle);
        let create_fence: FnVkCreateFence = dev_fn!("vkCreateFence", FnVkCreateFence);
        let destroy_fence: FnVkDestroyFence = dev_fn!("vkDestroyFence", FnVkDestroyFence);
        let wait_for_fences: FnVkWaitForFences = dev_fn!("vkWaitForFences", FnVkWaitForFences);
        let reset_fences: FnVkResetFences = dev_fn!("vkResetFences", FnVkResetFences);

        let pool_info = VkCommandPoolCreateInfo {
            s_type: VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,
            queue_family_index: queue_family,
        };
        let mut command_pool: VkHandle = 0;
        let r = create_command_pool(
            device as VkPtr,
            &pool_info,
            std::ptr::null(),
            &mut command_pool,
        );
        if r != VK_SUCCESS {
            return Err(format!("vkCreateCommandPool -> {r}"));
        }
        let alloc_info = VkCommandBufferAllocateInfo {
            s_type: VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
            p_next: std::ptr::null(),
            command_pool,
            level: VK_COMMAND_BUFFER_LEVEL_PRIMARY,
            command_buffer_count: 1,
        };
        let mut command_buffer: VkPtr = std::ptr::null_mut();
        let r = allocate_command_buffers(device as VkPtr, &alloc_info, &mut command_buffer);
        if r != VK_SUCCESS {
            destroy_command_pool(device as VkPtr, command_pool, std::ptr::null());
            return Err(format!("vkAllocateCommandBuffers -> {r}"));
        }
        let fence_info = VkFenceCreateInfo {
            s_type: VK_STRUCTURE_TYPE_FENCE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0, // unsignaled; FENCE_PENDING tracks whether a wait is owed
        };
        let mut fence: VkHandle = 0;
        let r = create_fence(device as VkPtr, &fence_info, std::ptr::null(), &mut fence);
        if r != VK_SUCCESS {
            destroy_command_pool(device as VkPtr, command_pool, std::ptr::null());
            return Err(format!("vkCreateFence -> {r}"));
        }

        godot::global::godot_print!(
            "[xreal] vk_bridge: device={device:#x} queue={queue:#x} family={queue_family}"
        );
        Ok(VkApi {
            device: device as VkPtr,
            queue: queue as VkPtr,
            queue_family,
            create_image,
            destroy_image,
            get_image_memory_requirements,
            allocate_memory,
            free_memory,
            bind_image_memory,
            get_memory_fd,
            destroy_command_pool,
            begin_command_buffer,
            end_command_buffer,
            reset_command_buffer,
            cmd_pipeline_barrier,
            cmd_copy_image,
            cmd_clear_color_image,
            queue_submit,
            queue_wait_idle,
            create_fence,
            destroy_fence,
            wait_for_fences,
            reset_fences,
            command_pool,
            command_buffer,
            fence,
            _lib: lib,
        })
    }
}

fn vk() -> Option<&'static VkApi> {
    VK.get()?.as_ref()
}

fn gl_mem() -> Option<&'static GlMem> {
    GL_MEM
        .get_or_init(|| match GlMem::load() {
            Ok(a) => Some(a),
            Err(e) => {
                broken(&format!("GL memory-object loader: {e}"));
                None
            }
        })
        .as_ref()
}

// ---------------------------------------------------------------------------------------------
// Bundle lifecycle
// ---------------------------------------------------------------------------------------------

/// Allocate one eye slot: an exportable RGBA8 `VkImage` + dedicated `VkDeviceMemory` on Godot's
/// device, its memory exported as an OPAQUE_FD (`vkGetMemoryFdKHR`) and imported into GL
/// (`glImportMemoryFdEXT` + `glTexStorageMem2DEXT`). The private EGL context MUST be current:
/// the caller is `xr_create_texture` inside the Vulkan tick. Returns the GL texture name the SDK
/// gets, or `None` (latching [`BROKEN`]) on failure.
pub fn create_eye_texture(width: i32, height: i32) -> Option<u32> {
    let api = vk()?;
    let gm = gl_mem()?;
    unsafe {
        // 1) Vulkan half: an exportable external-memory image with a dedicated allocation.
        let ext_info = VkExternalMemoryImageCreateInfo {
            s_type: VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
            p_next: std::ptr::null(),
            handle_types: VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT,
        };
        let image_info = VkImageCreateInfo {
            s_type: VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
            p_next: &ext_info as *const _ as *const c_void,
            flags: 0,
            image_type: VK_IMAGE_TYPE_2D,
            format: VK_FORMAT_R8G8B8A8_UNORM,
            extent: VkExtent3D {
                width: width as u32,
                height: height as u32,
                depth: 1,
            },
            mip_levels: 1,
            array_layers: 1,
            samples: VK_SAMPLE_COUNT_1_BIT,
            tiling: VK_IMAGE_TILING_OPTIMAL,
            usage: VK_IMAGE_USAGE_TRANSFER_DST_BIT
                | VK_IMAGE_USAGE_TRANSFER_SRC_BIT
                | VK_IMAGE_USAGE_SAMPLED_BIT
                | VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT,
            sharing_mode: VK_SHARING_MODE_EXCLUSIVE,
            queue_family_index_count: 0,
            p_queue_family_indices: std::ptr::null(),
            initial_layout: VK_IMAGE_LAYOUT_UNDEFINED,
        };
        let mut vk_image: VkHandle = 0;
        let r = (api.create_image)(api.device, &image_info, std::ptr::null(), &mut vk_image);
        if r != VK_SUCCESS {
            broken(&format!("vkCreateImage(exportable) -> {r}"));
            return None;
        }
        let mut reqs = VkMemoryRequirements {
            size: 0,
            alignment: 0,
            memory_type_bits: 0,
        };
        (api.get_image_memory_requirements)(api.device, vk_image, &mut reqs);
        if reqs.memory_type_bits == 0 || reqs.size == 0 {
            (api.destroy_image)(api.device, vk_image, std::ptr::null());
            broken(&format!(
                "image memory requirements empty (size={} bits={:#x})",
                reqs.size, reqs.memory_type_bits
            ));
            return None;
        }
        let dedicated = VkMemoryDedicatedAllocateInfo {
            s_type: VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO,
            p_next: std::ptr::null(),
            image: vk_image,
            buffer: 0,
        };
        let export_info = VkExportMemoryAllocateInfo {
            s_type: VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO,
            p_next: &dedicated as *const _ as *const c_void,
            handle_types: VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT,
        };
        let alloc = VkMemoryAllocateInfo {
            s_type: VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
            p_next: &export_info as *const _ as *const c_void,
            allocation_size: reqs.size,
            memory_type_index: reqs.memory_type_bits.trailing_zeros(),
        };
        let mut vk_memory: VkHandle = 0;
        let r = (api.allocate_memory)(api.device, &alloc, std::ptr::null(), &mut vk_memory);
        if r != VK_SUCCESS {
            (api.destroy_image)(api.device, vk_image, std::ptr::null());
            broken(&format!("vkAllocateMemory(exportable) -> {r}"));
            return None;
        }
        let r = (api.bind_image_memory)(api.device, vk_image, vk_memory, 0);
        if r != VK_SUCCESS {
            (api.free_memory)(api.device, vk_memory, std::ptr::null());
            (api.destroy_image)(api.device, vk_image, std::ptr::null());
            broken(&format!("vkBindImageMemory -> {r}"));
            return None;
        }

        // 2) Export the memory as an fd. Ownership passes to us, then to GL on import.
        let get_fd_info = VkMemoryGetFdInfoKHR {
            s_type: VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR,
            p_next: std::ptr::null(),
            memory: vk_memory,
            handle_type: VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT,
        };
        let mut fd: i32 = -1;
        let r = (api.get_memory_fd)(api.device, &get_fd_info, &mut fd);
        if r != VK_SUCCESS || fd < 0 {
            (api.free_memory)(api.device, vk_memory, std::ptr::null());
            (api.destroy_image)(api.device, vk_image, std::ptr::null());
            broken(&format!("vkGetMemoryFdKHR -> {r} (fd={fd})"));
            return None;
        }

        // 3) GL half: memory object + immutable texture on the private context. The import
        //    consumes the fd (success or failure, per the extension spec). Tiling must be set to
        //    OPTIMAL, matching the VkImage, BEFORE the storage call, and the memory object must
        //    be flagged dedicated because the Vulkan allocation is.
        while (gm.get_error)() != 0 {}
        let mut memobj: u32 = 0;
        (gm.create_memory_objects)(1, &mut memobj);
        let dedicated_flag: i32 = 1; // GL_TRUE
        (gm.memory_object_parameteriv)(memobj, GL_DEDICATED_MEMORY_OBJECT_EXT, &dedicated_flag);
        (gm.import_memory_fd)(memobj, reqs.size, GL_HANDLE_TYPE_OPAQUE_FD_EXT, fd);
        let mut prev_tex: i32 = 0;
        (gm.get_integerv)(GL_TEXTURE_BINDING_2D, &mut prev_tex);
        let mut gl_name: u32 = 0;
        (gm.gen_textures)(1, &mut gl_name);
        (gm.bind_texture)(GL_TEXTURE_2D, gl_name);
        (gm.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_TILING_EXT, GL_OPTIMAL_TILING_EXT);
        (gm.tex_storage_mem_2d)(GL_TEXTURE_2D, 1, GL_RGBA8, width, height, memobj, 0);
        for (pname, value) in [
            (GL_TEXTURE_MIN_FILTER, GL_LINEAR),
            (GL_TEXTURE_MAG_FILTER, GL_LINEAR),
            (GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE),
            (GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE),
        ] {
            (gm.tex_parameteri)(GL_TEXTURE_2D, pname, value);
        }
        let gl_err = (gm.get_error)();
        (gm.bind_texture)(GL_TEXTURE_2D, prev_tex as u32);
        if gl_err != 0 || gl_name == 0 || memobj == 0 {
            if gl_name != 0 {
                (gm.delete_textures)(1, &gl_name);
            }
            if memobj != 0 {
                (gm.delete_memory_objects)(1, &memobj);
            }
            (api.free_memory)(api.device, vk_memory, std::ptr::null());
            (api.destroy_image)(api.device, vk_image, std::ptr::null());
            broken(&format!(
                "GL memory-object import gl_err={gl_err:#x} (gl_name={gl_name} memobj={memobj})"
            ));
            return None;
        }

        godot::global::godot_print!(
            "[xreal] vk_bridge: eye slot {width}x{height} gl={gl_name} memobj={memobj} \
             vk_image={vk_image:#x} (alloc {} KiB, opaque-fd share)",
            reqs.size / 1024
        );
        BUNDLES.lock().expect("bundles mutex").push(EyeBundle {
            gl_name,
            gl_memobj: memobj,
            vk_image,
            vk_memory,
            width,
            height,
            first_use: true,
        });
        Some(gl_name)
    }
}

/// Whether `gl_name` is a bridge-owned eye slot (used by `xr_destroy_texture` to route
/// destruction here instead of the GL-path delete queue).
pub fn owns(gl_name: u32) -> bool {
    BUNDLES
        .lock()
        .expect("bundles mutex")
        .iter()
        .any(|b| b.gl_name == gl_name)
}

/// Queue a bundle for destruction on the next tick (any thread).
pub fn queue_destroy(gl_name: u32) {
    PENDING_DESTROY
        .lock()
        .expect("pending destroy mutex")
        .push(gl_name);
}

/// Tear down queued bundles. Tick thread only, with the private EGL context current, after the
/// frame's `vkQueueWaitIdle` so no submitted work still references the images.
fn drain_destroyed() {
    let pending: Vec<u32> =
        std::mem::take(&mut *PENDING_DESTROY.lock().expect("pending destroy mutex"));
    if pending.is_empty() {
        return;
    }
    let (Some(api), Some(gm)) = (vk(), gl_mem()) else {
        return;
    };
    let mut bundles = BUNDLES.lock().expect("bundles mutex");
    for gl_name in pending {
        let Some(pos) = bundles.iter().position(|b| b.gl_name == gl_name) else {
            continue;
        };
        let b = bundles.remove(pos);
        unsafe {
            (gm.delete_textures)(1, &b.gl_name);
            (gm.delete_memory_objects)(1, &b.gl_memobj);
            (api.destroy_image)(api.device, b.vk_image, std::ptr::null());
            (api.free_memory)(api.device, b.vk_memory, std::ptr::null());
        }
        godot::global::godot_print!("[xreal] vk_bridge: eye slot gl={gl_name} destroyed");
    }
}

// ---------------------------------------------------------------------------------------------
// Per-frame fill
// ---------------------------------------------------------------------------------------------

/// Publish the two eye SubViewports' Vulkan images (main thread, each frame; the handles can
/// change when Godot reallocates render targets).
pub fn set_eye_sources(left: EyeSource, right: EyeSource) {
    if !SRC_LOGGED.swap(true, Ordering::Relaxed) && left.valid {
        godot::global::godot_print!(
            "[xreal] vk_bridge eye-src probe: {}x{} srgb={} vk_image={:#x} (copy path {})",
            left.width,
            left.height,
            left.srgb,
            left.vk_image,
            "enabled: RGBA8 class"
        );
    }
    *EYE_SOURCES.lock().expect("eye sources mutex") = [left, right];
}

/// Copy the published eye sources into the acquired slots, then wait the queue idle (sync v1).
/// `targets` pairs each acquired slot's GL name with its eye index. Tick thread only, after
/// Godot's frame submission. Returns the number of eyes filled.
pub fn fill_eyes(targets: &[(u32, usize)]) -> u32 {
    let Some(api) = vk() else { return 0 };
    if BROKEN.load(Ordering::Relaxed) {
        return 0;
    }
    let solid = android_prop_i32(b"debug.xreal.vk_solid\0").unwrap_or(0);
    // Sync mode: 0 (default) waits the queue idle after this frame's submit, the correctness-first
    // v1. 1 pipelines by one frame: submit with a fence, and wait for the PREVIOUS frame's fence
    // here at entry, which in steady state has long signaled, so the CPU never stalls on the GPU.
    // The pipelined window is benign by construction: the compositor starts sampling a slot only
    // after SubmitCurrentFrame, our copy into it was enqueued before that, and the compositor's
    // next composite is a vsync away, while the not-yet-reusable command buffer is protected by
    // exactly this entry wait.
    let sync_mode = android_prop_i32(b"debug.xreal.vk_sync\0").unwrap_or(0);
    let n = FILL_LOG.fetch_add(1, Ordering::Relaxed);

    // Whatever the current mode, a pending fence from a pipelined frame must be waited out
    // before the command buffer is reset and before queued bundle teardown.
    unsafe {
        if FENCE_PENDING.swap(false, Ordering::Relaxed) {
            let r = (api.wait_for_fences)(api.device, 1, &api.fence, 1, 1_000_000_000);
            (api.reset_fences)(api.device, 1, &api.fence);
            if r != VK_SUCCESS {
                broken(&format!("vkWaitForFences -> {r}"));
                return 0;
            }
        }
    }
    // Queued bundle teardown: everything the GPU could still touch has completed here, whether
    // through the entry wait above (pipelined) or last frame's queue-wait-idle (v1). Before the
    // BUNDLES lock below, because it takes that lock itself.
    drain_destroyed();

    let sources = *EYE_SOURCES.lock().expect("eye sources mutex");
    let mut bundles = BUNDLES.lock().expect("bundles mutex");

    unsafe {
        let r = (api.reset_command_buffer)(api.command_buffer, 0);
        if r != VK_SUCCESS {
            broken(&format!("vkResetCommandBuffer -> {r}"));
            return 0;
        }
        let begin = VkCommandBufferBeginInfo {
            s_type: VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
            p_next: std::ptr::null(),
            flags: VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
            p_inheritance_info: std::ptr::null(),
        };
        let r = (api.begin_command_buffer)(api.command_buffer, &begin);
        if r != VK_SUCCESS {
            broken(&format!("vkBeginCommandBuffer -> {r}"));
            return 0;
        }

        let mut filled = 0u32;
        for &(gl_name, eye) in targets {
            let Some(bundle) = bundles.iter_mut().find(|b| b.gl_name == gl_name) else {
                continue;
            };
            let src = sources[eye.min(1)];
            let copy_ok = src.valid
                && src.vk_image != 0
                && src.width == bundle.width
                && src.height == bundle.height;

            // Acquire the slot from the foreign (GL/compositor) side. First use discards.
            let old_layout = if bundle.first_use {
                VK_IMAGE_LAYOUT_UNDEFINED
            } else {
                VK_IMAGE_LAYOUT_GENERAL
            };
            bundle.first_use = false;
            let acquire = VkImageMemoryBarrier {
                s_type: VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
                p_next: std::ptr::null(),
                src_access_mask: 0,
                dst_access_mask: VK_ACCESS_TRANSFER_WRITE_BIT,
                old_layout,
                new_layout: VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                src_queue_family_index: VK_QUEUE_FAMILY_EXTERNAL,
                dst_queue_family_index: api.queue_family,
                image: bundle.vk_image,
                subresource_range: COLOR_RANGE,
            };
            (api.cmd_pipeline_barrier)(
                api.command_buffer,
                VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,
                VK_PIPELINE_STAGE_TRANSFER_BIT,
                0,
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                1,
                &acquire,
            );

            if solid > 0 || !copy_ok {
                // Bring-up ladder steps 4/5 (and the no-source fallback): clear the slot to a
                // per-eye color, animated when vk_solid=2, black when the source is just absent.
                let t = if solid == 2 {
                    (n % 120) as f32 / 120.0
                } else {
                    1.0
                };
                let color: [f32; 4] = if solid == 0 {
                    [0.0, 0.0, 0.0, 1.0]
                } else if eye == 0 {
                    [t, 0.0, 0.0, 1.0]
                } else {
                    [0.0, 0.0, t, 1.0]
                };
                (api.cmd_clear_color_image)(
                    api.command_buffer,
                    bundle.vk_image,
                    VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                    &color,
                    1,
                    &COLOR_RANGE,
                );
            } else {
                // Fill v1: transition Godot's source to TRANSFER_SRC, raw-copy, and restore the
                // layout EXACTLY so Godot's internal tracker never notices. The source ends its
                // Godot frame sampled (SHADER_READ_ONLY_OPTIMAL); if validation or the ladder
                // proves otherwise on device, this pair is the first place to fix.
                let src_in = VkImageMemoryBarrier {
                    s_type: VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
                    p_next: std::ptr::null(),
                    src_access_mask: VK_ACCESS_MEMORY_WRITE_BIT | VK_ACCESS_MEMORY_READ_BIT,
                    dst_access_mask: VK_ACCESS_TRANSFER_READ_BIT,
                    old_layout: VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                    new_layout: VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                    src_queue_family_index: u32::MAX, // VK_QUEUE_FAMILY_IGNORED
                    dst_queue_family_index: u32::MAX,
                    image: src.vk_image,
                    subresource_range: COLOR_RANGE,
                };
                (api.cmd_pipeline_barrier)(
                    api.command_buffer,
                    VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
                    VK_PIPELINE_STAGE_TRANSFER_BIT,
                    0,
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    1,
                    &src_in,
                );
                let region = VkImageCopy {
                    src_subresource: COLOR_LAYERS,
                    src_offset: VkOffset3D { x: 0, y: 0, z: 0 },
                    dst_subresource: COLOR_LAYERS,
                    dst_offset: VkOffset3D { x: 0, y: 0, z: 0 },
                    extent: VkExtent3D {
                        width: bundle.width as u32,
                        height: bundle.height as u32,
                        depth: 1,
                    },
                };
                (api.cmd_copy_image)(
                    api.command_buffer,
                    src.vk_image,
                    VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                    bundle.vk_image,
                    VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                    1,
                    &region,
                );
                let src_out = VkImageMemoryBarrier {
                    s_type: VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
                    p_next: std::ptr::null(),
                    src_access_mask: VK_ACCESS_TRANSFER_READ_BIT,
                    dst_access_mask: VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT,
                    old_layout: VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                    new_layout: VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                    src_queue_family_index: u32::MAX,
                    dst_queue_family_index: u32::MAX,
                    image: src.vk_image,
                    subresource_range: COLOR_RANGE,
                };
                (api.cmd_pipeline_barrier)(
                    api.command_buffer,
                    VK_PIPELINE_STAGE_TRANSFER_BIT,
                    VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
                    0,
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    1,
                    &src_out,
                );
            }

            // Release the slot back to the foreign side for the compositor's GL sampling.
            let release = VkImageMemoryBarrier {
                s_type: VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
                p_next: std::ptr::null(),
                src_access_mask: VK_ACCESS_TRANSFER_WRITE_BIT,
                dst_access_mask: 0,
                old_layout: VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                new_layout: VK_IMAGE_LAYOUT_GENERAL,
                src_queue_family_index: api.queue_family,
                dst_queue_family_index: VK_QUEUE_FAMILY_EXTERNAL,
                image: bundle.vk_image,
                subresource_range: COLOR_RANGE,
            };
            (api.cmd_pipeline_barrier)(
                api.command_buffer,
                VK_PIPELINE_STAGE_TRANSFER_BIT,
                VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT,
                0,
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                1,
                &release,
            );
            filled += 1;
        }

        let r = (api.end_command_buffer)(api.command_buffer);
        if r != VK_SUCCESS {
            broken(&format!("vkEndCommandBuffer -> {r}"));
            return 0;
        }
        let submit = VkSubmitInfo {
            s_type: VK_STRUCTURE_TYPE_SUBMIT_INFO,
            p_next: std::ptr::null(),
            wait_semaphore_count: 0,
            p_wait_semaphores: std::ptr::null(),
            p_wait_dst_stage_mask: std::ptr::null(),
            command_buffer_count: 1,
            p_command_buffers: &api.command_buffer,
            signal_semaphore_count: 0,
            p_signal_semaphores: std::ptr::null(),
        };
        let with_fence = sync_mode == 1;
        let r = (api.queue_submit)(
            api.queue,
            1,
            &submit,
            if with_fence { api.fence } else { 0 },
        );
        if r != VK_SUCCESS {
            broken(&format!("vkQueueSubmit -> {r}"));
            return 0;
        }
        if with_fence {
            // Pipelined sync: the wait happens at the next tick's entry (see above). Note the
            // SYNC_FD->EGL fence design (sync v2) is blocked on VK_KHR_external_semaphore_fd,
            // which Godot's device does not enable; this pipelining is the stock-template escape.
            FENCE_PENDING.store(true, Ordering::Relaxed);
        } else {
            // Sync v1: correctness first. The SDK samples on its own GLThread after
            // SubmitCurrentFrame, so everything submitted above must have completed by then.
            let r = (api.queue_wait_idle)(api.queue);
            if r != VK_SUCCESS {
                broken(&format!("vkQueueWaitIdle -> {r}"));
                return 0;
            }
        }
        drop(bundles);
        if n < 8 || n.is_multiple_of(300) {
            godot::global::godot_print!(
                "[xreal] vk_bridge fill #{n}: targets={} filled={filled} solid={solid}",
                targets.len()
            );
        }
        filled
    }
}
