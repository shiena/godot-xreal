//! Minimal emulation of Unity's native-plugin interface, enough to drive
//! `libXREALXRPlugin.so`.
//!
//! `libXREALXRPlugin.so` is a **Unity native plugin**: Unity's engine calls
//! `UnityPluginLoad(IUnityInterfaces*)` at startup, and `InitUserDefinedSettings` →
//! `DisplayManager::LoadDisplay` then queries `IUnityGraphics::GetRenderer()` to choose the
//! display backend. Godot never does this, so the stored `IUnityInterfaces*` is null and
//! `LoadDisplay` segfaults (device-confirmed).
//!
//! We provide a tiny `IUnityInterfaces` whose `GetInterface` hands back an `IUnityGraphics`
//! that reports **OpenGL ES 3**, and `NULL` for everything else. RE of `LoadDisplay`
//! (AArch64 disasm) shows that is sufficient: with `renderer == GLES3` the Vulkan-only
//! branch is skipped (`cmp w8,#0x15; b.ne`), and the branch that uses the other requested
//! interface is guarded by a null check (`cbz`), so returning null makes it skip safely.
//!
//! Struct layouts and the GUID/enum constants come from Unity's PUBLIC PluginAPI headers
//! (`IUnityInterface.h`, `IUnityGraphics.h`) — they are not XREAL-specific.

use std::ffi::c_void;
use std::ptr;

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

/// `IUnityGraphics` (IUnityGraphics.h) — a struct of function pointers. The plugin calls
/// the first member (`GetRenderer`) via `ldr x8,[iface]; blr x8`.
#[repr(C)]
struct IUnityGraphics {
    get_renderer: extern "C" fn() -> i32,
    register_device_event_callback: extern "C" fn(*mut c_void),
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

extern "C" fn gfx_get_renderer() -> i32 {
    K_UNITY_GFX_RENDERER_OPENGLES30
}
extern "C" fn gfx_register_device_event_callback(_callback: *mut c_void) {}

static UNITY_GRAPHICS: IUnityGraphics = IUnityGraphics {
    get_renderer: gfx_get_renderer,
    register_device_event_callback: gfx_register_device_event_callback,
};

fn graphics_ptr() -> *mut c_void {
    ptr::addr_of!(UNITY_GRAPHICS) as *mut c_void
}

extern "C" fn get_interface(guid: *const UnityInterfaceGuid) -> *mut c_void {
    if guid.is_null() {
        return ptr::null_mut();
    }
    let guid = unsafe { &*guid };
    if guid.high == IUNITY_GRAPHICS_GUID.high && guid.low == IUNITY_GRAPHICS_GUID.low {
        graphics_ptr()
    } else {
        ptr::null_mut()
    }
}
extern "C" fn register_interface(_guid: *const UnityInterfaceGuid, _ptr: *mut c_void) {}
extern "C" fn get_interface_split(high: u64, low: u64) -> *mut c_void {
    if high == IUNITY_GRAPHICS_GUID.high && low == IUNITY_GRAPHICS_GUID.low {
        graphics_ptr()
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
