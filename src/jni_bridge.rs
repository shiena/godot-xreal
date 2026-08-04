//! Android Activity acquisition for the XREAL session bootstrap.
//!
//! `InitUserDefinedSettings` needs the host `Activity` as a JNI `jobject`, which the Unity SDK
//! calls `unityActivity`. On Android we read it from the process-wide [`ndk_context`]. When
//! nothing has published a context yet, [`activity_ptr`] returns `None` and the session bootstrap
//! reports "no Android Activity" (see `docs/develop/plans/port-plan.md`).
//!
//! **Device-confirmed:** Godot does NOT populate `ndk_context`, because it uses its own Java and
//! native bridge rather than the `ndk-context` or `android-activity` crates, so
//! `ndk_context::android_context()` panics with *"android context was not initialized"*.
//! We catch that here: letting it unwind into the session `OnceLock` would leave it
//! uninitialized and re-panic every frame, spamming "Invalid call error code 1337".

use std::ffi::c_void;

/// The Android `Activity` `jobject` pointer to hand to `InitUserDefinedSettings`.
///
/// Returns `None` on non-Android targets, and whenever no Android context has been published to
/// the process, which is the current case under Godot; see the module docs.
#[cfg(target_os = "android")]
pub fn activity_ptr() -> Option<*mut c_void> {
    // `android_context()` panics when the process-global context is unset, which is the normal case
    // under Godot. Catch it rather than letting it unwind into the session bootstrap; panic=unwind is
    // active, because gdext relies on it.
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
/// Godot does not populate `ndk_context`, so we do it ourselves: take the host `Activity` from the
/// Java side and publish it, along with the `JavaVM`, into the process-global context that
/// [`activity_ptr`] reads. A global ref is created and intentionally leaked so the `jobject` stays
/// valid for the process lifetime. It is guarded so it initializes only once.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_godot_game_XrealBridge_nativeRegisterActivity<'local>(
    env: jni::JNIEnv<'local>,
    _class: jni::objects::JClass<'local>,
    activity: jni::objects::JObject<'local>,
) {
    use std::sync::atomic::{AtomicBool, Ordering};

    static REGISTERED: AtomicBool = AtomicBool::new(false);
    // Fast path: already registered. The real claim is the CAS below, AFTER both JNI handles succeed,
    // so a transient failure here never permanently blocks a later registration attempt.
    if REGISTERED.load(Ordering::SeqCst) {
        return;
    }

    let Ok(vm) = env.get_java_vm() else { return };
    let Ok(global) = env.new_global_ref(&activity) else {
        return;
    };

    // Both handles are in hand, so claim registration now. The CAS makes this the sole winner even
    // when two threads passed the load above, and the loser drops its global ref and returns.
    if REGISTERED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    let vm_ptr = vm.get_java_vm_pointer() as *mut c_void;
    let activity_ptr = global.as_raw() as *mut c_void;
    // Keep the global ref alive for the whole process (ndk_context stores the raw ptr).
    std::mem::forget(global);

    // SAFETY: both pointers come from valid JNI handles; `vm` outlives the process and
    // the activity is a leaked global ref.
    unsafe { ndk_context::initialize_android_context(vm_ptr, activity_ptr) };
}

/// The `MediaProjection` the user consented to, as a raw `jobject`, or null when there is none.
///
/// `HWEncoderSetMediaProjection` wants exactly this, and the SDK's C# passes
/// `AndroidJavaObject.GetRawObject()`. A null projection is not a neutral value: reverse
/// engineering showed `addInternalAudio:true` builds an `AudioPlaybackCaptureConfiguration` from
/// it (see `docs/develop/archive/codex-audio-mix-analysis.md`), so a null one leaves app-audio capture
/// unstarted and the encoder's mixer with nothing to add to the microphone.
///
/// It is backed by a global ref we own (see [`MEDIA_PROJECTION_OWNER`]), because Java-side
/// ownership ends the moment `onProjectionReady` returns while the encoder needs the object for
/// the whole capture.
static MEDIA_PROJECTION: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Owns the global ref behind [`MEDIA_PROJECTION`] so it can be released instead of leaked.
///
/// The release rule is deliberate and narrow: **the ref is dropped only when a new projection
/// replaces it, and only after the new ref has been created**, never on a revoke. Two facts from
/// disassembling `libmedia_codec.so` force that shape:
///
/// * `HWEncoderSetMediaProjection` takes its **own** `NewGlobalRef` on the object, and the holder
///   it builds stores `JNIEnv*`, that new ref, and our raw `jobject`. Releasing our ref therefore
///   cannot dangle the encoder's copy, and the leak this replaces was never protecting it.
/// * That holder keeps **our raw `jobject` value** and compares it against the next projection it
///   is handed, skipping the call when they are equal. Had we released our ref first, JNI could
///   hand the same handle value back for the next grant and the encoder would silently keep using
///   the revoked projection. Creating the new ref while the old one is still alive makes that
///   impossible: JNI cannot reuse a live handle.
///
/// Holding the ref past a revoke costs one live global ref until the next grant, which is bounded,
/// unlike the unbounded one-per-grant leak this replaces.
#[cfg(target_os = "android")]
static MEDIA_PROJECTION_OWNER: std::sync::Mutex<Option<jni::objects::GlobalRef>> =
    std::sync::Mutex::new(None);

/// Raw `jobject` of the consented `MediaProjection`, or null. Null on desktop.
pub fn media_projection_ptr() -> *mut c_void {
    MEDIA_PROJECTION.load(std::sync::atomic::Ordering::Acquire)
}

/// JNI: called from `XrealProjection.onProjectionReady`, and with null when it is revoked.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_godot_game_XrealProjection_nativeSetMediaProjection<'local>(
    env: jni::JNIEnv<'local>,
    _class: jni::objects::JClass<'local>,
    projection: jni::objects::JObject<'local>,
) {
    use std::sync::atomic::Ordering;

    if projection.is_null() {
        // Revoked: readers must stop handing this to the encoder immediately, but the ref itself stays
        // alive until a new projection replaces it. See MEDIA_PROJECTION_OWNER.
        MEDIA_PROJECTION.store(std::ptr::null_mut(), Ordering::Release);
        return;
    }
    let Ok(global) = env.new_global_ref(&projection) else {
        return;
    };
    let raw = global.as_raw() as *mut c_void;
    // Order matters: the new ref already exists here, so the previous one (dropped below, which is
    // what actually calls DeleteGlobalRef) cannot have its handle value recycled into `raw`.
    let previous = MEDIA_PROJECTION_OWNER
        .lock()
        .expect("media projection owner mutex")
        .replace(global);
    MEDIA_PROJECTION.store(raw, Ordering::Release);
    drop(previous);
}

/// Glasses hot-plug event counters. The JNI callbacks below run on the Android UI thread, as a
/// DisplayManager listener, so they only bump these counters, and `XrealHeadTracker::process` polls
/// them on the Godot main thread and re-emits them as `glasses_connected` and
/// `glasses_disconnected` signals. They are counters rather than flags, so a fast disconnect
/// followed by a reconnect is never coalesced away. They are defined for every target so the node
/// can poll unconditionally, and they stay 0 on desktop.
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
