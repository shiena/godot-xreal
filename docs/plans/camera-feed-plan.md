# XREAL RGB camera → Godot `CameraFeed` plan

Status: **IMPLEMENTED 2026-07-13 (device-verified, full colour)** — `src/camera_feed.rs`
(`XrealCameraFeed`). Deltas from the plan below:

- The `CameraFeed` subclass + `set_ycbcr_images` shipped as designed, **but sampling via
  `CameraTexture` shows only a placeholder for script-fed feeds** — so the feed also keeps two
  plain `ImageTexture`s (`get_y_texture()` / `get_cbcr_texture()`) that the 3D panel shader
  samples directly.
- `set_ycbcr_images` only exists from godot-rust **`api-4-6`** (crate feature bumped).
- Colour conversion is a port of the SDK's `YUVTransRGB` (full-range BT.601) in the panel shader.
- **The RGB camera conflicts with 6DoF SLAM** (both consume the camera): with the feed on, the
  demo forces 3DoF (`set_tracking_type(1)` in `demo/main.gd`); the DISP pose still carries full
  orientation.
- Known failure mode: a pink/magenta panel = wedged camera service — a crashed client kept the
  camera held (`Recv Frame -99` in logcat, `StartRGBCameraDataCapture` returns the `u64::MAX`
  error sentinel). Recovery requires **replugging the glasses' USB**; an app/device restart is
  not enough. Handled in commit `1461f7b`: `src/native.rs` surfaces the sentinel as a failed
  start, `demo/main.gd` latches `_cam_failed` instead of hammering the dead service, and the
  capture is released on graceful exit.

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
