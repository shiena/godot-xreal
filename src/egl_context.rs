//! The private EGL context for the Vulkan glasses bridge (vulkan-path-plan.md stage 2).
//!
//! Under a Vulkan renderer Godot owns no EGL context, but the SDK compositor still consumes
//! client GL texture names, so the bridge creates its own: an ES3 context, surfaceless when
//! `EGL_KHR_surfaceless_context` allows it and on a 1x1 pbuffer otherwise, shared with nothing
//! (EGLImages are context-independent, which is the whole trick).
//!
//! Discipline, from the stage-2 design review: the context is made current *around each SDK
//! graphics operation* and unbound afterwards, never left current. A permanently-current context
//! would make "an EGL context happens to be current" true for bystander code between frames, and
//! `renderer_is_gl()`-style gates are what keep that from being misread as "GL renderer"; see
//! `camera_feed.rs`. Use [`bind`] / [`unbind`], or [`with_current`] which pairs them.
//!
//! Everything is dlsym'd from `libEGL.so`; on desktop the load fails and every entry point
//! reports unavailable, matching the crate's convention.

#![allow(dead_code)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use libloading::Library;

const EGL_DEFAULT_DISPLAY: *mut c_void = std::ptr::null_mut();
const EGL_NO_SURFACE: *mut c_void = std::ptr::null_mut();
const EGL_NO_CONTEXT: *mut c_void = std::ptr::null_mut();
const EGL_OPENGL_ES_API: u32 = 0x30A0;
const EGL_SURFACE_TYPE: i32 = 0x3033;
const EGL_PBUFFER_BIT: i32 = 0x0001;
const EGL_RENDERABLE_TYPE: i32 = 0x3040;
const EGL_OPENGL_ES3_BIT: i32 = 0x0040;
const EGL_RED_SIZE: i32 = 0x3024;
const EGL_GREEN_SIZE: i32 = 0x3023;
const EGL_BLUE_SIZE: i32 = 0x3022;
const EGL_ALPHA_SIZE: i32 = 0x3021;
const EGL_DEPTH_SIZE: i32 = 0x3025;
const EGL_STENCIL_SIZE: i32 = 0x3026;
const EGL_NONE: i32 = 0x3038;
const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_EXTENSIONS: i32 = 0x3055;

type FnEglGetDisplay = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type FnEglInitialize = unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32;
type FnEglBindApi = unsafe extern "C" fn(u32) -> u32;
type FnEglChooseConfig =
    unsafe extern "C" fn(*mut c_void, *const i32, *mut *mut c_void, i32, *mut i32) -> u32;
type FnEglCreateContext =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i32) -> *mut c_void;
type FnEglCreatePbufferSurface =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *const i32) -> *mut c_void;
type FnEglMakeCurrent =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32;
type FnEglGetError = unsafe extern "C" fn() -> u32;
type FnEglQueryString = unsafe extern "C" fn(*mut c_void, i32) -> *const std::ffi::c_char;

struct Egl {
    get_display: FnEglGetDisplay,
    initialize: FnEglInitialize,
    bind_api: FnEglBindApi,
    choose_config: FnEglChooseConfig,
    create_context: FnEglCreateContext,
    create_pbuffer_surface: FnEglCreatePbufferSurface,
    make_current: FnEglMakeCurrent,
    get_error: FnEglGetError,
    query_string: FnEglQueryString,
    _lib: Library,
}

impl Egl {
    fn load() -> Result<Self, String> {
        unsafe {
            let lib = Library::new("libEGL.so").map_err(|e| format!("dlopen libEGL.so: {e}"))?;
            macro_rules! sym {
                ($name:literal, $ty:ty) => {
                    *lib.get::<$ty>(concat!($name, "\0").as_bytes())
                        .map_err(|e| format!("dlsym {}: {e}", $name))?
                };
            }
            Ok(Egl {
                get_display: sym!("eglGetDisplay", FnEglGetDisplay),
                initialize: sym!("eglInitialize", FnEglInitialize),
                bind_api: sym!("eglBindAPI", FnEglBindApi),
                choose_config: sym!("eglChooseConfig", FnEglChooseConfig),
                create_context: sym!("eglCreateContext", FnEglCreateContext),
                create_pbuffer_surface: sym!("eglCreatePbufferSurface", FnEglCreatePbufferSurface),
                make_current: sym!("eglMakeCurrent", FnEglMakeCurrent),
                get_error: sym!("eglGetError", FnEglGetError),
                query_string: sym!("eglQueryString", FnEglQueryString),
                _lib: lib,
            })
        }
    }
}

/// The created context: display + context + the surface to bind (null when surfaceless works).
struct PrivateContext {
    display: *mut c_void,
    context: *mut c_void,
    surface: *mut c_void,
}

// Raw EGL handles; the context is only ever bound and unbound from the render thread, and the
// handles themselves are just process-global pointers.
unsafe impl Send for PrivateContext {}
unsafe impl Sync for PrivateContext {}

static EGL: OnceLock<Option<Egl>> = OnceLock::new();
static CONTEXT: OnceLock<Option<PrivateContext>> = OnceLock::new();
/// Sanity latch: set while the private context is bound, so a nested bind is caught as a bug.
static BOUND: AtomicBool = AtomicBool::new(false);

fn egl() -> Option<&'static Egl> {
    EGL.get_or_init(|| match Egl::load() {
        Ok(e) => Some(e),
        Err(e) => {
            godot::global::godot_warn!("[xreal] egl_context: {e}");
            None
        }
    })
    .as_ref()
}

/// Create the context on first use. Returns `None`, once, with a warning, when any step fails;
/// the bridge then stays disabled (fail closed to the stage-1 behavior).
fn context() -> Option<&'static PrivateContext> {
    CONTEXT
        .get_or_init(|| {
            let e = egl()?;
            unsafe {
                let display = (e.get_display)(EGL_DEFAULT_DISPLAY);
                if display.is_null() {
                    godot::global::godot_warn!("[xreal] egl_context: eglGetDisplay -> null");
                    return None;
                }
                // Initializing an already-initialized display is legal and refcount-free; under
                // Vulkan nobody else has, so this is the real init.
                let (mut major, mut minor) = (0i32, 0i32);
                if (e.initialize)(display, &mut major, &mut minor) == 0 {
                    godot::global::godot_warn!(
                        "[xreal] egl_context: eglInitialize failed, err={:#x}",
                        (e.get_error)()
                    );
                    return None;
                }
                (e.bind_api)(EGL_OPENGL_ES_API);
                let config_attrs: [i32; 17] = [
                    EGL_RENDERABLE_TYPE,
                    EGL_OPENGL_ES3_BIT,
                    EGL_SURFACE_TYPE,
                    EGL_PBUFFER_BIT,
                    EGL_RED_SIZE,
                    8,
                    EGL_GREEN_SIZE,
                    8,
                    EGL_BLUE_SIZE,
                    8,
                    EGL_ALPHA_SIZE,
                    8,
                    EGL_DEPTH_SIZE,
                    0,
                    EGL_STENCIL_SIZE,
                    0,
                    EGL_NONE,
                ];
                let mut config: *mut c_void = std::ptr::null_mut();
                let mut num = 0i32;
                if (e.choose_config)(display, config_attrs.as_ptr(), &mut config, 1, &mut num) == 0
                    || num == 0
                {
                    godot::global::godot_warn!(
                        "[xreal] egl_context: eglChooseConfig found no ES3 pbuffer config, err={:#x}",
                        (e.get_error)()
                    );
                    return None;
                }
                let ctx_attrs: [i32; 3] = [EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE];
                let context =
                    (e.create_context)(display, config, EGL_NO_CONTEXT, ctx_attrs.as_ptr());
                if context.is_null() {
                    godot::global::godot_warn!(
                        "[xreal] egl_context: eglCreateContext failed, err={:#x}",
                        (e.get_error)()
                    );
                    return None;
                }
                // Surfaceless first (Adreno advertises EGL_KHR_surfaceless_context), 1x1 pbuffer
                // as the fallback.
                let exts = {
                    let p = (e.query_string)(display, EGL_EXTENSIONS);
                    if p.is_null() {
                        String::new()
                    } else {
                        std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
                    }
                };
                let mut surface = EGL_NO_SURFACE;
                if !exts.contains("EGL_KHR_surfaceless_context") {
                    let pb_attrs: [i32; 5] = [EGL_WIDTH, 1, EGL_HEIGHT, 1, EGL_NONE];
                    surface = (e.create_pbuffer_surface)(display, config, pb_attrs.as_ptr());
                    if surface.is_null() {
                        godot::global::godot_warn!(
                            "[xreal] egl_context: 1x1 pbuffer creation failed, err={:#x}",
                            (e.get_error)()
                        );
                        return None;
                    }
                }
                godot::global::godot_print!(
                    "[xreal] egl_context: private ES3 context created (EGL {major}.{minor}, \
                     {})",
                    if surface.is_null() {
                        "surfaceless"
                    } else {
                        "1x1 pbuffer"
                    }
                );
                Some(PrivateContext {
                    display,
                    context,
                    surface,
                })
            }
        })
        .as_ref()
}

/// Whether the private context exists or can be created. Creating is cheap and one-shot, so this
/// is also the bridge's "EGL side viable" probe.
pub fn available() -> bool {
    context().is_some()
}

/// Make the private context current on the calling thread. Returns `false` (with a warning) on
/// failure. Call [`unbind`] when the SDK graphics operation is done; the pair is what keeps the
/// context from ever *lingering* current.
pub fn bind() -> bool {
    let (Some(e), Some(c)) = (egl(), context()) else {
        return false;
    };
    if BOUND.swap(true, Ordering::Relaxed) {
        godot::global::godot_warn!("[xreal] egl_context: nested bind (caller bug)");
    }
    let ok = unsafe { (e.make_current)(c.display, c.surface, c.surface, c.context) } != 0;
    if !ok {
        BOUND.store(false, Ordering::Relaxed);
        godot::global::godot_warn!(
            "[xreal] egl_context: eglMakeCurrent failed, err={:#x}",
            unsafe { (e.get_error)() }
        );
    }
    ok
}

/// Release the private context from the calling thread.
pub fn unbind() {
    let (Some(e), Some(c)) = (egl(), context()) else {
        return;
    };
    unsafe { (e.make_current)(c.display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT) };
    BOUND.store(false, Ordering::Relaxed);
}

/// Run `f` with the private context current, unbinding afterwards even on an early return.
/// Returns `None` when the context could not be bound.
pub fn with_current<T>(f: impl FnOnce() -> T) -> Option<T> {
    if !bind() {
        return None;
    }
    let out = f();
    unbind();
    Some(out)
}
