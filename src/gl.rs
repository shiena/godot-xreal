//! Minimal GLES3 binding for the XREAL display path.
//!
//! `libXREALXRPlugin.so`'s display provider (see `docs/plans/frame-submission-plan.md`) asks the
//! engine to allocate its render textures through `IUnityXRDisplay::CreateTexture`. This module
//! is that engine side: it `dlopen`s `libGLESv3.so` and exposes just enough GL to allocate a
//! texture and copy pixels into it.
//!
//! **All functions here must be called on Godot's rendering thread** (via
//! `RenderingServer::call_on_render_thread`), where an EGL context is current. There is no EGL
//! context on the main thread, so `glGenTextures`/`glClear` there would be a no-op or crash.
//!
//! On desktop the `dlopen` fails and every entry point returns `None`/does nothing, matching the
//! rest of the crate's "native libs absent → no-op" behaviour.

// GL helpers: some entry points (e.g. delete_texture) are retained for completeness/unused, and on
// desktop every entry point is a dummy no-op. Allow dead code on both targets.
#![allow(dead_code)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
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
// glTexStorage3D(target, levels, internalformat, width, height, depth) — immutable-storage allocation.
type FnTexStorage3D = unsafe extern "C" fn(u32, i32, u32, i32, i32, i32);
type FnFramebufferTextureLayer = unsafe extern "C" fn(u32, u32, u32, i32, i32);
// glCopyImageSubData(srcName, srcTarget, srcLevel, srcX,srcY,srcZ, dstName, dstTarget, dstLevel,
// dstX,dstY,dstZ, srcW,srcH,srcD) — GLES 3.2 direct texel copy (writes any array layer, no FBO/blit).
type FnCopyImageSubData =
    unsafe extern "C" fn(u32, u32, i32, i32, i32, i32, u32, u32, i32, i32, i32, i32, i32, i32, i32);
type FnGetIntegerv = unsafe extern "C" fn(u32, *mut i32);
type FnIsEnabled = unsafe extern "C" fn(u32) -> u8;
type FnEnable = unsafe extern "C" fn(u32);
type FnDisable = unsafe extern "C" fn(u32);

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
    framebuffer_texture_layer: FnFramebufferTextureLayer,
    get_integerv: FnGetIntegerv,
    is_enabled: FnIsEnabled,
    enable: FnEnable,
    disable: FnDisable,
    _lib: Library,
}

impl Gl {
    fn load() -> Result<Self, String> {
        unsafe {
            let lib = Library::new(GLES_LIB).map_err(|e| format!("dlopen {GLES_LIB}: {e}"))?;
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
                framebuffer_texture_layer: sym!(
                    "glFramebufferTextureLayer",
                    FnFramebufferTextureLayer
                ),
                get_integerv: sym!("glGetIntegerv", FnGetIntegerv),
                is_enabled: sym!("glIsEnabled", FnIsEnabled),
                enable: sym!("glEnable", FnEnable),
                disable: sym!("glDisable", FnDisable),
                _lib: lib,
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

/// Scratch framebuffers reused for every `fill`/`blit`, created lazily on the render thread so no
/// FBO name is generated or deleted per frame. Index 0 = draw/fill target, index 1 = blit source.
static SCRATCH_FBO: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

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

/// Allocate a 2D RGBA8 texture of the given size and return its GL name (`None` on failure).
///
/// `_srgb` is intentionally ignored: `GL_RGBA8` (UNORM) is the **correct** eye-texture format for this
/// port, confirmed on device 2026-07-17. Godot's `gl_compatibility` renderer outputs display-ready,
/// sRGB-encoded bytes, and the XREAL compositor passthrough-samples the eye texture and writes the
/// sampled value to the display without re-encoding. An A/B test allocating the eye texture as
/// `GL_SRGB8_ALPHA8` (same bytes, sRGB-typed) came out ~26% too dark — the compositor applies a
/// sample-time sRGB→linear decode — proving UNORM `GL_RGBA8` is right here. (Unity's port uses an
/// sRGB-typed target because it renders in *linear* space; our display-ready bytes must not be
/// decoded.) See `docs/archive/multiview-investigation.md` (2026-07-17 color-space test).
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
            GL_RGBA8,
            width,
            height,
            0,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
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

/// Allocate a `GL_TEXTURE_2D_ARRAY` with `layers` layers (RGBA8), for the SDK's Multiview /
/// Single-Pass-Instanced path (`CreateTexture` with `textureArrayLength == 2`). The compositor
/// binds this as a layered multiview framebuffer; a plain 2D texture there yields
/// `GL_INVALID_FRAMEBUFFER_OPERATION` (black). Returns the GL name (`None` on failure).
///
/// **Immutable storage.** The array is allocated with `glTexStorage3D` (immutable) when available,
/// falling back to mutable `glTexImage3D` otherwise — mirroring Unity's `ApiGLES::CreateTexture`,
/// which takes `glTexStorage3DEXT` for a `Tex2DArray` when the driver supports immutable storage
/// (the Adreno 710 does) and only uses `glTexImage3D` as a fallback.
///
/// NOTE: this matching-Unity change was an *experiment* to fix Multiview's black right eye (the
/// theory: libnr_api imports the array via per-layer 2D `glTextureView`s, which need immutable
/// storage). It was **tested on device 2026-07-17 and did NOT fix the right eye** — immutable
/// allocation succeeds (`immutable=true`) and layer 1 fills, but the compositor still presents black
/// on the right (screencap right stddev=0.0). So immutable is not the blocker; the wall is inside
/// libnr_api. The change is kept dormant (Multiview is opt-in and shelved) as a faithful Unity match.
/// See `docs/archive/multiview-investigation.md` (2026-07-17). The mutable path is the fallback for GL
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

        // Prefer immutable storage (matches Unity). A single mip level; pin BASE/MAX level so the
        // texture is mip-complete for whatever sampler state the compositor binds it with.
        let immutable = match g.tex_storage_3d {
            Some(tex_storage_3d) => {
                (g.tex_parameteri)(GL_TEXTURE_2D_ARRAY, GL_TEXTURE_BASE_LEVEL, 0);
                (g.tex_parameteri)(GL_TEXTURE_2D_ARRAY, GL_TEXTURE_MAX_LEVEL, 0);
                tex_storage_3d(
                    GL_TEXTURE_2D_ARRAY,
                    1,
                    GL_RGBA8 as u32,
                    width,
                    height,
                    layers,
                );
                if (g.get_error)() == 0 {
                    true
                } else {
                    // Immutable allocation failed (e.g. format/driver quirk): drain the error and
                    // retry mutable on the same, still-mutable texture object.
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
                GL_RGBA8,
                width,
                height,
                layers,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
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

/// A persistent RGBA8 2D scratch texture used to normalise the eye SubViewport's format before
/// copying it into an array layer (see [`blit_texture_to_layer`]). Created lazily at eye size.
static TEMP_LAYER_TEX: AtomicU32 = AtomicU32::new(0);

/// Get (create once) the RGBA8 scratch texture at `w`×`h`. Assumes a stable eye size.
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
        GL_RGBA8,
        w,
        h,
        0,
        GL_RGBA,
        GL_UNSIGNED_BYTE,
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

/// Copy 2D `src` into a single `layer` of a `GL_TEXTURE_2D_ARRAY` (`dst_array`). Used to fill the
/// per-eye layers of the Multiview swapchain texture.
///
/// Two GL quirks force a two-step path on this hardware:
///   1. `glBlitFramebuffer` straight into a `glFramebufferTextureLayer` attachment at **layer > 0 is a
///      silent no-op on the Adreno GLES driver** (returns a complete framebuffer, writes nothing) —
///      the true cause of the black Multiview right eye (layer 1). `glClear` there *does* work, so the
///      NR compositor was never the problem.
///   2. `glCopyImageSubData` **can** write layer > 0, but it is a raw byte copy with no format
///      conversion — copying the eye SubViewport (whose GL format is not plain `RGBA8`) directly into
///      the `RGBA8` array scrambles the colours (Multiview looked colour-corrupted vs Multipass).
///
/// So: **blit the source into an `RGBA8` scratch texture first** (`glBlitFramebuffer` converts the
/// format exactly as the Multipass eye blit does, giving matching colours), **then
/// `glCopyImageSubData` the scratch into the array layer** (`RGBA8`→`RGBA8`, exact, and layer > 0
/// works). Falls back to the direct FBO blit only if `glCopyImageSubData`/scratch is unavailable or
/// the sizes differ (pre-3.2 devices).
static LAYER_LOG: AtomicU32 = AtomicU32::new(0);
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
        // Preferred path: format-converting blit into an RGBA8 scratch, then exact copy into the layer.
        if let Some(copy_image_sub_data) = g.copy_image_sub_data {
            if src_w == dst_w && src_h == dst_h {
                if let Some(temp) = temp_layer_tex(g, dst_w, dst_h) {
                    // Convert the source into the RGBA8 scratch (identical to the Multipass eye blit).
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
                    // CopyImageSubData failed (unexpected) — fall through to the blit path below.
                }
            }
        }

        let mut prev_draw: i32 = 0;
        let mut prev_read: i32 = 0;
        (g.get_integerv)(GL_DRAW_FRAMEBUFFER_BINDING, &mut prev_draw);
        (g.get_integerv)(GL_READ_FRAMEBUFFER_BINDING, &mut prev_read);

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
                "[xreal] blit_to_layer dst={dst_array} layer={layer} src={src}: read_ok={read_ok} draw_ok={draw_ok}"
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
/// This is the option-(a) validation fill: proving the XREAL compositor displays an
/// engine-owned texture at all. Preserves the previously bound draw framebuffer and the
/// scissor-test enable so Godot's own rendering is left undisturbed.
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
            (g.clear_color)(r, g_, b, 1.0);
            (g.clear)(GL_COLOR_BUFFER_BIT);
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

/// Blit `src` (size `src_w`×`src_h`) into `dst` (size `dst_w`×`dst_h`) as a straight copy (no Y-flip;
/// both share GL bottom-left origin — see the body comment).
///
/// Blits Godot's rendered viewport color into an eye texture each frame.
pub fn blit_texture(src: u32, src_w: i32, src_h: i32, dst: u32, dst_w: i32, dst_h: i32) {
    let Some(g) = gl() else { return };
    if src == 0 || dst == 0 {
        return;
    }
    unsafe {
        let mut prev_draw: i32 = 0;
        let mut prev_read: i32 = 0;
        (g.get_integerv)(GL_DRAW_FRAMEBUFFER_BINDING, &mut prev_draw);
        (g.get_integerv)(GL_READ_FRAMEBUFFER_BINDING, &mut prev_read);

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

/// Blit Godot's just-rendered window content (the default framebuffer / back buffer, fbo 0) into an
/// eye texture. Godot's root viewport renders direct-to-screen, so it has no sampleable offscreen
/// texture (`texture_get_native_handle` returns 0); reading fbo 0 gets its pixels instead. Straight copy, no Y-flip.
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
