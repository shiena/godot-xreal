//! Stage-0 probe for the Vulkan rendering path: can this device share a self-allocated
//! `AHardwareBuffer` between a renderer and the GL compositor path?
//!
//! The planned Vulkan bridge (see `docs/plans/vulkan-path-plan.md`) is: allocate an RGBA8
//! AHardwareBuffer per eye, bind it as the Vulkan render target, import the same AHB as an
//! EGLImage-backed `GL_TEXTURE_2D`, and hand that GL name to the SDK compositor exactly as today.
//! Everything hinges on the gralloc allocating RGBA8 with
//! `GPU_COLOR_OUTPUT | GPU_SAMPLED_IMAGE` and the GLES driver accepting the imported image as
//! both an FBO color attachment (render into it) and a texture source (sample out of it). This
//! module proves or kills exactly that, on the CURRENT GL build, with no Vulkan anywhere:
//!
//! 1. `AHardwareBuffer_isSupported` / `allocate`: RGBA8, eye-buffer size (1968x1134), usage
//!    `GPU_COLOR_OUTPUT | GPU_SAMPLED_IMAGE` (this usage combo was never probed; the camera-era
//!    probe used `CPU_WRITE_OFTEN | GPU_SAMPLED_IMAGE`).
//! 2. `eglGetNativeClientBufferANDROID` -> `eglCreateImageKHR` -> `glEGLImageTargetTexture2DOES`
//!    onto a fresh `GL_TEXTURE_2D` (revived from the reverted camera PoC, commit c5b9a67).
//! 3. Attach to an FBO, clear to a known color, `glReadPixels` it back (GPU_COLOR_OUTPUT works).
//! 4. Blit from that FBO into a plain RGBA8 texture and read THAT back (the texture's contents
//!    are GPU-readable through the normal texture path; the sampled-image proxy short of a full
//!    shader draw -- the definitive sampling test is handing the texture to the SDK compositor,
//!    which is a follow-up once this probe reads GO).
//!
//! Runs ONCE, a few seconds in, on the render/GL thread (primary trigger:
//! `unity_plugin::run_frame_tick` frame 120; fallback without a live session:
//! `XrealHeadTracker::process` frame 600 via `call_on_render_thread`). Every resource is released
//! and every touched piece of GL state restored, so the frame it runs in renders normally.
//! Kill-switch: `adb shell setprop debug.xreal.ahb_probe 0` skips it entirely. Results go to the
//! log as `[xreal] ahb_probe: ...` lines plus one final `VERDICT: GO / NO-GO` line.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use libloading::Library;

use crate::session::android_prop_i32;

/// One Pro eye-buffer size; the exact allocation the Vulkan path would make per eye.
const PROBE_W: i32 = 1968;
const PROBE_H: i32 = 1134;
/// `run_frame_tick` frame the probe fires on (~2 s at 60 Hz: session + first frames well settled).
const PROBE_FRAME: u64 = 120;

// <android/hardware_buffer.h>
const AHB_FORMAT_R8G8B8A8_UNORM: u32 = 1;
const AHB_USAGE_GPU_SAMPLED_IMAGE: u64 = 0x100;
const AHB_USAGE_GPU_COLOR_OUTPUT: u64 = 0x200;
// <EGL/egl.h> / <EGL/eglext.h>
const EGL_NONE: i32 = 0x3038;
const EGL_IMAGE_PRESERVED_KHR: i32 = 0x30D2;
const EGL_NATIVE_BUFFER_ANDROID: u32 = 0x3140;
// GLES
const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE_BINDING_2D: u32 = 0x8069;
const GL_RGBA: u32 = 0x1908;
const GL_RGBA8: i32 = 0x8058;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_LINEAR: i32 = 0x2601;
const GL_CLAMP_TO_EDGE: i32 = 0x812F;
const GL_READ_FRAMEBUFFER: u32 = 0x8CA8;
const GL_DRAW_FRAMEBUFFER: u32 = 0x8CA9;
const GL_READ_FRAMEBUFFER_BINDING: u32 = 0x8CAA;
const GL_DRAW_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
const GL_COLOR_BUFFER_BIT: u32 = 0x0000_4000;
const GL_SCISSOR_TEST: u32 = 0x0C11;
const GL_COLOR_CLEAR_VALUE: u32 = 0x0C22;

/// The clear color rendered into the AHB, as float clear + expected readback bytes.
const CLEAR_RGBA: [f32; 4] = [0.2, 0.4, 0.8, 1.0];
const EXPECT_RGBA: [u8; 4] = [51, 102, 204, 255];
/// Per-channel tolerance on readback (rounding differences between clear and UNORM storage).
const TOLERANCE: u8 = 2;

#[repr(C)]
struct AHardwareBufferDesc {
    width: u32,
    height: u32,
    layers: u32,
    format: u32,
    usage: u64,
    stride: u32,
    rfu0: u32,
    rfu1: u64,
}

type FnAhbIsSupported = unsafe extern "C" fn(*const AHardwareBufferDesc) -> i32;
type FnAhbAllocate = unsafe extern "C" fn(*const AHardwareBufferDesc, *mut *mut c_void) -> i32;
type FnAhbDescribe = unsafe extern "C" fn(*mut c_void, *mut AHardwareBufferDesc);
type FnAhbRelease = unsafe extern "C" fn(*mut c_void);

type FnEglGetCurrentDisplay = unsafe extern "C" fn() -> *mut c_void;
type FnEglGetCurrentContext = unsafe extern "C" fn() -> *mut c_void;
type FnEglGetError = unsafe extern "C" fn() -> u32;
type FnEglGetProcAddress = unsafe extern "C" fn(*const u8) -> *mut c_void;
type FnEglCreateImageKHR =
    unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void, *const i32) -> *mut c_void;
type FnEglDestroyImageKHR = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;
type FnEglGetNativeClientBuffer = unsafe extern "C" fn(*const c_void) -> *mut c_void;
type FnGlEglImageTargetTexture2DOES = unsafe extern "C" fn(u32, *mut c_void);

type FnGenTextures = unsafe extern "C" fn(i32, *mut u32);
type FnDeleteTextures = unsafe extern "C" fn(i32, *const u32);
type FnBindTexture = unsafe extern "C" fn(u32, u32);
type FnTexParameteri = unsafe extern "C" fn(u32, u32, i32);
type FnTexImage2D = unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
type FnGenFramebuffers = unsafe extern "C" fn(i32, *mut u32);
type FnDeleteFramebuffers = unsafe extern "C" fn(i32, *const u32);
type FnBindFramebuffer = unsafe extern "C" fn(u32, u32);
type FnFramebufferTexture2D = unsafe extern "C" fn(u32, u32, u32, u32, i32);
type FnCheckFramebufferStatus = unsafe extern "C" fn(u32) -> u32;
type FnBlitFramebuffer = unsafe extern "C" fn(i32, i32, i32, i32, i32, i32, i32, i32, u32, u32);
type FnClearColor = unsafe extern "C" fn(f32, f32, f32, f32);
type FnClear = unsafe extern "C" fn(u32);
type FnReadPixels = unsafe extern "C" fn(i32, i32, i32, i32, u32, u32, *mut c_void);
type FnGetError = unsafe extern "C" fn() -> u32;
type FnGetIntegerv = unsafe extern "C" fn(u32, *mut i32);
type FnGetFloatv = unsafe extern "C" fn(u32, *mut f32);
type FnIsEnabled = unsafe extern "C" fn(u32) -> u8;
type FnEnable = unsafe extern "C" fn(u32);
type FnDisable = unsafe extern "C" fn(u32);

/// Everything the probe dlsyms, self-contained so nothing in `gl.rs` / `native.rs` grows
/// probe-only surface. The `Library` handles keep the .so mapped for the process lifetime
/// (they are opened once, inside the one-shot).
struct Api {
    ahb_is_supported: Option<FnAhbIsSupported>,
    ahb_allocate: FnAhbAllocate,
    ahb_describe: Option<FnAhbDescribe>,
    ahb_release: FnAhbRelease,
    egl_get_current_display: FnEglGetCurrentDisplay,
    egl_get_current_context: FnEglGetCurrentContext,
    egl_get_error: FnEglGetError,
    egl_create_image: FnEglCreateImageKHR,
    egl_destroy_image: FnEglDestroyImageKHR,
    egl_get_native_client_buffer: FnEglGetNativeClientBuffer,
    gl_egl_image_target_texture_2d: FnGlEglImageTargetTexture2DOES,
    gen_textures: FnGenTextures,
    delete_textures: FnDeleteTextures,
    bind_texture: FnBindTexture,
    tex_parameteri: FnTexParameteri,
    tex_image_2d: FnTexImage2D,
    gen_framebuffers: FnGenFramebuffers,
    delete_framebuffers: FnDeleteFramebuffers,
    bind_framebuffer: FnBindFramebuffer,
    framebuffer_texture_2d: FnFramebufferTexture2D,
    check_framebuffer_status: FnCheckFramebufferStatus,
    blit_framebuffer: FnBlitFramebuffer,
    clear_color: FnClearColor,
    clear: FnClear,
    read_pixels: FnReadPixels,
    get_error: FnGetError,
    get_integerv: FnGetIntegerv,
    get_floatv: FnGetFloatv,
    is_enabled: FnIsEnabled,
    enable: FnEnable,
    disable: FnDisable,
    _libs: Vec<Library>,
}

impl Api {
    fn load() -> Result<Self, String> {
        let gles = unsafe { Library::new("libGLESv3.so") }
            .map_err(|e| format!("dlopen libGLESv3.so: {e}"))?;
        let egl =
            unsafe { Library::new("libEGL.so") }.map_err(|e| format!("dlopen libEGL.so: {e}"))?;
        let android = unsafe { Library::new("libandroid.so") }
            .map_err(|e| format!("dlopen libandroid.so: {e}"))?;

        macro_rules! sym {
            ($lib:expr, $name:literal, $ty:ty) => {
                unsafe {
                    *$lib
                        .get::<$ty>(concat!($name, "\0").as_bytes())
                        .map_err(|e| format!("dlsym {}: {e}", $name))?
                }
            };
        }
        macro_rules! sym_opt {
            ($lib:expr, $name:literal, $ty:ty) => {
                unsafe {
                    $lib.get::<$ty>(concat!($name, "\0").as_bytes())
                        .map(|s| *s)
                        .ok()
                }
            };
        }

        let egl_get_proc_address: FnEglGetProcAddress =
            sym!(egl, "eglGetProcAddress", FnEglGetProcAddress);
        // Extension entry points: try a direct dlsym first, then eglGetProcAddress, the spec route.
        macro_rules! ext_sym {
            ($lib:expr, $name:literal, $ty:ty) => {{
                let direct: Option<$ty> = sym_opt!($lib, $name, $ty);
                match direct {
                    Some(f) => f,
                    None => {
                        let p = unsafe {
                            egl_get_proc_address(concat!($name, "\0").as_bytes().as_ptr())
                        };
                        if p.is_null() {
                            return Err(format!(
                                "{} unavailable (dlsym + eglGetProcAddress)",
                                $name
                            ));
                        }
                        unsafe { std::mem::transmute::<*mut c_void, $ty>(p) }
                    }
                }
            }};
        }

        Ok(Api {
            ahb_is_supported: sym_opt!(android, "AHardwareBuffer_isSupported", FnAhbIsSupported),
            ahb_allocate: sym!(android, "AHardwareBuffer_allocate", FnAhbAllocate),
            ahb_describe: sym_opt!(android, "AHardwareBuffer_describe", FnAhbDescribe),
            ahb_release: sym!(android, "AHardwareBuffer_release", FnAhbRelease),
            egl_get_current_display: sym!(egl, "eglGetCurrentDisplay", FnEglGetCurrentDisplay),
            egl_get_current_context: sym!(egl, "eglGetCurrentContext", FnEglGetCurrentContext),
            egl_get_error: sym!(egl, "eglGetError", FnEglGetError),
            egl_create_image: sym!(egl, "eglCreateImageKHR", FnEglCreateImageKHR),
            egl_destroy_image: sym!(egl, "eglDestroyImageKHR", FnEglDestroyImageKHR),
            egl_get_native_client_buffer: ext_sym!(
                egl,
                "eglGetNativeClientBufferANDROID",
                FnEglGetNativeClientBuffer
            ),
            gl_egl_image_target_texture_2d: ext_sym!(
                gles,
                "glEGLImageTargetTexture2DOES",
                FnGlEglImageTargetTexture2DOES
            ),
            gen_textures: sym!(gles, "glGenTextures", FnGenTextures),
            delete_textures: sym!(gles, "glDeleteTextures", FnDeleteTextures),
            bind_texture: sym!(gles, "glBindTexture", FnBindTexture),
            tex_parameteri: sym!(gles, "glTexParameteri", FnTexParameteri),
            tex_image_2d: sym!(gles, "glTexImage2D", FnTexImage2D),
            gen_framebuffers: sym!(gles, "glGenFramebuffers", FnGenFramebuffers),
            delete_framebuffers: sym!(gles, "glDeleteFramebuffers", FnDeleteFramebuffers),
            bind_framebuffer: sym!(gles, "glBindFramebuffer", FnBindFramebuffer),
            framebuffer_texture_2d: sym!(gles, "glFramebufferTexture2D", FnFramebufferTexture2D),
            check_framebuffer_status: sym!(
                gles,
                "glCheckFramebufferStatus",
                FnCheckFramebufferStatus
            ),
            blit_framebuffer: sym!(gles, "glBlitFramebuffer", FnBlitFramebuffer),
            clear_color: sym!(gles, "glClearColor", FnClearColor),
            clear: sym!(gles, "glClear", FnClear),
            read_pixels: sym!(gles, "glReadPixels", FnReadPixels),
            get_error: sym!(gles, "glGetError", FnGetError),
            get_integerv: sym!(gles, "glGetIntegerv", FnGetIntegerv),
            get_floatv: sym!(gles, "glGetFloatv", FnGetFloatv),
            is_enabled: sym!(gles, "glIsEnabled", FnIsEnabled),
            enable: sym!(gles, "glEnable", FnEnable),
            disable: sym!(gles, "glDisable", FnDisable),
            _libs: vec![gles, egl, android],
        })
    }
}

static RAN: AtomicBool = AtomicBool::new(false);

/// Per-frame trigger from `run_frame_tick` (render thread). Fires the probe once at
/// [`PROBE_FRAME`].
pub fn maybe_run(frame: u64) {
    if frame == PROBE_FRAME {
        run_once();
    }
}

/// Run the probe if it has not run yet. MUST be called on the render/GL thread (the probe
/// verifies the EGL context itself and reports rather than crashes when it is absent).
/// `debug.xreal.ahb_probe 0` disables it.
pub fn run_once() {
    if android_prop_i32(b"debug.xreal.ahb_probe\0") == Some(0) {
        return;
    }
    if RAN.swap(true, Ordering::Relaxed) {
        return;
    }
    let api = match Api::load() {
        Ok(api) => api,
        Err(e) => {
            // Desktop (no libandroid/libEGL) lands here; one quiet line, not a warning.
            godot::global::godot_print!("[xreal] ahb_probe: skipped ({e})");
            return;
        }
    };
    let verdict = unsafe { probe(&api) };
    match verdict {
        Ok(detail) => godot::global::godot_print!(
            "[xreal] ahb_probe VERDICT: GO - RGBA8 AHB renders + reads back through GL ({detail}); \
             the Vulkan->GL eye-buffer bridge is viable, proceed to stage 1"
        ),
        Err(e) => godot::global::godot_print!(
            "[xreal] ahb_probe VERDICT: NO-GO - {e}; the Vulkan plan's stage-0 gate failed, \
             see docs/plans/vulkan-path-plan.md"
        ),
    }
}

/// The probe body. Returns `Ok(summary)` on GO. Restores all touched GL state and frees every
/// resource on every path.
unsafe fn probe(api: &Api) -> Result<String, String> {
    // -- context sanity ---------------------------------------------------------------------
    let display = (api.egl_get_current_display)();
    let context = (api.egl_get_current_context)();
    if display.is_null() || context.is_null() {
        return Err("no current EGL display/context (not on the render thread?)".into());
    }

    // -- 1) gralloc: isSupported + allocate -------------------------------------------------
    let desc = AHardwareBufferDesc {
        width: PROBE_W as u32,
        height: PROBE_H as u32,
        layers: 1,
        format: AHB_FORMAT_R8G8B8A8_UNORM,
        usage: AHB_USAGE_GPU_COLOR_OUTPUT | AHB_USAGE_GPU_SAMPLED_IMAGE,
        stride: 0,
        rfu0: 0,
        rfu1: 0,
    };
    let supported = api.ahb_is_supported.map(|f| f(&desc));
    godot::global::godot_print!(
        "[xreal] ahb_probe: isSupported(RGBA8 {PROBE_W}x{PROBE_H} GPU_COLOR_OUTPUT|GPU_SAMPLED_IMAGE) \
         = {supported:?}"
    );

    let mut ahb: *mut c_void = std::ptr::null_mut();
    let status = (api.ahb_allocate)(&desc, &mut ahb);
    if status != 0 || ahb.is_null() {
        return Err(format!("AHardwareBuffer_allocate -> {status}"));
    }
    let stride = api.ahb_describe.map(|f| {
        let mut got = AHardwareBufferDesc {
            width: 0,
            height: 0,
            layers: 0,
            format: 0,
            usage: 0,
            stride: 0,
            rfu0: 0,
            rfu1: 0,
        };
        f(ahb, &mut got);
        got.stride
    });
    godot::global::godot_print!(
        "[xreal] ahb_probe: allocate ok, allocator row stride = {stride:?} px"
    );

    // Everything below touches GL state; snapshot what we clobber and restore it in `cleanup`,
    // which also owns releasing the graphics objects, so error paths can simply `?` out.
    let mut prev_draw_fbo: i32 = 0;
    let mut prev_read_fbo: i32 = 0;
    let mut prev_tex: i32 = 0;
    let mut prev_clear: [f32; 4] = [0.0; 4];
    (api.get_integerv)(GL_DRAW_FRAMEBUFFER_BINDING, &mut prev_draw_fbo);
    (api.get_integerv)(GL_READ_FRAMEBUFFER_BINDING, &mut prev_read_fbo);
    (api.get_integerv)(GL_TEXTURE_BINDING_2D, &mut prev_tex);
    (api.get_floatv)(GL_COLOR_CLEAR_VALUE, prev_clear.as_mut_ptr());
    let scissor_was_on = (api.is_enabled)(GL_SCISSOR_TEST) != 0;

    let mut image: *mut c_void = std::ptr::null_mut();
    let mut textures = [0u32; 2]; // [ahb-backed, plain dst]
    let mut fbos = [0u32; 2]; // [ahb fbo, dst fbo]

    let cleanup = |image: *mut c_void, textures: &[u32; 2], fbos: &[u32; 2]| {
        (api.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, prev_draw_fbo as u32);
        (api.bind_framebuffer)(GL_READ_FRAMEBUFFER, prev_read_fbo as u32);
        (api.bind_texture)(GL_TEXTURE_2D, prev_tex as u32);
        (api.clear_color)(prev_clear[0], prev_clear[1], prev_clear[2], prev_clear[3]);
        if scissor_was_on {
            (api.enable)(GL_SCISSOR_TEST);
        } else {
            (api.disable)(GL_SCISSOR_TEST);
        }
        if fbos.iter().any(|&f| f != 0) {
            (api.delete_framebuffers)(2, fbos.as_ptr());
        }
        if textures.iter().any(|&t| t != 0) {
            (api.delete_textures)(2, textures.as_ptr());
        }
        if !image.is_null() {
            (api.egl_destroy_image)(display, image);
        }
        (api.ahb_release)(ahb);
        while (api.get_error)() != 0 {}
    };
    // Error helper: release everything, then fail.
    macro_rules! bail {
        ($($arg:tt)*) => {{
            cleanup(image, &textures, &fbos);
            return Err(format!($($arg)*));
        }};
    }

    // -- 2) AHB -> EGLImage -> GL_TEXTURE_2D ------------------------------------------------
    let client_buf = (api.egl_get_native_client_buffer)(ahb);
    if client_buf.is_null() {
        bail!("eglGetNativeClientBufferANDROID -> null");
    }
    let attrs: [i32; 3] = [EGL_IMAGE_PRESERVED_KHR, 1, EGL_NONE];
    image = (api.egl_create_image)(
        display,
        std::ptr::null_mut(), // EGL_NO_CONTEXT, required for EGL_NATIVE_BUFFER_ANDROID
        EGL_NATIVE_BUFFER_ANDROID,
        client_buf,
        attrs.as_ptr(),
    );
    if image.is_null() {
        let egl_err = (api.egl_get_error)();
        bail!("eglCreateImageKHR(AHB) -> null, egl_err={egl_err:#x}");
    }

    while (api.get_error)() != 0 {}
    (api.gen_textures)(2, textures.as_mut_ptr());
    (api.bind_texture)(GL_TEXTURE_2D, textures[0]);
    (api.gl_egl_image_target_texture_2d)(GL_TEXTURE_2D, image);
    let gl_err = (api.get_error)();
    if gl_err != 0 {
        bail!("glEGLImageTargetTexture2DOES(TEXTURE_2D) gl_err={gl_err:#x}");
    }
    for (pname, value) in [
        (GL_TEXTURE_MIN_FILTER, GL_LINEAR),
        (GL_TEXTURE_MAG_FILTER, GL_LINEAR),
        (GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE),
        (GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE),
    ] {
        (api.tex_parameteri)(GL_TEXTURE_2D, pname, value);
    }
    godot::global::godot_print!("[xreal] ahb_probe: EGLImage import + texture bind ok");

    // -- 3) render into it: FBO attach + clear + readback -----------------------------------
    (api.gen_framebuffers)(2, fbos.as_mut_ptr());
    (api.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, fbos[0]);
    (api.framebuffer_texture_2d)(
        GL_DRAW_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        textures[0],
        0,
    );
    let fbo_status = (api.check_framebuffer_status)(GL_DRAW_FRAMEBUFFER);
    if fbo_status != GL_FRAMEBUFFER_COMPLETE {
        bail!("AHB texture as color attachment: framebuffer status {fbo_status:#x} (GPU_COLOR_OUTPUT not honored)");
    }
    (api.disable)(GL_SCISSOR_TEST);
    (api.clear_color)(CLEAR_RGBA[0], CLEAR_RGBA[1], CLEAR_RGBA[2], CLEAR_RGBA[3]);
    (api.clear)(GL_COLOR_BUFFER_BIT);

    let read_px = |fbo: u32, x: i32, y: i32| -> [u8; 4] {
        let mut px = [0u8; 4];
        (api.bind_framebuffer)(GL_READ_FRAMEBUFFER, fbo);
        (api.read_pixels)(
            x,
            y,
            1,
            1,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            px.as_mut_ptr() as *mut c_void,
        );
        px
    };
    let matches = |px: &[u8; 4]| -> bool {
        px.iter()
            .zip(EXPECT_RGBA)
            .all(|(&a, b)| a.abs_diff(b) <= TOLERANCE)
    };

    let center = read_px(fbos[0], PROBE_W / 2, PROBE_H / 2);
    let corner = read_px(fbos[0], 0, 0);
    let gl_err = (api.get_error)();
    godot::global::godot_print!(
        "[xreal] ahb_probe: clear+readback center={center:?} corner={corner:?} expect={EXPECT_RGBA:?} \
         gl_err={gl_err:#x}"
    );
    if gl_err != 0 || !matches(&center) || !matches(&corner) {
        bail!("render-into-AHB readback mismatch (center={center:?} corner={corner:?} gl_err={gl_err:#x})");
    }

    // -- 4) read out of it: blit AHB -> plain RGBA8 texture + readback ----------------------
    const DST: i32 = 64;
    (api.bind_texture)(GL_TEXTURE_2D, textures[1]);
    (api.tex_image_2d)(
        GL_TEXTURE_2D,
        0,
        GL_RGBA8,
        DST,
        DST,
        0,
        GL_RGBA,
        GL_UNSIGNED_BYTE,
        std::ptr::null(),
    );
    (api.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, fbos[1]);
    (api.framebuffer_texture_2d)(
        GL_DRAW_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        textures[1],
        0,
    );
    let dst_status = (api.check_framebuffer_status)(GL_DRAW_FRAMEBUFFER);
    if dst_status != GL_FRAMEBUFFER_COMPLETE {
        bail!("dst framebuffer status {dst_status:#x}");
    }
    (api.bind_framebuffer)(GL_READ_FRAMEBUFFER, fbos[0]);
    (api.blit_framebuffer)(
        0,
        0,
        PROBE_W,
        PROBE_H,
        0,
        0,
        DST,
        DST,
        GL_COLOR_BUFFER_BIT,
        GL_LINEAR as u32,
    );
    let blit_px = read_px(fbos[1], DST / 2, DST / 2);
    let gl_err = (api.get_error)();
    godot::global::godot_print!(
        "[xreal] ahb_probe: blit-out readback {blit_px:?} expect={EXPECT_RGBA:?} gl_err={gl_err:#x}"
    );
    if gl_err != 0 || !matches(&blit_px) {
        bail!("blit-out-of-AHB readback mismatch ({blit_px:?} gl_err={gl_err:#x})");
    }

    cleanup(image, &textures, &fbos);
    Ok(format!(
        "isSupported={supported:?}, stride={stride:?}, clear/readback + blit-out verified at \
         {PROBE_W}x{PROBE_H}"
    ))
}
