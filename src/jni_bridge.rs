//! Android Activity acquisition for the XREAL session bootstrap.
//!
//! `InitUserDefinedSettings` needs the host `Activity` as a JNI `jobject` (the Unity SDK
//! calls it `unityActivity`). On Android we read it from the process-wide
//! [`ndk_context`]. If nothing has published a context yet, [`activity_ptr`] returns
//! `None` and the session bootstrap reports "no Android Activity" (see `docs/plans/port-plan.md`).
//!
//! **Device-confirmed:** Godot does NOT populate `ndk_context` (it uses its own
//! Java↔native bridge, not the `ndk-context`/`android-activity` crates). So
//! `ndk_context::android_context()` panics with *"android context was not initialized"*.
//! We catch that here — letting it unwind into the session `OnceLock` would leave it
//! uninitialized and re-panic every frame ("Invalid call error code 1337" spam).

use std::ffi::c_void;

/// The Android `Activity` `jobject` pointer to hand to `InitUserDefinedSettings`.
///
/// Returns `None` on non-Android targets and whenever no Android context has been
/// published to the process (the current case under Godot — see module docs).
#[cfg(target_os = "android")]
pub fn activity_ptr() -> Option<*mut c_void> {
    // `android_context()` panics when the process-global context is unset, which is the
    // normal case under Godot. Catch it rather than letting it unwind (panic=unwind is
    // active — gdext relies on it) into the session bootstrap.
    let ctx = std::panic::catch_unwind(ndk_context::android_context).ok()?;
    let activity = ctx.context();
    (!activity.is_null()).then_some(activity)
}

#[cfg(not(target_os = "android"))]
pub fn activity_ptr() -> Option<*mut c_void> {
    None
}

/// JNI entry point called from `XrealBridge.register(Activity)` (see
/// `android/build/src/main/java/com/godot/game/XrealBridge.java`).
///
/// Godot does not populate `ndk_context`, so we do it ourselves: take the host `Activity`
/// from the Java side and publish it (plus the `JavaVM`) into the process-global context
/// that [`activity_ptr`] reads. A global ref is created and intentionally leaked so the
/// `jobject` stays valid for the process lifetime. Guarded so it only initializes once.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_godot_game_XrealBridge_nativeRegisterActivity<'local>(
    env: jni::JNIEnv<'local>,
    _class: jni::objects::JClass<'local>,
    activity: jni::objects::JObject<'local>,
) {
    use std::sync::atomic::{AtomicBool, Ordering};

    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.swap(true, Ordering::SeqCst) {
        return;
    }

    let Ok(vm) = env.get_java_vm() else { return };
    let Ok(global) = env.new_global_ref(&activity) else {
        return;
    };

    let vm_ptr = vm.get_java_vm_pointer() as *mut c_void;
    let activity_ptr = global.as_raw() as *mut c_void;
    // Keep the global ref alive for the whole process (ndk_context stores the raw ptr).
    std::mem::forget(global);

    // SAFETY: both pointers come from valid JNI handles; `vm` outlives the process and
    // the activity is a leaked global ref.
    unsafe { ndk_context::initialize_android_context(vm_ptr, activity_ptr) };
}

/// Glasses hot-plug event counters. The JNI callbacks below run on the Android UI thread
/// (DisplayManager listener), so they only bump these counters; `XrealHeadTracker::process`
/// (Godot main thread) polls them and re-emits as `glasses_connected` / `glasses_disconnected`
/// signals. Counters — not flags — so a fast disconnect→reconnect is never coalesced away.
/// Defined for every target so the node can poll unconditionally (they stay 0 on desktop).
static GLASSES_CONNECT_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static GLASSES_DISCONNECT_COUNT: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

/// Monotonic count of glasses-connected events observed so far.
pub fn glasses_connect_count() -> u32 {
    GLASSES_CONNECT_COUNT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Monotonic count of glasses-disconnected events observed so far.
pub fn glasses_disconnect_count() -> u32 {
    GLASSES_DISCONNECT_COUNT.load(std::sync::atomic::Ordering::Relaxed)
}

/// JNI: the XREAL glasses display was added (`DisplayManager.onDisplayAdded`).
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_godot_game_XrealBridge_nativeOnGlassesConnected<'local>(
    _env: jni::JNIEnv<'local>,
    _class: jni::objects::JClass<'local>,
    _display_id: jni::sys::jint,
) {
    GLASSES_CONNECT_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// JNI: the XREAL glasses display was removed (`DisplayManager.onDisplayRemoved`).
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_godot_game_XrealBridge_nativeOnGlassesDisconnected<'local>(
    _env: jni::JNIEnv<'local>,
    _class: jni::objects::JClass<'local>,
    _display_id: jni::sys::jint,
) {
    GLASSES_DISCONNECT_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}
