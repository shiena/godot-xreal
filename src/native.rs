//! Runtime binding to the vendored XREAL `.so` libraries via `dlopen`/`dlsym`.
//!
//! We deliberately avoid linking the XREAL libraries at build time: they only exist for
//! Android arm64, and keeping them out of the link line lets the same GDExtension load
//! in a desktop editor (where [`XrealNative::load`] simply returns `Err` and the node
//! no-ops).
//!
//! Symbols are resolved once and the owning [`libloading::Library`] handles are kept
//! alive in the struct for the lifetime of the resolved function pointers.

// Desktop never loads the `.so`, so this FFI module's items are dead there; keep the lint on Android.
#![cfg_attr(not(target_os = "android"), allow(dead_code))]

use libloading::Library;

use std::ffi::c_void;

use crate::ffi::{
    FnControlSetI32, FnCreateFrame, FnCreateSession, FnGetDeviceType, FnGetFrameMetaData,
    FnGetHeadPoseAtTime, FnGetHeadPoseDisplay, FnGetPluginVersion, FnGlassesEventCallback,
    FnHmdTimeNanos, FnInitUserDefinedSettings, FnIsSessionStarted, FnLoadApi, FnNrBufferSpecCreate,
    FnNrBufferSpecSetI32, FnNrBufferSpecSetSize, FnNrBufferSpecSetU32, FnNrBufferSpecSetU64,
    FnNrBufferViewportGetSwapchain, FnNrFrameAcquireBuffers, FnNrFrameCompose, FnNrFrameCreate,
    FnNrFrameGetBufferViewport, FnNrFrameGetViewportCount, FnNrFrameNoArgs, FnNrFrameSendMetaData,
    FnNrFrameSetBufferViewport, FnNrFrameSetBufferViewport3, FnNrFrameSetColorTextures,
    FnNrHandleDestroy, FnNrRenderingAcquireFrame, FnNrRenderingCreate, FnNrRenderingGetI32,
    FnNrRenderingOneHandle, FnNrRenderingSetGraphicContext, FnNrRenderingSetI32,
    FnNrRenderingSetU64, FnNrSwapchainCreate, FnNrSwapchainCreateAndroidSurface,
    FnNrSwapchainGetRecommendBufferCount, FnNrSwapchainSetBuffers, FnNrViewportCreate,
    FnNrViewportSetF32x2, FnNrViewportSetI32, FnNrViewportSetNearFar, FnNrViewportSetPtr,
    FnNrViewportSetU32, FnNrViewportSetU64, FnQueryInt, FnSetGlassesEventCallback,
    FnSetNativeErrorCallback, FnSwitchTrackingType, FnUnityPluginLoad, FnVoid, NrGraphicContext,
    NrHandle, NrPose, UserDefinedSettings,
};
use crate::ffi::{
    FnDisposeRgbCameraDataHandle, FnStartRgbCameraCapture, FnStopRgbCameraCapture,
    FnTryAcquireLatestImage, FnTryGetRgbCameraDataPlane, NrSize2i,
};

const SESSION_LIB: &str = "libXREALNativeSessionManager.so";
const PLUGIN_LIB: &str = "libXREALXRPlugin.so";
const NR_LOADER_LIB: &str = "libnr_loader.so";
const GLES_LIB: &str = "libGLESv3.so";
const EGL_LIB: &str = "libEGL.so";

type FnGlGenTextures = unsafe extern "C" fn(i32, *mut u32);
type FnGlDeleteTextures = unsafe extern "C" fn(i32, *const u32);
type FnGlBindTexture = unsafe extern "C" fn(u32, u32);
type FnGlTexParameteri = unsafe extern "C" fn(u32, u32, i32);
type FnGlTexImage2D = unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
type FnGlGetError = unsafe extern "C" fn() -> u32;

const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_LINEAR: i32 = 0x2601;
const GL_CLAMP_TO_EDGE: i32 = 0x812F;

pub struct XrealNative {
    // Keep the libraries loaded; the function pointers below borrow from them.
    _session_lib: Library,
    _plugin_lib: Option<Library>,

    // Perception (libXREALNativeSessionManager.so) — RE-confirmed signatures.
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

    // RGB camera (libXREALXRPlugin.so, flat C ABI; see docs/plans/camera-feed-plan.md). Poll path.
    rgb_start_capture: Option<FnStartRgbCameraCapture>,
    rgb_stop_capture: Option<FnStopRgbCameraCapture>,
    rgb_try_acquire_latest: Option<FnTryAcquireLatestImage>,
    rgb_get_data_plane: Option<FnTryGetRgbCameraDataPlane>,
    rgb_dispose_handle: Option<FnDisposeRgbCameraDataHandle>,

    // Session / control (libXREALXRPlugin.so) — optional, used for full bootstrap.
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

    // Read-only device info (libXREALXRPlugin.so) — exposed via XrealSystem.
    get_plugin_version: Option<FnGetPluginVersion>,
    get_device_type: Option<FnGetDeviceType>,

    // Direct NR compositor/rendering API (libnr_loader.so) — RE / unverified.
    nr_rendering: Option<NrRenderingApi>,
    gl: Option<GlTextureApi>,
    nr_rendering_handle: Option<NrHandle>,
    nr_buffer_spec_handle: Option<NrHandle>,
    nr_swapchain_handle: Option<NrHandle>,
    nr_viewport_handles: Vec<NrHandle>,
    gl_texture_ids: Vec<u32>,
    ahb_buffers: Vec<*mut c_void>,
    egl_images: Vec<*mut c_void>,
    android_surface: *mut c_void,
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
    // (0x53bd8) from its runtime address. See docs/reference/reverse-engineering.md.
    display_manager_desc_ptr: Option<*mut c_void>,
}

#[allow(dead_code)]
struct NrRenderingApi {
    // Keep the loader alive; all function pointers below are borrowed from it.
    _lib: Library,

    rendering_create: FnNrRenderingCreate,
    rendering_acquire_frame: FnNrRenderingAcquireFrame,
    rendering_start: FnNrRenderingOneHandle,
    rendering_stop: FnNrRenderingOneHandle,
    rendering_destroy: FnNrRenderingOneHandle,
    rendering_pause: FnNrRenderingOneHandle,
    rendering_resume: FnNrRenderingOneHandle,
    rendering_init_set_graphic_context: FnNrRenderingSetGraphicContext,
    rendering_init_set_flags: FnNrRenderingSetU64,
    rendering_init_set_screen_buffer_mode: FnNrRenderingSetI32,
    rendering_set_embedded_data_mode: FnNrRenderingSetI32,
    rendering_get_frame_buffer_mode: FnNrRenderingGetI32,

    buffer_spec_create: FnNrBufferSpecCreate,
    buffer_spec_destroy: FnNrHandleDestroy,
    buffer_spec_set_size: FnNrBufferSpecSetSize,
    buffer_spec_set_texture_format: FnNrBufferSpecSetI32,
    buffer_spec_set_samples: FnNrBufferSpecSetU32,
    buffer_spec_set_create_flags: FnNrBufferSpecSetU64,

    swapchain_create: FnNrSwapchainCreate,
    swapchain_create_ex: FnNrSwapchainCreate,
    swapchain_create_android_surface: FnNrSwapchainCreateAndroidSurface,
    swapchain_destroy: FnNrHandleDestroy,
    swapchain_set_buffers: FnNrSwapchainSetBuffers,
    swapchain_get_recommend_buffer_count: FnNrSwapchainGetRecommendBufferCount,

    viewport_create: FnNrViewportCreate,
    viewport_destroy: FnNrHandleDestroy,
    viewport_set_type: FnNrViewportSetI32,
    viewport_set_target_component: FnNrViewportSetI32,
    viewport_set_transform: FnNrViewportSetPtr,
    viewport_set_source_uv: FnNrViewportSetPtr,
    viewport_set_source_fov: FnNrViewportSetPtr,
    viewport_set_scene_near_far: FnNrViewportSetNearFar,
    viewport_set_swapchain: FnNrViewportSetU64,
    viewport_add_swapchain: FnNrViewportSetU64,
    viewport_set_quad_size: FnNrViewportSetF32x2,
    viewport_set_multiview_layer: FnNrViewportSetU32,
    viewport_set_flags: FnNrViewportSetU64,

    frame_create: FnNrFrameCreate,
    frame_destroy: FnNrHandleDestroy,
    frame_acquire_buffers: FnNrFrameAcquireBuffers,
    frame_get_viewport_count: FnNrFrameGetViewportCount,
    frame_get_buffer_viewport: FnNrFrameGetBufferViewport,
    frame_set_color_textures: FnNrFrameSetColorTextures,
    frame_set_buffer_viewport: FnNrFrameSetBufferViewport,
    frame_set_buffer_viewport3: FnNrFrameSetBufferViewport3,
    frame_compose: FnNrFrameNoArgs,
    frame_compose5: FnNrFrameCompose,
    frame_submit: FnNrFrameNoArgs,
    frame_send_metadata: FnNrFrameSendMetaData,
    viewport_get_swapchain: FnNrBufferViewportGetSwapchain,
}

#[derive(Debug)]
struct NrSwapchainProbe {
    buffer_spec: NrHandle,
    swapchain: NrHandle,
    recommend_count: u32,
}

#[repr(C)]
struct NrRectf {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

#[repr(C)]
struct NrFov4f {
    left: f32,
    right: f32,
    up: f32,
    down: f32,
}

#[repr(C)]
struct NrTransform {
    qx: f32,
    qy: f32,
    qz: f32,
    qw: f32,
    px: f32,
    py: f32,
    pz: f32,
}

type FnEglGetCurrentContext = unsafe extern "C" fn() -> *mut c_void;
type FnEglGetCurrentDisplay = unsafe extern "C" fn() -> *mut c_void;
type FnEglCreateImageKHR =
    unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void, *const i32) -> *mut c_void;
type FnEglDestroyImageKHR = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;
type FnEglGetNativeClientBufferANDROID = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type FnEglGetError = unsafe extern "C" fn() -> u32;

/// One RGB-camera frame as planar YCbCr: `(y, y_w, y_h, cbcr, c_w, c_h)` — the Y plane (R8, full-res)
/// plus an interleaved CbCr buffer (RG8, half-res), the layout `set_ycbcr_images` + a YCbCr shader expect.
pub type YuvFrame = (Vec<u8>, i32, i32, Vec<u8>, i32, i32);

// EGL_GL_TEXTURE_2D_KHR from <EGL/eglext.h>
const EGL_GL_TEXTURE_2D_KHR: u32 = 0x30B1;
// EGL_NATIVE_BUFFER_ANDROID from <EGL/eglext.h>
const EGL_NATIVE_BUFFER_ANDROID: u32 = 0x3140;

// AHardwareBuffer_allocate(const AHardwareBuffer_Desc*, AHardwareBuffer**)
type FnAHardwareBufferAllocate =
    unsafe extern "C" fn(*const AHardwareBufferDesc, *mut *mut c_void) -> i32;
type FnAHardwareBufferRelease = unsafe extern "C" fn(*mut c_void);
// glEGLImageTargetTexture2DOES(target, image) from GL_OES_EGL_image
type FnGlEglImageTargetTexture2DOES = unsafe extern "C" fn(u32, *mut c_void);

/// Mirror of AHardwareBuffer_Desc (android/hardware_buffer.h, API 26+).
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

struct GlTextureApi {
    _lib: Library,
    _egl_lib: Library,
    _android_lib: Option<Library>,
    gen_textures: FnGlGenTextures,
    delete_textures: FnGlDeleteTextures,
    bind_texture: FnGlBindTexture,
    tex_parameteri: FnGlTexParameteri,
    tex_image_2d: FnGlTexImage2D,
    get_error: FnGlGetError,
    egl_get_current_context: FnEglGetCurrentContext,
    egl_get_current_display: FnEglGetCurrentDisplay,
    egl_create_image_khr: FnEglCreateImageKHR,
    egl_destroy_image_khr: FnEglDestroyImageKHR,
    ahb_allocate: Option<FnAHardwareBufferAllocate>,
    ahb_release: Option<FnAHardwareBufferRelease>,
    gl_egl_image_target_texture: Option<FnGlEglImageTargetTexture2DOES>,
    egl_get_native_client_buffer: Option<FnEglGetNativeClientBufferANDROID>,
    egl_get_error: Option<FnEglGetError>,
}

impl GlTextureApi {
    fn load() -> Result<Self, String> {
        unsafe {
            let lib = Library::new(GLES_LIB).map_err(|e| format!("dlopen {GLES_LIB}: {e}"))?;
            let egl_lib = Library::new(EGL_LIB).map_err(|e| format!("dlopen {EGL_LIB}: {e}"))?;

            macro_rules! sym {
                ($lib:ident, $name:literal, $ty:ty) => {
                    *$lib
                        .get::<$ty>(concat!($name, "\0").as_bytes())
                        .map_err(|e| format!("dlsym {}: {e}", $name))?
                };
            }

            // Try to load libandroid.so for AHardwareBuffer API (API 26+).
            let android_lib = Library::new("libandroid.so").ok();
            let ahb_allocate = android_lib.as_ref().and_then(|l| {
                l.get::<FnAHardwareBufferAllocate>(b"AHardwareBuffer_allocate\0")
                    .ok()
                    .map(|s| *s)
            });
            let ahb_release = android_lib.as_ref().and_then(|l| {
                l.get::<FnAHardwareBufferRelease>(b"AHardwareBuffer_release\0")
                    .ok()
                    .map(|s| *s)
            });
            // glEGLImageTargetTexture2DOES is a GL extension; might be in libGLESv2 or libEGL.
            let gl_egl_image_target_texture = lib
                .get::<FnGlEglImageTargetTexture2DOES>(b"glEGLImageTargetTexture2DOES\0")
                .ok()
                .map(|s| *s);

            // eglGetNativeClientBufferANDROID converts AHardwareBuffer → EGLClientBuffer.
            let egl_get_native_client_buffer = egl_lib
                .get::<FnEglGetNativeClientBufferANDROID>(b"eglGetNativeClientBufferANDROID\0")
                .ok()
                .map(|s| *s);
            let egl_get_error = egl_lib
                .get::<FnEglGetError>(b"eglGetError\0")
                .ok()
                .map(|s| *s);

            Ok(Self {
                gen_textures: sym!(lib, "glGenTextures", FnGlGenTextures),
                delete_textures: sym!(lib, "glDeleteTextures", FnGlDeleteTextures),
                bind_texture: sym!(lib, "glBindTexture", FnGlBindTexture),
                tex_parameteri: sym!(lib, "glTexParameteri", FnGlTexParameteri),
                tex_image_2d: sym!(lib, "glTexImage2D", FnGlTexImage2D),
                get_error: sym!(lib, "glGetError", FnGlGetError),
                egl_get_current_context: sym!(
                    egl_lib,
                    "eglGetCurrentContext",
                    FnEglGetCurrentContext
                ),
                egl_get_current_display: sym!(
                    egl_lib,
                    "eglGetCurrentDisplay",
                    FnEglGetCurrentDisplay
                ),
                egl_create_image_khr: sym!(egl_lib, "eglCreateImageKHR", FnEglCreateImageKHR),
                egl_destroy_image_khr: sym!(egl_lib, "eglDestroyImageKHR", FnEglDestroyImageKHR),
                ahb_allocate,
                ahb_release,
                gl_egl_image_target_texture,
                egl_get_native_client_buffer,
                egl_get_error,
                _lib: lib,
                _egl_lib: egl_lib,
                _android_lib: android_lib,
            })
        }
    }

    /// Create an EGLImage from a GL texture ID.
    /// Returns null on failure.
    fn create_egl_image(&self, tex_id: u32) -> *mut c_void {
        unsafe {
            let display = (self.egl_get_current_display)();
            let context = (self.egl_get_current_context)();
            if display.is_null() || context.is_null() {
                return std::ptr::null_mut();
            }
            (self.egl_create_image_khr)(
                display,
                context,
                EGL_GL_TEXTURE_2D_KHR,
                tex_id as usize as *mut c_void,
                std::ptr::null(),
            )
        }
    }

    fn destroy_egl_image(&self, image: *mut c_void) {
        if !image.is_null() {
            unsafe {
                let display = (self.egl_get_current_display)();
                if !display.is_null() {
                    (self.egl_destroy_image_khr)(display, image);
                }
            }
        }
    }

    /// Create AHardwareBuffer-backed GL textures for cross-process GPU sharing.
    /// Returns (ahb_pointers, gl_texture_ids) or an error string.
    /// AHBs are cross-process shareable; the Nebula compositor can import them.
    fn create_ahb_textures(
        &self,
        count: u32,
        width: u32,
        height: u32,
    ) -> Result<(Vec<*mut c_void>, Vec<u32>), String> {
        let allocate = self
            .ahb_allocate
            .ok_or("AHardwareBuffer_allocate not available")?;
        let gl_img_target = self
            .gl_egl_image_target_texture
            .ok_or("glEGLImageTargetTexture2DOES not available")?;

        const AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM: u32 = 1;
        // GPU_SAMPLED_IMAGE(0x100) | GPU_FRAMEBUFFER(0x200)
        const USAGE: u64 = 0x300;

        let mut ahbs: Vec<*mut c_void> = Vec::with_capacity(count as usize);
        let mut tex_ids: Vec<u32> = Vec::with_capacity(count as usize);

        unsafe {
            let display = (self.egl_get_current_display)();
            if display.is_null() {
                return Err("eglGetCurrentDisplay returned null".into());
            }

            for _ in 0..count {
                let desc = AHardwareBufferDesc {
                    width,
                    height,
                    layers: 1,
                    format: AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM,
                    usage: USAGE,
                    stride: 0,
                    rfu0: 0,
                    rfu1: 0,
                };
                let mut ahb: *mut c_void = std::ptr::null_mut();
                let alloc_s = allocate(&desc, &mut ahb);
                if alloc_s != 0 || ahb.is_null() {
                    // Clean up already allocated
                    if let Some(release) = self.ahb_release {
                        for &a in &ahbs {
                            release(a);
                        }
                    }
                    return Err(format!("AHardwareBuffer_allocate -> {alloc_s}"));
                }

                // Convert AHardwareBuffer → EGLClientBuffer via eglGetNativeClientBufferANDROID.
                // On modern Android, passing AHB pointer directly to eglCreateImageKHR fails;
                // the eglGetNativeClientBufferANDROID wrapper must be used instead.
                let egl_client_buf = if let Some(get_buf) = self.egl_get_native_client_buffer {
                    get_buf(ahb)
                } else {
                    ahb // Fallback: try passing AHB pointer directly (may fail)
                };
                if egl_client_buf.is_null() {
                    if let Some(release) = self.ahb_release {
                        release(ahb);
                    }
                    if let Some(release) = self.ahb_release {
                        for &a in &ahbs {
                            release(a);
                        }
                    }
                    return Err("eglGetNativeClientBufferANDROID returned null".into());
                }

                // Create EGLImage from AHB client buffer (EGL_NO_CONTEXT = null)
                let egl_image = (self.egl_create_image_khr)(
                    display,
                    std::ptr::null_mut(), // EGL_NO_CONTEXT
                    EGL_NATIVE_BUFFER_ANDROID,
                    egl_client_buf,
                    std::ptr::null(),
                );
                if egl_image.is_null() {
                    let egl_err = self.egl_get_error.map(|f| f()).unwrap_or(0);
                    if let Some(release) = self.ahb_release {
                        release(ahb);
                    }
                    if let Some(release) = self.ahb_release {
                        for &a in &ahbs {
                            release(a);
                        }
                    }
                    return Err(format!(
                        "eglCreateImageKHR(EGL_NATIVE_BUFFER_ANDROID) returned null, eglErr={egl_err:#x}"
                    ));
                }

                // Create GL texture backed by the AHB via the EGLImage
                let mut tex: u32 = 0;
                (self.gen_textures)(1, &mut tex);
                (self.bind_texture)(0x0DE1 /* GL_TEXTURE_2D */, tex);
                gl_img_target(0x0DE1 /* GL_TEXTURE_2D */, egl_image);
                (self.bind_texture)(0x0DE1, 0);

                // Destroy the EGLImage handle (the texture retains the AHB reference)
                (self.egl_destroy_image_khr)(display, egl_image);

                ahbs.push(ahb);
                tex_ids.push(tex);
            }
        }

        Ok((ahbs, tex_ids))
    }

    fn create_rgba_textures(&self, count: u32, width: i32, height: i32) -> Result<Vec<u32>, u32> {
        let mut textures = vec![0; count as usize];
        unsafe {
            while (self.get_error)() != 0 {}
            (self.gen_textures)(count as i32, textures.as_mut_ptr());
            if let Some(error) = self.take_gl_error("glGenTextures") {
                return Err(error);
            }
            godot::global::godot_print!(
                "[xreal] GL texture probe: generated count={}, first_id={}",
                textures.len(),
                textures.first().copied().unwrap_or_default()
            );
            for (index, texture) in textures.iter().enumerate() {
                (self.bind_texture)(GL_TEXTURE_2D, *texture);
                if let Some(error) = self.take_gl_error("glBindTexture") {
                    (self.delete_textures)(textures.len() as i32, textures.as_ptr());
                    return Err(error);
                }
                (self.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
                if let Some(error) = self.take_gl_error("glTexParameteri MIN_FILTER") {
                    (self.delete_textures)(textures.len() as i32, textures.as_ptr());
                    return Err(error);
                }
                (self.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
                if let Some(error) = self.take_gl_error("glTexParameteri MAG_FILTER") {
                    (self.delete_textures)(textures.len() as i32, textures.as_ptr());
                    return Err(error);
                }
                (self.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
                if let Some(error) = self.take_gl_error("glTexParameteri WRAP_S") {
                    (self.delete_textures)(textures.len() as i32, textures.as_ptr());
                    return Err(error);
                }
                (self.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
                if let Some(error) = self.take_gl_error("glTexParameteri WRAP_T") {
                    (self.delete_textures)(textures.len() as i32, textures.as_ptr());
                    return Err(error);
                }
                (self.tex_image_2d)(
                    GL_TEXTURE_2D,
                    0,
                    GL_RGBA as i32,
                    width,
                    height,
                    0,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    std::ptr::null(),
                );
                if let Some(error) = self.take_gl_error("glTexImage2D") {
                    godot::global::godot_print!(
                        "[xreal] GL texture probe failed at texture_index={index}, \
                         texture_id={texture}, size={}x{}",
                        width,
                        height
                    );
                    (self.delete_textures)(textures.len() as i32, textures.as_ptr());
                    return Err(error);
                }
            }
            (self.bind_texture)(GL_TEXTURE_2D, 0);
            if let Some(error) = self.take_gl_error("glBindTexture reset") {
                if !textures.is_empty() {
                    (self.delete_textures)(textures.len() as i32, textures.as_ptr());
                }
                return Err(error);
            }
        }
        Ok(textures)
    }

    fn delete_textures(&self, textures: &[u32]) {
        if textures.is_empty() {
            return;
        }
        unsafe {
            (self.delete_textures)(textures.len() as i32, textures.as_ptr());
        }
    }

    unsafe fn take_gl_error(&self, label: &str) -> Option<u32> {
        let error = (self.get_error)();
        if error == 0 {
            return None;
        }
        godot::global::godot_print!("[xreal] GL texture probe: {label} -> error {error}");
        Some(error)
    }
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
                rendering_acquire_frame: sym!("NRRenderingAcquireFrame", FnNrRenderingAcquireFrame),
                rendering_start: sym!("NRRenderingStart", FnNrRenderingOneHandle),
                rendering_stop: sym!("NRRenderingStop", FnNrRenderingOneHandle),
                rendering_destroy: sym!("NRRenderingDestroy", FnNrRenderingOneHandle),
                rendering_pause: sym!("NRRenderingPause", FnNrRenderingOneHandle),
                rendering_resume: sym!("NRRenderingResume", FnNrRenderingOneHandle),
                rendering_init_set_graphic_context: sym!(
                    "NRRenderingInitSetGraphicContext",
                    FnNrRenderingSetGraphicContext
                ),
                rendering_init_set_flags: sym!("NRRenderingInitSetFlags", FnNrRenderingSetU64),
                rendering_init_set_screen_buffer_mode: sym!(
                    "NRRenderingInitSetScreenBufferMode",
                    FnNrRenderingSetI32
                ),
                rendering_set_embedded_data_mode: sym!(
                    "NRRenderingSetEmbeddedDataMode",
                    FnNrRenderingSetI32
                ),
                rendering_get_frame_buffer_mode: sym!(
                    "NRRenderingGetFrameBufferMode",
                    FnNrRenderingGetI32
                ),

                buffer_spec_create: sym!("NRBufferSpecCreate", FnNrBufferSpecCreate),
                buffer_spec_destroy: sym!("NRBufferSpecDestroy", FnNrHandleDestroy),
                buffer_spec_set_size: sym!("NRBufferSpecSetSize", FnNrBufferSpecSetSize),
                buffer_spec_set_texture_format: sym!(
                    "NRBufferSpecSetTextureFormat",
                    FnNrBufferSpecSetI32
                ),
                buffer_spec_set_samples: sym!("NRBufferSpecSetSamples", FnNrBufferSpecSetU32),
                buffer_spec_set_create_flags: sym!(
                    "NRBufferSpecSetCreateFlags",
                    FnNrBufferSpecSetU64
                ),

                swapchain_create: sym!("NRSwapchainCreate", FnNrSwapchainCreate),
                swapchain_create_ex: sym!("NRSwapchainCreateEx", FnNrSwapchainCreate),
                swapchain_create_android_surface: sym!(
                    "NRSwapchainCreateAndroidSurface",
                    FnNrSwapchainCreateAndroidSurface
                ),
                swapchain_destroy: sym!("NRSwapchainDestroy", FnNrHandleDestroy),
                swapchain_set_buffers: sym!("NRSwapchainSetBuffers", FnNrSwapchainSetBuffers),
                swapchain_get_recommend_buffer_count: sym!(
                    "NRSwapchainGetRecommendBufferCount",
                    FnNrSwapchainGetRecommendBufferCount
                ),

                viewport_create: sym!("NRBufferViewportCreate", FnNrViewportCreate),
                viewport_destroy: sym!("NRBufferViewportDestroy", FnNrHandleDestroy),
                viewport_set_type: sym!("NRBufferViewportSetType", FnNrViewportSetI32),
                viewport_set_target_component: sym!(
                    "NRBufferViewportSetTargetComponent",
                    FnNrViewportSetI32
                ),
                viewport_set_transform: sym!("NRBufferViewportSetTransform", FnNrViewportSetPtr),
                viewport_set_source_uv: sym!("NRBufferViewportSetSourceUV", FnNrViewportSetPtr),
                viewport_set_source_fov: sym!("NRBufferViewportSetSourceFov", FnNrViewportSetPtr),
                viewport_set_scene_near_far: sym!(
                    "NRBufferViewportSetSceneNearFar",
                    FnNrViewportSetNearFar
                ),
                viewport_set_swapchain: sym!("NRBufferViewportSetSwapchain", FnNrViewportSetU64),
                viewport_add_swapchain: sym!("NRBufferViewportAddSwapchain", FnNrViewportSetU64),
                viewport_set_quad_size: sym!("NRBufferViewportSetQuadSize", FnNrViewportSetF32x2),
                viewport_set_multiview_layer: sym!(
                    "NRBufferViewportSetMultiviewLayer",
                    FnNrViewportSetU32
                ),
                viewport_set_flags: sym!("NRBufferViewportSetFlags", FnNrViewportSetU64),

                frame_create: sym!("NRFrameCreate", FnNrFrameCreate),
                frame_destroy: sym!("NRFrameDestroy", FnNrHandleDestroy),
                frame_acquire_buffers: sym!("NRFrameAcquireBuffers", FnNrFrameAcquireBuffers),
                frame_get_viewport_count: sym!(
                    "NRFrameGetViewportCount",
                    FnNrFrameGetViewportCount
                ),
                frame_get_buffer_viewport: sym!(
                    "NRFrameGetBufferViewport",
                    FnNrFrameGetBufferViewport
                ),
                frame_set_color_textures: sym!(
                    "NRFrameSetColorTextures",
                    FnNrFrameSetColorTextures
                ),
                frame_set_buffer_viewport: sym!(
                    "NRFrameSetBufferViewport",
                    FnNrFrameSetBufferViewport
                ),
                frame_set_buffer_viewport3: sym!(
                    "NRFrameSetBufferViewport",
                    FnNrFrameSetBufferViewport3
                ),
                frame_compose: sym!("NRFrameCompose", FnNrFrameNoArgs),
                frame_compose5: sym!("NRFrameCompose", FnNrFrameCompose),
                frame_submit: sym!("NRFrameSubmit", FnNrFrameNoArgs),
                frame_send_metadata: sym!("NRFrameSendMetaData", FnNrFrameSendMetaData),
                viewport_get_swapchain: sym!(
                    "NRBufferViewportGetSwapchain",
                    FnNrBufferViewportGetSwapchain
                ),
                _lib: lib,
            })
        }
    }

    fn resolved_symbol_count(&self) -> usize {
        45
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

    fn create_swapchain_probe(&self, rendering: NrHandle) -> Result<NrSwapchainProbe, i32> {
        let mut buffer_spec: NrHandle = 0;
        let create_spec_status = unsafe { (self.buffer_spec_create)(rendering, &mut buffer_spec) };
        if create_spec_status != 0 {
            return Err(create_spec_status);
        }
        if buffer_spec == 0 {
            return Err(-20);
        }

        // RE / unverified: Unity logs CreateBufferSpec: 1968 1134 on XREAL One Pro.
        let set_size_status =
            unsafe { (self.buffer_spec_set_size)(rendering, buffer_spec, 1968, 1134) };
        if set_size_status != 0 {
            let _ = unsafe { (self.buffer_spec_destroy)(rendering, buffer_spec) };
            return Err(set_size_status);
        }

        // RE / unverified: Unity creates non-MSAA textures in the inspected GLES path.
        let set_samples_status =
            unsafe { (self.buffer_spec_set_samples)(rendering, buffer_spec, 1) };
        if set_samples_status != 0 {
            godot::global::godot_print!(
                "[xreal] NRBufferSpecSetSamples(1) returned {set_samples_status}"
            );
        }

        // Try create flags = 1 to enable Android Surface mode for NRSwapchainCreateAndroidSurface.
        // Without this flag, calling NRSwapchainCreateAndroidSurface aborts with an assertion.
        let set_flags_status =
            unsafe { (self.buffer_spec_set_create_flags)(rendering, buffer_spec, 1) };
        godot::global::godot_print!("[xreal] NRBufferSpecSetCreateFlags(1) -> {set_flags_status}");

        let mut swapchain: NrHandle = 0;
        let create_swapchain_status =
            unsafe { (self.swapchain_create_ex)(rendering, buffer_spec, &mut swapchain) };
        if create_swapchain_status != 0 {
            let _ = unsafe { (self.buffer_spec_destroy)(rendering, buffer_spec) };
            return Err(create_swapchain_status);
        }
        if swapchain == 0 {
            let _ = unsafe { (self.buffer_spec_destroy)(rendering, buffer_spec) };
            return Err(-21);
        }

        let mut recommend_count = 0;
        let recommend_status = unsafe {
            (self.swapchain_get_recommend_buffer_count)(rendering, swapchain, &mut recommend_count)
        };

        if recommend_status != 0 {
            let _ = unsafe { (self.swapchain_destroy)(rendering, swapchain) };
            let _ = unsafe { (self.buffer_spec_destroy)(rendering, buffer_spec) };
            return Err(recommend_status);
        }
        Ok(NrSwapchainProbe {
            buffer_spec,
            swapchain,
            recommend_count,
        })
    }

    #[allow(dead_code)]
    fn probe_android_surface(
        &self,
        rendering: NrHandle,
        swapchain: NrHandle,
    ) -> Result<(*mut c_void, *mut c_void), i32> {
        let mut surface = std::ptr::null_mut();
        let mut native_window_or_holder = std::ptr::null_mut();
        let status = unsafe {
            (self.swapchain_create_android_surface)(
                rendering,
                swapchain,
                &mut surface,
                &mut native_window_or_holder,
            )
        };
        if status != 0 {
            return Err(status);
        }
        Ok((surface, native_window_or_holder))
    }

    fn create_viewport_probe(
        &self,
        rendering: NrHandle,
        swapchain: NrHandle,
        target_component: i32,
    ) -> Result<NrHandle, i32> {
        let mut viewport: NrHandle = 0;
        let create_status = unsafe { (self.viewport_create)(rendering, &mut viewport) };
        if create_status != 0 {
            return Err(create_status);
        }
        if viewport == 0 {
            return Err(-30);
        }

        let set_swapchain_status =
            unsafe { (self.viewport_set_swapchain)(rendering, viewport, swapchain) };
        if set_swapchain_status != 0 {
            let _ = unsafe { (self.viewport_destroy)(rendering, viewport) };
            return Err(set_swapchain_status);
        }

        let source_uv = NrRectf {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        };
        // RE / unverified: approximate one-eye FOV angles in radians from One Pro
        // calibration logs (1920x1080, fx/fy around 2190/2215). The exact order/units
        // still need confirmation.
        let source_fov = NrFov4f {
            left: -0.414,
            right: 0.414,
            up: 0.239,
            down: -0.239,
        };
        let transform = NrTransform {
            qx: 0.0,
            qy: 0.0,
            qz: 0.0,
            qw: 1.0,
            px: 0.0,
            py: 0.0,
            pz: 0.0,
        };
        let set_type_status = unsafe { (self.viewport_set_type)(rendering, viewport, 0) };
        let set_target_status =
            unsafe { (self.viewport_set_target_component)(rendering, viewport, target_component) };
        let set_transform_status = unsafe {
            (self.viewport_set_transform)(
                rendering,
                viewport,
                &transform as *const _ as *const c_void,
            )
        };
        let set_uv_status = unsafe {
            (self.viewport_set_source_uv)(
                rendering,
                viewport,
                &source_uv as *const _ as *const c_void,
            )
        };
        let set_fov_status = unsafe {
            (self.viewport_set_source_fov)(
                rendering,
                viewport,
                &source_fov as *const _ as *const c_void,
            )
        };
        let set_near_far_status =
            unsafe { (self.viewport_set_scene_near_far)(rendering, viewport, 0.1, 1000.0) };
        let set_flags_status = unsafe { (self.viewport_set_flags)(rendering, viewport, 0) };
        godot::global::godot_print!(
            "[xreal] NR viewport config: type={set_type_status}, target={set_target_status}, \
             transform={set_transform_status}, uv={set_uv_status}, fov={set_fov_status}, \
             near_far={set_near_far_status}, flags={set_flags_status}, \
             target_component={target_component}"
        );
        Ok(viewport)
    }

    fn probe_frame_compose(
        &self,
        rendering: NrHandle,
        viewports: &[NrHandle],
        gl_texture_ids: &[u32],
    ) -> Result<(), i32> {
        use std::os::raw::c_void;

        let mut frame: NrHandle = 0;
        let create_status = unsafe { (self.frame_create)(rendering, &mut frame) };
        godot::global::godot_print!(
            "[xreal] NR frame probe: Create -> status={create_status}, frame={frame}"
        );
        if frame == 0 {
            return Err(-4000 - create_status);
        }

        if !gl_texture_ids.is_empty() {
            let ptrs: Vec<*const c_void> = gl_texture_ids
                .iter()
                .map(|id| (*id as usize) as *const c_void)
                .collect();
            let set_status = unsafe {
                (self.frame_set_color_textures)(rendering, frame, ptrs.as_ptr(), ptrs.len() as u32)
            };
            godot::global::godot_print!(
                "[xreal] NR frame probe: SetColorTextures(count={}) -> {set_status}",
                ptrs.len()
            );
            if set_status != 0 {
                let _ = unsafe { (self.frame_destroy)(rendering, frame) };
                return Err(-4100 - set_status);
            }
        }

        // viewport indices are 1-based (target_component: 1=left, 2=right)
        for (index, viewport) in viewports.iter().enumerate() {
            let vp_idx = (index + 1) as u32;
            let set_viewport_status =
                unsafe { (self.frame_set_buffer_viewport)(rendering, frame, vp_idx, *viewport) };
            if set_viewport_status != 0 {
                let _ = unsafe { (self.frame_destroy)(rendering, frame) };
                return Err(-4200 - set_viewport_status);
            }
        }

        let compose_status = unsafe { (self.frame_compose)(rendering, frame) };
        let destroy_status = unsafe { (self.frame_destroy)(rendering, frame) };
        if compose_status != 0 {
            return Err(-4300 - compose_status);
        }
        if destroy_status != 0 {
            return Err(-4400 - destroy_status);
        }
        Ok(())
    }

    fn set_swapchain_buffers(
        &self,
        rendering: NrHandle,
        swapchain: NrHandle,
        buffers: &mut [*mut c_void],
    ) -> i32 {
        unsafe {
            (self.swapchain_set_buffers)(
                rendering,
                swapchain,
                buffers.len() as u32,
                buffers.as_mut_ptr(),
            )
        }
    }
}

// SAFETY: `display_manager_desc_ptr` points into `libXREALXRPlugin.so`'s read-write data
// section (the `UnityXRNextFrameDesc` function-local static). It is written only from the
// session-init thread (before the `OnceLock` is populated) and then treated as read-only.
// All other raw pointers in XrealNative are function pointers or Library handles, which are
// inherently `Send`.
unsafe impl Send for XrealNative {}

impl XrealNative {
    /// `dlopen` the XREAL libraries and resolve the symbols needed for 3DoF.
    ///
    /// Returns `Err` (without panicking) when the libraries are missing — the expected
    /// case on desktop/editor builds.
    pub fn load() -> Result<Self, String> {
        unsafe {
            let session_lib =
                Library::new(SESSION_LIB).map_err(|e| format!("dlopen {SESSION_LIB}: {e}"))?;
            let plugin_lib = Library::new(PLUGIN_LIB).ok();

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

            // RGB camera exports (libXREALXRPlugin.so). See docs/plans/camera-feed-plan.md.
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

            let nr_rendering = NrRenderingApi::load().ok();
            let gl = GlTextureApi::load().ok();

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
                    // Publish the library base so LIB_BASE readers (reassert_update_metrics_on_render_thread) work.
                    // On Android publish it WITHOUT installing the SIGSEGV sigaction (a no-op there —
                    // libsigchain wins — and it destabilised the process); off-Android use install().
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
                nr_rendering,
                gl,
                nr_rendering_handle: None,
                nr_buffer_spec_handle: None,
                nr_swapchain_handle: None,
                nr_viewport_handles: Vec::new(),
                gl_texture_ids: Vec::new(),
                ahb_buffers: Vec::new(),
                egl_images: Vec::new(),
                android_surface: std::ptr::null_mut(),
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

    /// Resume the session — Unity calls this on app resume; it activates the perception
    /// subsystem (a freshly `CreateSession`'d session stays paused, so `IsSessionStarted`
    /// is false and no pose flows until this runs). No-op if the symbol is unavailable.
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

    /// Fetch the **display** subsystem head pose (libXREALXRPlugin.so `GetHeadPoseAtTime`) as the
    /// raw 16-float block it writes — the pose the compositor reprojects with. `None` when the
    /// export is absent or the query fails. The 16-float layout (a 4×4 row-major transform,
    /// device-pinned) is decoded caller-side; see the RE map in `docs/archive/multiview-investigation.md`.
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

    /// Whether the RGB-camera C ABI is available (libXREALXRPlugin.so present + symbols resolved).
    pub fn rgb_camera_available(&self) -> bool {
        self.rgb_start_capture.is_some()
            && self.rgb_try_acquire_latest.is_some()
            && self.rgb_get_data_plane.is_some()
    }

    /// Start RGB-camera capture in **poll mode** (null callback). Returns the capture handle for
    /// [`Self::rgb_camera_stop`], or `None` if the export is unavailable or the SDK reports failure.
    /// NOTE: in poll mode a successful start returns a `0` handle (there is no callback registration
    /// to track) — that is **not** a failure; capture is enabled and [`Self::rgb_camera_grab_y`] then
    /// works (device-confirmed). A wedged glasses camera — e.g. an unclean prior exit left it holding
    /// the connection, so NRSDK rejects the new one ("RgbCamera Recv Frame, -99" / "Plugin Start
    /// failed") — instead returns the `u64::MAX` (-1) error sentinel; surface that as `None` so the
    /// caller doesn't cache a dead handle and drive an unfed (pink) panel.
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
            if !acquire(&mut frame_handle, &mut resolution, &mut timestamp) {
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

    /// Poll the latest RGB-camera frame and copy its planes as **Y** (full-res 8-bit) plus a
    /// **CbCr** buffer interleaved from the chroma planes (I420: plane 1 = V/Cr, plane 2 = U/Cb, both
    /// half-res). Returns `(y, y_w, y_h, cbcr, c_w, c_h)` where `cbcr` is `[Cb, Cr, Cb, Cr, …]`
    /// (`Cb = U`, `Cr = V`) — the RG8 layout Godot's `set_ycbcr_images` + a YCbCr shader expect.
    /// The frame handle is disposed before returning.
    pub fn rgb_camera_grab_yuv(&self) -> Option<YuvFrame> {
        let acquire = self.rgb_try_acquire_latest?;
        let get_plane = self.rgb_get_data_plane?;
        let dispose = self.rgb_dispose_handle;

        let mut frame_handle: i32 = 0;
        let mut resolution = NrSize2i::default();
        let mut timestamp: u64 = 0;
        if !unsafe { acquire(&mut frame_handle, &mut resolution, &mut timestamp) } {
            return None;
        }
        // Copy plane `idx` into an owned buffer. Plane pointers are valid until the handle is disposed.
        let read_plane = |idx: i32| -> Option<(Vec<u8>, i32, i32)> {
            let mut ptr: *mut c_void = std::ptr::null_mut();
            let mut sz = NrSize2i::default();
            let ok = unsafe { get_plane(frame_handle, idx, &mut ptr, &mut sz) };
            if ok && !ptr.is_null() && sz.width > 0 && sz.height > 0 {
                let len = (sz.width as usize) * (sz.height as usize);
                let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) }.to_vec();
                Some((bytes, sz.width, sz.height))
            } else {
                None
            }
        };
        let out = (|| {
            let (y, yw, yh) = read_plane(0)?; // Y (full-res)
            let (v, _, _) = read_plane(1)?; // plane 1 = V (Cr), half-res
            let (u, uw, uh) = read_plane(2)?; // plane 2 = U (Cb), half-res
            let n = (uw as usize) * (uh as usize);
            let m = n.min(u.len()).min(v.len());
            let mut cbcr = Vec::with_capacity(m * 2);
            for i in 0..m {
                cbcr.push(u[i]); // Cb = U
                cbcr.push(v[i]); // Cr = V
            }
            Some((y, yw, yh, cbcr, uw, uh))
        })();
        if let Some(d) = dispose {
            unsafe { d(frame_handle) };
        }
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

    /// Reset the 3DoF forward direction (no-op if the plugin/symbol is unavailable).
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

    /// `SetGlassesSpaceMode(NRGlassesSpaceMode)` (libXREALXRPlugin.so) — how the glasses' X1
    /// chip anchors the virtual screen in space (follow / world-anchor / …). RE / unverified
    /// enum values; exposed so the mode can be probed at runtime from GDScript. The C wrapper
    /// safely returns 0 until NativeGlasses is ready. `None` when the symbol is absent.
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

        // DO NOT call CreateFrame() or SubmitCurrentFrame() here:
        // DisplayManager+0x120 is managed by the XREAL SDK's own rendering thread (GLThread).
        // Calling CreateFrame tries to destroy the SDK's live frame via NativeRendering::DestroyFrame,
        // which fails (frame is locked by the render thread) and crashes in LogHelper::Error
        // (fault addr 0xb9a40998bac55c8a — a valid SDK frame handle passed to fprintf-style log).
        // Device-confirmed: both CreateFrame and SubmitCurrentFrame crash with SIGSEGV at
        // the DestroyFrame path when the SDK's rendering thread owns DisplayManager+0x120.
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

    /// RE / unverified: start the lower NR rendering/display pipeline and keep its handle
    /// alive for the lifetime of this native wrapper. No frames are submitted yet.
    pub fn nr_rendering_start_persistent(&mut self) -> Result<(), i32> {
        if self.nr_rendering_handle.is_some() {
            return Ok(());
        }
        let api = self.nr_rendering.as_ref().ok_or(-1)?;
        let mut rendering: NrHandle = 0;
        let status = unsafe { (api.rendering_create)(&mut rendering) };
        if status != 0 {
            return Err(status);
        }
        if rendering == 0 {
            return Err(-2);
        }
        // Set the EGL context via NRGraphicContext{type=5 (OpenGL ES), context=EGLContext}.
        let egl_ctx = match self.gl.as_ref() {
            Some(gl) => unsafe { (gl.egl_get_current_context)() as *mut std::os::raw::c_void },
            None => std::ptr::null_mut(),
        };
        let nr_gfx_ctx = NrGraphicContext {
            gfx_type: 5, // NRGraphicContextType::NRGRAPHICCONTEXT_OPENGLES
            _pad: [0; 4],
            context: egl_ctx,
        };
        let gc_status = unsafe { (api.rendering_init_set_graphic_context)(rendering, &nr_gfx_ctx) };
        godot::global::godot_print!(
            "[xreal] NRRenderingInitSetGraphicContext(type=5, egl={egl_ctx:?}) -> {gc_status}"
        );

        let flags_status = unsafe { (api.rendering_init_set_flags)(rendering, 0) };
        godot::global::godot_print!("[xreal] NRRenderingInitSetFlags(0) -> {flags_status}");

        let screen_buffer_mode_status =
            unsafe { (api.rendering_init_set_screen_buffer_mode)(rendering, 1) };
        godot::global::godot_print!(
            "[xreal] NRRenderingInitSetScreenBufferMode(1) -> {screen_buffer_mode_status}"
        );

        let start_status = unsafe { (api.rendering_start)(rendering) };
        if start_status != 0 {
            let _ = unsafe { (api.rendering_destroy)(rendering) };
            return Err(start_status);
        }
        godot::global::godot_print!("[xreal] NRRenderingStart -> {start_status}");

        // Probe embedded data modes; mode 2 was observed as the last call in the original
        // Unity log. Without mode 2 the compositor shows Compose=24 (display not available).
        for mode in [0i32, 1, 2] {
            let edm_s = unsafe { (api.rendering_set_embedded_data_mode)(rendering, mode) };
            godot::global::godot_print!(
                "[xreal] NRRenderingSetEmbeddedDataMode({mode}) -> {edm_s}"
            );
        }
        let swapchain_probe = api.create_swapchain_probe(rendering);
        godot::global::godot_print!(
            "[xreal] NR swapchain probe: CreateBufferSpec(1968x1134) -> CreateSwapchainEx -> \
             GetRecommendBufferCount = {swapchain_probe:?}"
        );
        match swapchain_probe {
            Ok(probe) => {
                self.nr_buffer_spec_handle = Some(probe.buffer_spec);
                self.nr_swapchain_handle = Some(probe.swapchain);
                godot::global::godot_print!(
                    "[xreal] NR swapchain retained: recommend_count={}",
                    probe.recommend_count
                );
                // Try AHardwareBuffer-backed textures (cross-process shareable).
                // The Nebula compositor lives in a separate process and cannot use plain
                // GL texture IDs (which are process-local). AHardwareBuffer solves this:
                // the compositor can import the same buffer handles across the process boundary.
                let texture_probe: Result<usize, i32> = 'tp: {
                    let gl = match self.gl.as_ref() {
                        Some(g) => g,
                        None => break 'tp Err(-50),
                    };

                    // First: try AHardwareBuffer path.
                    match gl.create_ahb_textures(probe.recommend_count, 1968, 1134) {
                        Ok((ahbs, tex_ids)) => {
                            godot::global::godot_print!(
                                "[xreal] AHardwareBuffer: created {} AHBs + GL textures",
                                ahbs.len()
                            );
                            // Pass AHB pointers (cross-process) to the swapchain.
                            let mut ahb_ptrs: Vec<*mut c_void> = ahbs.clone();
                            let set_s = api.set_swapchain_buffers(
                                rendering,
                                probe.swapchain,
                                &mut ahb_ptrs,
                            );
                            godot::global::godot_print!(
                                "[xreal] NRSwapchainSetBuffers(ahb_ptrs) -> {set_s}"
                            );
                            if set_s == 0 {
                                self.gl_texture_ids = tex_ids;
                                self.ahb_buffers = ahbs;
                                break 'tp Ok(self.gl_texture_ids.len());
                            }
                            // SetBuffers rejected AHBs; fall through to GL texture fallback.
                            godot::global::godot_print!(
                                "[xreal] AHB SetBuffers rejected (s={set_s}), trying raw GL IDs"
                            );
                            if let Some(release) = gl.ahb_release {
                                for &a in &ahbs {
                                    unsafe {
                                        release(a);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            godot::global::godot_print!(
                                "[xreal] create_ahb_textures failed: {e}; trying raw GL IDs"
                            );
                        }
                    }

                    // Fallback: raw GL textures (Compose=22 = cross-process invalid, but
                    // confirms the frame submission path works end-to-end).
                    let textures = match gl.create_rgba_textures(probe.recommend_count, 1968, 1134)
                    {
                        Ok(t) => t,
                        Err(e) => break 'tp Err(e as i32),
                    };
                    let mut raw_ids: Vec<*mut c_void> = textures
                        .iter()
                        .map(|&id| id as usize as *mut c_void)
                        .collect();
                    let set_status =
                        api.set_swapchain_buffers(rendering, probe.swapchain, &mut raw_ids);
                    godot::global::godot_print!(
                        "[xreal] NRSwapchainSetBuffers(raw_gl_ids) -> {set_status}"
                    );
                    if set_status != 0 {
                        break 'tp Err(set_status);
                    }
                    self.gl_texture_ids = textures;
                    self.egl_images = Vec::new();
                    break 'tp Ok(self.gl_texture_ids.len());
                };
                godot::global::godot_print!(
                    "[xreal] NR swapchain buffer setup = {texture_probe:?}"
                );
                let left_viewport_probe = api.create_viewport_probe(rendering, probe.swapchain, 1);
                let right_viewport_probe = api.create_viewport_probe(rendering, probe.swapchain, 2);
                godot::global::godot_print!(
                    "[xreal] NR viewport probe: left={left_viewport_probe:?}, \
                     right={right_viewport_probe:?}"
                );
                if let (Ok(left_viewport), Ok(right_viewport)) =
                    (left_viewport_probe, right_viewport_probe)
                {
                    self.nr_viewport_handles.push(left_viewport);
                    self.nr_viewport_handles.push(right_viewport);
                    let frame_compose_probe = api.probe_frame_compose(
                        rendering,
                        &self.nr_viewport_handles,
                        &self.gl_texture_ids,
                    );
                    godot::global::godot_print!(
                        "[xreal] NR frame probe: Create -> SetColorTextures -> \
                         SetBufferViewport[2] -> Compose = \
                         {frame_compose_probe:?}"
                    );
                }
            }
            Err(status) => {
                godot::global::godot_print!("[xreal] NR swapchain probe failed: {status}");
            }
        }
        // Log the populate callback desc to understand what the Unity plugin provides.
        let desc_info = crate::unity_plugin::populate_registered_display_frame_desc_once();
        godot::global::godot_print!("[xreal] populate_once result: {desc_info:?}");

        self.nr_rendering_handle = Some(rendering);
        Ok(())
    }

    /// Submit one frame to the NR compositor. Call on every rendered frame.
    pub fn nr_frame_submit(&self) -> Result<u32, i32> {
        let api = self.nr_rendering.as_ref().ok_or(-1)?;
        let rendering = self.nr_rendering_handle.ok_or(-2)?;

        let mut frame: NrHandle = 0;
        let acquire_s = unsafe { (api.rendering_acquire_frame)(rendering, &mut frame) };
        if acquire_s != 0 || frame == 0 {
            return Err(-3000 - acquire_s);
        }

        // AcquiredFrames have no viewports by default; associate our left+right viewports.
        let mut vp_statuses = [99i32; 4];
        for (i, &vp) in self.nr_viewport_handles.iter().enumerate().take(2) {
            // 1-based index (Unity uses vp_idx=1 for left, vp_idx=2 for right)
            vp_statuses[i] =
                unsafe { (api.frame_set_buffer_viewport)(rendering, frame, (i + 1) as u32, vp) };
        }

        static FRAME_CTR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = FRAME_CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let mut vp_count: u32 = 0;
        let vp_count_s = unsafe { (api.frame_get_viewport_count)(rendering, frame, &mut vp_count) };

        let compose_s = unsafe { (api.frame_compose)(rendering, frame) };
        let submit_s = unsafe { (api.frame_submit)(rendering, frame) };
        let _ = unsafe { (api.frame_destroy)(rendering, frame) };

        if n.is_multiple_of(60) {
            godot::global::godot_print!(
                "[xreal] frame #{n}: Acquire={acquire_s} SetVP={:?} VpCount(s={vp_count_s})={vp_count} \
                 Compose={compose_s} Submit={submit_s}",
                &vp_statuses[..self.nr_viewport_handles.len().min(2)]
            );
        }

        if compose_s != 0 {
            return Err(-4300 - compose_s);
        }
        if submit_s != 0 {
            return Err(-4400 - submit_s);
        }
        Ok(0)
    }

    /// The GL texture IDs allocated for the NR swapchain (one per buffer slot).
    pub fn gl_texture_ids(&self) -> &[u32] {
        &self.gl_texture_ids
    }
}

impl Drop for XrealNative {
    fn drop(&mut self) {
        if let (Some(api), Some(rendering)) = (self.nr_rendering.as_ref(), self.nr_rendering_handle)
        {
            if let Some(gl) = self.gl.as_ref() {
                gl.delete_textures(&self.gl_texture_ids);
            }
            self.gl_texture_ids.clear();
            for viewport in self.nr_viewport_handles.drain(..) {
                let _ = unsafe { (api.viewport_destroy)(rendering, viewport) };
            }
            if let Some(swapchain) = self.nr_swapchain_handle.take() {
                let _ = unsafe { (api.swapchain_destroy)(rendering, swapchain) };
            }
            if let Some(buffer_spec) = self.nr_buffer_spec_handle.take() {
                let _ = unsafe { (api.buffer_spec_destroy)(rendering, buffer_spec) };
            }
            let _ = unsafe { (api.rendering_stop)(rendering) };
            let _ = unsafe { (api.rendering_destroy)(rendering) };
        }
        if self.display_manager_rendering_initialized {
            if let Some(deinitialize) = self.deinitialize_rendering {
                unsafe { deinitialize() };
            }
        }
    }
}
