# XREAL RGB camera → Godot `CameraFeed` plan

Status: **IMPLEMENTED 2026-07-13 (device-verified, full colour)** — `src/camera_feed.rs`
(`XrealCameraFeed`). Deltas from the plan below:

- The `CameraFeed` subclass + `set_ycbcr_images` shipped as designed, **but sampling via
  `CameraTexture` shows only a placeholder for script-fed feeds** — so the feed also keeps two
  plain `ImageTexture`s (`get_y_texture()` / `get_cbcr_texture()`) that the 3D panel shader
  samples directly.
- Since that made the `set_ycbcr_images` route consumer-less (every addon consumer — panel, blend,
  stream, photo — samples the plain textures), the per-frame `set_ycbcr_images` push is **gated
  behind `feed_camera_server` (default OFF)** as of 2026-07-21: it cost a duplicate GPU upload per
  frame for a route nothing displays. Set it `true` only for external code that consumes
  `CameraServer` feeds through the standard API.
- `set_ycbcr_images` only exists from godot-rust **`api-4-6`** (crate feature bumped).
- Colour conversion is a port of the SDK's `YUVTransRGB` (full-range BT.601) in the panel shader.
- **The RGB camera does _not_ conflict with 6DoF SLAM** (disproved on-device 2026-07-18, commit
  `f56dd3a`): SLAM runs on the Leopard grayscale cameras and the RGB camera is a separate device —
  One Pro logcat shows 6DoF + RGB coexisting with zero `GetPoseWithStates` failures. The demo
  originally forced 3DoF with the feed on (`set_tracking_type(1)`, on the shared-camera theory);
  that force is dropped, so the configured tracking mode (6DoF by default) stays live with the
  camera on.
- Known failure mode: a pink/magenta panel = wedged camera service — a crashed client kept the
  camera held (`Recv Frame -99` in logcat, `StartRGBCameraDataCapture` returns the `u64::MAX`
  error sentinel). Recovery requires **replugging the glasses' USB AND then fully restarting the
  app** (2026-07-21: a replug alone is not enough — a running process's native session is bound to
  the old connection, so its retries keep failing; and an app restart alone is not enough either).
  Handled in commit `1461f7b`: `src/native.rs` surfaces the sentinel as a failed start,
  `demo/main.gd` latches `_cam_failed` instead of hammering the dead service, and the capture is
  released on graceful exit.
- Second wedge signature (2026-07-21): after an app kill mid-capture, `Start…` can *succeed*
  (handle=0) with **zero frames ever arriving** — and the stuck pipeline destabilised SLAM into a
  position runaway (~0.5 m/s drift, no `GetPoseWithStates` errors). `xreal_camera.gd` catches this
  with a 5 s first-frame watchdog (fails as "wedged", same recovery). The demo pops a modal dialog
  on either signature (`demo/main.gd` `_show_error_dialog`).

## Per-frame cost: 208 ms/s → 15.7 ms/s (2026-07-21, device-verified)

`poll_frame` cost **3,466 µs per grab, 60 times a second — 208 ms of CPU per second, 20.8 % of a
core**. It is now **525 µs at 29.4 grabs/s = 15.7 ms/s (1.5 %)**. Four changes, each measured on the
X4000 + One Pro before being kept:

| # | change | per grab | commit |
|---|---|---|---|
| 0 | baseline | 3,466 µs × 60/s | |
| 1 | skip the grab when the frame timestamp is unchanged | ×29.4/s instead of ×60/s | `ae1d305` |
| 2 | vectorise the CbCr interleave (903 → 167 µs) | 3,148 µs | `497117e` |
| 3 | upload through a PBO, drop `PackedByteArray`/`Image` | 1,130 µs | `fcd7a79` |
| 4 | upload straight from the SDK's plane pointers | **525 µs** | `e7bb8f7` |

Four findings worth keeping:

- **The SDK calls are free.** `TryAcquireLatestImage` = 4 µs, `DisposeRGBCameraDataHandle` = 5 µs,
  and the three `TryGetRGBCameraDataPlane` calls together = **0 µs**. This refutes the old
  "the floor is the SDK acquire" conclusion in `docs/archive/camera-zero-copy-investigation.md`;
  every microsecond was on our side. Disassembly: `docs/archive/codex-camera-acquire-analysis.md`.
- **The camera publishes at 30 fps** (confirmed: 29.4 grabs/s against 59.7 polls/s), so a 60 Hz poll
  did every frame's work twice. Gate on the timestamp `TryAcquireLatestImage` already returns —
  *not* on the SDK's `TryGetRGBCameraFrame` flag, which is a destructive unlocked read-and-clear.
- **Adreno's `glTexSubImage2D` from client memory costs ~1.78 ns per _texel_, not per byte** (1651 µs
  for 921,600 R8 texels; 409 µs for 230,400 RG8 texels — equal per texel, 2× apart per byte). That is
  the driver tiling every texel on the CPU. Sourcing from a pixel-unpack buffer moves that pass to
  the GPU and the cost becomes per-byte (2.3 GB/s), which is where change 3's 2,060 → 597 µs came from.
- **Godot's Android main loop is the GL thread** — `eglGetCurrentContext()` from `_process` returns
  non-null — so `poll_frame` can issue GL inline. `crate::gl::has_current_context()` checks that at
  runtime rather than assuming it; where it is false the feed falls back to the `Image` path.

Not adopted, both measured and rejected:

- **Splitting CbCr into two R8 textures** to drop the interleave: it removed the expected 100 µs but
  added ~60 µs of chroma upload and ~65 µs to the *luma* upload (three PBO orphans per frame instead
  of two), for a net 525 → 543 µs. No gain, and it would have changed the public
  `get_cbcr_texture()` API across five GDScript consumers and three shaders. Reverted.
- **Zero-copy via AHardwareBuffer/EGLImage**: impossible through the public C ABI — see the archive
  memo. Commit `c5b9a67` should not be revived.

Per-stage timing is still in the code, behind
`adb shell setprop debug.xreal.camera_timing 1` (then toggle the camera off/on):

```
[xreal] camera timing/grab (us, n=120): acquire=4 planes=0 interleave=99 dispose=5 \
        packed=0 image=0 feed=0 upload_y=270 upload_cbcr=138 | total=525
```

### Getting pixels on the CPU (OpenCV etc.)

The direct path never materialises a CPU copy, so **there is no CPU pixel accessor exposed to
GDScript today** — and `texture.get_image()` is not it (GPU readback, and the direct path bypasses
Godot's `update()`, so its cached image goes stale). Options, in order of cost:

- **In-process Rust/C++** — `XrealNative::rgb_camera_with_frame()` lends `&[u8]` straight into the
  SDK's buffer. Work inside the closure costs **nothing extra**; the Y plane is already a dense
  1280×720 8-bit grayscale image (no conversion, which is what most CV wants). The pointers die when
  the closure returns, so anything threaded needs a copy.
- **GDScript** — needs a `#[func]` returning `PackedByteArray`; cost is one memcpy at the measured
  4.1 GB/s: **~225 µs** for Y alone, **~340 µs** for all three planes. The three planes are
  contiguous in one buffer, so a single copy is enough — but note the order is **Y, V, U = YV12**,
  not I420 (`cv::COLOR_YUV2BGR_YV12`). Contiguity is a wrapper implementation detail, so assert it
  at runtime and fall back to three copies.

Goal: expose the XREAL glasses' RGB camera to Godot as a **`CameraFeed`** (subclass), so any
`CameraTexture` / shader can sample the live camera — the Godot-native equivalent of the reference app's
`XrealFrameSource`. Feasibility: **confirmed on both sides** (see below); this note is the RE map +
design + spike plan.

## Why `CameraFeed` (inheritance is the idiomatic path)

Godot's `CameraFeed` (extends `RefCounted`) is designed to be subclassed by a camera source:

- godot-rust 0.5 / `api-4-4` **generates** `pub trait ICameraFeed: GodotClass<Base = CameraFeed>`
  with overridable virtuals `fn activate_feed() -> bool` and `fn deactivate_feed()`
  (`target/**/godot-core-*/out/classes/camera_feed.rs`). So:
  `#[derive(GodotClass)] #[class(base = CameraFeed)]` + `#[godot_api] impl ICameraFeed`.
- Frame push (public, on `CameraFeed`): `set_rgb_image(Image)` (FEED_RGB, RGB8),
  `set_ycbcr_images(y, cbcr)` (FEED_YCBCR_SEP: Y = R8, CbCr = RG8 interleaved),
  `set_ycbcr_image(Image)`, `set_external(w, h)` + `get_texture_tex_id()`.
- Register: `CameraServer::singleton().add_feed(feed)`; a `CameraTexture` (`camera_feed_id` +
  `which_feed`) then samples it. CameraFeed is instantiable (`GodotDefault`).

A non-inheriting variant (plain `CameraFeed` + push from a node's `process`) works too, but then
`activate_feed`/`deactivate_feed` can't gate the camera — inheritance is the clean model.

## XREAL native RGB camera C ABI (the decisive part)

The XREAL Unity SDK reaches the camera through a **flat C ABI** — NOT the obfuscated
`libnr_api.so::NRGetProcAddr` table, and NOT a Java/AAR-only path. The functions are plain global
(`T`) exports of **`libXREALXRPlugin.so`**, which this project already `dlopen`s. So the exact same
`native.rs` dlsym pattern used for head pose / display applies.

Source: the SDK's `XREALPlugin` P/Invoke layer
(`com.xreal.xr/Runtime/Scripts/XREALRGBCamera.cs`, `[DllImport(LibName)]`).

Exports (addresses in the vendored `jniLibs/arm64-v8a/libXREALXRPlugin.so`; signatures from the
C# `[DllImport]` + demangled `SessionManager::*` internals):

| export | addr | signature (C ABI) |
|---|---|---|
| `StartRGBCameraDataCapture` | `0x53c50` | `uint64 (*cb)(RGBCameraDataFrame, void* userData), void* userData) -> uint64` (returns a callback handle; pass `cb=null` to poll instead) |
| `StopRGBCameraDataCapture`  | `0x53d6c` | `(uint64 callbackHandle) -> bool` |
| `TryGetRGBCameraFrame`      | `0x53e7c` | `(uint64* timeStamp) -> bool` |
| `TryAcquireLatestImage`     | `0x53f8c` | `(int32* frameHandle, NRSize2i* resolution, uint64* timeStamp) -> bool` |
| `TryGetRGBCameraDataPlane`  | `0x540ac` | `(int32 frameHandle, int32 planeIndex, void** dataPtr, NRSize2i* size) -> bool` |
| `IsRGBCameraDataHandleValid`| `0x541dc` | `(int32 frameHandle) -> bool` |
| `DisposeRGBCameraDataHandle`| `0x542ec` | `(int32 frameHandle) -> void` |
| `GetRgbCameraPluginState`   | `0x4a884` | `(NRRgbCameraPluginState* out) -> ...` (readiness; via `NativeGlasses::GetRgbCameraPluginState`) |

Structs:
- `NRSize2i { int32 width; int32 height; }`  (== Unity `Vector2Int`).
- `RGBCameraDataFrame { uint64 timeStamp; NRSize2i resolution; uint64 rawDataSize; void* rawData; }`
  (the callback's frame; the poll path uses `frameHandle` + per-plane pointers instead).

Internal (evidence, all `t` local in libXREALXRPlugin.so): `SessionManager::StartRGBCameraDataCapture(
void(*)(RGBCameraDataFrameToUnity, void*), void*)`, `::TryAcquireLatestImage(int*, NRSize2i*, uint64*)`,
`::TryGetRGBCameraDataPlane(int, int, void**, NRSize2i*)`, `NativeRGBCamera::GetRGBCameraData(...)`,
`FunctionLoader::InitRGBCameraWrapper()`.

## Frame format (I420 planar, device-derived)

From `XREALRGBCameraTexture.LoadYUVFormatTexture` (SDK) — **YUV 4:2:0 planar**, 3 tightly-packed
8-bit planes (each plane's byte size == `size.x * size.y`, i.e. stride == width, no padding):

| planeIndex | plane | dims | notes |
|---|---|---|---|
| 0 | **Y** | W × H         | full-res luma (8-bit) |
| 1 | **V** | W/2 × H/2     | chroma (note: index 1 is V) |
| 2 | **U** | W/2 × H/2     | chroma (index 2 is U) |

The reference app's frame source uses **only plane 0 (Y)** as full-res grayscale for ORB matching.

Mapping to Godot:
- **Grayscale (spike):** wrap Y into an RGB8 `Image` (r=g=b=Y) → `set_rgb_image()`. Simplest path to
  "camera on screen".
- **Full color:** `set_ycbcr_images(y, cbcr)` — Y as R8 (W×H), CbCr as RG8 (W/2×H/2) built by
  **interleaving U and V** (`cbcr[2i]=U`, `cbcr[2i+1]=V`; remember plane 1 = V, plane 2 = U). Godot's
  YCbCr feed shader does the BT.601-ish conversion; verify colors on device.

## Bridge design

1. `src/native.rs` — dlsym the 8 exports above from the already-open `plugin_lib` (own fn-pointer
   types in `ffi.rs`: `NRSize2i`, `RGBCameraDataFrame`, the callback type). Add
   `XrealNative::rgb_camera_*` wrappers (start/stop/acquire/plane/valid/dispose/state).
2. `src/session.rs` — thin `XrealSession` methods (start/stop capture, acquire-latest → planes).
3. `src/camera_feed.rs` (new) — `#[class(base = CameraFeed)] XrealCameraFeed`:
   - `activate_feed()` → ensure CAMERA permission + plugin ready → `StartRGBCameraDataCapture` → `true`.
   - a per-frame driver (poll from a node's `process`, or the capture callback marshaled to the main
     thread like `glasses_events`): `TryAcquireLatestImage` → `TryGetRGBCameraDataPlane(0)` (spike:
     Y only) → build `Image` → `set_rgb_image()` / `set_ycbcr_images()` → `DisposeRGBCameraDataHandle`.
   - `deactivate_feed()` → `StopRGBCameraDataCapture`.
   - Register via `CameraServer::singleton().add_feed(...)` (e.g. a `#[func] register()` or in
     `XrealSystem`).
4. Demo: a `TextureRect` with a `CameraTexture` (`camera_feed_id`, `which_feed = FEED_RGBA`/`Y`) to show it.

## Caveats to resolve on device

1. **Android `CAMERA` permission** — the SDK ships `XREALAndroidPermissionsManager` +
   `nrunityandroidpermission-release.aar`; in Godot, add `<uses-permission android:name="android.permission.CAMERA"/>`
   and request at runtime (`OS.request_permission("CAMERA")` / the Android plugin). Without it,
   `StartRGBCameraDataCapture` fails / `GetRgbCameraPluginState` reports not-ready.
2. **Plugin/USB readiness** — the RGB camera runs over the glasses' USB link (internally
   `libnr_libusb` / `libnr_rgb_camera`). Gate `activate_feed` on `GetRgbCameraPluginState`, and only
   after the existing session bootstrap (UnityPluginLoad + CreateSession). There may be a
   `FunctionLoader::InitRGBCameraWrapper` step the SDK runs first — check the plugin state export.
3. **Plane stride / size** — confirm from `TryGetRGBCameraDataPlane`'s `size` and the frame's
   `rawDataSize` that stride == width (no row padding) before `LoadRawTextureData`-style copies.
4. **Threading** — the capture callback runs on an SDK thread; do Godot `Image`/`CameraFeed` updates
   on the main thread (reuse the `glasses_events` queue/marshal pattern). Polling from `process`
   avoids this entirely for the spike.
5. **Lifetime** — `frameHandle` must be `DisposeRGBCameraDataHandle`d every frame; the plane pointers
   are only valid until dispose.

## References

- Godot: `servers/camera/camera_feed.{h,cpp}`, `servers/camera_server.cpp`;
  generated `target/**/godot-core-*/out/classes/camera_feed.rs` (`ICameraFeed`, `set_rgb_image`, …).
- XREAL SDK (`com.xreal.xr.tar.gz`): `Runtime/Scripts/XREALRGBCamera.cs` (the C ABI),
  `Runtime/Scripts/Android/Camera Features/XREALRGBCameraTexture.cs` (plane format),
  `.../FrameProvider/RGBCameraFrameProvider.cs`, `Samples~/Camera Features/.../RGBCameraExample.cs`.
- Reference app: its camera frame source + a frame-source abstraction (`IFrameSource`) we mirror.
