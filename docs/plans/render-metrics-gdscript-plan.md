# Render Metrics → GDScript

Status: **IMPLEMENTED 2026-07-16 (device-verified).** Exposes the XREAL SDK's "Render Metrics" (present
FPS / dropped / early / latency telemetry) to GDScript so an app can read the compositor stats at runtime.

## Implemented design (what shipped)

We do **not** revive `DisplayManager::UpdateMetrics`. Its per-frame path is a Unity *stat sink* (it pushes
the numbers it fetches from NR into a reporter callback at `DM+0x68` that Godot doesn't provide), so we
keep it neutered (ret at entry, `src/signal_guard.rs`). Instead we read the **same source** it reads —
the process-global NR compositor metrics service — by binding the flat `NRMetrics*` C API directly:

- `src/metrics.rs` — a self-contained module (mirrors `src/controller_probe.rs`): `dlopen`
  libnr_loader.so, `NRMetricsCreate` + `NRMetricsStart` our **own** handle (lazily, retrying until the NR
  runtime is up — the loader trampolines return `1` before then), then the `NRMetricsGet*` getters.
  The handle is a control/query token onto the global metrics service, **not** keyed to a rendering /
  session handle, so an independently-created handle reads the live runtime's real counters (RE verdict
  confirmed on device — see the addendum below).
- `src/system.rs` — `XrealSystem` `#[func]` getters: `get_present_fps()`, `get_dropped_frame_count()`,
  `get_early_frame_count()`, `get_frame_present_count()`, `get_extended_frame_count()`,
  `get_teared_frame_count()`, `get_frame_composite_time_ms()`, `get_app_frame_latency_ms()`,
  `get_render_metrics_diagnostics()`. Counts return `-1` / times & fps return `-1.0` until the handle is up.

### Device results (2026-07-16, One Pro, `192.168.0.4:5555`)

Polled from the render tick over ~16 s while the app rendered:

```
fps=61  present=1  dropped=2→0→1→51→56 (varies, live)  early=0  extended=0
teared=None (getter returns non-zero NRResult on this device)  composite_ns=0
latency_ns≈22–28 ms (motion-to-photon, sane)
```

- **Independent-handle verdict confirmed:** our own `NRMetricsCreate`d handle returns live, varying
  counters (dropped/latency change between polls) — proving the NR metrics service updates continuously
  per submitted frame, independent of `UpdateMetrics`. So GDScript can poll the getters at any cadence.
- **`NRMetricsGetPresentFps` is `i32`, not `f32`** (device-corrected the static-RE guess): it writes an
  integer present rate (~60/61). Reading it as `f32` yielded denormal garbage (~8.4e-44 = the raw bits of
  60). `src/metrics.rs` reads it as `i32`.
- **`GetTearedFrameCount` needed `NRMetricsSetFeatureEnable` — now wired** (2026-07-16 follow-up). The
  teared getter returned a non-zero `NRResult` until the feature was enabled. `NRMetricsFeature` was RE'd
  from the XREAL Unity SDK's own exported `EnableTearedFrameCount` / `EnableRenderBackColor` (which the
  reference app toggles via `XREALDisplaySubsystemExtensions.Enable*` → the exported plugin funcs at
  `libXREALXRPlugin.so 0x47fb4/0x47fcc` → `DisplayManager::EnableTearedFrameCount(bool)` @0x6dbd0 /
  `EnableRenderBackColor(bool)` @0x6dbf4 → `NativeMetrics::SetFeatureEnable(feature, enable)` @0x93abc):
  **`NRMetricsFeature` = `1` TearedFrameCount, `2` RenderBackColor** (a bitmask; feature 2 is a debug
  *rendering* feature, not a metric, so we do not enable it). `src/metrics.rs` now calls
  `NRMetricsSetFeatureEnable(handle, 1, 1)` right after `NRMetricsStart` — the NR-level equivalent of the
  SDK's `EnableTearedFrameCount(true)`. Device-confirmed: teared then reads `0` (and `1` on a torn frame)
  instead of the `None` error.
- **`GetFrameCompositeTime` reads `0`** on this device. It is **not** feature-gated (no `Enable*` wrapper
  exists for it, and its getter returns success `0`, not an error), so it is a real `0` here — likely
  populated only under specific compositor conditions. No action available from our side.

Everything below is the original plan / RE notes, kept for reference.

---

## Original plan (superseded by the section above)

Status was: **not started (planned).** Expose the XREAL SDK's "Render Metrics" (per-frame present/drop/early
telemetry) to GDScript so an app can read FPS / dropped-frame / latency stats at runtime.

## Background

The SDK documents a **Render Metrics** feature that logs, once per second:

```
FrameMetrics: FPS=60, frNum=61, UpdPrd=…, prd=…, postPrd=…, sdkPrd=…, FPC=1.08, EFC=0, early=0, drop=8 …
```

That log is produced by `DisplayManager::UpdateMetrics` (libXREALXRPlugin.so @0x68974), which dispatches
a metrics **reporter callback** (`blr [[DM+0x68]+0x18]`) and then reads `NativeMetrics::GetMetricsData`.
In Unity, the engine installs that reporter; Godot doesn't, so the slot is garbage (`0x13`) and
`UpdateMetrics` SIGBUS'd on the GL thread ~1 s in.

**Our current fix (`reassert_update_metrics_on_render_thread`, commit `6116aa8`) neuters `UpdateMetrics`
by returning at its entry** — so the SDK's built-in metrics loop no longer runs at all. Metrics are
therefore **not** available via `UpdateMetrics`, by design (it was only ever a crash for us).

## The metrics are still obtainable — call the NR metrics API directly

`libnr_loader.so` exports the full **`NRMetrics*` flat C API** (dlsym-able exactly like the `NRController*`
API we already bind in `src/controller_probe.rs` and the `NRRendering*` API in `src/native.rs`). The NR
compositor (libnr_api) tracks present/drop/early for the frames we submit via `SubmitCurrentFrame`
regardless of whether `UpdateMetrics` runs, so a direct query returns live numbers.

Exported functions (verified in the vendored `libnr_loader.so`):

- Lifecycle: `NRMetricsCreate`, `NRMetricsStart`, `NRMetricsStop`, `NRMetricsPause`, `NRMetricsResume`,
  `NRMetricsDestroy`, `NRMetricsSetFeatureEnable`.
- Getters (map 1:1 to the doc's `FrameMetrics` fields):

  | SDK `FrameMetrics` field | NR getter |
  |---|---|
  | FPS | `NRMetricsGetPresentFps` |
  | drop / dropA | `NRMetricsGetDroppedFrameCount` |
  | early / earlyA | `NRMetricsGetEarlyFrameCount` |
  | FPC (single-frame present count) | `NRMetricsGetCurrFramePresentCount` |
  | prediction / app latency | `NRMetricsGetAppFrameLatency` |
  | composite time | `NRMetricsGetFrameCompositeTime` |
  | ExtendedFrame | `NRMetricsGetExtendedFrameCount` |
  | teared frames | `NRMetricsGetTearedFrameCount` |

## Implementation sketch

Mirror the existing NR-API bindings — no new patterns needed:

1. **Bind** the `NRMetrics*` fn-pointers from `libnr_loader.so` in `src/native.rs` (same `Library::get`
   dlsym approach as `NrRenderingApi` / the controller probe). Add the `NRMetrics*` typedefs to
   `src/ffi.rs`.
2. **Create + start** an `NRMetrics` handle during session bootstrap (`src/session.rs::try_start`, after
   `create_session` / on the render thread if it needs a live rendering context) and keep it in the
   session; `Stop`/`Destroy` on teardown.
3. **Expose** via `XrealSystem` `#[func]` pollable getters (same shape as `get_glasses_temperature_level`
   / `get_last_native_error_code`), e.g. `get_present_fps() -> f64`, `get_dropped_frame_count() -> i64`,
   `get_early_frame_count()`, `get_frame_present_count()`, `get_app_frame_latency()`. GDScript then reads
   `XrealSystem.new().get_dropped_frame_count()` each frame (or on a timer).

## Open question to verify on-device (one probe)

`NRMetricsGet*` take a handle from `NRMetricsCreate`. Confirm whether **our own** handle reports the
runtime's real counts, or whether the numbers only populate on the handle the SDK's `DisplayManager`
already created/started (`NativeMetrics` wrapping an `NRMetricsWrapper`, at `DM+0x70`). The API looks
like a per-runtime global query (so an independent handle should work), but create+start+get once on
device and check for non-zero, sane values before wiring up the getters. If an independent handle reads
zero, fall back to reaching the SDK's `NativeMetrics::GetMetricsData` (@0x92ea4) via `DM+0x70`.

## References

- Native-API binding patterns to copy: `src/controller_probe.rs` (NRController), `src/native.rs`
  (`NrRenderingApi` / `NrSwapchain*`), and the `XrealSystem` pollable getters in `src/system.rs`.
- Why `UpdateMetrics` is neutered: `docs/archive/multiview-investigation.md` ("RESOLVED 2026-07-16").

## RE addendum 2026-07-16 — exact NRMetrics ABI + independent-handle verdict

### Verdict

Bind and own a new `NRMetrics` handle. It is not necessary to recover the Unity plugin's
`NativeMetrics` or `DisplayManager` instance. The metrics handle is an opaque pointer-sized token, but
it is **not constructed from, or keyed by, an `NRRenderingHandle` or `NRSessionHandle`**:

- `NRMetricsCreate` in the loader (`libnr_loader.so` `0x1f5e78`) forwards only the caller's `x0`.
  `NativeMetrics::Create` passes `x0 = this + 0x20` at `libXREALXRPlugin.so:0x92b48..0x92b54`, proving
  that the sole argument is `NRMetricsHandle *out_handle`.
- `libnr_api.so` resolves that name to `0xc0c324` (`NRGetProcAddr` comparison
  `0xc0c12c..0xc0c13c`, result `0xc0c260..0xc0c268`). The shim moves the caller's out pointer to `x1`
  and calls the process-global NR service object at `0x244dde8` (`0xc0c32c..0xc0c340`). There is no
  rendering/session/context argument from which a per-rendering accumulator could be selected.
- Start similarly resolves to `0xc0c34c` (`0xc0c140..0xc0c150`, result `0xc0c26c`) and sends just the
  metrics token to the same global service (`0xc0c354..0xc0c368`). The getters resolve to
  `0xc0c394..0xc0c46c`; each inserts the same global service object as backend argument 0 and forwards
  `(handle, out_value)` as backend arguments 1 and 2.

Therefore separate client handles are control/query handles onto the shared compositor metrics
service; they do not accumulate only frames submitted through that handle (a metrics handle cannot
submit or identify frames). A separately created and started handle reads the active runtime's real
compositor counters. `NRMetricsStart` enables collection/querying for that metrics client; it does not
attach the client to a rendering handle. This is a static ABI/backend verdict, not an on-device value
probe; retain a one-shot sanity log (FPS near the display refresh rate and increasing present count) to
detect a service/version regression.

If a future SDK version contradicts this verdict and returns only zeroes, the known fallback is the
SDK-owned token at `DisplayManager + 0x70` (`NativeMetrics`) then `+0x20` (token), as demonstrated by
loads at `0x92eec` and `0x92d04`. Recovering that token would require first capturing the
`DisplayManager *` at an existing instance-bearing patch/call site (for example the render-thread
`signal_guard`/`UpdateMetrics` site); it is not available from the gfx provider context by a verified
public offset. Do not invent a provider-context offset without new RE evidence.

### Exact flat C ABI

All functions return `NRResult` in `w0`; `0` is success. Unity branches to its success path with
`cbz w0` after Create (`0x92b58`), Start (`0x92d10`) and every getter (first at `0x92efc`). The loader's
missing-dispatch path returns `1` (for example `0x1f5ea0`, repeated in every trampoline). No getter
returns its metric directly in `w0` or `s0`: every getter takes `(x0 = handle, x1 = out pointer)` and
returns only `NRResult` in `w0`.

```c
typedef int32_t NRResult;
typedef void *NRMetricsHandle;

NRResult NRMetricsCreate(NRMetricsHandle *out_handle);
NRResult NRMetricsStart(NRMetricsHandle handle);
NRResult NRMetricsStop(NRMetricsHandle handle);
NRResult NRMetricsGetCurrFramePresentCount(NRMetricsHandle handle, int32_t *out_value);
NRResult NRMetricsGetExtendedFrameCount(NRMetricsHandle handle, int32_t *out_value);
NRResult NRMetricsGetTearedFrameCount(NRMetricsHandle handle, int32_t *out_value);
NRResult NRMetricsGetEarlyFrameCount(NRMetricsHandle handle, int32_t *out_value);
NRResult NRMetricsGetDroppedFrameCount(NRMetricsHandle handle, int32_t *out_value);
NRResult NRMetricsGetFrameCompositeTime(NRMetricsHandle handle, uint64_t *out_ns);
NRResult NRMetricsGetAppFrameLatency(NRMetricsHandle handle, uint64_t *out_ns);
NRResult NRMetricsGetPresentFps(NRMetricsHandle handle, float *out_fps);
NRResult NRMetricsDestroy(NRMetricsHandle handle);
NRResult NRMetricsPause(NRMetricsHandle handle);
NRResult NRMetricsResume(NRMetricsHandle handle);
NRResult NRMetricsSetFeatureEnable(NRMetricsHandle handle, int32_t feature, int32_t enable);
```

The lifecycle trampolines are at loader `0x1f5e78`, `0x1f5eac`, `0x1f5ee0`, `0x1f60b4`,
`0x1f60e8`, and `0x1f611c`. Getter trampolines are `0x1f5f14`, `0x1f5f48`, `0x1f5f7c`,
`0x1f5fb0`, `0x1f5fe4`, `0x1f6018`, `0x1f604c`, and `0x1f6080`. `SetFeatureEnable` is
`0x1f6150`; its backend shim at `libnr_api.so:0xc0c51c..0xc0c53c` explicitly forwards the two
32-bit values through `w2/w3`, after the handle in `x1`. `enable` is an integer boolean (`0`/`1`).

The signed count type is established by the Unity `FrameMetrics` layout: the result block is zeroed,
then its `TearedFrameCount` word is initialized to `-1` with `str w8, [out + 0x8]` at
`0x92ed4..0x92ee4`. Getter out addresses are `out+0x0` (`0x92ef0`), `+0x4` (`0x93048`), `+0x8`
(`0x931c4`), `+0x0c`, and `+0x10`; the 64-bit fields are aligned at `+0x18` (`0x935fc`) and `+0x20`
(`0x93770`), and FPS is the 32-bit field at `+0x28` (`0x938e4`). Thus counts are `int32_t`, times are
`uint64_t`, and FPS is `float`, not direct scalar return values.

### FrameMetrics mapping and units

| NR getter | Unity `FrameMetrics` offset | SDK/log meaning | Native type and unit |
|---|---:|---|---|
| `GetCurrFramePresentCount` | `+0x00` | `FPC` / `framePresentCount` | `int32_t`, count for current frame |
| `GetExtendedFrameCount` | `+0x04` | `EFC` / `ExtendedFrameCount` | `int32_t`, frames |
| `GetTearedFrameCount` | `+0x08` | `TearedFrameCount` | `int32_t`, frames (`-1` sentinel when unavailable) |
| `GetEarlyFrameCount` | `+0x0c` | `early` / `EarlyFrameCount` | `int32_t`, frames |
| `GetDroppedFrameCount` | `+0x10` | `drop` / `droppedFrameCount` | `int32_t`, frames |
| `GetFrameCompositeTime` | `+0x18` | composite/post-composite timing | `uint64_t`, nanoseconds |
| `GetAppFrameLatency` | `+0x20` | prediction / motion-to-photon input | `uint64_t`, nanoseconds |
| `GetPresentFps` | `+0x28` | `FPS` | `float`, frames/second |

The call/field sequence is visible in `NativeMetrics::GetMetricsData`: wrapper slots `+0x18` and
`+0x20` at `0x92ee8..0x92ef8` and `0x93040..0x93050`, slot `+0x28` at `0x931bc..0x931cc`, and the
last three at `0x935f4..0x93604`, `0x93768..0x93778`, and `0x938dc..0x938ec`. The two time values are
integer nanoseconds; convert to seconds with `value as f64 * 1e-9` (the same scale used for Unity's
motion-to-photon report), or to milliseconds with `* 1e-6`. `frNum` and display refresh rate are
derived/report-side values, not additional `NRMetrics*` getters in this ABI.

### Create/start and thread requirements

`Create` needs only a writable out-handle. It does not accept rendering, session, Android context,
EGL, or GL state (`libXREALXRPlugin.so:0x92b48..0x92b54`; backend `0xc0c324..0xc0c340`). `Start`
needs only that non-null handle (`0x92d00..0x92d0c`; backend `0xc0c34c..0xc0c368`). The backend paths
dispatch through the global NR service/IPC layer and contain no EGL/GL calls or graphics-context
arguments, so Create/Start/get/stop/destroy are not GL-thread-affine. They may run on the session/main
thread. Start after the NR session/rendering service is live so returned values are meaningful, even
though liveness is not an ABI precondition and no rendering handle is passed. Polling from one thread
and keeping lifecycle mutation serialized is the conservative ownership model.

### Ready-to-paste Rust binding

```rust
use libloading::Library;
use std::ffi::c_void;

type NrResult = i32;
type NrMetricsHandle = *mut c_void;
type FnNrMetricsCreate = unsafe extern "C" fn(*mut NrMetricsHandle) -> NrResult;
type FnNrMetricsHandle = unsafe extern "C" fn(NrMetricsHandle) -> NrResult;
type FnNrMetricsGetI32 = unsafe extern "C" fn(NrMetricsHandle, *mut i32) -> NrResult;
type FnNrMetricsGetU64 = unsafe extern "C" fn(NrMetricsHandle, *mut u64) -> NrResult;
type FnNrMetricsGetF32 = unsafe extern "C" fn(NrMetricsHandle, *mut f32) -> NrResult;
type FnNrMetricsSetFeature = unsafe extern "C" fn(NrMetricsHandle, i32, i32) -> NrResult;

struct NrMetricsApi {
    // Keep the loader alive while any resolved pointer can be called.
    _lib: Library,
    create: FnNrMetricsCreate,
    start: FnNrMetricsHandle,
    stop: FnNrMetricsHandle,
    get_curr_frame_present_count: FnNrMetricsGetI32,
    get_extended_frame_count: FnNrMetricsGetI32,
    get_teared_frame_count: FnNrMetricsGetI32,
    get_early_frame_count: FnNrMetricsGetI32,
    get_dropped_frame_count: FnNrMetricsGetI32,
    get_frame_composite_time: FnNrMetricsGetU64,
    get_app_frame_latency: FnNrMetricsGetU64,
    get_present_fps: FnNrMetricsGetF32,
    destroy: FnNrMetricsHandle,
    pause: FnNrMetricsHandle,
    resume: FnNrMetricsHandle,
    set_feature_enable: FnNrMetricsSetFeature,
}

// After loading all fields from libnr_loader.so with Library::get(...):
let mut metrics: NrMetricsHandle = std::ptr::null_mut();
let result = unsafe { (api.create)(&mut metrics) };
if result != 0 || metrics.is_null() {
    return Err(format!("NRMetricsCreate failed: {result}"));
}
let result = unsafe { (api.start)(metrics) };
if result != 0 {
    unsafe { (api.destroy)(metrics) };
    return Err(format!("NRMetricsStart failed: {result}"));
}

let mut fps = 0.0_f32;
let mut dropped = 0_i32;
let fps_result = unsafe { (api.get_present_fps)(metrics, &mut fps) };
let dropped_result = unsafe { (api.get_dropped_frame_count)(metrics, &mut dropped) };
// Publish a value only when its own result is zero; do not mistake an error code for a metric.

// Serialized teardown:
unsafe {
    (api.stop)(metrics);
    (api.destroy)(metrics);
}
```

Resolve the symbols by their exact exported names shown above and keep `Library` in the API struct,
matching `NrRenderingApi`. Do not link `libnr_loader.so` at build time. `Stop` before `Destroy`; use
`Pause`/`Resume` only for session suspension, and leave `SetFeatureEnable` unused until its feature enum
values are independently identified.
