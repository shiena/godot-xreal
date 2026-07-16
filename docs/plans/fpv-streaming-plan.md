# First-Person-View streaming (libmedia_codec HW encoder): plan + status

Status: **IMPLEMENTED (compiles/parses clean; on-device verification pending).** Ports the XREAL
`FirstPersonViewStreamingCast` sample's streaming path — the native hardware encoder — to Godot.

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
