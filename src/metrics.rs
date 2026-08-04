//! NRMetrics reader (`libnr_loader.so`), the XREAL SDK's render-metrics source: the present FPS and
//! the dropped, early, teared and extended frame counts, the composite time and the
//! motion-to-photon latency, exposed to GDScript through [`crate::system::XrealSystem`].
//!
//! ## Why not `DisplayManager::UpdateMetrics`
//!
//! The SDK's own metrics loop (`DisplayManager::UpdateMetrics @libXREALXRPlugin.so 0x68974`) is a
//! *reporter*, a sink: once per second it fetches the numbers from NR through
//! `NativeMetrics::GetMetricsData`, which calls the same `NRMetrics*` getters we bind here, and then
//! **pushes** them into a Unity stat sink at `DisplayManager+0x68`
//! (`[[DM+0x68]+0x10](propertyId, float)`). Godot installs no such sink, so that slot is garbage and
//! `UpdateMetrics` SIGBUS'd on the render thread about 1 s in, which is why we neuter it with a ret
//! at entry (see [`crate::signal_guard`]). The reporter slot is a consumer, **not** a place to put
//! an NR function, and the numbers themselves already come from NR. So instead of reviving the Unity
//! sink we read the same source directly.
//!
//! ## Independent handle (RE-confirmed, codex 2026-07-16)
//!
//! `NRMetricsCreate` takes only an out-handle, and that handle is a control and query token onto the
//! **process-global** NR compositor metrics service, a `libnr_api.so` global at `0x244dde8`, rather
//! than an accumulator keyed to a rendering or session handle. A handle we create and start
//! ourselves therefore reads the live runtime's real compositor counters for the frames the app
//! submits, with no need to recover the SDK's `NativeMetrics` at `DisplayManager+0x70`. See
//! docs/develop/plans/render-metrics-gdscript-plan.md, "RE addendum 2026-07-16". The loader trampolines
//! return `1` before the NR runtime is up, so calling `start()` early is safe and simply retries.
//!
//! ABI: every function returns `NRResult`, an `i32` where `0` is success. Counts are `i32`, with
//! teared using `-1` as an "unavailable" sentinel, and times are `u64` nanoseconds. The getters
//! write through an out-pointer and never return the value directly. `NRMetricsGetPresentFps`
//! writes an **`i32`** present rate of about 60, not an `f32`. That is device-confirmed: reading it
//! as `f32` yields denormal garbage around 8.4e-44, the raw bits of the integer 60, which corrects
//! the static-RE guess.

use libloading::Library;
use std::sync::Mutex;

type FnCreate = unsafe extern "C" fn(*mut u64) -> i32;
type FnOneHandle = unsafe extern "C" fn(u64) -> i32;
type FnGetI32 = unsafe extern "C" fn(u64, *mut i32) -> i32;
type FnGetU64 = unsafe extern "C" fn(u64, *mut u64) -> i32;
type FnSetFeature = unsafe extern "C" fn(u64, i32, i32) -> i32;

/// `NRMetricsFeature` bitmask values, RE'd from the XREAL Unity SDK's exported
/// `EnableTearedFrameCount` and `EnableRenderBackColor`. The first forwards to
/// `DisplayManager::EnableTearedFrameCount(bool)` @libXREALXRPlugin.so:0x6dbd0, which calls
/// `NativeMetrics::SetFeatureEnable(1, enable)`, and the second calls `SetFeatureEnable(2, ...)`.
/// `TearedFrameCount` is a metric we read, while `RenderBackColor` (2) is a debug *rendering*
/// feature rather than a metric, so we leave it off. Composite time and latency are not
/// feature-gated.
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

// SAFETY: the fn-pointers resolve into libnr_loader.so, which `_lib` keeps mapped, and `handle` is
// an opaque pointer-sized token owned by the NR runtime. It is touched only under the Mutex.
unsafe impl Send for Metrics {}

static METRICS: Mutex<Option<Metrics>> = Mutex::new(None);

/// `dlopen` libnr_loader.so, run `NRMetricsCreate` and `NRMetricsStart` on a metrics handle, and
/// keep it alive. It is idempotent and retryable: on failure, for instance while the NR runtime is
/// not up yet and the loader stubs return `1`, nothing is stored, so a later call retries. It
/// returns a one-line diagnostic.
fn start_locked(slot: &mut Option<Metrics>) -> String {
    if let Some(m) = slot.as_ref() {
        return format!(
            "[xreal] render metrics already started (handle={:#x})",
            m.handle
        );
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
            // The NR runtime is likely not up yet, since the loader stub returns 1, so retry on the next call.
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
        // instead of an error. The Unity SDK does the same through its exported
        // `EnableTearedFrameCount(true)`, which calls `NativeMetrics::SetFeatureEnable(1, true)`.
        let set_feature_enable = lib
            .get::<FnSetFeature>(b"NRMetricsSetFeatureEnable\0")
            .ok()
            .map(|f| *f);
        let teared_feature = set_feature_enable
            .map(|f| f(handle, NR_METRICS_FEATURE_TEARED_FRAME_COUNT, 1))
            .unwrap_or(-1);

        *slot = Some(Metrics {
            stop: lib.get::<FnOneHandle>(b"NRMetricsStop\0").ok().map(|f| *f),
            destroy: lib
                .get::<FnOneHandle>(b"NRMetricsDestroy\0")
                .ok()
                .map(|f| *f),
            get_present_fps: lib
                .get::<FnGetI32>(b"NRMetricsGetPresentFps\0")
                .ok()
                .map(|f| *f),
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

/// Ensure the metrics handle is created and started, then run `f` with the live handle. It returns
/// `None` when the handle could not be started yet, whether the NR runtime is not up or the symbols
/// are missing.
fn with_metrics<T>(f: impl FnOnce(&Metrics) -> Option<T>) -> Option<T> {
    let mut slot = METRICS.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
        let _ = start_locked(&mut slot);
    }
    slot.as_ref().and_then(f)
}

/// Force a start attempt and return the one-line diagnostic, for a GDScript status readout.
pub fn diagnostics() -> String {
    let mut slot = METRICS.lock().unwrap_or_else(|e| e.into_inner());
    start_locked(&mut slot)
}

macro_rules! get_i32 {
    ($name:ident, $field:ident) => {
        /// `None` when the metrics handle is unavailable or the getter reports an error.
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

/// Present rate in frames per second, an integer around 60. `None` when the handle is unavailable.
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

/// App frame latency, the motion-to-photon input, in nanoseconds. `None` when unavailable.
pub fn app_frame_latency_ns() -> Option<u64> {
    with_metrics(|m| {
        let f = m.get_app_frame_latency?;
        let mut v: u64 = 0;
        (unsafe { f(m.handle, &mut v) } == 0).then_some(v)
    })
}

/// Stop and destroy the metrics handle, best-effort. Called on session teardown.
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
