# First-Person-View streaming (libmedia_codec HW encoder): plan + status

Status: **WORKING + device-verified (2026-07-19). FPV streaming records a valid, playable mp4.** Both
the encoder-start crash and the empty-mp4 (no frames reaching the encoder) are fixed; a 13.3 s test
recorded 396 frames @ ~30 fps, 1280×720 H.264, decoding clean and showing the real head-POV scene.
Ports the XREAL `FirstPersonViewStreamingCast` sample's streaming path — the native hardware encoder —
to Godot. (Audio track is empty for now: `RECORD_AUDIO` isn't requested, so the mic path is silent —
see the "Secondary" note below.)

## Device verification (2026-07-18) — crash fixed; frame feeding is the open item (codex handoff)

**Crash on `HWEncoderStart` — FIXED (root cause found by disassembling `libmedia_codec.so`).**
Toggling 配信 crashed with `SIGSEGV, null+0x38` inside `HWEncoderStart` (then, after the first fix,
`HWEncoderSetMediaProjection`). Disassembly showed both call a shared helper (`0x2045a0`) that loads a
**global manager singleton at `[0x952dd8]` and dereferences it at `+0x38`** — and the singleton was
**null**. That singleton is created by `libmedia_codec`'s **`JNI_OnLoad`** (needs the real JavaVM), which
only runs when the lib is loaded via **`System.loadLibrary`** — but we merely `dlopen`'d it from Rust.
Two fixes in WIP `e0f6208`:
1. `XrealBridge.ensureNativeLibrariesLoaded` now `System.loadLibrary("media_codec")` (before `godot_xreal`).
2. `video_encoder::start` calls `HWEncoderSetMediaProjection(handle, null)` right after `SetConfigration`,
   matching the SDK's `NativeEncoder.SetConfigration` (null projection = the RGB-camera/texture path; a
   non-null `MediaProjection` is only for *screen* capture).
Result: streaming starts + stops cleanly, no crash (device-verified).

**RESOLVED (was: recorded mp4 is 0 bytes — no frames reach the encoder).** Symptom:
`demo/stream_manager.gd` renders the head-POV into a `SubViewport` and each frame did
`_gl_tex_id = RenderingServer.texture_get_native_handle(_viewport.get_texture().get_rid())` then
`stream_push_frame` (→ `HWEncoderUpdateSurface`). On device **`texture_get_native_handle` returns 0**, so
`_gl_tex_id` never becomes non-zero and **no frame is ever pushed** (confirmed with per-frame logging).
Tried + did NOT help: resolving/pushing inside `RenderingServer.call_on_render_thread` (GL-current). So
the SubViewport's GL color texture handle is not being obtained (either the viewport isn't producing a
render-target texture we can read, or this is the wrong API for a viewport texture in the 4.7
Compatibility renderer). Next steps: confirm the SubViewport actually renders (probe its texture),
find the correct way to get its GL texture name for the Compatibility renderer, and feed that to
`stream_push_frame`. The SDK feeds `RenderTexture.GetNativeTexturePtr()` (the GL texture name for GLES3).
Once frames flow and the mp4 plays, amend the WIP commit.

**HOST-SIDE ROOT CAUSE + FIX (device-verified 2026-07-19: 396 frames @ ~30 fps, plays clean):** Godot 4.7.1's
`ViewportTexture::get_rid()` returns a texture **proxy**. The Compatibility renderer creates that proxy
while the viewport render target still has no size, so the copied GLES3 `tex_id` is 0. When the render
target is later allocated, its real color texture receives a GL name, but
`TextureStorage::texture_get_native_handle` simply returns the passed RID's own `tex_id`; it does not
follow `proxy_to`. This also explains why `get_image()` works: `ViewportTexture::get_image()` bypasses
the proxy and reads `vp->texture_rid` directly. The zero-copy fix in `demo/stream_manager.gd` calls
`RenderingServer.viewport_get_texture(_viewport.get_viewport_rid())` to obtain that real render-target
color-texture RID, then resolves its native handle and calls `stream_push_frame` inside
`call_on_render_thread`. The GL name is resolved every frame so render-target reallocations are safe.
No GPU copy is required.

Secondary (not the video blocker): `STREAM_WITH_MIC` is `true` but `RECORD_AUDIO` is never requested at
runtime — `demo/main.gd` only requests `CAMERA` — so the mic permission is denied (`granted=false`).
When tackling audio, either disable mic (isolates video) or request `RECORD_AUDIO` before `stream_start`.
The mic was ruled out of the crash (it still crashed with `addMicphoneAudio:false`).


## What the SDK sample actually is

`Samples~/Camera Features/FirstPersonStreammingCast` streams via `XREALVideoCapture` →
`FrameCaptureContext` → `VideoEncoder` → **`NativeEncoder`**, which is a thin `[DllImport("libmedia_codec")]`
wrapper over the native **`libmedia_codec.so`** (a MediaCodec-backed H.264 encoder + muxer). The
`LitJson` + `Network` (`LocalServerSearcher`) code is only LAN discovery/handshake glue to find the
receiver — the encode + RTP is done by the native lib. So there **is** a streaming API, not just a
C# "send to server" sample.

## The native API (flat C exports in `libmedia_codec.so`, all `T`/dynsym → dlsym-able)

```
XREALErrorCode HWEncoderCreate(UInt64* out_handle)
XREALErrorCode HWEncoderSetConfigration(UInt64 handle, const char* configJson)
XREALErrorCode HWEncoderStart(UInt64 handle)
XREALErrorCode HWEncoderUpdateSurface(UInt64 handle, IntPtr gl_texture_id, UInt64 timestamp)  // per frame
XREALErrorCode HWEncoderStop(UInt64 handle)
XREALErrorCode HWEncoderDestroy(UInt64 handle)
// also: HWEncoderNotifyAudioData / AdjustVolume / SetMediaProjection / StartWithRenderInstance (Vulkan) / *OnlyAudioRecorder
```
`NEEDED`: libGLESv3 / libEGL / libmediandk / libvulkan / libandroid — all system libs, so shipping just
`libmedia_codec.so` suffices (no libVulkanSupport needed on the GLES path).

**Config JSON** (`EncodeTypes.cs`):
`{"width":W,"height":H,"bitRate":B,"fps":F,"codecType":C,"outPutPath":"…","useStepTime":0,"useAlpha":false,"useLinnerTexture":true,"addMicphoneAudio":…,"addInternalAudio":…,"audioSampleRate":16000,"audioBitRate":128000}`
— **`codecType` 0=local mp4 / 1=RTMP / 2=RTP**; `outPutPath` is a file path or an `rtp://ip:port` /
`rtmp://…` URL. Same encoder does recording and live streaming — just change the URL.

**Frame input is a GL texture:** `HWEncoderUpdateSurface(handle, gl_texture_id, timestamp)`. The sample
feeds `RenderTexture.GetNativeTexturePtr()` (the GL texture name). The encoder reads it on the **current
EGL context**, so the call must be on the render thread.

## The Godot port (this repo)

- **`src/video_encoder.rs`** — dlopen `libmedia_codec.so`, resolve `HWEncoder*`, `start(output,w,h,bitrate,fps)`
  (create + config + start; `codecType` from the URL scheme), `submit_frame(gl_tex_id, ts)` (render
  thread), `stop()`, `is_active()`.
- **`XrealSystem`** `#[func]`s: `stream_start` / `stream_push_frame` / `stream_stop` / `is_stream_active`.
- **Vendor**: `libmedia_codec.so` → `jniLibs/arm64-v8a/` (vendor_xreal_libs.{sh,ps1}, godot_xreal.gdextension
  `[dependencies]`, build.{sh,ps1}). App is `gl_compatibility` (GLES), matching the encoder's GL texture input.
- **`demo/stream_manager.gd`** (phone-menu "配信" toggle, カメラ tab — like the SDK's cast this is an
  **Eyes/RGB-camera feature (One Series only)**; gated on `is_camera_supported()` so the encoder is never
  opened on the camera-less Air 2 Ultra, avoiding the same freeze the camera hit): renders the head-locked
  view into a SubViewport,
  gets its GL id via `RenderingServer.texture_get_native_handle`, and each frame pushes it inside a
  `RenderingServer.call_on_render_thread` callback (so `HWEncoderUpdateSurface` runs on the render thread).
- **Receiving server**: `log/stream_server/` (gitignored) — `receive.ps1` runs ffplay/ffmpeg against
  `stream.sdp` to view/record the RTP stream; prints the PC's `rtp://IP:5555` addresses.

## On-device TODO (the real unknowns)
1. **GL-context/thread correctness of `UpdateSurface`** — the encoder must read Godot's SubViewport GL
   texture on the render thread's EGL context. First bring-up: **`codecType 0` (local mp4)** (default when
   `STREAM_TARGET` is empty) — records on-device, `adb pull`, play — validates the encode pipeline with no
   network. Then set `STREAM_TARGET = "rtp://<PC>:5555"` for live RTP to `log/stream_server`.
2. Confirm the config fields the encoder needs (bitrate/fps), the timestamp unit, and that a
   `useLinnerTexture`/`useAlpha` mismatch doesn't garble the frame.
## Audio
`stream_start(..., with_mic, with_internal_audio)`: **`with_mic`** sets `addMicphoneAudio` — the encoder
captures the mic natively (needs `RECORD_AUDIO`, added by the export plugin). **`with_internal_audio`**
sets `addInternalAudio` and is fed by `stream_push_audio(bytes, nSamples, bytesPerSample, channels,
sampleRate, fmt)` → `HWEncoderNotifyAudioData` (mono s16, `fmt` 0 — from an `AudioEffectCapture` on the
master bus, the Godot analog of the SDK's `AudioRecordTool` `OnAudioFilterRead`). The demo enables the
mic (`STREAM_WITH_MIC`) and leaves internal audio off (it plays no sound).
