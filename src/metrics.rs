//! NRMetrics reader (`libnr_loader.so`) — the XREAL SDK's render-metrics source (present FPS / dropped
//! / early / teared / extended frame counts, composite time, motion-to-photon latency), exposed to
//! GDScript via [`crate::system::XrealSystem`].
//!
//! ## Why not `DisplayManager::UpdateMetrics`
//!
//! The SDK's own metrics loop (`DisplayManager::UpdateMetrics @libXREALXRPlugin.so 0x68974`) is a
//! *reporter/sink*: once per second it fetches the numbers from NR (`NativeMetrics::GetMetricsData` →
//! the same `NRMetrics*` getters we bind here) and then **pushes** them into a Unity stat sink at
//! `DisplayManager+0x68` (`[[DM+0x68]+0x10](propertyId, float)`). Godot installs no such sink, so that
//! slot is garbage and `UpdateMetrics` SIGBUS'd on the render thread ~1 s in — which is why we neuter it
//! (ret at entry, see [`crate::signal_guard`]). The reporter slot is a consumer, **not** a place to put
//! an NR function; the numbers themselves already come from NR. So instead of reviving the Unity sink we
//! read the same source directly.
//!
//! ## Independent handle (RE-confirmed, codex 2026-07-16)
//!
//! `NRMetricsCreate` takes only an out-handle; the handle is a control/query token onto the
//! **process-global** NR compositor metrics service (`libnr_api.so` global at `0x244dde8`), not an
//! accumulator keyed to a rendering/session handle. So a handle we create + start ourselves reads the
//! live runtime's real compositor counters for the frames the app submits — no need to recover the SDK's
//! `NativeMetrics` (`DisplayManager+0x70`). See docs/plans/render-metrics-gdscript-plan.md
//! ("RE addendum 2026-07-16"). The loader trampolines return `1` before the NR runtime is up, so calling
//! `start()` early is safe and simply retries.
//!
//! ABI: every function returns `NRResult` (`i32`, `0` = success); counts are `i32` (teared uses `-1` as
//! an "unavailable" sentinel), times are `u64` nanoseconds. Getters write through an out-pointer and
//! never return the value directly. `NRMetricsGetPresentFps` writes an **`i32`** present rate (~60), not
//! an `f32` — device-confirmed (reading it as `f32` yields denormal garbage ~8.4e-44 = the raw bits of
//! integer 60), correcting the static-RE guess.

use libloading::Library;
use std::sync::Mutex;

type FnCreate = unsafe extern "C" fn(*mut u64) -> i32;
type FnOneHandle = unsafe extern "C" fn(u64) -> i32;
type FnGetI32 = unsafe extern "C" fn(u64, *mut i32) -> i32;
type FnGetU64 = unsafe extern "C" fn(u64, *mut u64) -> i32;
type FnSetFeature = unsafe extern "C" fn(u64, i32, i32) -> i32;

/// `NRMetricsFeature` bitmask values (RE'd from the XREAL Unity SDK's exported
/// `EnableTearedFrameCount` / `EnableRenderBackColor`, which forward to
/// `DisplayManager::EnableTearedFrameCount(bool)` @libXREALXRPlugin.so:0x6dbd0 →
/// `NativeMetrics::SetFeatureEnable(1, enable)` and `EnableRenderBackColor` → `SetFeatureEnable(2, ...)`).
/// `TearedFrameCount` is a metric we read; `RenderBackColor` (2) is a debug *rendering* feature, not a
/// metric, so we do not enable it. Composite time / latency are not feature-gated.
const NR_METRICS_FEATURE_TEARED_FRAME_COUNT: i32 = 1;

struct Metrics {
    _lib: Library, // keep libnr_loader.so mapped for the fn-pointers' lifetime
    stop: Option<FnOneHandle>,
    destroy: Option<FnOneHandle>,
    get_present_fps: Option<FnGetI32>,
    get_dropped_frame_count: Option<FnGetI32>,
    get_early_frame_count: Option<FnGetI32>,
    get_curr_frame_present_count: Option<FnGetI32>,
    get_extended_frame_count: Option<FnGetI32>,
    get_teared_frame_count: Option<FnGetI32>,
    get_frame_composite_time: Option<FnGetU64>,
    get_app_frame_latency: Option<FnGetU64>,
    handle: u64,
}

// SAFETY: the fn-pointers resolve into libnr_loader.so (kept mapped by `_lib`); `handle` is an opaque
// pointer-sized token owned by the NR runtime. Only touched under the Mutex.
unsafe impl Send for Metrics {}

static METRICS: Mutex<Option<Metrics>> = Mutex::new(None);

/// `dlopen` libnr_loader.so, `NRMetricsCreate` + `NRMetricsStart` a metrics handle, and keep it alive.
/// Idempotent and retryable: on failure (e.g. the NR runtime is not up yet — the loader stubs return
/// `1`) nothing is stored, so a later call retries. Returns a one-line diagnostic.
fn start_locked(slot: &mut Option<Metrics>) -> String {
    if let Some(m) = slot.as_ref() {
        return format!("[xreal] render metrics already started (handle={:#x})", m.handle);
    }
    unsafe {
        let lib = match Library::new("libnr_loader.so") {
            Ok(l) => l,
            Err(e) => return format!("[xreal] metrics dlopen failed: {e}"),
        };
        let create = match lib.get::<FnCreate>(b"NRMetricsCreate\0") {
            Ok(f) => *f,
            Err(e) => return format!("[xreal] metrics dlsym NRMetricsCreate failed: {e}"),
        };
        let start_fn = match lib.get::<FnOneHandle>(b"NRMetricsStart\0") {
            Ok(f) => *f,
            Err(e) => return format!("[xreal] metrics dlsym NRMetricsStart failed: {e}"),
        };

        let mut handle: u64 = 0;
        let cr = create(&mut handle);
        if cr != 0 || handle == 0 {
            // NR runtime likely not up yet (loader stub returns 1) — retry on the next call.
            return format!("[xreal] NRMetricsCreate not ready (result={cr})");
        }
        let sr = start_fn(handle);
        if sr != 0 {
            if let Ok(d) = lib.get::<FnOneHandle>(b"NRMetricsDestroy\0") {
                (*d)(handle);
            }
            return format!("[xreal] NRMetricsStart failed (result={sr})");
        }

        // Enable the TearedFrameCount feature so `NRMetricsGetTearedFrameCount` returns a real value
        // instead of an error — the same thing the Unity SDK does via its exported
        // `EnableTearedFrameCount(true)` (→ `NativeMetrics::SetFeatureEnable(1, true)`).
        let set_feature_enable = lib.get::<FnSetFeature>(b"NRMetricsSetFeatureEnable\0").ok().map(|f| *f);
        let teared_feature = set_feature_enable
            .map(|f| f(handle, NR_METRICS_FEATURE_TEARED_FRAME_COUNT, 1))
            .unwrap_or(-1);

        *slot = Some(Metrics {
            stop: lib.get::<FnOneHandle>(b"NRMetricsStop\0").ok().map(|f| *f),
            destroy: lib.get::<FnOneHandle>(b"NRMetricsDestroy\0").ok().map(|f| *f),
            get_present_fps: lib.get::<FnGetI32>(b"NRMetricsGetPresentFps\0").ok().map(|f| *f),
            get_dropped_frame_count: lib
                .get::<FnGetI32>(b"NRMetricsGetDroppedFrameCount\0")
                .ok()
                .map(|f| *f),
            get_early_frame_count: lib
                .get::<FnGetI32>(b"NRMetricsGetEarlyFrameCount\0")
                .ok()
                .map(|f| *f),
            get_curr_frame_present_count: lib
                .get::<FnGetI32>(b"NRMetricsGetCurrFramePresentCount\0")
                .ok()
                .map(|f| *f),
            get_extended_frame_count: lib
                .get::<FnGetI32>(b"NRMetricsGetExtendedFrameCount\0")
                .ok()
                .map(|f| *f),
            get_teared_frame_count: lib
                .get::<FnGetI32>(b"NRMetricsGetTearedFrameCount\0")
                .ok()
                .map(|f| *f),
            get_frame_composite_time: lib
                .get::<FnGetU64>(b"NRMetricsGetFrameCompositeTime\0")
                .ok()
                .map(|f| *f),
            get_app_frame_latency: lib
                .get::<FnGetU64>(b"NRMetricsGetAppFrameLatency\0")
                .ok()
                .map(|f| *f),
            _lib: lib,
            handle,
        });
        format!(
            "[xreal] render metrics started (handle={handle:#x}, TearedFrameCount enable={teared_feature})"
        )
    }
}

/// Ensure the metrics handle is created + started, then run `f` with the live handle. Returns `None`
/// when the metrics handle could not be started yet (NR runtime not up / symbols missing).
fn with_metrics<T>(f: impl FnOnce(&Metrics) -> Option<T>) -> Option<T> {
    let mut slot = METRICS.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
        let _ = start_locked(&mut slot);
    }
    slot.as_ref().and_then(f)
}

/// Force a start attempt and return the one-line diagnostic (for a GDScript status readout).
pub fn diagnostics() -> String {
    let mut slot = METRICS.lock().unwrap_or_else(|e| e.into_inner());
    start_locked(&mut slot)
}

macro_rules! get_i32 {
    ($name:ident, $field:ident) => {
        /// `None` when the metrics handle is not available or the getter reports an error.
        pub fn $name() -> Option<i32> {
            with_metrics(|m| {
                let f = m.$field?;
                let mut v: i32 = 0;
                (unsafe { f(m.handle, &mut v) } == 0).then_some(v)
            })
        }
    };
}

get_i32!(dropped_frame_count, get_dropped_frame_count);
get_i32!(early_frame_count, get_early_frame_count);
get_i32!(frame_present_count, get_curr_frame_present_count);
get_i32!(extended_frame_count, get_extended_frame_count);
get_i32!(teared_frame_count, get_teared_frame_count);

/// Present rate in frames/second (integer, ~60). `None` when the metrics handle is not available.
pub fn present_fps() -> Option<i32> {
    with_metrics(|m| {
        let f = m.get_present_fps?;
        let mut v: i32 = 0;
        (unsafe { f(m.handle, &mut v) } == 0).then_some(v)
    })
}

/// Composite time in nanoseconds. `None` when unavailable.
pub fn frame_composite_time_ns() -> Option<u64> {
    with_metrics(|m| {
        let f = m.get_frame_composite_time?;
        let mut v: u64 = 0;
        (unsafe { f(m.handle, &mut v) } == 0).then_some(v)
    })
}

/// App frame latency (motion-to-photon input) in nanoseconds. `None` when unavailable.
pub fn app_frame_latency_ns() -> Option<u64> {
    with_metrics(|m| {
        let f = m.get_app_frame_latency?;
        let mut v: u64 = 0;
        (unsafe { f(m.handle, &mut v) } == 0).then_some(v)
    })
}

/// Stop + destroy the metrics handle (best-effort). Called on session teardown.
#[allow(dead_code)]
pub fn shutdown() {
    let mut slot = METRICS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = slot.take() {
        unsafe {
            if let Some(stop) = m.stop {
                stop(m.handle);
            }
            if let Some(destroy) = m.destroy {
                destroy(m.handle);
            }
        }
    }
}
