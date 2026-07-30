# Stage 3 design: Vulkan camera rendering re-optimization

## Decision

Use Godot’s global `RenderingDevice.texture_update()` on the render thread, backed by persistent `Texture2DRD` resources.

Do not extend the stage-2 raw Vulkan bridge for the primary implementation. A custom mapped staging ring plus `vkCmdCopyBufferToImage` could remove one CPU copy, but it would also make this extension responsible for Godot-owned texture layouts, access synchronization, staging-buffer fences, and ordering against sampling. That risk is not justified until `texture_update` has been measured on the Beam Pro.

One correction to the current plan wording: `texture_update` is not safely callable inline from the existing main-thread `poll_frame()`. RenderingDevice access must be dispatched through `RenderingServer.call_on_render_thread()`, because rendering internals may live on a separate thread. [Godot documents that requirement explicitly](https://docs.godotengine.org/en/latest/classes/class_renderingserver.html?highlight=render_loop).

The expected result is “same performance class as 525 µs,” not a promise of 525 µs. Godot necessarily adds an owned-byte-buffer handoff that the GL path avoids.

## 1. Upload mechanism and copy accounting

### Main-thread acquisition

On each `poll_frame()`:

1. First publish any render-thread completion from the previous poll, as described below.
2. Call the existing `session.rgb_camera_with_frame()` on the main thread.
3. Keep the existing timestamp gate and SDK-holder mutex discipline unchanged.
4. While the acquired handle is alive:

   - Copy Y directly from the SDK plane into a reusable `PackedByteArray`.
   - Interleave U/V directly into a reusable CbCr `PackedByteArray`; remove the intermediate `Vec<u8>` for this path.
   - Perform `copy_luma()` into the retained CPU buffer when `cpu_luma_step > 0`.
   - Record timestamp, dimensions, mean luma, and acquisition timings.

5. Return from the closure, allowing the SDK handle to be disposed.
6. Move the two reference-counted arrays into a bounded upload mailbox and schedule one render-thread callable.

The SDK pointers never escape `rgb_camera_with_frame()`. The internal SDK mutex remains held only around `TryAcquireLatestImage`, exactly as now; neither the plane copy nor GPU work expands its critical section.

### Render-thread upload

The callable:

1. Lazily creates the two RD textures if necessary.
2. Calls:

   ```text
   rd.texture_update(y_rd_rid, 0, y_bytes)
   rd.texture_update(cbcr_rd_rid, 0, cbcr_bytes)
   ```

3. Checks both returned `Error` values.
4. Publishes a completion record to the main thread.

The textures use:

- Y: `DATA_FORMAT_R8_UNORM`, 1280×720.
- CbCr: `DATA_FORMAT_R8G8_UNORM`, 640×360.
- `TEXTURE_TYPE_2D`, one layer, one mip, one sample.
- Usage: `TEXTURE_USAGE_SAMPLING_BIT | TEXTURE_USAGE_CAN_UPDATE_BIT`.

The driver should copy the byte payload into an upload/staging allocation and perform a GPU buffer-to-optimal-image transfer. That moves Adreno’s tiled-image conversion out of a per-texel CPU path, which is the Vulkan equivalent of the successful PBO mechanism.

### Copies per camera frame

With `cpu_luma_step == 0`:

- Y:

  1. SDK plane → owned `PackedByteArray`.
  2. `PackedByteArray` → Godot/driver staging memory.
  3. GPU staging buffer → tiled R8 image; this is a GPU transfer, not another CPU byte copy.

- CbCr:

  1. U/V planes → interleaved owned `PackedByteArray` in one transform/write.
  2. `PackedByteArray` → Godot/driver staging memory.
  3. GPU staging buffer → tiled RG8 image.

Thus the CPU writes two full payloads, approximately `2 × 1,382,400` bytes per camera frame. CbCr interleaving is part of the first payload construction.

With `cpu_luma_step > 0`, retain the current additional luma copy. At step 1 that adds 921,600 output bytes and the measured source-cache traversal; it is deliberately independent of the GPU path.

Whether Godot can sometimes adopt rather than copy the array is an implementation detail and must not be assumed in the design or accounting.

## 2. Texture identity and lifetime

Change both getter return types from `ImageTexture` to the common base `Texture2D`:

```rust
fn get_y_texture(&self) -> Option<Gd<Texture2D>>;
fn get_cbcr_texture(&self) -> Option<Gd<Texture2D>>;
```

Under GL, store/upcast the existing `ImageTexture`; its PBO path remains unchanged. Under Vulkan, store `Texture2DRD`. All current consumers pass these resources to shader texture uniforms, so they require `Texture2D`, not specifically `ImageTexture`.

For each plane:

- Create the RD RID on the render thread with `texture_create`.
- Create a `Texture2DRD` wrapper and assign its `texture_rd_rid`.
- Keep both the wrapper and underlying RD RID for the feed’s lifetime.
- Never replace either resource during ordinary updates. Consumers therefore retain stable texture identity.

`Texture2DRD` is explicitly intended to expose a directly created RenderingDevice texture to materials and meshes. [Godot’s class documentation confirms this contract](https://docs.godotengine.org/en/4.5/classes/class_texture2drd.html).

Do not rely on Rust `Drop` to call RenderingDevice. Add explicit renderer-lifecycle cleanup:

1. Stop accepting camera upload jobs.
2. Let or cancel queued jobs and wait until no render callback owns their arrays.
3. Clear both `Texture2DRD.texture_rd_rid` properties.
4. On the render thread, call `rd.free_rid()` for both RD RIDs.
5. Drop the wrappers only afterward.

Run this before the RenderingDevice and the stage-2 render callback infrastructure disappear. `RenderingServer.free_rid()` alone would not free an underlying RD texture; Godot documents the two ownership layers separately. [RenderingServer texture ownership notes](https://docs.godotengine.org/en/stable/classes/class_renderingserver.html).

A camera off/on cycle may retain the textures, avoiding allocation churn. Full feed destruction or application renderer teardown must release them.

## 3. Threading and publication contract

The main thread continues to grab SDK frames. Moving acquisition to the render thread would unnecessarily hold the render loop inside SDK code and complicate signal delivery.

Use a two-slot “latest wins” mailbox:

```text
main thread: SDK acquire → copy/CPU luma → pending slot
                                      ↓
render thread: texture_update Y/CbCr → completed generation
                                      ↓
next main poll: publish CPU snapshot → frame_changed
```

Each slot owns its `PackedByteArray`s, generation, timestamp, CPU-luma snapshot, dimensions, and timings. No raw SDK pointer crosses threads.

`frame_changed` is emitted on the main thread only after the render thread has successfully issued both `texture_update` calls. Immediately before emission:

- Publish the matching `y_cpu` and `y_cpu_size`.
- Install the `Texture2DRD` wrappers if this was the first successful upload.
- Update counters and timing state.

Consequently, a signal handler cannot observe new CPU luma with old textures, or one plane from a different generation. As with the GL PBO path, “uploaded” means the GPU command has been accepted and correctly ordered, not that the GPU is already idle.

When `feed_camera_server == true`, keep the complete Image path. `set_ycbcr_images()` requires `Image` objects and emits `frame_changed` itself; mixing that route with the asynchronous RD path would either duplicate work or violate its existing signal behavior.

`poll_frame()` should return `true` when it publishes a completed generation, rather than when it merely queues one. Document this one-poll pipeline explicitly.

## 4. Latency and pacing

The camera produces approximately 30 Hz while polling/rendering runs around 60 Hz.

Drops can occur at two points:

- The SDK already exposes “latest frame”; a frame superseded before acquisition is inherently dropped.
- If a render upload is outstanding and another camera frame arrives, replace the not-yet-submitted pending slot with the newer frame. Never build an unbounded queue.

Do not overwrite the slot currently borrowed by the render thread.

Normal latency:

- Acquisition occurs during one main iteration.
- `call_on_render_thread` uploads before a subsequent render synchronization point.
- Completion is published and signaled on the next main poll.

This adds up to one main-loop interval to CPU notification. Depending on Godot’s callback scheduling, the displayed texture can update in the same rendered frame or the following one. The conservative bound is one extra 60 Hz frame versus the inline GL PBO path.

Avoid the stage-2 frame-drawn callback for camera uploads: it is intentionally post-render, which guarantees an additional rendered-frame delay. Use `call_on_render_thread()` instead.

## 5. `camera_feed.rs` integration

Add a renderer-first dispatch:

1. Existing GL direct branch: unchanged.
2. Vulkan RD branch when all are true:

   - Renderer is not GL.
   - `debug.xreal.vulkan_camera == 1`.
   - Global RenderingDevice exists.
   - `feed_camera_server == false`.
   - Vulkan camera failure latch is clear.

3. Existing Image branch: universal fallback.

The kill switch defaults off for the first device build:

```text
adb shell setprop debug.xreal.vulkan_camera 1
```

Sample it at capture start, matching `camera_timing`, so camera off/on applies changes without restarting. Any structural failure latches `VK_CAMERA_FAILED` for that capture and falls back to Image on the next frame. Because the getters return `Texture2D`, fallback may replace the backing resource without another public API change.

Add timing fields:

- `snapshot_y`
- `interleave`
- `cpu_luma`
- `queue_wait` — acquisition to render callback
- `rd_update_y`
- `rd_update_cbcr`
- `publish_lag` — render completion to main publication
- `dropped_pending`
- `upload_errors`

Report the path as `vk_rd`. The existing `upload_y/upload_cbcr` labels may remain aliases, but logs must state that these measure CPU/API submission time only, not GPU completion.

## 6. Ranked failure modes and cheapest probes

1. **Wrong render-thread access or lifetime**

   Probe: enable only RD texture creation and a one-shot constant upload. Require no thread-safety error, crash, or invalid RID during startup and clean exit.

2. **`texture_update` stalls**

   Probe: upload Y only for 300 frames and log median/p95/max API time and FPS; then add CbCr. If calls repeatedly exceed approximately 1 ms or force 60→30/45 FPS, kill the primary design.

3. **R8/RG8 sampling or format support on Adreno 710**

   Probe: query `texture_is_format_supported_for_usage`, then upload synthetic Y and CbCr ramps. Verify channels through the existing conversion shader. If RG8 alone fails, test two R8 chroma textures only as a diagnostic—not as an automatic contract change.

4. **Texture2DRD wrapper lifetime or identity**

   Probe: cache both getters once in GDScript, run camera off/on, and verify the cached resources resume updating without reacquisition. Then exit through the app button under validation.

5. **Cross-plane generation mismatch**

   Probe: synthetic alternating frames whose Y and CbCr encode the same sequence bit. A mismatch produces an unmistakable alternating color error.

6. **Mailbox pacing error**

   Probe: deliberately delay every tenth render upload. Confirm bounded memory, rising `dropped_pending`, recovery to the newest frame, and no stale burst afterward.

7. **Validation/layout errors**

   Probe: Vulkan validation log during the synthetic test, then normal camera. Any layout/access error attributable to `texture_update` is a Godot/backend issue; do not paper over it with stage-2 barriers.

Escalate to a persistent mapped Vulkan staging ring only if the path is correct but `texture_update`’s second CPU copy is measured as the dominant regression. That implementation should use extension-owned VkImages wrapped with `texture_create_from_extension`, not issue raw writes into ordinary Godot-created textures.

## 7. Device verification: 5–10 minute soak

- Confirm kill switch off selects `path=image`; GL build still reports `path=direct`.
- Enable Vulkan camera and restart capture; confirm `path=vk_rd`.
- Verify full-color live image, orientation, Y/CbCr channel order, and no pink/green corruption.
- Cache getter results once and confirm they continue updating.
- Verify `frame_changed` averages approximately camera rate, not render rate and not double rate.
- In the signal handler, compare CPU-luma generation/mean against the published texture test pattern.
- Test `cpu_luma_step` values 0, 1, and 2.
- Record median/p95/max for snapshot, interleave, both updates, queue wait, and total CPU cost.
- Confirm 58–60 FPS with glasses rendering active.
- Create temporary render pressure; confirm bounded drops and newest-frame recovery.
- Toggle camera off/on at least five times.
- Exercise panel, blend capture, photo capture, and any Vulkan-supported consumer.
- Run once with `feed_camera_server=true`; confirm deliberate Image fallback and exactly one signal per frame.
- Watch validation, Godot errors, Scudo, and camera-service logs.
- Exit through the app button while capture is active; require clean texture teardown and an immediately successful next launch/capture.
- Continue to 10 minutes; require stable memory, no growing pending queue, no FPS decay, and no camera wedge.