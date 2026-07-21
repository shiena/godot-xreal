# Camera copy-path load investigation — AHardwareBuffer / direct-upload PoCs (2026-07-21)

Status: **MEASURED, then REVERTED by design.** Both PoC implementations live in git history —
commit `c5b9a67` (`poc(camera): AHardwareBuffer/EGLImage and render-thread direct-upload camera
paths`), reverted in `b041ff9`. `git show c5b9a67` / `git checkout c5b9a67` to revive. This memo is
the durable record for the follow-up RE (see [Next: RE targets](#next-re-targets)).

Test device: X4000 (Beam Pro), Android 14 / API 34, Adreno 710, XREAL One Pro, camera 1280×720
(Y) + 640×360 (interleaved CbCr), polled at ~60 fps via `XrealCameraFeed::poll_frame`.

## Problem

`poll_frame` (main thread) cost **~2.8–3.6 ms/frame** — ~20% of a 60 fps frame budget — for a
1280×720 camera frame. The copy chain per frame was: SDK plane → `Vec` (memcpy + byte-wise CbCr
interleave) → `PackedByteArray` (copy) → `Image::create_from_data` (copy + alloc) →
`ImageTexture.update` (GPU upload). ~4 traversals of ~1.4 MB plus per-frame allocations.

## PoC 1: `eglGetNativeClientBufferANDROID` zero-upload (`debug.xreal.camera_ahb=1`)

Design: allocate an **R8 `AHardwareBuffer`** (CPU_WRITE_OFTEN | GPU_SAMPLED_IMAGE), wrap via
`eglGetNativeClientBufferANDROID` → `eglCreateImageKHR(EGL_NATIVE_BUFFER_ANDROID)`, and bind the
EGLImage **over the Y `ImageTexture`'s GL storage** (`glEGLImageTargetTexture2DOES` on
`GL_TEXTURE_2D`, texture id from `texture_get_native_handle`, on the render thread). Per-frame Y
updates would then be one CPU row-copy into the locked mapping — no GL call, no upload, shaders and
GDScript untouched.

**Result: DEAD on this device.** `AHardwareBuffer_allocate(R8 1280x720)` → status 1. The gralloc
support probe (`AHardwareBuffer_isSupported`, CPU_WRITE_OFTEN | GPU_SAMPLED_IMAGE):

```
R8=0   RGBA8=1   YCbCr420(0x23)=1
```

R8 (API 33+) is simply not supported by this SoC's gralloc despite API 34. Remaining AHB options
and why they were not pursued:

- **`Y8Cb8Cr8_420` (supported)** — the "native" zero-copy format, but sampling requires
  `GL_TEXTURE_EXTERNAL_OES` (`ExternalTexture` + `set_external_buffer_id(EGLImage)`); whether
  Godot's *spatial* shaders can sample an OES texture in the Compatibility renderer is unverified,
  and (see the floor finding below) the additional win over PoC 2 would be ≈ 0 anyway.
- **RGBA8 (supported)** — packing Y as 4 px/texel (W/4 wide) breaks hardware bilinear filtering
  and needs shader surgery; full-res RGBA8 with Y in R means 4× memory and strided CPU writes.

Note: no camera-side `AHardwareBuffer` exists to import zero-copy — `libXREALXRPlugin.so` neither
exports nor imports any AHardwareBuffer symbol; `TryGetRGBCameraDataPlane` hands out CPU-mapped
plane pointers only.

## PoC 2: render-thread direct upload (`debug.xreal.camera_direct=1`)

Design: dispatch the whole grab to the render thread (`call_on_render_thread`, per-frame
`Callable`); there, `glTexSubImage2D` the **Y plane straight from the SDK plane pointer**
(`GL_RED`/`UNPACK_ALIGNMENT 1`, no staging at all), interleave CbCr into one reused buffer and
upload as `GL_RG`. Kills the Vec/PackedByteArray/Image/update chain entirely.

**Result: works, verified visually** (preview panel correct, both eyes):

| mode | poll cost | thread |
|---|---|---|
| copy (shipped path) | 2.8–3.6 ms/frame | main |
| direct | **2.5–2.7 ms/frame** | render (main ≈ 0: dispatch only) |

Net CPU ≈ −25%, and the main thread is freed of ~3 ms/frame (moved to the render thread — a
trade: the render thread also runs the eye-blit/frame_tick work, so if IT is the bottleneck this
is not free).

## Key finding: the floor is the SDK acquire

Removing ~2.3 MB of per-frame copies + allocations saved only ~0.8 ms. The remaining ~2.5 ms must
be dominated by **`TryAcquireLatestImage` / `TryGetRGBCameraDataPlane` themselves** (suspected
internal copy or IPC wait inside the SDK), plus ~0.5–1 ms of `glTexSubImage2D` driver copy. Every
client-side scheme pays this floor, which is why the OES/YCbCr420 variant was dropped: it could
only shave the driver-copy slice while adding AHB lock/copy of its own.

## Next: RE targets

To break the floor, RE the camera path in `libXREALXRPlugin.so` (all thin wrappers around
libnr_api):

- `TryAcquireLatestImage` — what does "acquire" do? If it snapshots/copies the frame into the
  CPU-visible buffer, find where the source lives (ION/dmabuf? AHardwareBuffer inside libnr_api?).
- `TryGetRGBCameraFrame` (unexplored export, distinct from `TryAcquireLatestImage`) — may return a
  richer frame object; check whether it exposes a hardware buffer / fd that could be imported
  zero-copy after all.
- `GetRgbCameraPluginState`, `IsRGBCameraDataHandleValid` — supporting cast for the above.

If a dmabuf/AHardwareBuffer surfaces at that layer, revive commit `c5b9a67`: the EGLImage import
+ `ExternalTexture` (`set_external_buffer_id`) machinery there is the right consumer for it, and
`isSupported YCbCr420=1` says the allocator side is ready.

## Misc findings (worth keeping)

- Force-stopping the app while the camera is capturing **wedges the glasses camera service**.
  Two signatures: (a) `StartRGBCameraDataCapture` → sentinel ("Recv Frame -99"); (b) start
  "succeeds" (handle=0) but zero frames arrive — and in the observed case the stuck pipeline
  destabilised SLAM into a position runaway (pose position drifting ~0.5 m/s to 150 m+). Recovery
  = **replug the glasses USB AND fully restart the app** (a running process's native session is
  bound to the old connection; its retries never succeed). Turn the camera off before killing the
  app in test loops.
- One Godot headless APK export produced a broken package **missing
  `assets/.godot/extension_list.cfg`** (GDExtension silently absent at runtime, `.so` present but
  never registered; no dlopen error). Re-exporting fixed it. Worth checking in the APK when the
  extension mysteriously "isn't loaded": `extension_list.cfg` must exist in `assets/.godot/`.
- `RenderingServer.texture_get_native_handle` resolves script-created `ImageTexture`s to GL ids
  fine on the render thread (used by both PoCs), and raw `glTexSubImage2D` into a Godot-owned R8/
  RG8 texture displays correctly — Godot does not fight external updates to its texture storage.
