//! Minimal emulation of Unity's native-plugin interface, enough to drive
//! `libXREALXRPlugin.so`.
//!
//! `libXREALXRPlugin.so` is a **Unity native plugin**: Unity's engine calls
//! `UnityPluginLoad(IUnityInterfaces*)` at startup, and `InitUserDefinedSettings` wires Unity
//! XR display/input providers through that registry. Godot never does this, so the stored
//! `IUnityInterfaces*` is null and `LoadDisplay` segfaults (device-confirmed).
//!
//! We provide a tiny `IUnityInterfaces` whose `GetInterface` hands back:
//! - `IUnityGraphics` reporting **OpenGL ES 3**.
//! - Minimal Unity XR display/input/meshing registries. XREAL registers provider callback
//!   structs into those registries; Godot later invokes only initialize/start to mirror the
//!   Unity lifecycle enough for `NativeHMD` / `NativePerception` creation.
//!
//! Struct layouts and the graphics GUID/enum constants come from Unity's PUBLIC PluginAPI
//! headers (`IUnityInterface.h`, `IUnityGraphics.h`). The XR GUIDs/provider layouts are
//! **RE / unverified**, recovered from `libXREALXRPlugin.so` AArch64 disassembly and
//! relocation tables; see `docs/reference/reverse-engineering.md`.

use std::ffi::{c_char, c_void, CStr};
use std::ptr;
use std::sync::{
    atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering},
    Mutex,
};

/// `UnityInterfaceGUID` (IUnityInterface.h): a 128-bit interface id `{high, low}`.
#[repr(C)]
struct UnityInterfaceGuid {
    high: u64,
    low: u64,
}

/// `IUnityGraphics` GUID (IUnityGraphics.h:
/// `UNITY_REGISTER_INTERFACE_GUID(0x7CBA0A9CA4DDB544, 0x8C5AD4926EB17B11, IUnityGraphics)`).
const IUNITY_GRAPHICS_GUID: UnityInterfaceGuid = UnityInterfaceGuid {
    high: 0x7CBA_0A9C_A4DD_B544,
    low: 0x8C5A_D492_6EB1_7B11,
};

/// `kUnityGfxRendererOpenGLES30` (IUnityGraphics.h).
const K_UNITY_GFX_RENDERER_OPENGLES30: i32 = 11;

// RE / unverified Unity XR interface GUIDs requested by libXREALXRPlugin.so.
const IUNITY_XR_DISPLAY_GUID: UnityInterfaceGuid = UnityInterfaceGuid {
    high: 0x940E_64D2_E522_43EC,
    low: 0xA348_F302_6B1B_1193,
};
const IUNITY_XR_DISPLAY_HELPER_GUID: UnityInterfaceGuid = UnityInterfaceGuid {
    high: 0xAB69_5A1C_9411_4266,
    low: 0x0BDB_5A1B_3F7A_54B8,
};
const IUNITY_XR_MESHING_GUID: UnityInterfaceGuid = UnityInterfaceGuid {
    high: 0x3007_FD58_85A3_46EF,
    low: 0x9EEB_2C84_AA0A_9DD9,
};
const IUNITY_XR_INPUT_GUID: UnityInterfaceGuid = UnityInterfaceGuid {
    high: 0x2B53_FA87_1CDA_6802,
    low: 0x942B_CA0C_8EF1_3193,
};

/// `IUnityGraphics` (IUnityGraphics.h) — a struct of function pointers. The plugin calls
/// the first member (`GetRenderer`) via `ldr x8,[iface]; blr x8`.
#[repr(C)]
struct IUnityGraphics {
    get_renderer: extern "C" fn() -> i32,
    register_device_event_callback: extern "C" fn(*mut c_void),
}

type LifecycleCallback = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;
type GfxThreadStartCallback =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut UnityXrRenderingCapabilities) -> i32;
type GfxThreadSimpleCallback = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;
type PopulateNextFrameDescCallback =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_void, *mut c_void) -> i32;

/// RE / unverified Unity XR lifecycle-provider layout copied by XREAL before registration.
#[repr(C)]
#[derive(Clone, Copy)]
struct UnityXrLifecycleProvider {
    user_data: *mut c_void,
    initialize: Option<LifecycleCallback>,
    start: Option<LifecycleCallback>,
    stop: Option<LifecycleCallback>,
    shutdown: Option<LifecycleCallback>,
}

#[derive(Clone, Copy)]
struct RegisteredLifecycle {
    label: &'static str,
    context: usize,
    user_data: usize,
    initialize: Option<LifecycleCallback>,
    start: Option<LifecycleCallback>,
}

/// RE / unverified: Unity XR rendering capabilities. XREAL currently writes only bytes
/// at offsets 0 and 2 in `DisplayManager::GfxThreadStart`; keep spare space for SDK drift.
#[repr(C)]
#[derive(Clone, Copy)]
struct UnityXrRenderingCapabilities {
    bytes: [u8; 64],
}

/// RE / unverified Unity XR display graphics-thread provider layout.
#[repr(C)]
#[derive(Clone, Copy)]
struct UnityXrGfxThreadProvider {
    user_data: *mut c_void,
    start: Option<GfxThreadStartCallback>,
    submit_current_frame: Option<GfxThreadSimpleCallback>,
    populate_next_frame_desc: Option<PopulateNextFrameDescCallback>,
    stop: Option<GfxThreadSimpleCallback>,
}

#[derive(Clone, Copy)]
struct RegisteredGfxThreadProvider {
    context: usize,
    user_data: usize,
    start: Option<GfxThreadStartCallback>,
    submit_current_frame: Option<GfxThreadSimpleCallback>,
    populate_next_frame_desc: Option<PopulateNextFrameDescCallback>,
}

/// `IUnityXRDisplayInterface` (Unity XR SDK `IUnityXRDisplay.h`). Slot order is confirmed by
/// disassembly of the `DisplayManager` wrappers that call each slot (see
/// `docs/plans/frame-submission-plan.md`). The SDK reaches through +0x18/+0x20/+0x28 to make the engine
/// allocate/query/free its render textures; the earlier 3-member struct was truncated there.
#[repr(C)]
struct IUnityXrDisplay {
    register_lifecycle_provider:
        extern "C" fn(*const c_char, *const c_char, *const UnityXrLifecycleProvider) -> i32, // +0x00
    register_display_provider: extern "C" fn(*mut c_void, *const c_void) -> i32, // +0x08
    register_provider_for_graphics_thread: extern "C" fn(*mut c_void, *const c_void) -> i32, // +0x10
    create_texture: extern "C" fn(*mut c_void, *const UnityXrRenderTextureDesc, *mut u32) -> i32, // +0x18
    query_texture_desc: extern "C" fn(*mut c_void, u32, *mut UnityXrRenderTextureDesc) -> i32, // +0x20
    destroy_texture: extern "C" fn(*mut c_void, u32) -> i32, // +0x28
    get_platform_data: extern "C" fn(*mut c_void, *mut *mut c_void) -> i32, // +0x30
    create_occlusion_mesh: extern "C" fn(*mut c_void, u32, u32, *mut u32) -> i32, // +0x38
    destroy_occlusion_mesh: extern "C" fn(*mut c_void, u32) -> i32, // +0x40
    set_occlusion_mesh: extern "C" fn(*mut c_void, u32, *mut c_void, u32, *mut u32, u32) -> i32, // +0x48
}

/// `UnityXRRenderTextureDesc` (0x30 bytes) as the **vendored `libXREALXRPlugin.so` 3.1.0** builds it.
/// Re-confirmed by disassembly of `DisplayManager::CreateTexture @0x69530`, which fills:
/// colorFormat(+0x00)=0, color(+0x08)=native tex, depthFormat(+0x10)=0, depth(+0x18)=0,
/// width(+0x20), height(+0x24), textureArrayLength(+0x28), flags(+0x2c). `color`/`depth` are Unity's
/// `UnityXRTextureData` union (8 bytes = `nativePtr` | `referenceTextureId`; verified via the reference
/// app's IL2CPP managed mirror).
///
/// Version caveat: Unity's newer XR SDK (Unity 6) inserts `shadingRateFormat` (int32) + `shadingRate`
/// (`UnityXRTextureData`) between `depth` and `width`, making the struct 0x40 bytes with width→+0x30.
/// The 3.1.0 plugin we vendor predates that, so this 0x30 layout is correct here — but if the vendored
/// plugin is ever rebuilt against a newer SDK, add those two fields (else CreateTexture reads garbage
/// width/height).
#[repr(C)]
#[derive(Clone, Copy)]
struct UnityXrRenderTextureDesc {
    color_format: u32,         // +0x00
    _pad0: u32,                // +0x04
    color: u64,                // +0x08  UnityXRTextureData (native GL texture name)
    depth_format: u32,         // +0x10
    _pad1: u32,                // +0x14
    depth: u64,                // +0x18
    width: u32,                // +0x20
    height: u32,               // +0x24
    texture_array_length: u32, // +0x28
    flags: u32,                // +0x2c
}

/// One engine-owned render texture handed to the SDK. `id` is the `UnityXRRenderTextureId` we
/// return from `CreateTexture`; `gl_id` is the GL texture name the compositor samples.
#[derive(Clone, Copy)]
struct XrTexture {
    id: u32,
    gl_id: u32,
    width: i32,
    height: i32,
    /// Number of array layers: 1 for a plain `GL_TEXTURE_2D` (multipass), 2 for a
    /// `GL_TEXTURE_2D_ARRAY` (Multiview / Single-Pass-Instanced — both eyes in one texture).
    layers: i32,
    /// The `color_format` and `flags` the SDK passed to `CreateTexture`. `QueryTextureDesc` must echo
    /// them back (esp. for the Multiview 2-layer array — reporting a 1-layer/flags-0 descriptor for a
    /// 2-layer object mis-registers the NR swapchain so layer 1 never gets our content; the right eye
    /// then shows a fixed cleared gray. See docs/archive/codex-righteye-analysis.md.
    color_format: u32,
    flags: u32,
}

static XR_TEXTURES: Mutex<Vec<XrTexture>> = Mutex::new(Vec::new());
static XR_TEXTURE_NEXT_ID: AtomicU32 = AtomicU32::new(1);
static XR_QUERY_LOG: AtomicU32 = AtomicU32::new(0);

/// Per-eye projection + eye pose, read from `UnityXRNextFrameDesc.renderPasses[k].renderParams[0]`
/// by the frame tick and consumed by `XrealHeadTracker` to set the eye cameras. `l/r/t/b` are the
/// XREAL half-angle tangents; `px/py/pz` is `deviceAnchorToEyePose.position`.
#[derive(Clone, Copy)]
pub struct EyeProj {
    pub valid: bool,
    pub l: f32,
    pub r: f32,
    pub t: f32,
    pub b: f32,
    pub px: f32,
    pub py: f32,
    pub pz: f32,
}
impl EyeProj {
    const ZERO: EyeProj = EyeProj {
        valid: false,
        l: 0.0,
        r: 0.0,
        t: 0.0,
        b: 0.0,
        px: 0.0,
        py: 0.0,
        pz: 0.0,
    };
}
static STEREO_PROJ: Mutex<[EyeProj; 2]> = Mutex::new([EyeProj::ZERO; 2]);

/// The latest per-eye projection/pose the SDK wrote into the frame descriptor (or `valid=false`).
pub fn stereo_projection() -> [EyeProj; 2] {
    *STEREO_PROJ.lock().expect("stereo proj mutex")
}

/// Per-eye source GL textures + size, published each frame from `XrealHeadTracker::process`.
/// When both are non-zero these are offscreen SubViewport textures (real stereo); when both are
/// zero but the size is set, the frame tick blits Godot's default framebuffer into both eyes
/// (mono fallback); when nothing is set it draws the test pattern.
static GODOT_EYE_TEX_L: AtomicU32 = AtomicU32::new(0);
static GODOT_EYE_TEX_R: AtomicU32 = AtomicU32::new(0);
static GODOT_SRC_W: AtomicI32 = AtomicI32::new(0);
static GODOT_SRC_H: AtomicI32 = AtomicI32::new(0);

/// Publish per-eye offscreen SubViewport textures for stereo blit.
pub fn set_godot_eye_sources(left: u32, right: u32, width: i32, height: i32) {
    GODOT_EYE_TEX_L.store(left, Ordering::Relaxed);
    GODOT_EYE_TEX_R.store(right, Ordering::Relaxed);
    GODOT_SRC_W.store(width, Ordering::Relaxed);
    GODOT_SRC_H.store(height, Ordering::Relaxed);
}

/// Mono fallback: publish just the window size (no offscreen textures) so the frame tick blits
/// the default framebuffer into both eyes.
pub fn set_godot_source_size(width: i32, height: i32) {
    GODOT_EYE_TEX_L.store(0, Ordering::Relaxed);
    GODOT_EYE_TEX_R.store(0, Ordering::Relaxed);
    GODOT_SRC_W.store(width, Ordering::Relaxed);
    GODOT_SRC_H.store(height, Ordering::Relaxed);
}

/// Look up an engine texture by its `UnityXRRenderTextureId`, returning `(gl_id, width, height)`.
fn xr_texture_for(id: u32) -> Option<(u32, i32, i32, i32)> {
    XR_TEXTURES
        .lock()
        .expect("xr textures mutex")
        .iter()
        .find(|t| t.id == id)
        .map(|t| (t.gl_id, t.width, t.height, t.layers))
}

#[repr(C)]
struct IUnityXrDisplayHelper {
    register_texture_provider: extern "C" fn(*mut c_void) -> i32,
    property_to_id: extern "C" fn(*mut c_void, *const c_char, i32) -> i32,
}

#[repr(C)]
struct IUnityXrInput {
    register_lifecycle_provider:
        extern "C" fn(*const c_char, *const c_char, *const UnityXrLifecycleProvider) -> i32,
    register_input_provider: extern "C" fn(*mut c_void, *const c_void) -> i32,
    set_device_connected: extern "C" fn(*mut c_void, i32) -> i32,
}

#[repr(C)]
struct IUnityXrMeshing {
    unused_0: extern "C" fn() -> i32,
    unused_1: extern "C" fn() -> i32,
    unused_2: extern "C" fn() -> i32,
    unused_3: extern "C" fn() -> i32,
    unused_4: extern "C" fn() -> i32,
    register_lifecycle_provider:
        extern "C" fn(*const c_char, *const c_char, *const UnityXrLifecycleProvider) -> i32,
}

/// `IUnityInterfaces` (IUnityInterface.h) — the registry of interface getters.
#[repr(C)]
struct IUnityInterfaces {
    get_interface: extern "C" fn(*const UnityInterfaceGuid) -> *mut c_void,
    register_interface: extern "C" fn(*const UnityInterfaceGuid, *mut c_void),
    get_interface_split: extern "C" fn(u64, u64) -> *mut c_void,
    register_interface_split: extern "C" fn(u64, u64, *mut c_void),
}

// `IUnityInterfaces`/`IUnityGraphics` hold only function pointers, which are `Sync`, so the
// statics below are sound to share across threads (the plugin only reads them).
unsafe impl Sync for IUnityGraphics {}
unsafe impl Sync for IUnityInterfaces {}
unsafe impl Sync for IUnityXrDisplay {}
unsafe impl Sync for IUnityXrDisplayHelper {}
unsafe impl Sync for IUnityXrInput {}
unsafe impl Sync for IUnityXrMeshing {}

extern "C" fn gfx_get_renderer() -> i32 {
    K_UNITY_GFX_RENDERER_OPENGLES30
}
extern "C" fn gfx_register_device_event_callback(_callback: *mut c_void) {}

static UNITY_GRAPHICS: IUnityGraphics = IUnityGraphics {
    get_renderer: gfx_get_renderer,
    register_device_event_callback: gfx_register_device_event_callback,
};

/// Texture provider function registered by XREAL via `RegisterTextureProvider`.
/// Signature: fn(context: *mut c_void, texture_id: u32, out_desc: *mut c_void) -> i32
/// Called by `DisplayManager::QueryTextureDesc` to map a swapchain buffer index to a
/// GL texture handle. If null, QueryTextureDesc will branch through a null pointer → crash.
type TextureProviderFn = unsafe extern "C" fn(*mut c_void, u32, *mut c_void) -> i32;

#[derive(Clone, Copy)]
struct RegisteredTextureProvider {
    context: usize,
    provider_fn: TextureProviderFn,
}

static DISPLAY_CONTEXT: u8 = 0;
static INPUT_CONTEXT: u8 = 0;
static MESH_CONTEXT: u8 = 0;
static DISPLAY_LIFECYCLE: Mutex<Option<RegisteredLifecycle>> = Mutex::new(None);
static INPUT_LIFECYCLE: Mutex<Option<RegisteredLifecycle>> = Mutex::new(None);
static MESH_LIFECYCLE: Mutex<Option<RegisteredLifecycle>> = Mutex::new(None);
static GFX_THREAD_PROVIDER: Mutex<Option<RegisteredGfxThreadProvider>> = Mutex::new(None);
static TEXTURE_PROVIDER: Mutex<Option<RegisteredTextureProvider>> = Mutex::new(None);
/// The XREAL input provider callbacks (COPIED at registration — the struct the SDK hands
/// `RegisterInputProvider` is transient and its memory is reused after the call, so we must copy the
/// function addresses, which point into stable `libXREALXRPlugin.so` code). RE (SDK 3.1.0, device
/// struct dump): `+0x20` = `$_9` → `InputManager::UpdateDeviceState` (the per-frame HMD update Unity
/// normally drives), `+0x30` = `$_11` → `NativePerception::Recenter` (the real recenter that resets
/// the perception origin the compositor reprojects against).
#[derive(Clone, Copy)]
struct RegisteredInputProvider {
    recenter: usize,            // callback at struct +0x30
    update_device_state: usize, // callback at struct +0x20
}
static INPUT_PROVIDER: Mutex<Option<RegisteredInputProvider>> = Mutex::new(None);
static LIFECYCLE_STARTED: AtomicBool = AtomicBool::new(false);
static GFX_THREAD_STARTED: AtomicBool = AtomicBool::new(false);
static FRAME_TICK_COUNT: AtomicU64 = AtomicU64::new(0);

/// Current OS thread id — used to tell whether the SDK drives any callback from its own
/// rendering thread vs. Godot's render thread. Returns 0 off-Android.
fn current_tid() -> i64 {
    #[cfg(target_os = "android")]
    {
        unsafe { libc::gettid() as i64 }
    }
    #[cfg(not(target_os = "android"))]
    {
        0
    }
}

fn cstr_lossy(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return "<null>".to_string();
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

fn copy_lifecycle(
    label: &'static str,
    context: *const u8,
    id: *const c_char,
    name: *const c_char,
    provider: *const UnityXrLifecycleProvider,
    previous: Option<RegisteredLifecycle>,
) -> Option<RegisteredLifecycle> {
    if provider.is_null() {
        godot::global::godot_warn!("[xreal] Unity XR {label} provider registration got null");
        return None;
    }

    let provider = unsafe { *provider };
    if previous.map(|registered| {
        registered.user_data == provider.user_data as usize
            && registered.initialize.is_some() == provider.initialize.is_some()
            && registered.start.is_some() == provider.start.is_some()
    }) != Some(true)
    {
        godot::global::godot_print!(
            "[xreal] Unity XR {label} provider registered: id={}, name={}, init={}, start={}",
            cstr_lossy(id),
            cstr_lossy(name),
            provider.initialize.is_some(),
            provider.start.is_some()
        );
    }
    Some(RegisteredLifecycle {
        label,
        context: context as usize,
        user_data: provider.user_data as usize,
        initialize: provider.initialize,
        start: provider.start,
    })
}

fn run_callback(provider: RegisteredLifecycle, phase: &'static str, callback: LifecycleCallback) {
    let status = unsafe {
        callback(
            provider.context as *mut c_void,
            provider.user_data as *mut c_void,
        )
    };
    godot::global::godot_print!(
        "[xreal] Unity XR {} {} callback -> {}",
        provider.label,
        phase,
        status
    );
}

extern "C" fn xr_register_display_lifecycle_provider(
    id: *const c_char,
    name: *const c_char,
    provider: *const UnityXrLifecycleProvider,
) -> i32 {
    let mut lifecycle = DISPLAY_LIFECYCLE.lock().expect("display lifecycle mutex");
    if let Some(provider) = copy_lifecycle(
        "display",
        ptr::addr_of!(DISPLAY_CONTEXT),
        id,
        name,
        provider,
        *lifecycle,
    ) {
        *lifecycle = Some(provider);
    }
    0
}

extern "C" fn xr_register_input_lifecycle_provider(
    id: *const c_char,
    name: *const c_char,
    provider: *const UnityXrLifecycleProvider,
) -> i32 {
    let mut lifecycle = INPUT_LIFECYCLE.lock().expect("input lifecycle mutex");
    if let Some(provider) = copy_lifecycle(
        "input",
        ptr::addr_of!(INPUT_CONTEXT),
        id,
        name,
        provider,
        *lifecycle,
    ) {
        *lifecycle = Some(provider);
    }
    0
}

extern "C" fn xr_register_mesh_lifecycle_provider(
    id: *const c_char,
    name: *const c_char,
    provider: *const UnityXrLifecycleProvider,
) -> i32 {
    let mut lifecycle = MESH_LIFECYCLE.lock().expect("mesh lifecycle mutex");
    if let Some(provider) = copy_lifecycle(
        "mesh",
        ptr::addr_of!(MESH_CONTEXT),
        id,
        name,
        provider,
        *lifecycle,
    ) {
        *lifecycle = Some(provider);
    }
    0
}

extern "C" fn xr_register_display_provider(
    _context: *mut c_void,
    _callbacks: *const c_void,
) -> i32 {
    godot::global::godot_print!("[xreal] Unity XR display provider callbacks registered");
    0
}

extern "C" fn xr_register_gfx_thread_provider(
    context: *mut c_void,
    callbacks: *const c_void,
) -> i32 {
    if callbacks.is_null() {
        godot::global::godot_warn!(
            "[xreal] Unity XR display graphics-thread callbacks registered as null"
        );
        return 0;
    }

    let callbacks = unsafe { *(callbacks as *const UnityXrGfxThreadProvider) };
    let mut provider = GFX_THREAD_PROVIDER
        .lock()
        .expect("gfx-thread provider mutex");
    let previous = *provider;
    *provider = Some(RegisteredGfxThreadProvider {
        context: context as usize,
        user_data: callbacks.user_data as usize,
        start: callbacks.start,
        submit_current_frame: callbacks.submit_current_frame,
        populate_next_frame_desc: callbacks.populate_next_frame_desc,
    });
    if previous.map(|registered| {
        registered.context == context as usize
            && registered.user_data == callbacks.user_data as usize
            && registered.start.is_some() == callbacks.start.is_some()
            && registered.submit_current_frame.is_some() == callbacks.submit_current_frame.is_some()
            && registered.populate_next_frame_desc.is_some()
                == callbacks.populate_next_frame_desc.is_some()
    }) != Some(true)
    {
        godot::global::godot_print!(
            "[xreal] Unity XR display graphics-thread callbacks registered: start={}, submit={}, \
             populate={}, stop={}, populate_ptr={:?}",
            callbacks.start.is_some(),
            callbacks.submit_current_frame.is_some(),
            callbacks.populate_next_frame_desc.is_some(),
            callbacks.stop.is_some(),
            callbacks
                .populate_next_frame_desc
                .map(|callback| callback as *const c_void)
        );
    }
    0
}

extern "C" fn xr_register_input_provider(context: *mut c_void, callbacks: *const c_void) -> i32 {
    if !callbacks.is_null() {
        // Dump the provider struct's first 10 pointer words so we can find the per-frame tick
        // callback (a function inside libXREALXRPlugin.so that wraps InputManager::UpdateHMDState /
        // UpdateEyeData). We only READ + store here — no call — so this is crash-safe.
        let words: [usize; 10] = unsafe { *(callbacks as *const [usize; 10]) };
        let dump: Vec<String> = words
            .iter()
            .enumerate()
            .map(|(i, w)| format!("[+{:#04x}]={:#018x}", i * 8, w))
            .collect();
        godot::global::godot_print!(
            "[xreal] input provider @ {callbacks:?} ctx={context:?} struct: {}",
            dump.join(" ")
        );
        // Copy the callback CODE addresses now (the struct itself is transient — words[6] = +0x30
        // recenter, words[4] = +0x20 update-device-state).
        *INPUT_PROVIDER.lock().expect("input provider mutex") = Some(RegisteredInputProvider {
            recenter: words[6],
            update_device_state: words[4],
        });
        let _ = context;
    }
    godot::global::godot_print!("[xreal] Unity XR input provider callbacks registered (stored)");
    0
}

/// Invoke the XREAL input provider's **recenter** callback (`HandleRecenter`), which the SDK wires to
/// `NativePerception::Recenter()` — the real recenter that resets the *perception tracking origin*
/// the glasses compositor reprojects against (unlike `RecenterGlasses`, which is a no-op on our pose).
///
/// RE (SDK 3.1.0, device-dumped struct): the provider callbacks live at
/// `RegisterInputProvider(callbacks)`; the recenter lambda `$_11::__invoke(void*, void*)` sits at
/// struct offset **+0x30** and ignores both args. Returns `true` if the callback was invoked.
pub fn call_input_recenter() -> bool {
    let provider = *INPUT_PROVIDER.lock().expect("input provider mutex");
    let Some(provider) = provider else {
        godot::global::godot_print!("[xreal] call_input_recenter: no input provider stored");
        return false;
    };
    if provider.recenter == 0 {
        godot::global::godot_print!("[xreal] call_input_recenter: recenter callback is null");
        return false;
    }
    // `$_11::__invoke(void*, void*) -> i32` → NativePerception::Recenter (both args ignored).
    let recenter: extern "C" fn(*mut c_void, *mut c_void) -> i32 =
        unsafe { std::mem::transmute(provider.recenter) };
    let r = recenter(ptr::null_mut(), ptr::null_mut());
    godot::global::godot_print!("[xreal] input recenter (NativePerception::Recenter) -> {r}");
    true
}

/// Drive the per-frame HMD input update that Unity normally runs each frame. This calls
/// `UpdateDeviceState(deviceId=0)` → `InputManager::UpdateHMDState` → **`DisplayManager::OnBeforeRender()`**
/// (refreshes the render pose the glasses compositor reprojects against) + `NativePerception::GetHeadPose`.
/// Without it, the compositor's render pose stays frozen at session start and our content world-anchors
/// there instead of staying a head-locked peek window. Returns the callback's status (or -1 if absent).
///
/// The `UnityXRInputDeviceState*` out-arg is written via plain stores (no Unity allocator callbacks in
/// `UpdateHMDState`), so a large zeroed scratch buffer is safe.
pub fn call_input_update_hmd() -> i32 {
    let provider = *INPUT_PROVIDER.lock().expect("input provider mutex");
    let Some(provider) = provider else { return -1 };
    if provider.update_device_state == 0 {
        return -1;
    }
    // `$_9::__invoke(void*, void*, u32 deviceId, UnityXRInputUpdateType, UnityXRInputDeviceState*)`.
    let update: extern "C" fn(*mut c_void, *mut c_void, u32, u32, *mut c_void) -> i32 =
        unsafe { std::mem::transmute(provider.update_device_state) };
    let mut buf = [0u8; 1024];
    // deviceId 0 = HMD; updateType 1 = BeforeRender. RE (codex + our cross-check, see
    // docs/archive/codex-headlock-analysis.md): InputManager::UpdateHMDState @0x7aa3c calls
    // DisplayManager::OnBeforeRender @0x66fa8 ONLY when updateType == 1 (guard `cmp w1,#0x1; b.ne`
    // @0x7aa68). OnBeforeRender refreshes DM+0x100, which SubmitFrame passes to
    // NRFrameSetRenderingPose(frame, *(DM+0x100)) — the pose the compositor reprojects the layer
    // against. With updateType 0 (Dynamic) OnBeforeRender is skipped, DM+0x100 stays frozen at the
    // session-start pose, and our render world-anchors there instead of head-locking. So we must use
    // updateType 1 here (this is why the earlier updateType-0 attempt had no visual effect).
    update(
        ptr::null_mut(),
        ptr::null_mut(),
        0,
        1,
        buf.as_mut_ptr() as *mut c_void,
    )
}

extern "C" fn xr_set_device_connected(_context: *mut c_void, device_id: i32) -> i32 {
    godot::global::godot_print!("[xreal] Unity XR input device state set: {device_id}");
    0
}

extern "C" fn xr_register_texture_provider(context: *mut c_void) -> i32 {
    // XREAL calls this to pass its internal texture-provider object (context).
    // `DisplayManager::QueryTextureDesc` references `DisplayManager+0x8` and `+0x38` which
    // the SDK populates internally during NativeRendering::Start — we just log here.
    godot::global::godot_print!("[xreal] RegisterTextureProvider: context={context:?}");
    if !context.is_null() {
        // Try to read the first pointer from context to diagnose the vtable layout.
        let vtable_ptr = unsafe { *(context as *const usize) };
        godot::global::godot_print!(
            "[xreal] RegisterTextureProvider: context[0]={vtable_ptr:#018x}"
        );
    }
    0
}

extern "C" fn xr_property_to_id(_context: *mut c_void, name: *const c_char, _flags: i32) -> i32 {
    // Stable non-zero ids are sufficient for XREAL's cached shader-property bookkeeping.
    let mut hash: i32 = 17;
    for byte in cstr_lossy(name).bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(byte as i32);
    }
    hash & 0x7fff_ffff
}

extern "C" fn xr_unused() -> i32 {
    0
}

/// `IUnityXRDisplay::CreateTexture` (+0x18). The SDK's `OverlayBase::CreateBuffer` calls this
/// 7× (color=NULL → engine allocates). We allocate a GL texture and return a fresh id; the
/// compositor later samples this texture once `QueryTextureDesc` hands its GL name to
/// `SetSwapChainBuffers`. Runs on Godot's render thread (invoked under `GfxThreadStart`).
extern "C" fn xr_create_texture(
    _handle: *mut c_void,
    desc: *const UnityXrRenderTextureDesc,
    out_id: *mut u32,
) -> i32 {
    if desc.is_null() || out_id.is_null() {
        return 1;
    }
    let desc = unsafe { *desc };
    let width = desc.width as i32;
    let height = desc.height as i32;
    let srgb = (desc.flags & 0x10) != 0;
    // `textureArrayLength >= 2` = the SDK's Multiview / Single-Pass-Instanced path: it wants ONE
    // 2-layer array texture (both eyes), which it binds as a layered multiview framebuffer. A plain
    // 2D texture there causes `GL_INVALID_FRAMEBUFFER_OPERATION` → black. `layers == 1` is the
    // multipass path (a normal 2D texture per eye).
    let layers = if desc.texture_array_length >= 2 {
        desc.texture_array_length as i32
    } else {
        1
    };
    // If the SDK passed an existing native texture (color != 0), CreateBuffer took the
    // `[DM+0x10]==0x15` path (GetSwapChainBuffers → CreateTexture(color=that buffer)); the SDK
    // owns/registers that swapchain texture and expects the engine to render INTO it. In that
    // case SetSwapChainBuffers early-returns without QueryTextureDesc — so we must adopt the
    // provided texture rather than allocate our own. color == 0 → the SDK expects the engine to
    // allocate (the GLES path).
    let gl_id = if desc.color != 0 {
        desc.color as u32
    } else if layers >= 2 {
        match crate::gl::alloc_texture_array(width, height, layers, srgb) {
            Some(t) => t,
            None => {
                godot::global::godot_warn!(
                    "[xreal] CreateTexture array {width}x{height}x{layers} failed (GL alloc)"
                );
                return 1;
            }
        }
    } else {
        match crate::gl::alloc_texture(width, height, srgb) {
            Some(t) => t,
            None => {
                godot::global::godot_warn!(
                    "[xreal] CreateTexture {width}x{height} failed (GL alloc)"
                );
                return 1;
            }
        }
    };
    let id = XR_TEXTURE_NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let count = {
        let mut textures = XR_TEXTURES.lock().expect("xr textures mutex");
        textures.push(XrTexture {
            id,
            gl_id,
            width,
            height,
            layers,
            color_format: desc.color_format,
            flags: desc.flags,
        });
        textures.len()
    };
    unsafe { *out_id = id };
    if count <= 8 {
        godot::global::godot_print!(
            "[xreal] CreateTexture #{count} tid={}: {width}x{height} color={:#x} color_format={} \
             flags={} arraylen={} -> id={id} gl_tex={gl_id} engine_allocated={}",
            current_tid(),
            desc.color,
            desc.color_format,
            desc.flags,
            desc.texture_array_length,
            desc.color == 0
        );
    }
    0
}

/// `IUnityXRDisplay::QueryTextureDesc` (+0x20). Called from `OverlayBase::SetSwapChainBuffers`;
/// the SDK reads `color` (+0x08), `width` (+0x20) and `height` (+0x24) from what we write and
/// registers the GL name into the NR swapchain.
extern "C" fn xr_query_texture_desc(
    _handle: *mut c_void,
    tex_id: u32,
    out: *mut UnityXrRenderTextureDesc,
) -> i32 {
    if out.is_null() {
        return 1;
    }
    let entry = XR_TEXTURES
        .lock()
        .expect("xr textures mutex")
        .iter()
        .find(|t| t.id == tex_id)
        .copied();
    let Some(entry) = entry else {
        godot::global::godot_warn!("[xreal] QueryTextureDesc: unknown id={tex_id}");
        return 1;
    };
    if XR_QUERY_LOG.fetch_add(1, Ordering::Relaxed) < 8 {
        godot::global::godot_print!(
            "[xreal] QueryTextureDesc tid={} id={tex_id} -> gl_tex={} {}x{} layers={} flags={} \
             color_format={} (SetSwapChainBuffers is registering our texture)",
            current_tid(),
            entry.gl_id,
            entry.width,
            entry.height,
            entry.layers,
            entry.flags,
            entry.color_format
        );
    }
    unsafe {
        // Echo back the SDK's own color_format / flags / array length. Reporting a 1-layer / flags-0
        // descriptor for our 2-layer array mis-registers the NR swapchain (Multiview right eye = gray).
        *out = UnityXrRenderTextureDesc {
            color_format: entry.color_format,
            _pad0: 0,
            color: entry.gl_id as u64,
            depth_format: 0,
            _pad1: 0,
            depth: 0,
            width: entry.width as u32,
            height: entry.height as u32,
            texture_array_length: entry.layers as u32,
            flags: entry.flags,
        };
    }
    0
}

/// `IUnityXRDisplay::DestroyTexture` (+0x28). Drop our record; GL deletion is deferred (this can
/// be called off the render thread during teardown, where our context is not current).
extern "C" fn xr_destroy_texture(_handle: *mut c_void, tex_id: u32) -> i32 {
    let mut textures = XR_TEXTURES.lock().expect("xr textures mutex");
    if let Some(pos) = textures.iter().position(|t| t.id == tex_id) {
        textures.remove(pos);
    }
    0
}

extern "C" fn xr_get_platform_data(_handle: *mut c_void, _out: *mut *mut c_void) -> i32 {
    0
}
extern "C" fn xr_create_occlusion_mesh(
    _handle: *mut c_void,
    _num_vertices: u32,
    _num_indices: u32,
    out_id: *mut u32,
) -> i32 {
    if !out_id.is_null() {
        unsafe { *out_id = 0 };
    }
    0
}
extern "C" fn xr_destroy_occlusion_mesh(_handle: *mut c_void, _mesh_id: u32) -> i32 {
    0
}
extern "C" fn xr_set_occlusion_mesh(
    _handle: *mut c_void,
    _mesh_id: u32,
    _vertices: *mut c_void,
    _num_vertices: u32,
    _indices: *mut u32,
    _num_indices: u32,
) -> i32 {
    0
}

static UNITY_XR_DISPLAY: IUnityXrDisplay = IUnityXrDisplay {
    register_lifecycle_provider: xr_register_display_lifecycle_provider,
    register_display_provider: xr_register_display_provider,
    register_provider_for_graphics_thread: xr_register_gfx_thread_provider,
    create_texture: xr_create_texture,
    query_texture_desc: xr_query_texture_desc,
    destroy_texture: xr_destroy_texture,
    get_platform_data: xr_get_platform_data,
    create_occlusion_mesh: xr_create_occlusion_mesh,
    destroy_occlusion_mesh: xr_destroy_occlusion_mesh,
    set_occlusion_mesh: xr_set_occlusion_mesh,
};

static UNITY_XR_DISPLAY_HELPER: IUnityXrDisplayHelper = IUnityXrDisplayHelper {
    register_texture_provider: xr_register_texture_provider,
    property_to_id: xr_property_to_id,
};

static UNITY_XR_INPUT: IUnityXrInput = IUnityXrInput {
    register_lifecycle_provider: xr_register_input_lifecycle_provider,
    register_input_provider: xr_register_input_provider,
    set_device_connected: xr_set_device_connected,
};

static UNITY_XR_MESHING: IUnityXrMeshing = IUnityXrMeshing {
    unused_0: xr_unused,
    unused_1: xr_unused,
    unused_2: xr_unused,
    unused_3: xr_unused,
    unused_4: xr_unused,
    register_lifecycle_provider: xr_register_mesh_lifecycle_provider,
};

fn graphics_ptr() -> *mut c_void {
    ptr::addr_of!(UNITY_GRAPHICS) as *mut c_void
}

fn xr_display_ptr() -> *mut c_void {
    ptr::addr_of!(UNITY_XR_DISPLAY) as *mut c_void
}

fn xr_display_helper_ptr() -> *mut c_void {
    ptr::addr_of!(UNITY_XR_DISPLAY_HELPER) as *mut c_void
}

fn xr_input_ptr() -> *mut c_void {
    ptr::addr_of!(UNITY_XR_INPUT) as *mut c_void
}

fn xr_meshing_ptr() -> *mut c_void {
    ptr::addr_of!(UNITY_XR_MESHING) as *mut c_void
}

extern "C" fn get_interface(guid: *const UnityInterfaceGuid) -> *mut c_void {
    if guid.is_null() {
        return ptr::null_mut();
    }
    let guid = unsafe { &*guid };
    if guid.high == IUNITY_GRAPHICS_GUID.high && guid.low == IUNITY_GRAPHICS_GUID.low {
        graphics_ptr()
    } else if guid.high == IUNITY_XR_DISPLAY_GUID.high && guid.low == IUNITY_XR_DISPLAY_GUID.low {
        xr_display_ptr()
    } else if guid.high == IUNITY_XR_DISPLAY_HELPER_GUID.high
        && guid.low == IUNITY_XR_DISPLAY_HELPER_GUID.low
    {
        xr_display_helper_ptr()
    } else if guid.high == IUNITY_XR_INPUT_GUID.high && guid.low == IUNITY_XR_INPUT_GUID.low {
        xr_input_ptr()
    } else if guid.high == IUNITY_XR_MESHING_GUID.high && guid.low == IUNITY_XR_MESHING_GUID.low {
        xr_meshing_ptr()
    } else {
        ptr::null_mut()
    }
}
extern "C" fn register_interface(_guid: *const UnityInterfaceGuid, _ptr: *mut c_void) {}
extern "C" fn get_interface_split(high: u64, low: u64) -> *mut c_void {
    if high == IUNITY_GRAPHICS_GUID.high && low == IUNITY_GRAPHICS_GUID.low {
        graphics_ptr()
    } else if high == IUNITY_XR_DISPLAY_GUID.high && low == IUNITY_XR_DISPLAY_GUID.low {
        xr_display_ptr()
    } else if high == IUNITY_XR_DISPLAY_HELPER_GUID.high && low == IUNITY_XR_DISPLAY_HELPER_GUID.low
    {
        xr_display_helper_ptr()
    } else if high == IUNITY_XR_INPUT_GUID.high && low == IUNITY_XR_INPUT_GUID.low {
        xr_input_ptr()
    } else if high == IUNITY_XR_MESHING_GUID.high && low == IUNITY_XR_MESHING_GUID.low {
        xr_meshing_ptr()
    } else {
        ptr::null_mut()
    }
}
extern "C" fn register_interface_split(_high: u64, _low: u64, _ptr: *mut c_void) {}

static UNITY_INTERFACES: IUnityInterfaces = IUnityInterfaces {
    get_interface,
    register_interface,
    get_interface_split,
    register_interface_split,
};

/// Pointer to the process-global fake `IUnityInterfaces`, to pass to `UnityPluginLoad`.
pub fn interfaces_ptr() -> *mut c_void {
    ptr::addr_of!(UNITY_INTERFACES) as *mut c_void
}

/// Invoke provider initialize/start callbacks registered by `libXREALXRPlugin.so`.
///
/// RE / unverified: Unity normally owns this lifecycle. Godot only calls the callbacks after
/// `InitUserDefinedSettings` and `CreateSession`, so XREAL's singleton wrappers exist before
/// `NativeHMD` / `NativePerception` are constructed.
pub fn start_registered_providers() {
    if LIFECYCLE_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }

    let display = *DISPLAY_LIFECYCLE.lock().expect("display lifecycle mutex");
    let input = *INPUT_LIFECYCLE.lock().expect("input lifecycle mutex");

    if let Some(provider) = display {
        if let Some(callback) = provider.initialize {
            run_callback(provider, "initialize", callback);
        }
    }
    if let Some(provider) = input {
        if let Some(callback) = provider.initialize {
            run_callback(provider, "initialize", callback);
        }
    }
    if let Some(provider) = display {
        if let Some(callback) = provider.start {
            run_callback(provider, "start", callback);
        }
    }
    // InputStart must be called to register NativeGlasses in SessionManager before the
    // XREAL Nebula service sends action callbacks (~6 seconds after session start). Without
    // InputStart, SessionManager::HandleActionCallback calls NativeGlasses::GetActionData
    // on a null pointer and crashes (SIGSEGV fault addr 0x8).
    // Device-confirmed: the crash happens WITHOUT InputStart. Re-enabled.
    if let Some(provider) = input {
        if let Some(callback) = provider.start {
            run_callback(provider, "start", callback);
        }
    }
    // GfxThreadStart requires an active EGL context (CreateSwapchainEx allocates GL textures).
    // On Godot main thread there is no EGL context, so we defer GfxThreadStart to the first
    // call of run_render_thread_tick() which is invoked from RenderingServer::call_on_render_thread
    // (i.e., from Godot's rendering thread where the EGL context is active).

    let mesh_registered = MESH_LIFECYCLE
        .lock()
        .expect("mesh lifecycle mutex")
        .is_some();
    godot::global::godot_print!(
        "[xreal] Unity XR lifecycle start complete: display={}, input={}, mesh={}",
        display.is_some(),
        input.is_some(),
        mesh_registered
    );
}

/// Invoke the registered Unity XR display graphics-thread submit callback once.
///
/// RE / unverified: Unity normally calls this on the graphics thread after frame setup.
/// We use it only as a diagnostic probe until the frame descriptor ABI is known.
pub fn submit_registered_display_frame_once() -> Option<i32> {
    let provider = *GFX_THREAD_PROVIDER
        .lock()
        .expect("gfx-thread provider mutex");
    let provider = provider?;
    let callback = provider.submit_current_frame?;
    Some(unsafe {
        callback(
            provider.context as *mut c_void,
            provider.user_data as *mut c_void,
        )
    })
}

/// Invoke the registered Unity XR display frame-description callback once with a temp buffer.
///
/// RE / unverified: disassembly shows XREAL reads the hints buffer through at least +0x50
/// and writes the next-frame descriptor through at least +0x584. These buffers are deliberately
/// oversized and zero-filled; the result is diagnostic only and is not interpreted as Unity ABI.
pub fn populate_registered_display_frame_desc_once() -> Option<(i32, usize, u8, u8)> {
    let provider = *GFX_THREAD_PROVIDER
        .lock()
        .expect("gfx-thread provider mutex");
    let provider = provider?;
    let callback = provider.populate_next_frame_desc?;
    let hints = [0_u8; 0x80];
    let mut desc = [0_u8; 0x600];
    let status = unsafe {
        callback(
            provider.context as *mut c_void,
            provider.user_data as *mut c_void,
            hints.as_ptr() as *const c_void,
            desc.as_mut_ptr() as *mut c_void,
        )
    };
    let nonzero = desc.iter().filter(|byte| **byte != 0).count();
    let read_u32 = |offset: usize| -> u32 {
        u32::from_ne_bytes(desc[offset..offset + 4].try_into().expect("u32 desc slice"))
    };
    let read_u64 = |offset: usize| -> u64 {
        u64::from_ne_bytes(desc[offset..offset + 8].try_into().expect("u64 desc slice"))
    };
    // Dump all non-zero 32-bit words to find GL texture IDs (small integers ~1-200).
    // Texture IDs are likely near the overlay count at 0x580.
    let mut nonzero_words = Vec::new();
    let mut i = 0usize;
    while i + 4 <= desc.len() {
        let v = read_u32(i);
        if v != 0 {
            nonzero_words.push(format!("[{i:#05x}]={v}"));
        }
        i += 4;
    }
    // Also dump raw bytes 0x580-0x5ff for overlay descriptor analysis.
    let overlay_region: Vec<String> = desc[0x580..0x5c0]
        .chunks(4)
        .enumerate()
        .map(|(j, chunk)| {
            let v = u32::from_ne_bytes(chunk.try_into().unwrap_or([0; 4]));
            format!("[{:#05x}]={v}", 0x580 + j * 4)
        })
        .collect();
    godot::global::godot_print!(
        "[xreal] Unity XR populate desc detail: status={status}, nonzero={nonzero}, \
         u32[0x00]={}, u32[0x04]={}, u32[0x08]={}, u32[0x0c]={}, u32[0x10]={}, \
         u32[0x14]={}, u64[0x24]=0x{:x}, u64[0x28]=0x{:x}, u64[0x30]=0x{:x}, \
         u64[0x38]=0x{:x}, u64[0x3f0]=0x{:x}, u64[0x410]=0x{:x}, u64[0x450]=0x{:x}, \
         u32[0x580]={}, u32[0x584]={}",
        read_u32(0x00),
        read_u32(0x04),
        read_u32(0x08),
        read_u32(0x0c),
        read_u32(0x10),
        read_u32(0x14),
        read_u64(0x24),
        read_u64(0x28),
        read_u64(0x30),
        read_u64(0x38),
        read_u64(0x3f0),
        read_u64(0x410),
        read_u64(0x450),
        read_u32(0x580),
        read_u32(0x584)
    );
    godot::global::godot_print!("[xreal] desc nonzero u32s: {}", nonzero_words.join(", "));
    godot::global::godot_print!("[xreal] desc overlay region: {}", overlay_region.join(", "));
    Some((status, nonzero, desc[0], desc[0x580]))
}

/// Invoke the registered `PopulateNextFrameDesc` callback with a caller-supplied `desc` pointer.
///
/// The canonical use is to pass the `DisplayManager` function-local static at
/// `lib_base + 0xdb400` so that `0xdb410` (the byte `CreateFrame()` / `SubmitCurrentFrame()`
/// gate on) gets written with a non-zero render-pass count. Zero-fill hints are sufficient for
/// the probe; real hints from Unity contain scale/focus-plane data that we don't need yet.
///
/// RE: disassembly shows the static lives at `libXREALXRPlugin.so + 0xdb400`; the field at
/// `+0x10` is a `strh`-initialised u16 that `CreateFrame()` reads with `ldrb` (first byte).
/// `PopulateNextFrameDesc` is expected to write a non-zero render-pass count there.
pub fn populate_registered_display_frame_desc_with_ptr(desc: *mut c_void) -> i32 {
    let provider = *GFX_THREAD_PROVIDER
        .lock()
        .expect("gfx-thread provider mutex");
    let Some(provider) = provider else {
        godot::global::godot_print!("[xreal] populate_with_ptr: no gfx-thread provider registered");
        return -1;
    };
    let Some(callback) = provider.populate_next_frame_desc else {
        godot::global::godot_print!(
            "[xreal] populate_with_ptr: PopulateNextFrameDesc not registered"
        );
        return -2;
    };
    let hints = [0_u8; 0x80];
    let status = unsafe {
        callback(
            provider.context as *mut c_void,
            provider.user_data as *mut c_void,
            hints.as_ptr() as *const c_void,
            desc,
        )
    };
    godot::global::godot_print!("[xreal] PopulateNextFrameDesc(desc={desc:?}): status={status}");
    status
}

/// Called each frame from `XrealHeadTracker::process` via `RenderingServer::call_on_render_thread`.
///
/// Must run on Godot's rendering thread because `GfxThreadStart` calls
/// `DisplayManager::GfxThreadStart` → `NativeRendering::GfxThreadStart` →
/// `OverlayBase::CreateBuffer` → `NativeRendering::CreateSwapchainEx` which allocates
/// GL textures and therefore requires an active EGL context. Godot's main thread has no
/// EGL context; the rendering thread does.
///
/// On the first call: invokes `GfxThreadStart` (once), which triggers
/// `NativeRendering: GfxThreadStart End`, `OverlayBase::SetSwapChainBuffers`, and
/// `NativeRendering::AcquireFrame` (stores a frame handle in `DisplayManager+0x120`).
/// On subsequent calls: drives `PopulateNextFrameDesc` so the SDK's GLThread always has
/// a fresh frame handle for `SubmitCurrentFrame`.
pub fn run_render_thread_tick() {
    if !GFX_THREAD_STARTED.swap(true, Ordering::SeqCst) {
        if let Some(provider) = *GFX_THREAD_PROVIDER
            .lock()
            .expect("gfx-thread provider mutex")
        {
            if let Some(callback) = provider.start {
                let mut capabilities = UnityXrRenderingCapabilities { bytes: [0; 64] };
                let status = unsafe {
                    callback(
                        provider.context as *mut c_void,
                        provider.user_data as *mut c_void,
                        &mut capabilities,
                    )
                };
                godot::global::godot_print!(
                    "[xreal] render thread: GfxThreadStart tid={} -> {status}, \
                     capabilities[0]={}, capabilities[2]={}",
                    current_tid(),
                    capabilities.bytes[0],
                    capabilities.bytes[2]
                );
            }
        }
        // We intentionally do NOT start the direct libnr_loader `NRRendering*` path here.
        // `DisplayManager` owns its own `NativeRendering` (constructed in `Initialize`, started by
        // `GfxThreadStart` above); a second NR rendering instance conflicts with it. The engine's
        // job now is only to feed textures through the `IUnityXRDisplay` interface, so
        // `GfxThreadStart → CreateDisplayLayer → CreateBuffer` calls our `CreateTexture`.
    }
    run_frame_tick();
}

/// Called each frame from `XrealHeadTracker::process` once the session is live.
///
/// Drives `PopulateNextFrameDesc` with a temporary buffer so that:
/// 1. On the first call after `GfxThreadStart`: triggers `OverlayBase::SetSwapChainBuffers`
///    (registers the 7 GL textures created by `DisplayOverlay::CreateBuffer` with the
///    XREAL swapchain compositor), and calls `NativeRendering::AcquireFrame` which stores a
///    valid frame handle in `DisplayManager+0x120`.
/// 2. On subsequent calls: re-acquires the next swapchain buffer so the XREAL GLThread's
///    `SubmitCurrentFrame` always has a fresh frame handle to submit.
///
/// We deliberately use a temporary `desc` buffer (not `lib_base+0xdb400`) so that
/// `0xdb410` is never modified — that gate byte must stay 0 to keep `SubmitCurrentFrame`
/// on the safe `SetBufferViewport + NativeRendering::SubmitFrame` path.
pub fn run_frame_tick() {
    let n = FRAME_TICK_COUNT.fetch_add(1, Ordering::Relaxed);

    // Drive the per-frame HMD input update (→ DisplayManager::OnBeforeRender) BEFORE populating the
    // frame, so the render pose the compositor reprojects against is refreshed to the live head pose
    // instead of freezing at session start (which world-anchors our render). Runs on the render
    // thread — a different thread from XrealHeadTracker::process's session-manager pose read, so the
    // two pose pipelines are not touched on the same thread in the same frame.
    let hmd_update = call_input_update_hmd();
    if n < 5 || n % 300 == 0 {
        godot::global::godot_print!(
            "[xreal] input UpdateDeviceState(HMD)->{hmd_update} (frame {n})"
        );
    }

    let provider = *GFX_THREAD_PROVIDER
        .lock()
        .expect("gfx-thread provider mutex");
    let Some(provider) = provider else { return };
    let Some(callback) = provider.populate_next_frame_desc else {
        return;
    };

    // `desc` is a real `UnityXRNextFrameDesc`. The first call runs `OverlayBase::SetSwapChainBuffers`
    // (registering our textures) then `AcquireFrame`; every call writes `renderPasses[k].textureId`
    // (+0x00 / +0xfc) and `renderPassesCount` (+0x580). It MUST be a plain engine buffer — never
    // `lib_base+0xdb400` — so the unrelated `0xdb410` CreateFrame gate stays untouched.
    let hints = [0_u8; 0x80];
    let mut desc = [0_u8; 0x600];
    let pop_status = unsafe {
        callback(
            provider.context as *mut c_void,
            provider.user_data as *mut c_void,
            hints.as_ptr() as *const c_void,
            desc.as_mut_ptr() as *mut c_void,
        )
    };

    let read_u32 = |offset: usize| -> u32 {
        u32::from_ne_bytes(desc[offset..offset + 4].try_into().expect("desc u32 slice"))
    };
    let pass_count = read_u32(0x580);
    // renderPasses[0].renderParamsCount (== 2 in Multiview single-pass-instanced).
    let rp0_count = read_u32(0x0f8);
    let tex_ids = [read_u32(0x00), read_u32(0xfc)];
    // Multiview is ONE render pass with TWO renderParams (not two passes). The RIGHT eye's
    // renderParams[1] lives at desc+0x80 (EyeProj base 0x78), NOT renderPasses[1] (desc+0xfc, which
    // is 0 here). Reading eye 1 from desc+0xfc gave the right eye a garbage/degenerate frustum → the
    // right SubViewport rendered black → right-eye-black. See docs/archive/codex-righteye-analysis.md.
    let multiview = pass_count == 1 && rp0_count >= 2 && tex_ids[0] != 0 && tex_ids[1] == 0;

    // Each renderParams block carries the SDK's per-eye projection (half-angle tangents at
    // base+0x28..0x34) and eye pose position (deviceAnchorToEyePose at base+0x08..0x10). Eye-1 base:
    // Multipass = 0xfc (renderPasses[1]); Multiview = 0x78 (renderPasses[0].renderParams[1] @desc+0x80).
    let read_f32 = |offset: usize| -> f32 {
        f32::from_ne_bytes(desc[offset..offset + 4].try_into().expect("desc f32 slice"))
    };
    {
        let mut proj = STEREO_PROJ.lock().expect("stereo proj mutex");
        let bases = if multiview {
            [0x00usize, 0x78usize]
        } else {
            [0x00usize, 0xfcusize]
        };
        for (k, base) in bases.into_iter().enumerate() {
            let valid = if multiview {
                rp0_count as usize > k
            } else {
                pass_count as usize > k
            };
            proj[k] = EyeProj {
                valid,
                px: read_f32(base + 0x08),
                py: read_f32(base + 0x0c),
                pz: read_f32(base + 0x10),
                l: read_f32(base + 0x28),
                r: read_f32(base + 0x2c),
                t: read_f32(base + 0x30),
                b: read_f32(base + 0x34),
            };
        }
        if n < 3 {
            godot::global::godot_print!(
                "[xreal] eye proj L: l={:.4} r={:.4} t={:.4} b={:.4} pos=({:.4},{:.4},{:.4}) | \
                 R: l={:.4} r={:.4} t={:.4} b={:.4} pos=({:.4},{:.4},{:.4})",
                proj[0].l,
                proj[0].r,
                proj[0].t,
                proj[0].b,
                proj[0].px,
                proj[0].py,
                proj[0].pz,
                proj[1].l,
                proj[1].r,
                proj[1].t,
                proj[1].b,
                proj[1].px,
                proj[1].py,
                proj[1].pz,
            );
        }
    }

    // Blit Godot's rendered viewport into each acquired eye texture. Until the viewport texture is
    // published (`GODOT_SRC_TEX == 0`), fall back to an animated per-eye test pattern so we can
    // still tell the compositor path is alive.
    let eye_src = [
        GODOT_EYE_TEX_L.load(Ordering::Relaxed),
        GODOT_EYE_TEX_R.load(Ordering::Relaxed),
    ];
    let src_w = GODOT_SRC_W.load(Ordering::Relaxed);
    let src_h = GODOT_SRC_H.load(Ordering::Relaxed);
    let have_size = src_w > 0 && src_h > 0;
    let phase = (n % 180) as f32 / 180.0;
    let mut filled = 0u32;
    for (eye, &tex_id) in tex_ids.iter().enumerate().take(pass_count.max(1) as usize) {
        if tex_id == 0 {
            continue;
        }
        if let Some((gl_tex, dw, dh, layers)) = xr_texture_for(tex_id) {
            if layers >= 2 {
                // Multiview / single-pass-instanced: ONE array texture, both eyes in its layers
                // (layer 0 = left, layer 1 = right). renderPassesCount is 1 here.
                for layer in 0..layers.min(2) {
                    let src = eye_src[layer as usize];
                    if src != 0 && have_size {
                        crate::gl::blit_texture_to_layer(src, src_w, src_h, gl_tex, layer, dw, dh);
                    }
                }
            } else {
                let src = eye_src[eye.min(1)];
                if src != 0 && have_size {
                    // Real stereo: this eye's offscreen SubViewport texture.
                    crate::gl::blit_texture(src, src_w, src_h, gl_tex, dw, dh);
                } else if have_size {
                    // Mono fallback: Godot's default framebuffer into both eyes.
                    crate::gl::blit_default_framebuffer(gl_tex, src_w, src_h, dw, dh);
                } else {
                    let (r, g, b) = if eye == 0 {
                        (0.6 + 0.4 * phase, 0.05, 0.6 - 0.4 * phase)
                    } else {
                        (0.05, 0.6 - 0.4 * phase, 0.6 + 0.4 * phase)
                    };
                    crate::gl::fill_texture(gl_tex, r, g, b);
                }
            }
            filled += 1;
        }
    }

    // Present the frame. `SubmitCurrentFrame` runs `SetBufferViewport` + `NativeRendering::SubmitFrame`
    // (the actual present of our registered buffers) then `UpdateMetrics`. UpdateMetrics used to
    // SIGBUS on a null metrics callback; `patch_update_metrics` neuters it, so this is now safe and
    // also advances the swapchain (AcquireFrame rotates to the next buffer next frame).
    let submit_status = provider.submit_current_frame.map(|callback| unsafe {
        callback(
            provider.context as *mut c_void,
            provider.user_data as *mut c_void,
        )
    });

    if n < 5 || n % 300 == 0 {
        godot::global::godot_print!(
            "[xreal] frame_tick #{n} tid={}: populate={pop_status} passes={pass_count} \
             tex0={} tex1={} filled={filled} submit={submit_status:?} \
             eye_l={} eye_r={} src={src_w}x{src_h}",
            current_tid(),
            tex_ids[0],
            tex_ids[1],
            eye_src[0],
            eye_src[1]
        );
    }
}

/// Return which Unity XR display graphics-thread callbacks have been registered.
pub fn display_gfx_callback_status() -> (bool, bool, bool) {
    let provider = *GFX_THREAD_PROVIDER
        .lock()
        .expect("gfx-thread provider mutex");
    match provider {
        Some(provider) => (
            provider.start.is_some(),
            provider.submit_current_frame.is_some(),
            provider.populate_next_frame_desc.is_some(),
        ),
        None => (false, false, false),
    }
}
