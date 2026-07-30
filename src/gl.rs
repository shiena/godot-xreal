//! Minimal GLES3 binding for the XREAL display path.
//!
//! `libXREALXRPlugin.so`'s display provider (see `docs/plans/frame-submission-plan.md`) asks the
//! engine to allocate its render textures through `IUnityXRDisplay::CreateTexture`. This module
//! is that engine side: it `dlopen`s `libGLESv3.so` and exposes just enough GL to allocate a
//! texture and copy pixels into it.
//!
//! **Call everything here from Godot's rendering thread**, through
//! `RenderingServer::call_on_render_thread`, the one place an EGL context is guaranteed current.
//!
//! This header used to add "there is no EGL context on the main thread". That is wrong, at least on
//! Android: measured on the X4000 on 2026-07-21, [`has_current_context`] called from `_process`
//! returns `Some(true)`, because Godot's Android main loop *is* the GL thread. Keep using
//! `call_on_render_thread` anyway, since the contract rather than the coincidence is what holds
//! across platforms and thread models. Note the consequence, though: on Android that call reorders
//! work within one thread rather than moving it to another core.
//!
//! On desktop the `dlopen` fails and every entry point returns `None` or does nothing, matching the
//! rest of the crate's behaviour when the native libs are absent.

// GL helpers. Some entry points, delete_texture among them, are retained for completeness and go
// unused, and on desktop every entry point is a dummy no-op. Allow dead code on both targets.
#![allow(dead_code)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::OnceLock;

use libloading::Library;

const GLES_LIB: &str = "libGLESv3.so";

type FnGenTextures = unsafe extern "C" fn(i32, *mut u32);
type FnDeleteTextures = unsafe extern "C" fn(i32, *const u32);
type FnBindTexture = unsafe extern "C" fn(u32, u32);
type FnTexParameteri = unsafe extern "C" fn(u32, u32, i32);
type FnTexImage2D = unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
type FnGetError = unsafe extern "C" fn() -> u32;
type FnGenFramebuffers = unsafe extern "C" fn(i32, *mut u32);
type FnBindFramebuffer = unsafe extern "C" fn(u32, u32);
type FnFramebufferTexture2D = unsafe extern "C" fn(u32, u32, u32, u32, i32);
type FnCheckFramebufferStatus = unsafe extern "C" fn(u32) -> u32;
type FnBlitFramebuffer = unsafe extern "C" fn(i32, i32, i32, i32, i32, i32, i32, i32, u32, u32);
type FnClearColor = unsafe extern "C" fn(f32, f32, f32, f32);
type FnClear = unsafe extern "C" fn(u32);
type FnTexImage3D =
    unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
// glTexStorage3D(target, levels, internalformat, width, height, depth): immutable-storage allocation.
type FnTexStorage3D = unsafe extern "C" fn(u32, i32, u32, i32, i32, i32);
type FnFramebufferTextureLayer = unsafe extern "C" fn(u32, u32, u32, i32, i32);
// glCopyImageSubData(srcName, srcTarget, srcLevel, srcX,srcY,srcZ, dstName, dstTarget, dstLevel,
// dstX,dstY,dstZ, srcW,srcH,srcD): the GLES 3.2 direct texel copy. It writes any array layer and
// needs no FBO or blit.
type FnCopyImageSubData =
    unsafe extern "C" fn(u32, u32, i32, i32, i32, i32, u32, u32, i32, i32, i32, i32, i32, i32, i32);
// glGetTexLevelParameteriv(target, level, pname, params), from GLES 3.1. It probes the source
// texture's internal format, which gates the direct same-format layer copy.
type FnGetTexLevelParameteriv = unsafe extern "C" fn(u32, i32, u32, *mut i32);
type FnGetIntegerv = unsafe extern "C" fn(u32, *mut i32);
/// `glGetFloatv`, for the one piece of state we touch that is not an integer: the clear colour.
type FnGetFloatv = unsafe extern "C" fn(u32, *mut f32);
type FnIsEnabled = unsafe extern "C" fn(u32) -> u8;
type FnEnable = unsafe extern "C" fn(u32);
type FnDisable = unsafe extern "C" fn(u32);
type FnGenBuffers = unsafe extern "C" fn(i32, *mut u32);
type FnBindBuffer = unsafe extern "C" fn(u32, u32);
type FnBufferData = unsafe extern "C" fn(u32, isize, *const c_void, u32);
type FnBufferSubData = unsafe extern "C" fn(u32, isize, isize, *const c_void);
type FnTexSubImage2D = unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
type FnPixelStorei = unsafe extern "C" fn(u32, i32);
type FnEglGetCurrentContext = unsafe extern "C" fn() -> *mut c_void;

const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE_2D_ARRAY: u32 = 0x8C1A;
const GL_RGBA: u32 = 0x1908;
const GL_RGBA8: i32 = 0x8058;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_LINEAR: i32 = 0x2601;
const GL_CLAMP_TO_EDGE: i32 = 0x812F;
const GL_TEXTURE_BASE_LEVEL: u32 = 0x813C;
const GL_TEXTURE_MAX_LEVEL: u32 = 0x813D;
const GL_READ_FRAMEBUFFER: u32 = 0x8CA8;
const GL_DRAW_FRAMEBUFFER: u32 = 0x8CA9;
const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
const GL_COLOR_BUFFER_BIT: u32 = 0x0000_4000;
const GL_DRAW_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
const GL_READ_FRAMEBUFFER_BINDING: u32 = 0x8CAA;
const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
const GL_SCISSOR_TEST: u32 = 0x0C11;
const GL_COLOR_CLEAR_VALUE: u32 = 0x0C22;
const GL_TEXTURE_BINDING_2D: u32 = 0x8069;
const GL_TEXTURE_INTERNAL_FORMAT: u32 = 0x1003;
const GL_RGB10_A2: i32 = 0x8059;
const GL_UNSIGNED_INT_2_10_10_10_REV: u32 = 0x8368;
const GL_PIXEL_UNPACK_BUFFER: u32 = 0x88EC;
const GL_PIXEL_UNPACK_BUFFER_BINDING: u32 = 0x8A45;
const GL_STREAM_DRAW: u32 = 0x88E0;
const GL_UNPACK_ALIGNMENT: u32 = 0x0CF5;
/// Single-channel 8-bit pixel data (a luma plane).
pub const GL_RED: u32 = 0x1903;
/// Two-channel 8-bit pixel data (an interleaved CbCr plane).
pub const GL_RG: u32 = 0x8227;

struct Gl {
    gen_textures: FnGenTextures,
    delete_textures: FnDeleteTextures,
    bind_texture: FnBindTexture,
    tex_parameteri: FnTexParameteri,
    tex_image_2d: FnTexImage2D,
    get_error: FnGetError,
    gen_framebuffers: FnGenFramebuffers,
    bind_framebuffer: FnBindFramebuffer,
    framebuffer_texture_2d: FnFramebufferTexture2D,
    check_framebuffer_status: FnCheckFramebufferStatus,
    blit_framebuffer: FnBlitFramebuffer,
    clear_color: FnClearColor,
    clear: FnClear,
    tex_image_3d: FnTexImage3D,
    /// Immutable-storage 3D/array allocation. Optional: absent on GL implementations without it, in
    /// which case [`alloc_texture_array`] falls back to mutable `glTexImage3D` (mirrors Unity's
    /// `ApiGLES::CreateTexture` caps-gated branch).
    tex_storage_3d: Option<FnTexStorage3D>,
    /// GLES 3.2 `glCopyImageSubData`. Optional: absent pre-3.2. Used to write a `GL_TEXTURE_2D_ARRAY`
    /// layer directly, because `glBlitFramebuffer` into a layer > 0 attachment is a silent no-op on
    /// the Adreno GLES driver (the cause of the black Multiview right eye).
    copy_image_sub_data: Option<FnCopyImageSubData>,
    /// GLES 3.1 `glGetTexLevelParameteriv`. Optional (absent pre-3.1). Probes the eye SubViewport
    /// texture's internal format once; the direct same-format layer copy is gated on the result.
    get_tex_level_parameteriv: Option<FnGetTexLevelParameteriv>,
    framebuffer_texture_layer: FnFramebufferTextureLayer,
    get_integerv: FnGetIntegerv,
    get_floatv: FnGetFloatv,
    is_enabled: FnIsEnabled,
    enable: FnEnable,
    disable: FnDisable,
    gen_buffers: FnGenBuffers,
    bind_buffer: FnBindBuffer,
    buffer_data: FnBufferData,
    buffer_sub_data: FnBufferSubData,
    tex_sub_image_2d: FnTexSubImage2D,
    pixel_storei: FnPixelStorei,
    /// `eglGetCurrentContext`, from `libEGL.so`. Only [`has_current_context`] uses it, to check this
    /// module's threading assumption on a live device. It is optional, and a missing symbol simply
    /// makes the probe report "unknown".
    egl_get_current_context: Option<FnEglGetCurrentContext>,
    _lib: Library,
    _egl_lib: Option<Library>,
}

impl Gl {
    fn load() -> Result<Self, String> {
        unsafe {
            let lib = Library::new(GLES_LIB).map_err(|e| format!("dlopen {GLES_LIB}: {e}"))?;
            // Optional: only the current-context probe needs it, so a failure must not disable GL.
            let egl_lib = Library::new("libEGL.so").ok();
            macro_rules! sym {
                ($name:literal, $ty:ty) => {
                    *lib.get::<$ty>(concat!($name, "\0").as_bytes())
                        .map_err(|e| format!("dlsym {}: {e}", $name))?
                };
            }
            Ok(Gl {
                gen_textures: sym!("glGenTextures", FnGenTextures),
                delete_textures: sym!("glDeleteTextures", FnDeleteTextures),
                bind_texture: sym!("glBindTexture", FnBindTexture),
                tex_parameteri: sym!("glTexParameteri", FnTexParameteri),
                tex_image_2d: sym!("glTexImage2D", FnTexImage2D),
                get_error: sym!("glGetError", FnGetError),
                gen_framebuffers: sym!("glGenFramebuffers", FnGenFramebuffers),
                bind_framebuffer: sym!("glBindFramebuffer", FnBindFramebuffer),
                framebuffer_texture_2d: sym!("glFramebufferTexture2D", FnFramebufferTexture2D),
                check_framebuffer_status: sym!(
                    "glCheckFramebufferStatus",
                    FnCheckFramebufferStatus
                ),
                blit_framebuffer: sym!("glBlitFramebuffer", FnBlitFramebuffer),
                clear_color: sym!("glClearColor", FnClearColor),
                clear: sym!("glClear", FnClear),
                tex_image_3d: sym!("glTexImage3D", FnTexImage3D),
                // Optional (GLES 3.0 core, but load non-fatally so a missing symbol degrades to the
                // mutable `glTexImage3D` fallback rather than disabling the whole display path).
                tex_storage_3d: lib
                    .get::<FnTexStorage3D>(b"glTexStorage3D\0")
                    .map(|s| *s)
                    .ok(),
                // Optional (GLES 3.2). Non-fatal so pre-3.2 devices fall back to the blit path.
                copy_image_sub_data: lib
                    .get::<FnCopyImageSubData>(b"glCopyImageSubData\0")
                    .map(|s| *s)
                    .ok(),
                // Optional, from GLES 3.1. When it is absent the direct copy is never taken, since the probe stays
                // unknown.
                get_tex_level_parameteriv: lib
                    .get::<FnGetTexLevelParameteriv>(b"glGetTexLevelParameteriv\0")
                    .map(|s| *s)
                    .ok(),
                framebuffer_texture_layer: sym!(
                    "glFramebufferTextureLayer",
                    FnFramebufferTextureLayer
                ),
                get_integerv: sym!("glGetIntegerv", FnGetIntegerv),
                get_floatv: sym!("glGetFloatv", FnGetFloatv),
                is_enabled: sym!("glIsEnabled", FnIsEnabled),
                enable: sym!("glEnable", FnEnable),
                disable: sym!("glDisable", FnDisable),
                gen_buffers: sym!("glGenBuffers", FnGenBuffers),
                bind_buffer: sym!("glBindBuffer", FnBindBuffer),
                buffer_data: sym!("glBufferData", FnBufferData),
                buffer_sub_data: sym!("glBufferSubData", FnBufferSubData),
                tex_sub_image_2d: sym!("glTexSubImage2D", FnTexSubImage2D),
                pixel_storei: sym!("glPixelStorei", FnPixelStorei),
                egl_get_current_context: egl_lib.as_ref().and_then(|l| {
                    l.get::<FnEglGetCurrentContext>(b"eglGetCurrentContext\0")
                        .map(|s| *s)
                        .ok()
                }),
                _lib: lib,
                _egl_lib: egl_lib,
            })
        }
    }
}

static GL: OnceLock<Option<Gl>> = OnceLock::new();

fn gl() -> Option<&'static Gl> {
    GL.get_or_init(|| match Gl::load() {
        Ok(g) => Some(g),
        Err(e) => {
            godot::global::godot_warn!("[xreal] gl: {e} (display path disabled)");
            None
        }
    })
    .as_ref()
}

/// Does the calling thread have a current EGL context, that is, is it legal to call anything else
/// in this module from here? `Some(false)` means there is no context, and `None` means the probe is
/// unavailable, with no `libEGL.so` or no symbol, so the answer is unknown.
///
/// This module's header asserts there is no context on the main thread, and the probe exists to
/// check that on a live device before anyone relies on it either way.
pub fn has_current_context() -> Option<bool> {
    let f = gl()?.egl_get_current_context?;
    Some(!unsafe { f() }.is_null())
}

/// Is Godot running on the GL (Compatibility) renderer? Resolved once, from the actual runtime
/// renderer, not the project setting, and logged with the decision it gates.
///
/// The existing glasses display path is GL-only: eye SubViewport textures are handed to the SDK
/// compositor as client GL texture names, which requires Godot itself to own an EGL context. Under
/// the Vulkan renderers (Forward+ / Mobile) that context does not exist, so callers skip the
/// glasses submission entirely until the Vulkan-side bridge lands (the stage-2 AHardwareBuffer
/// share plus a private EGL context; see `docs/plans/vulkan-path-plan.md`). Head tracking, the
/// SDK session and the phone display stay renderer-independent.
pub fn renderer_is_gl() -> bool {
    use godot::classes::RenderingServer;
    use godot::obj::Singleton;
    static IS_GL: OnceLock<bool> = OnceLock::new();
    *IS_GL.get_or_init(|| {
        let rs = RenderingServer::singleton();
        let method = rs.get_current_rendering_method().to_string();
        let driver = rs.get_current_rendering_driver_name().to_string();
        let is_gl = driver.starts_with("opengl");
        godot::global::godot_print!(
            "[xreal] renderer: method={method} driver={driver} -> glasses GL display path {}",
            if is_gl {
                "ENABLED"
            } else {
                "DISABLED (Vulkan bridge not yet implemented: phone display + tracking only)"
            }
        );
        is_gl
    })
}

/// Scratch framebuffers reused for every fill and blit, created lazily on the render thread so no
/// FBO name is generated or deleted per frame. Index 0 is the draw and fill target, index 1 the
/// blit source.
static SCRATCH_FBO: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

/// Pixel-unpack buffers reused by [`upload_plane_pbo`], one per plane slot so the two uploads never
/// contend for the same buffer. Created lazily on the render thread; never deleted.
static SCRATCH_PBO: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

unsafe fn scratch_pbo(g: &Gl, slot: usize) -> u32 {
    let existing = SCRATCH_PBO[slot].load(Ordering::Relaxed);
    if existing != 0 {
        return existing;
    }
    let mut pbo: u32 = 0;
    (g.gen_buffers)(1, &mut pbo);
    SCRATCH_PBO[slot].store(pbo, Ordering::Relaxed);
    pbo
}

/// Upload one tightly-packed 8-bit plane into an existing `GL_TEXTURE_2D` through a persistent
/// pixel-unpack buffer. `slot` selects which PBO to reuse, 0 for luma and 1 for chroma;
/// `components` is [`GL_RED`] or [`GL_RG`]; and `tex` is the GL name from
/// `RenderingServer::texture_get_native_handle`. It returns `false` when GL is unavailable, the
/// sizes disagree, or the driver raised an error.
///
/// **Why a PBO.** Measured on the X4000 with an Adreno 710, a plain `glTexSubImage2D` from client
/// memory costs about 1.78 ns per *texel* whatever the bytes per texel: 1651 us for 921,600 R8
/// texels and 409 us for 230,400 RG8 texels. That signature is the driver tiling and swizzling
/// every texel on the CPU. With the source in a buffer object the driver is free to do that pass on
/// the GPU instead.
///
/// Godot's renderer owns the GL state, so every binding this touches is saved and restored.
pub fn upload_plane_pbo(
    slot: usize,
    tex: u32,
    width: i32,
    height: i32,
    components: u32,
    data: &[u8],
) -> bool {
    let Some(g) = gl() else { return false };
    let bytes_per_texel = match components {
        GL_RED => 1,
        GL_RG => 2,
        _ => return false,
    };
    let expected = (width as usize) * (height as usize) * bytes_per_texel;
    if tex == 0 || width <= 0 || height <= 0 || data.len() < expected || slot >= SCRATCH_PBO.len() {
        return false;
    }
    unsafe {
        let pbo = scratch_pbo(g, slot);
        if pbo == 0 {
            return false;
        }
        let (mut prev_tex, mut prev_pbo, mut prev_align) = (0i32, 0i32, 4i32);
        (g.get_integerv)(GL_TEXTURE_BINDING_2D, &mut prev_tex);
        (g.get_integerv)(GL_PIXEL_UNPACK_BUFFER_BINDING, &mut prev_pbo);
        (g.get_integerv)(GL_UNPACK_ALIGNMENT, &mut prev_align);
        while (g.get_error)() != 0 {} // drop any pre-existing error so ours is attributable

        (g.bind_buffer)(GL_PIXEL_UNPACK_BUFFER, pbo);
        // Orphan, then refill: re-specifying the store lets the driver hand back fresh memory instead of
        // stalling until the previous frame's transfer has been consumed.
        (g.buffer_data)(
            GL_PIXEL_UNPACK_BUFFER,
            expected as isize,
            std::ptr::null(),
            GL_STREAM_DRAW,
        );
        (g.buffer_sub_data)(
            GL_PIXEL_UNPACK_BUFFER,
            0,
            expected as isize,
            data.as_ptr() as *const c_void,
        );
        // R8 rows are not 4-byte aligned; without this the driver reads padded rows.
        (g.pixel_storei)(GL_UNPACK_ALIGNMENT, 1);
        (g.bind_texture)(GL_TEXTURE_2D, tex);
        (g.tex_sub_image_2d)(
            GL_TEXTURE_2D,
            0,
            0,
            0,
            width,
            height,
            components,
            GL_UNSIGNED_BYTE,
            std::ptr::null(), // offset 0 into the bound pixel-unpack buffer
        );
        let err = (g.get_error)();

        (g.pixel_storei)(GL_UNPACK_ALIGNMENT, prev_align);
        (g.bind_texture)(GL_TEXTURE_2D, prev_tex as u32);
        (g.bind_buffer)(GL_PIXEL_UNPACK_BUFFER, prev_pbo as u32);
        if err != 0 {
            godot::global::godot_warn!("[xreal] gl: PBO upload slot {slot} -> GL error {err:#x}");
        }
        err == 0
    }
}

unsafe fn scratch_fbo(g: &Gl, slot: usize) -> u32 {
    let existing = SCRATCH_FBO[slot].load(Ordering::Relaxed);
    if existing != 0 {
        return existing;
    }
    let mut fbo: u32 = 0;
    (g.gen_framebuffers)(1, &mut fbo);
    SCRATCH_FBO[slot].store(fbo, Ordering::Relaxed);
    fbo
}

/// Allocate a 2D `GL_RGB10_A2` texture of the given size and return its GL name, or `None` on
/// failure. It backs the Multipass per-eye swapchain textures.
///
/// `_srgb` is intentionally ignored: the eye texture has to be a UNORM format and NOT sRGB-typed,
/// confirmed on device 2026-07-17. Godot's `gl_compatibility` renderer outputs display-ready,
/// sRGB-encoded values, and the XREAL compositor passthrough-samples the eye texture and writes the
/// sampled value to the display without re-encoding. An A/B test allocating the eye texture as
/// `GL_SRGB8_ALPHA8`, the same bytes but sRGB-typed, came out about 26% too dark, because the
/// compositor applies a sample-time sRGB-to-linear decode. Unity's port uses an sRGB-typed target
/// because it renders in *linear* space, whereas our display-ready values must not be decoded. See
/// `docs/archive/multiview-investigation.md`, the 2026-07-17 color-space test.
///
/// **`GL_RGB10_A2`**, a UNORM format like the previous `GL_RGBA8`, deliberately matches Godot's
/// `gl_compatibility` 3D render-target format, probed as `0x8059` on device 2026-07-21; see
/// [`alloc_texture_array`], which made the same switch first. Identical formats let
/// [`blit_texture`] fill the eye with one exact `glCopyImageSubData`, with no conversion and no FBO
/// state, instead of a converting `glBlitFramebuffer`. Verified on device 2026-07-21: the colours
/// match the blit path.
pub fn alloc_texture(width: i32, height: i32, _srgb: bool) -> Option<u32> {
    let g = gl()?;
    unsafe {
        while (g.get_error)() != 0 {}
        let mut tex: u32 = 0;
        (g.gen_textures)(1, &mut tex);
        if tex == 0 || (g.get_error)() != 0 {
            return None;
        }
        (g.bind_texture)(GL_TEXTURE_2D, tex);
        (g.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        (g.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        (g.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        (g.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
        (g.tex_image_2d)(
            GL_TEXTURE_2D,
            0,
            GL_RGB10_A2,
            width,
            height,
            0,
            GL_RGBA,
            GL_UNSIGNED_INT_2_10_10_10_REV,
            std::ptr::null(),
        );
        (g.bind_texture)(GL_TEXTURE_2D, 0);
        if (g.get_error)() != 0 {
            (g.delete_textures)(1, &tex);
            return None;
        }
        Some(tex)
    }
}

/// Allocate a `GL_TEXTURE_2D_ARRAY` with `layers` layers, for the SDK's Multiview, or
/// Single-Pass-Instanced, path, where `CreateTexture` passes `textureArrayLength == 2`. The
/// compositor binds this as a layered multiview framebuffer, and a plain 2D texture there yields
/// `GL_INVALID_FRAMEBUFFER_OPERATION` and a black image. It returns the GL name, or `None` on
/// failure.
///
/// **Format: `GL_RGB10_A2`**, deliberately matching Godot's `gl_compatibility` 3D render-target
/// format, probed on device as a SubViewport internal format of `0x8059` on 2026-07-21. Matching
/// formats let [`blit_texture_to_layer`] fill each eye layer with ONE exact `glCopyImageSubData`
/// straight from the SubViewport. GLES forbids format-converting copies, and
/// `glCopyTexSubImage3D` from RGB10_A2 to RGBA8 raises `GL_INVALID_OPERATION`, tested 2026-07-21,
/// so an RGBA8 array would force a converting blit through a scratch texture first, at twice the
/// bandwidth. Like RGBA8, RGB10_A2 is UNORM, so the compositor's passthrough sample applies no sRGB
/// decode (see [`alloc_texture`]) and the colours match; only the precision differs, 10-bit being a
/// superset of the source's own values.
///
/// **Immutable storage.** The array is allocated with the immutable `glTexStorage3D` when that is
/// available, falling back to the mutable `glTexImage3D` otherwise, mirroring Unity's
/// `ApiGLES::CreateTexture`, which takes `glTexStorage3DEXT` for a `Tex2DArray` when the driver
/// supports immutable storage, as the Adreno 710 does, and uses `glTexImage3D` only as a fallback.
///
/// NOTE: this matching-Unity change was an *experiment* to fix Multiview's black right eye, on the
/// theory that libnr_api imports the array through per-layer 2D `glTextureView`s, which need
/// immutable storage. It was **tested on device 2026-07-17 and did NOT fix the right eye**:
/// immutable allocation succeeds, reporting `immutable=true`, and layer 1 fills, but the compositor
/// still presents black on the right, with a screencap right stddev of 0.0. Immutable storage is
/// therefore not the blocker, and the wall is inside libnr_api. The change stays dormant, since
/// Multiview is opt-in and shelved, as a faithful Unity match. See
/// `docs/archive/multiview-investigation.md`, 2026-07-17. The mutable path is the fallback for GL
/// implementations lacking immutable storage.
pub fn alloc_texture_array(width: i32, height: i32, layers: i32, _srgb: bool) -> Option<u32> {
    let g = gl()?;
    unsafe {
        while (g.get_error)() != 0 {}
        let mut tex: u32 = 0;
        (g.gen_textures)(1, &mut tex);
        if tex == 0 || (g.get_error)() != 0 {
            return None;
        }
        (g.bind_texture)(GL_TEXTURE_2D_ARRAY, tex);
        (g.tex_parameteri)(GL_TEXTURE_2D_ARRAY, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        (g.tex_parameteri)(GL_TEXTURE_2D_ARRAY, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        (g.tex_parameteri)(GL_TEXTURE_2D_ARRAY, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        (g.tex_parameteri)(GL_TEXTURE_2D_ARRAY, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);

        // Prefer immutable storage, which matches Unity. There is a single mip level, and pinning the BASE
        // and MAX levels keeps the texture mip-complete for whatever sampler state the compositor binds it
        // with.
        let immutable = match g.tex_storage_3d {
            Some(tex_storage_3d) => {
                (g.tex_parameteri)(GL_TEXTURE_2D_ARRAY, GL_TEXTURE_BASE_LEVEL, 0);
                (g.tex_parameteri)(GL_TEXTURE_2D_ARRAY, GL_TEXTURE_MAX_LEVEL, 0);
                tex_storage_3d(
                    GL_TEXTURE_2D_ARRAY,
                    1,
                    GL_RGB10_A2 as u32,
                    width,
                    height,
                    layers,
                );
                if (g.get_error)() == 0 {
                    true
                } else {
                    // Immutable allocation failed, perhaps from a format or driver quirk, so drain the error and retry
                    // mutable on the same, still-mutable texture object.
                    while (g.get_error)() != 0 {}
                    false
                }
            }
            None => false,
        };
        if !immutable {
            (g.tex_image_3d)(
                GL_TEXTURE_2D_ARRAY,
                0,
                GL_RGB10_A2,
                width,
                height,
                layers,
                0,
                GL_RGBA,
                GL_UNSIGNED_INT_2_10_10_10_REV,
                std::ptr::null(),
            );
        }
        (g.bind_texture)(GL_TEXTURE_2D_ARRAY, 0);
        let err = (g.get_error)();
        if err != 0 {
            godot::global::godot_warn!(
                "[xreal] alloc_texture_array {width}x{height}x{layers} immutable={immutable} gl_err={err}"
            );
            (g.delete_textures)(1, &tex);
            return None;
        }
        godot::global::godot_print!(
            "[xreal] alloc_texture_array {width}x{height}x{layers} immutable={immutable} tex={tex}"
        );
        Some(tex)
    }
}

/// A persistent 2D scratch texture, in the same `GL_RGB10_A2` format as the eye array, used to
/// normalise the eye SubViewport's format before copying it into an array layer, for when the
/// SubViewport's own format does NOT already match the array; see [`blit_texture_to_layer`]. It is
/// created lazily at eye size.
static TEMP_LAYER_TEX: AtomicU32 = AtomicU32::new(0);

/// Get the array-format scratch texture at `w` by `h`, creating it once. It assumes a stable eye
/// size.
unsafe fn temp_layer_tex(g: &Gl, w: i32, h: i32) -> Option<u32> {
    let existing = TEMP_LAYER_TEX.load(Ordering::Relaxed);
    if existing != 0 {
        return Some(existing);
    }
    while (g.get_error)() != 0 {}
    let mut tex: u32 = 0;
    (g.gen_textures)(1, &mut tex);
    if tex == 0 {
        return None;
    }
    (g.bind_texture)(GL_TEXTURE_2D, tex);
    (g.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    (g.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    (g.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    (g.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    (g.tex_image_2d)(
        GL_TEXTURE_2D,
        0,
        GL_RGB10_A2,
        w,
        h,
        0,
        GL_RGBA,
        GL_UNSIGNED_INT_2_10_10_10_REV,
        std::ptr::null(),
    );
    (g.bind_texture)(GL_TEXTURE_2D, 0);
    if (g.get_error)() != 0 {
        (g.delete_textures)(1, &tex);
        return None;
    }
    TEMP_LAYER_TEX.store(tex, Ordering::Relaxed);
    Some(tex)
}

/// Copy the 2D `src` into a single `layer` of a `GL_TEXTURE_2D_ARRAY`, `dst_array`. It fills the
/// per-eye layers of the Multiview swapchain texture.
///
/// Two GL quirks force a two-step path on this hardware:
///   1. `glBlitFramebuffer` straight into a `glFramebufferTextureLayer` attachment at **layer > 0
///      is a silent no-op on the Adreno GLES driver**: it returns a complete framebuffer and writes
///      nothing. That is the true cause of the black Multiview right eye, layer 1. `glClear` there
///      *does* work, so the NR compositor was never the problem.
///   2. `glCopyImageSubData` **can** write layer > 0, but it is a raw byte copy with no format
///      conversion, so copying the eye SubViewport, whose GL format is not plain `RGBA8`, directly
///      into the `RGBA8` array scrambles the colours, and Multiview looked colour-corrupted next to
///      Multipass.
///
/// Preferred path: because [`alloc_texture_array`] allocates the array in `GL_RGB10_A2`, the same
/// format Godot's `gl_compatibility` renderer gives the eye SubViewport, the layer fill is ONE
/// direct **`glCopyImageSubData` from the source into the layer**. Identical formats make it an
/// exact texel copy, so quirk 2 is moot, and it writes layer > 0 fine, so quirk 1 is moot too. It
/// is gated on a one-shot probe of the source's actual internal format,
/// `glGetTexLevelParameteriv`, so a renderer or config change that alters the SubViewport format
/// degrades safely instead of scrambling.
///
/// A single-pass `glCopyTexSubImage3D` read, convert and write was tried first, on 2026-07-21:
/// GLES's copy-conversion table forbids RGB10_A2 to RGBA8, raising `GL_INVALID_OPERATION` on
/// device, which is why the array format is matched to the source instead.
///
/// Fallback, taken when the source format differs from the array format, the probe is unavailable,
/// or the direct copy errors: **blit the source into a scratch texture in the array's format
/// first**, where `glBlitFramebuffer` converts and gives the same colours as the Multipass eye
/// blit, **then `glCopyImageSubData` the scratch into the array layer**, which is same-format,
/// exact, and works at layer > 0. It falls back further to the direct FBO blit only when
/// `glCopyImageSubData` or the scratch is unavailable, or the sizes differ, which means pre-3.2
/// devices, where the layer > 0 no-op still leaves a black right eye as before.
static LAYER_LOG: AtomicU32 = AtomicU32::new(0);
/// One-shot gate for the eye-source format probe: 0 means not yet probed, 1 means the probe ran and
/// the result is in [`PROBED_SRC_FMT`].
static PROBE_LOG: AtomicU32 = AtomicU32::new(0);
/// The probed source internal format, 0 until it is probed and when the probe is unavailable. The
/// direct same-format copies require it to equal the eye textures' `GL_RGB10_A2`.
static PROBED_SRC_FMT: AtomicU32 = AtomicU32::new(0);
/// Set after the direct same-format `glCopyImageSubData` into an array layer first fails, so later
/// frames skip the doomed attempt and go straight to the scratch fallback.
static DIRECT_COPY_BROKEN: AtomicBool = AtomicBool::new(false);

/// Probe the eye SubViewport texture's internal format once and return it; 0 means unknown, either
/// not yet probed successfully or `glGetTexLevelParameteriv` unavailable before GLES 3.1. It gates
/// the direct same-format copies in [`blit_texture`] and [`blit_texture_to_layer`], because GLES
/// restricts which format pairs the copy entry points may move between, so anything but an exact
/// match degrades to the converting-blit paths instead of scrambling.
unsafe fn probed_src_format(g: &Gl, src: u32) -> u32 {
    if PROBE_LOG
        .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        if let Some(get_tex_level_parameteriv) = g.get_tex_level_parameteriv {
            let mut prev_tex2d: i32 = 0;
            (g.get_integerv)(GL_TEXTURE_BINDING_2D, &mut prev_tex2d);
            let mut src_fmt: i32 = 0;
            (g.bind_texture)(GL_TEXTURE_2D, src);
            get_tex_level_parameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_INTERNAL_FORMAT, &mut src_fmt);
            (g.bind_texture)(GL_TEXTURE_2D, prev_tex2d as u32);
            let err = (g.get_error)();
            if err == 0 {
                PROBED_SRC_FMT.store(src_fmt as u32, Ordering::Relaxed);
            }
            godot::global::godot_print!(
                "[xreal] eye-src probe: src={src} internal_format={src_fmt:#x} gl_err={err} \
                 (direct copy {})",
                if src_fmt == GL_RGB10_A2 && err == 0 {
                    "enabled: matches eye-texture format"
                } else {
                    "disabled: eye textures are RGB10_A2, will convert via blit"
                }
            );
        } else {
            godot::global::godot_print!(
                "[xreal] eye-src probe: glGetTexLevelParameteriv unavailable (blit path)"
            );
        }
    }
    PROBED_SRC_FMT.load(Ordering::Relaxed)
}

pub fn blit_texture_to_layer(
    src: u32,
    src_w: i32,
    src_h: i32,
    dst_array: u32,
    layer: i32,
    dst_w: i32,
    dst_h: i32,
) {
    let Some(g) = gl() else { return };
    if src == 0 || dst_array == 0 {
        return;
    }
    unsafe {
        // Preferred path: with identical formats, as probed, ONE exact copy goes straight into the layer.
        if let Some(copy_image_sub_data) = g.copy_image_sub_data {
            if src_w == dst_w
                && src_h == dst_h
                && probed_src_format(g, src) == GL_RGB10_A2 as u32
                && !DIRECT_COPY_BROKEN.load(Ordering::Relaxed)
            {
                while (g.get_error)() != 0 {}
                copy_image_sub_data(
                    src,
                    GL_TEXTURE_2D,
                    0,
                    0,
                    0,
                    0,
                    dst_array,
                    GL_TEXTURE_2D_ARRAY,
                    0,
                    0,
                    0,
                    layer,
                    dst_w,
                    dst_h,
                    1,
                );
                let err = (g.get_error)();
                if LAYER_LOG.fetch_add(1, Ordering::Relaxed) < 8 {
                    godot::global::godot_print!(
                        "[xreal] direct_copy_to_layer dst={dst_array} layer={layer} src={src} \
                         {dst_w}x{dst_h}: gl_err={err}"
                    );
                }
                if err == 0 {
                    return;
                }
                // It failed, so remember that and fall through to the scratch two-step below.
                DIRECT_COPY_BROKEN.store(true, Ordering::Relaxed);
            }
        }

        // Fallback: format-converting blit into an array-format scratch, then exact copy into the
        // layer.
        if let Some(copy_image_sub_data) = g.copy_image_sub_data {
            if src_w == dst_w && src_h == dst_h {
                if let Some(temp) = temp_layer_tex(g, dst_w, dst_h) {
                    // Convert the source into the array-format scratch (same conversion as the
                    // Multipass eye blit).
                    blit_texture(src, src_w, src_h, temp, dst_w, dst_h);
                    while (g.get_error)() != 0 {}
                    copy_image_sub_data(
                        temp,
                        GL_TEXTURE_2D,
                        0,
                        0,
                        0,
                        0,
                        dst_array,
                        GL_TEXTURE_2D_ARRAY,
                        0,
                        0,
                        0,
                        layer,
                        dst_w,
                        dst_h,
                        1,
                    );
                    let err = (g.get_error)();
                    if LAYER_LOG.fetch_add(1, Ordering::Relaxed) < 8 {
                        godot::global::godot_print!(
                            "[xreal] copy_to_layer dst={dst_array} layer={layer} via temp={temp} {dst_w}x{dst_h}: gl_err={err}"
                        );
                    }
                    if err == 0 {
                        return;
                    }
                    // CopyImageSubData failed, which is unexpected, so fall through to the blit path below.
                }
            }
        }

        let mut prev_draw: i32 = 0;
        let mut prev_read: i32 = 0;
        (g.get_integerv)(GL_DRAW_FRAMEBUFFER_BINDING, &mut prev_draw);
        (g.get_integerv)(GL_READ_FRAMEBUFFER_BINDING, &mut prev_read);
        // glBlitFramebuffer is clipped by the scissor box. Godot can leave GL_SCISSOR_TEST enabled
        // with a box covering only part of the target, which would update only part of the layer, so
        // save the enable state here and disable it around the blit (same pattern as fill_texture).
        // The copy_image_sub_data paths above are scissor-immune and need no such guard.
        let scissor_was_on = (g.is_enabled)(GL_SCISSOR_TEST) != 0;

        let read_fbo = scratch_fbo(g, 1);
        let draw_fbo = scratch_fbo(g, 0);
        (g.bind_framebuffer)(GL_READ_FRAMEBUFFER, read_fbo);
        (g.framebuffer_texture_2d)(
            GL_READ_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_TEXTURE_2D,
            src,
            0,
        );
        (g.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, draw_fbo);
        (g.framebuffer_texture_layer)(
            GL_DRAW_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            dst_array,
            0,
            layer,
        );

        let read_ok = (g.check_framebuffer_status)(GL_READ_FRAMEBUFFER) == GL_FRAMEBUFFER_COMPLETE;
        let draw_ok = (g.check_framebuffer_status)(GL_DRAW_FRAMEBUFFER) == GL_FRAMEBUFFER_COMPLETE;
        if read_ok && draw_ok {
            if scissor_was_on {
                (g.disable)(GL_SCISSOR_TEST);
            }
            (g.blit_framebuffer)(
                0,
                0,
                src_w,
                src_h,
                0,
                0,
                dst_w,
                dst_h,
                GL_COLOR_BUFFER_BIT,
                GL_LINEAR as u32,
            );
            if scissor_was_on {
                (g.enable)(GL_SCISSOR_TEST);
            }
        }

        (g.framebuffer_texture_2d)(
            GL_READ_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_TEXTURE_2D,
            0,
            0,
        );
        (g.framebuffer_texture_layer)(GL_DRAW_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, 0, 0, 0);
        (g.bind_framebuffer)(GL_READ_FRAMEBUFFER, prev_read as u32);
        (g.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, prev_draw as u32);

        if LAYER_LOG.fetch_add(1, Ordering::Relaxed) < 8 {
            godot::global::godot_print!(
                "[xreal] blit_to_layer dst={dst_array} layer={layer} src={src}: read_ok={read_ok} draw_ok={draw_ok} scissor_was_on={scissor_was_on}"
            );
        }
    }
}

/// Delete a texture previously returned by [`alloc_texture`].
pub fn delete_texture(id: u32) {
    if id == 0 {
        return;
    }
    if let Some(g) = gl() {
        unsafe { (g.delete_textures)(1, &id) };
    }
}

/// Clear the given texture to a solid RGBA colour via the scratch framebuffer.
///
/// It started life as the bring-up validation fill, proving the XREAL compositor displays an
/// engine-owned texture at all, and now serves the frame-tick's last-resort branch, which clears
/// the eye textures to black before Godot has published a source size. It preserves the previously
/// bound draw framebuffer, the scissor-test enable and the clear colour, so Godot's own rendering
/// is left undisturbed.
static FILL_LOG_COUNT: AtomicU32 = AtomicU32::new(0);

pub fn fill_texture(tex: u32, r: f32, g_: f32, b: f32) {
    let Some(g) = gl() else { return };
    if tex == 0 {
        return;
    }
    unsafe {
        while (g.get_error)() != 0 {}
        let mut prev_draw_fbo: i32 = 0;
        (g.get_integerv)(GL_DRAW_FRAMEBUFFER_BINDING, &mut prev_draw_fbo);
        let scissor_was_on = (g.is_enabled)(GL_SCISSOR_TEST) != 0;

        let fbo = scratch_fbo(g, 0);
        (g.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, fbo);
        (g.framebuffer_texture_2d)(
            GL_DRAW_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_TEXTURE_2D,
            tex,
            0,
        );
        let status = (g.check_framebuffer_status)(GL_DRAW_FRAMEBUFFER);
        if status == GL_FRAMEBUFFER_COMPLETE {
            if scissor_was_on {
                (g.disable)(GL_SCISSOR_TEST);
            }
            // The clear colour is global state: leaving ours behind would tint whatever Godot
            // clears next with this diagnostic colour, so put the old one back afterwards.
            let mut prev_clear = [0.0_f32; 4];
            (g.get_floatv)(GL_COLOR_CLEAR_VALUE, prev_clear.as_mut_ptr());
            (g.clear_color)(r, g_, b, 1.0);
            (g.clear)(GL_COLOR_BUFFER_BIT);
            (g.clear_color)(prev_clear[0], prev_clear[1], prev_clear[2], prev_clear[3]);
            if scissor_was_on {
                (g.enable)(GL_SCISSOR_TEST);
            }
        }
        let gl_err = (g.get_error)();
        // Detach and restore the previous draw FBO.
        (g.framebuffer_texture_2d)(
            GL_DRAW_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_TEXTURE_2D,
            0,
            0,
        );
        (g.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, prev_draw_fbo as u32);

        if FILL_LOG_COUNT.fetch_add(1, Ordering::Relaxed) < 4 {
            godot::global::godot_print!(
                "[xreal] fill_texture tex={tex}: fbo_status={status:#x} complete={} \
                 prev_fbo={prev_draw_fbo} gl_err={gl_err}",
                status == GL_FRAMEBUFFER_COMPLETE
            );
        }
    }
}

/// Copy `src`, sized `src_w` by `src_h`, into `dst`, sized `dst_w` by `dst_h`, as a straight copy
/// with no Y-flip, since both share the GL bottom-left origin; see the body comment.
///
/// It fills a Multipass eye texture from Godot's rendered SubViewport each frame. Preferred path:
/// because [`alloc_texture`] allocates the eye texture in `GL_RGB10_A2`, the SubViewport's own
/// format, a same-size fill is ONE exact `glCopyImageSubData`, with no format conversion and no FBO
/// binds, completeness checks or state save and restore. It is gated on the same one-shot
/// source-format probe as [`blit_texture_to_layer`], and a format mismatch, a size mismatch or a
/// copy failure falls back to the converting `glBlitFramebuffer` below.
static BLIT2D_LOG: AtomicU32 = AtomicU32::new(0);
/// Set once the direct same-format 2D `glCopyImageSubData` first fails, so later frames skip the
/// doomed attempt and go straight to the blit fallback.
static DIRECT_COPY_2D_BROKEN: AtomicBool = AtomicBool::new(false);
pub fn blit_texture(src: u32, src_w: i32, src_h: i32, dst: u32, dst_w: i32, dst_h: i32) {
    let Some(g) = gl() else { return };
    if src == 0 || dst == 0 {
        return;
    }
    unsafe {
        // Preferred path: with identical formats, as probed, ONE exact copy runs with no FBO or state
        // churn.
        if let Some(copy_image_sub_data) = g.copy_image_sub_data {
            if src_w == dst_w
                && src_h == dst_h
                && probed_src_format(g, src) == GL_RGB10_A2 as u32
                && !DIRECT_COPY_2D_BROKEN.load(Ordering::Relaxed)
            {
                while (g.get_error)() != 0 {}
                copy_image_sub_data(
                    src,
                    GL_TEXTURE_2D,
                    0,
                    0,
                    0,
                    0,
                    dst,
                    GL_TEXTURE_2D,
                    0,
                    0,
                    0,
                    0,
                    dst_w,
                    dst_h,
                    1,
                );
                let err = (g.get_error)();
                if BLIT2D_LOG.fetch_add(1, Ordering::Relaxed) < 8 {
                    godot::global::godot_print!(
                        "[xreal] direct_copy_2d dst={dst} src={src} {dst_w}x{dst_h}: gl_err={err}"
                    );
                }
                if err == 0 {
                    return;
                }
                // It failed, so remember that and fall through to the blit below.
                DIRECT_COPY_2D_BROKEN.store(true, Ordering::Relaxed);
            }
        }

        let mut prev_draw: i32 = 0;
        let mut prev_read: i32 = 0;
        (g.get_integerv)(GL_DRAW_FRAMEBUFFER_BINDING, &mut prev_draw);
        (g.get_integerv)(GL_READ_FRAMEBUFFER_BINDING, &mut prev_read);
        // glBlitFramebuffer is clipped by the scissor box. Godot can leave GL_SCISSOR_TEST enabled
        // with a box covering only part of the target, which would update only part of the eye
        // texture, so save the enable state here and disable it around the blit (same pattern as
        // fill_texture). The copy_image_sub_data path above is scissor-immune and needs no guard.
        let scissor_was_on = (g.is_enabled)(GL_SCISSOR_TEST) != 0;

        let read_fbo = scratch_fbo(g, 1);
        let draw_fbo = scratch_fbo(g, 0);
        (g.bind_framebuffer)(GL_READ_FRAMEBUFFER, read_fbo);
        (g.framebuffer_texture_2d)(
            GL_READ_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_TEXTURE_2D,
            src,
            0,
        );
        (g.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, draw_fbo);
        (g.framebuffer_texture_2d)(
            GL_DRAW_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_TEXTURE_2D,
            dst,
            0,
        );

        let read_ok = (g.check_framebuffer_status)(GL_READ_FRAMEBUFFER) == GL_FRAMEBUFFER_COMPLETE;
        let draw_ok = (g.check_framebuffer_status)(GL_DRAW_FRAMEBUFFER) == GL_FRAMEBUFFER_COMPLETE;
        if read_ok && draw_ok {
            if scissor_was_on {
                (g.disable)(GL_SCISSOR_TEST);
            }
            // Straight copy (no Y-flip): the SubViewport render target and the eye texture share
            // GL bottom-left origin, matching blit_default_framebuffer (flipping showed upside-down).
            (g.blit_framebuffer)(
                0,
                0,
                src_w,
                src_h,
                0,
                0,
                dst_w,
                dst_h,
                GL_COLOR_BUFFER_BIT,
                GL_LINEAR as u32,
            );
            if scissor_was_on {
                (g.enable)(GL_SCISSOR_TEST);
            }
        }

        if BLIT2D_LOG.fetch_add(1, Ordering::Relaxed) < 8 {
            godot::global::godot_print!(
                "[xreal] blit_2d dst={dst} src={src} {dst_w}x{dst_h}: read_ok={read_ok} \
                 draw_ok={draw_ok} scissor_was_on={scissor_was_on}"
            );
        }

        (g.framebuffer_texture_2d)(
            GL_READ_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_TEXTURE_2D,
            0,
            0,
        );
        (g.framebuffer_texture_2d)(
            GL_DRAW_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_TEXTURE_2D,
            0,
            0,
        );
        (g.bind_framebuffer)(GL_READ_FRAMEBUFFER, prev_read as u32);
        (g.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, prev_draw as u32);
    }
}

/// Blit Godot's just-rendered window content, the default framebuffer or back buffer, fbo 0, into
/// an eye texture. Godot's root viewport renders direct to screen, so it has no sampleable
/// offscreen texture and `texture_get_native_handle` returns 0; reading fbo 0 gets its pixels
/// instead. It is a straight copy, with no Y-flip.
pub fn blit_default_framebuffer(dst: u32, src_w: i32, src_h: i32, dst_w: i32, dst_h: i32) {
    let Some(g) = gl() else { return };
    if dst == 0 {
        return;
    }
    unsafe {
        let mut prev_draw: i32 = 0;
        let mut prev_read: i32 = 0;
        (g.get_integerv)(GL_DRAW_FRAMEBUFFER_BINDING, &mut prev_draw);
        (g.get_integerv)(GL_READ_FRAMEBUFFER_BINDING, &mut prev_read);
        // glBlitFramebuffer is clipped by the scissor box. Godot can leave GL_SCISSOR_TEST enabled
        // with a box covering only part of the target, which would update only part of the eye
        // texture, so save the enable state here and disable it around the blit (same pattern as
        // fill_texture).
        let scissor_was_on = (g.is_enabled)(GL_SCISSOR_TEST) != 0;

        (g.bind_framebuffer)(GL_READ_FRAMEBUFFER, 0); // default framebuffer = window back buffer
        let draw_fbo = scratch_fbo(g, 0);
        (g.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, draw_fbo);
        (g.framebuffer_texture_2d)(
            GL_DRAW_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_TEXTURE_2D,
            dst,
            0,
        );

        if (g.check_framebuffer_status)(GL_DRAW_FRAMEBUFFER) == GL_FRAMEBUFFER_COMPLETE {
            if scissor_was_on {
                (g.disable)(GL_SCISSOR_TEST);
            }
            // Straight copy (no Y-flip): fbo 0 and the eye texture share GL bottom-left origin, so
            // flipping made it upside-down on the glasses.
            (g.blit_framebuffer)(
                0,
                0,
                src_w,
                src_h,
                0,
                0,
                dst_w,
                dst_h,
                GL_COLOR_BUFFER_BIT,
                GL_LINEAR as u32,
            );
            if scissor_was_on {
                (g.enable)(GL_SCISSOR_TEST);
            }
        }

        (g.framebuffer_texture_2d)(
            GL_DRAW_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_TEXTURE_2D,
            0,
            0,
        );
        (g.bind_framebuffer)(GL_READ_FRAMEBUFFER, prev_read as u32);
        (g.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, prev_draw as u32);
    }
}
