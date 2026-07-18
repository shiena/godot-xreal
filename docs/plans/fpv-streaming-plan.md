# First-Person-View streaming (libmedia_codec HW encoder): plan + status

Status: **WORKING + device-verified (2026-07-19). FPV streaming records a valid, playable mp4.** Both
the encoder-start crash and the empty-mp4 (no frames reaching the encoder) are fixed; a 13.3 s test
recorded 396 frames @ ~30 fps, 1280×720 H.264, decoding clean and showing the real head-POV scene.
Ports the XREAL `FirstPersonViewStreamingCast` sample's streaming path — the native hardware encoder —
to Godot. **Microphone audio also works** (2026-07-19, `fffb241`): the mic is captured as a non-silent
AAC track and the mp4 plays with sound in Windows Media Player — see the "Audio" section below.
**Live RTP also works** (2026-07-19): the Camera tab has a stream-destination field (empty = local mp4,
`rtp://<PC>:5555` = live RTP); a device test streamed `codecType 2` to a PC where ffmpeg received clean
1280×720 H.264 with the correct head-POV content. Caveat: the RTP path's audio is **proprietary**, so
standard receivers get **video only** — see "Audio over RTP" below.

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

Secondary — **RESOLVED 2026-07-19 (`fffb241`), see the Audio section.** `STREAM_WITH_MIC` is `true` but
`RECORD_AUDIO` was never requested at runtime (only `CAMERA` was), so the mic was denied and no audio
track was muxed. Fix: request `RECORD_AUDIO` at runtime and only enable `addMicphoneAudio` once granted.
(The mic was ruled out of the encoder-start crash — it still crashed with `addMicphoneAudio:false`.)


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
- **`demo/stream_manager.gd`** (phone-menu "配信" toggle, カメラ tab — **no RGB-camera gate**: the encoder
  feeds on our own SubViewport texture, not the camera, so streaming works on the camera-less Air 2 Ultra
  too. It casts the camera+AR blend only opportunistically when the camera is already on, otherwise the
  AR-only view): renders the head-locked view into a SubViewport,
  gets its GL id via `RenderingServer.texture_get_native_handle`, and each frame pushes it inside a
  `RenderingServer.call_on_render_thread` callback (so `HWEncoderUpdateSurface` runs on the render thread).
- **Receiving side**: XREAL's official **`StreamingReceiver.exe`** (Unity + FFmpeg), paired via
  `demo/stream_pairing.gd` (FIND-SERVER discovery + TCP handshake). It decodes video + audio. (An earlier
  ffmpeg-based receiver under `scripts/stream_server/` was removed once pairing worked — a plain SDP
  couldn't decode the proprietary RTP audio anyway; see "Audio over RTP".)

## On-device TODO (the real unknowns)
1. **GL-context/thread correctness of `UpdateSurface`** — the encoder must read Godot's SubViewport GL
   texture on the render thread's EGL context. First bring-up was **`codecType 0` (local mp4)** — records
   on-device, `adb pull`, play — validating the encode pipeline with no network. Live RTP now pairs with
   `StreamingReceiver.exe` (`demo/stream_pairing.gd`, FIND-SERVER) and streams to `rtp://<ip>:5555`.
2. Confirm the config fields the encoder needs (bitrate/fps), the timestamp unit, and that a
   `useLinnerTexture`/`useAlpha` mismatch doesn't garble the frame.
## Audio — **WORKING, device-verified 2026-07-19 (`fffb241`)**
`stream_start(..., with_mic, with_internal_audio)`: **`with_mic`** sets `addMicphoneAudio` — the encoder
captures the mic natively. This needs the **`RECORD_AUDIO` runtime permission**: the export plugin
declares it in the manifest, but as a *dangerous* permission it must also be **granted at runtime**, which
the demo now does — `stream_manager.setup()` requests it proactively (one-time dialog before streaming),
and `set_enabled` only passes `addMicphoneAudio=true` once `OS.get_granted_permissions()` shows it granted
(else it re-requests and streams video-only that once). Verified: **AAC 16 kHz mono, non-silent
(-40 dB RMS / -21 dB peak), plays with sound in Windows Media Player.**

**`with_internal_audio`** sets `addInternalAudio` and is fed by `stream_push_audio(bytes, nSamples,
bytesPerSample, channels, sampleRate, fmt)` → `HWEncoderNotifyAudioData` (mono s16, `fmt` 0 — from an
`AudioEffectCapture` on the master bus, the Godot analog of the SDK's `AudioRecordTool`
`OnAudioFilterRead`). The demo enables the mic (`STREAM_WITH_MIC`) and leaves internal audio off (it
plays no sound).

**Known-benign warnings** (not playback problems — WMP plays fine): ffmpeg reports an `Input buffer
exhausted` on ~2/307 AAC frames (the truncated final frame at stop) and non-monotonic video DTS
(we push above the configured 30 fps at the glasses' refresh rate, so some frames share a DTS —
players use PTS to display). **Open polish**: the encoder logs `Request requires
MODIFY_AUDIO_SETTINGS` (a *normal* permission it uses to set audio params); recording works without it,
but declaring `MODIFY_AUDIO_SETTINGS` in the export plugin would silence it.

### Audio over RTP — proprietary, not standard-decodable (RE'd 2026-07-19)
The encoder DOES send mic audio over RTP, but in a **non-standard framing** a plain SDP can't decode.
A UDP capture of a live `rtp://<PC>:5555` stream (`codecType 2`, `addMicphoneAudio:true`) showed four
flows — the encoder puts audio on **video-port + 2**, RTP convention:

| port | RTP payload type | what |
|------|------------------|------|
| 5555 | 96 | H.264 video (fragmented, len 17–1472, matches our `stream.sdp`) |
| 5556 | 72 | RTCP for video |
| 5557 | 97 | **audio** — fixed **772-byte** payloads, ~1024-sample RTP-timestamp step |
| 5558 | 72 | RTCP for audio |

So adding `m=audio 5557 RTP/AVP 97` to the SDP finds the packets — but they don't decode. Every audio
payload is a **constant 772 bytes** = a 4-byte magic **`ff ff ff 03`** + 768 bytes of opaque data (not
RFC 3640 AAC-hbr / not ADTS / not LATM — those sync words don't match, and constant-size frames aren't
natural AAC). This is XREAL's own packetization; their receiver decodes it, ffmpeg/ffplay/VLC can't.
Writing a custom `ff ff ff 03` depacketizer would be significant, uncertain effort — **so instead we
pair with XREAL's official receiver, which already decodes the full A/V (RESOLVED — see the FIND-SERVER
section above; audio is device-verified there).** That receiver, `StreammingReceiver_v1.2.0`, is a Unity
Windows app ("StreamingReceiver" by Nreal) bundling **FFmpeg** (`avcodec/avformat/swresample`) + **AVPro
Video** + `Audio360.dll` + `media_enc.dll`, with a `StreammingEncoder` (`Play,useAudio:`) pipeline and
LAN discovery (`FIND-SERVER`). (The **local mp4** path also carries the mic AAC directly.) (Capture tooling
used: a Python multi-port UDP sniffer — not committed.)

NB: "Nebula" (`com.xreal.evapro.nebula`) is XREAL's **Android** launcher on the host device, NOT this
PC receiver — an earlier note conflated them.

### Pairing with the official receiver — the FIND-SERVER protocol (from the SDK sample)
The SDK sample (`Samples~/Camera Features/FirstPersonStreammingCast`) reveals exactly how the sender
pairs with the `StreamingReceiver`, so our app could interop with it:
1. **Discovery** (`Network/LocalServerSearcher.cs`): the sender UDP-broadcasts the ASCII string
   **`FIND-SERVER`** to **`255.255.255.255:6001`**; the receiver (listening on 6001) replies with an
   ASCII **`"<IP>:<tcpPort>"`** string.
2. **Control channel** (`Network/NetWork*.cs`, TCP to `<IP>:<tcpPort>`): framed LitJson messages
   (`MessageType`: Connected/Disconnect/HeartBeat/EnterRoom/ExitRoom/UpdateCameraParam). Right before
   streaming the sender sends **`{"useAudio": <bool>}`** and waits for **`{"success": true}`**, then
   starts the capture. (The exact TCP framing is `Network/Tools/MessagePacker.cs` — would need RE.)
3. **RTP** (`Scripts/FirstPersonStreammingCast.cs:55`): the URL is **hard-coded `rtp://<IP>:5555`** — the
   discovered port is only the TCP control port, NOT the RTP port. So video lands on 5555 and audio on
   5557 (as captured), which is **exactly where our app already streams**.

**IMPLEMENTED + device-verified (2026-07-19).** Confirmed empirically that streaming to a fixed
`rtp://<PC>:5555` does nothing until the handshake runs (the receiver flashes then drops back to its
idle "FIND-SERVER" screen when no paired RTP arrives). The handshake now lives in
**`demo/stream_pairing.gd`** — a `PacketPeerUDP` broadcast + `StreamPeerTCP` framed-message state machine
(discover → connect → EnterRoom → `{"useAudio":bool}` → HeartBeat every 1 s). `stream_manager` starts it
on the Stream toggle and, on the async `paired(ip)` signal, immediately `stream_start`s to
`rtp://<ip>:5555`; `active_changed` reflects the real state back onto the toggle. Validated the whole
protocol against the real `StreamingReceiver.exe` twice — once with a Python client, once by driving the
actual GDScript headlessly — before the on-device run, which showed the receiver playing the head-POV
**video with audio**. Framing recap (little-endian): `[u16 length][u16 type][payload]`, length includes
the 4-byte header; a MsgSync(7) payload is `[u64 msgid][UTF-8 JSON]`; EnterRoom(4) ack is `{"result":true}`.
Since the destination is discovered, the phone menu no longer needs a stream-target field.
