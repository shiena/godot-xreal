# Stage 4 Design Review — FPV Streaming and MP4 Recording under Vulkan

## Executive decision

Use a stage-2-style OPAQUE_FD bridge, with two encoder bundles at the configured stream resolution. Copy the selected SubViewport into one bundle using the existing bridge-owned Vulkan command buffer, then encode the other bundle only after its previous-frame fence has completed.

All Vulkan encoder lifecycle calls—`HWEncoderCreate`, configuration, `SetMediaProjection`, `Start`, `UpdateSurface`, `Stop`, and `Destroy`—should execute from the Vulkan frame-drawn tick with the private EGL context current.

The proposed “copy, then immediately call `UpdateSurface` in the same pipelined tick” is unsafe: the current fence proves completion only at the next tick. `UpdateSurface` could sample an image while `vkCmdCopyImage` is still writing it. A previous-frame ping-pong bundle fixes that without restoring the 52–53 FPS `vkQueueWaitIdle` path.

## 1. Vulkan-to-encoder image path

Create two `EncoderBundle`s, structurally equivalent to `EyeBundle`:

```text
EncoderBundle {
    VkImage RGBA8,
    dedicated exportable VkDeviceMemory,
    GL memory object,
    GL_TEXTURE_2D name,
    width, height,
    first_use,
    timestamp_ns,
}
```

The Vulkan allocation is exported through `vkGetMemoryFdKHR` as OPAQUE_FD and imported into the existing private EGL context with `GL_EXT_memory_object_fd`. Do not use AHardwareBuffer; the device-verified extension constraint remains unchanged.

Per Vulkan tick:

1. Bind the private EGL context.
2. Wait for the previous bridge submission fence.
3. If an encoder bundle became ready, call:

   ```text
   HWEncoderUpdateSurface(handle, ready.gl_name, ready.timestamp_ns)
   glFinish()
   ```

4. Record eye copies and the next encoder copy into the same bridge-owned command buffer.
5. Submit once with the existing fence.
6. Run the glasses submission operation.
7. Unbind the private EGL context.

The encoder copy uses the same barriers as the eye bundles:

```text
EXTERNAL / GENERAL
    -> Godot queue / TRANSFER_DST_OPTIMAL
vkCmdCopyImage
Godot queue / TRANSFER_DST_OPTIMAL
    -> EXTERNAL / GENERAL
```

The SubViewport source is temporarily transitioned from its verified post-frame layout to `TRANSFER_SRC_OPTIMAL`, copied, then restored exactly.

Store the source timestamp in the destination bundle when recording the copy. The timestamp passed to the encoder one tick later must describe the copied frame, not the current tick.

### Why two bundles

At tick N, bundle A is known complete and sampled by the encoder while bundle B receives the new Vulkan copy. At tick N+1 they exchange roles. This gives:

- one frame of fixed latency;
- no same-frame Vulkan-write/GL-read race;
- no per-frame `vkQueueWaitIdle`;
- no overwrite of a bundle currently being sampled.

Call `glFinish()` after `UpdateSurface`. External-memory sharing does not itself supply execution synchronization from GL back to Vulkan. Given the documented current-context sampling contract, `glFinish()` is the cheapest defensible release before that bundle is reused on the next tick.

### Alternatives

- `RenderingDevice.texture_update` is not useful here. It uploads CPU bytes into Vulkan and produces no GL texture name. It solves the camera’s CPU-source problem, not Vulkan-to-GL sharing.
- A second EGL context adds a share-group problem and no capability. Reuse the private context already proven by stage 2.
- Driving a MediaCodec input `Surface` directly would replace vendor texture conversion, muxing, RTP, timestamps, and audio integration. That is a separate encoder implementation, not a stage-4 bridge.
- `HWEncoderStartWithRenderInstance` is intriguing but currently RE/unverified. Do not make it the production path without a backed ABI and an entry in `reverse-engineering.md`.

## 2. Encoder EGL-context capture

The important fact is not merely that a context is current during `UpdateSurface`. The texture name exists in the private context’s GL share group. If the encoder’s worker context is not shared with that group, the numeric name is meaningless there even though the underlying allocation is exportable.

The current private context is explicitly created with `EGL_NO_CONTEXT`, so it shares with nothing until the encoder creates a context sharing with it.

The repository does not establish which `HWEncoder*` call captures `eglGetCurrentContext`. Likely possibilities are:

- `Create` constructs the encoder’s GL helper;
- `Start` creates its GL thread/shared context;
- the first `UpdateSurface` initializes it lazily.

Therefore, running `Create/SetConfigration/Start` on the main thread with no EGL context is not an acceptable production assumption. Move the whole Vulkan lifecycle onto the tick thread with the private context bound. Move `Stop/Destroy` there too: teardown may destroy shared GL objects or join a GL thread whose assumptions were established at startup.

Implement an asynchronous lifecycle state machine:

```text
Idle
StartPending(config)
Running(encoder)
StopPending
Failed(error)
```

Under Vulkan, `stream_start` queues `StartPending` and returns `true` only to mean “request accepted.” The next frame-drawn tick performs the native start. GDScript must observe the state before reporting a durable active state. Similarly, recorder completion and gallery publication must wait until `Stop/Destroy` finishes and the state reaches `Idle`.

### Device probes

Run these before committing to the final lifecycle contract:

1. Log `eglGetCurrentContext` and thread ID at every `HWEncoder*` call.
2. Encode a solid imported texture with:
   - start on the main thread, update on the private context;
   - start and update on the private context.
3. Hook or trace `eglCreateContext` inside `libmedia_codec.so` and record its `share_context` argument. Frida on a debuggable build is the cheapest direct answer; static disassembly is the fallback.
4. Call `glIsTexture(bundle.gl_name)` immediately before `UpdateSurface`.
5. Confirm whether the first `UpdateSurface` creates the worker context.

If both startup placements work, retain tick-thread startup anyway: it removes dependency on undocumented lazy initialization.

## 3. Synchronization and tearing

The existing one-frame fence protects command-buffer reuse but does not make the current submission complete before a same-tick encoder call. That is the principal synchronization hazard.

With ping-pong:

```text
Tick N entry: wait fence for A
GL samples A; glFinish
Vulkan copies current source into B; submit fence

Tick N+1 entry: wait fence for B
GL samples B; glFinish
Vulkan copies current source into A
```

Possible tearing would appear as old and new regions within one decoded frame, especially if GL sampling overlaps a tiled Vulkan transfer.

The cheapest detector is a source that alternates full-frame magenta and green every rendered frame, with a large frame number in both the top and bottom halves. Capture the browser or local MP4 and search for:

- mixed magenta/green frames;
- top and bottom numbers from different phases;
- partially updated tile bands.

Add a temporary `debug.xreal.vk_encoder_sync=0` mode using `vkQueueWaitIdle` before encoding. If artifacts disappear only in that mode, synchronization is confirmed as the cause.

## 4. API and dispatch

Add a backend query:

```text
get_render_texture_encoder_backend()
0 = unsupported
1 = GL
2 = Vulkan bridge
```

Then:

```text
is_render_texture_encoder_supported() = backend != 0
```

Vulkan support should depend on successful Vulkan/OPAQUE_FD/GL-memory-object initialization, not on the glasses-rendering kill switch. The frame-drawn tick must run when either glasses rendering or Vulkan encoding has work.

Keep the existing GL frame closure unchanged:

```gdscript
texture_get_native_handle(...)
stream_push_frame(gl_name, timestamp)
```

Add a Vulkan API such as:

```text
stream_publish_viewport(viewport_rid: RID, timestamp_ns: int) -> int
```

Rust resolves each publication through:

```text
viewport RID
-> real render-target texture RID
-> RD texture RID
-> VkImage, format and extent
```

and stores it in a latest-wins mailbox. Resolution and format are validated before the tick records a copy.

Return values:

- `stream_publish_viewport`: `0` accepted, `-1` encoder idle, `-2` invalid RID/format/extent.
- Existing `stream_push_frame`: unchanged under GL; under Vulkan return `-2` and never interpret its integer argument as a GL name or `VkImage`.

Expose lifecycle state and last error so asynchronous Vulkan startup and shutdown are observable.

On every start, apply the resolution preset before creating SubViewports and bundles. Rebuild existing SubViewports when their size differs, not merely when stereo mode changes. Encoder bundles are keyed by final encoder output size and recreated between sessions after the previous fence, GL sampling, and native encoder teardown have completed. Resolution changes while active remain deferred until the next start.

## 5. MP4 recorder

Recording uses exactly the same Vulkan bundle and encoder path. Only `codecType` and `outPutPath` differ.

Microphone and app audio remain unaffected:

- microphone capture remains native and permission-gated;
- app audio remains the encoder’s MediaProjection-based playback capture;
- no Godot audio samples enter the bridge.

For Vulkan, `_stop()` must not immediately emit `finished(path)`. It requests stop, waits until the tick has executed `HWEncoderStop` and `HWEncoderDestroy`, and only then emits `finished`. This guarantees that `StorageHelper.save_video(path)` sees a finalized MP4 rather than racing the muxer.

Abnormal `_exit_tree` teardown should request tick-thread finalization when the rendering loop is alive. If shutdown has already removed that loop, use an explicit earlier application shutdown hook; do not silently destroy the Vulkan encoder from an arbitrary thread.

## 6. Ranked failure modes and probes

1. **Encoder worker context does not share the private context.**  
   Probe: private-context startup A/B, solid texture, trace `eglCreateContext.share_context`.

2. **Vulkan copy races GL sampling.**  
   Probe: alternating-color/frame-number test; compare ping-pong against forced `vkQueueWaitIdle`.

3. **GL-to-Vulkan reuse lacks completion.**  
   Probe: enable/disable post-`UpdateSurface` `glFinish`; look for tearing or driver faults.

4. **Source format/layout differs from the eye SubViewports.**  
   Probe: log format, extent, VkImage and actual post-frame layout; fail closed instead of copying.

5. **Frame-drawn tick is disabled when Vulkan glasses are disabled.**  
   Probe: record with the glasses bridge kill switch off and verify encoder tick counters advance.

6. **Lifecycle call requires the Android main thread or attached JNI state.**  
   Probe: start/stop a local MP4 from the bound tick before testing RTP; inspect native status and crash stack.

7. **Stop returns before mux finalization or deadlocks joining the encoder GL thread.**  
   Probe: repeated 2-second Record/Stop loops; verify non-zero, decodable MP4 after every transition.

8. **Resize leaves stale source handles or bundles.**  
   Probe: LOW → HIGH → stereo → mono across separate sessions, checking dimensions and allocation logs.

9. **Performance regression.**  
   Probe: stream at 1280×720 while glasses and camera are active; compare FPS, encoder cadence and thermal state.

## 7. Device verification checklist

- Build and run Vulkan Mobile on the Beam Pro.
- Local solid-pattern MP4 before network testing.
- Real AR-only stream, then camera+AR blend.
- Run `scripts/stream_server/fpv_server.py`.
- Complete discovery/pairing and confirm RTP video and audio ports receive traffic.
- Open the served browser page and verify that the browser itself renders continuously changing live video; packet arrival or ffmpeg logs alone do not pass.
- Observe head motion, colors, blend alignment, frame cadence and audio for at least ten minutes.
- Run the alternating-color tearing probe and inspect decoded output.
- Record with no audio, mic, app audio, and both audio sources.
- Stop recording, wait for finalization, then verify:
  - non-zero MP4;
  - expected dimensions, H.264 duration and frame count;
  - AAC presence when requested;
  - playback to the end;
  - successful gallery publication and playback from the gallery.
- Repeat across resolution levels and stereo/mono recording.
- Perform clean stop/restart loops for streaming and recording, including mutual-exclusion checks.
- Run the existing GL Compatibility build afterward:
  - current GL-name frame push unchanged;
  - browser live stream works;
  - MP4 and audio remain valid;
  - gallery publishing remains correct;
  - no Vulkan bridge initialization appears in logs.