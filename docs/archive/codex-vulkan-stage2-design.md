# Stage 2 Design — Glasses Rendering under Vulkan

> Provenance: written by codex (codex-cli 0.144.4) on 2026-07-30 as the independent second
> opinion for vulkan-path-plan.md stage 2, from the repo sources and plan docs, without seeing
> our own draft. The adopted/amended verdicts live in `docs/plans/vulkan-path-plan.md`.

Status: proposed implementation design  
Target: Godot 4.7 Mobile renderer, Android, Adreno 710  
Scope: restore glasses rendering while retaining the proven GLES fake-`IUnityXRDisplay` compositor path

## 1. Decision

Use the existing two-eye `SubViewport` rig and the SDK’s existing multipass GL swapchain unchanged. For every texture requested through `IUnityXRDisplay::CreateTexture`, allocate one RGBA8 `AHardwareBuffer`, import it into both APIs, and retain:

- An AHB-backed `VkImage`, owned by `godot-xreal`.
- An `EGLImageKHR`-backed `GL_TEXTURE_2D`, also owned by `godot-xreal`.
- The existing small Unity texture ID returned to the SDK.
- Optional Godot `RenderingDevice` RIDs used for probing and fallback, but not required by the primary path.

The primary Vulkan fill operation should be a bridge-owned fullscreen Vulkan pass that samples the exposed eye `SubViewport` image and writes into the acquired AHB image. It should run on Godot’s Vulkan queue after Godot’s rendering for that frame.

This is preferable to making `RenderingDevice.texture_copy` the production path because:

1. A copy is legal only when the viewport and AHB formats are copy-compatible.
2. `texture_create_from_extension` creates a Godot view/tracker around the foreign image but does not provide external queue-family ownership transfer or an exportable completion fence. Godot’s implementation explicitly creates only a view over the supplied image; it does not own the image or memory ([RenderingDevice implementation](https://github.com/godotengine/godot/blob/master/servers/rendering/rendering_device.cpp#L1667-L1703), [Vulkan driver implementation](https://github.com/godotengine/godot/blob/master/drivers/vulkan/rendering_device_driver_vulkan.cpp#L2239-L2269)).
3. A fullscreen pass provides defined conversion into RGBA8 and gives the bridge explicit control of the AHB image’s Vulkan layout, foreign ownership, and completion semaphore.

Keep a `texture_copy` fast path behind a capability flag after its source format has been proven compatible. Do not begin with direct-to-AHB `SubViewport` rendering; Godot 4.7 exposes no supported API for replacing a `SubViewport`’s internally allocated render target.

Multipass remains the only Stage 2 shipping mode. An AHB layer-array path is unproven and brings no rendering reduction with two independent Godot viewports.

## 2. Resource model and lifetime

`xr_create_texture` currently runs seven times. Under Vulkan, each request with `color == 0` and `textureArrayLength <= 1` creates a `VkGlEyeBuffer`:

```text
VkGlEyeBuffer
├── AHardwareBuffer*
├── VkImage
├── VkDeviceMemory
├── VkImageView
├── EGLImageKHR
├── GLuint texture_2d
├── Unity texture ID
├── width, height, original flags/color_format
├── current owner: Vulkan or FOREIGN/GL
└── synchronization objects
```

Seven 1968×1134 RGBA8 buffers cost approximately 62.5 MiB before allocator metadata. This is expected and preferable to introducing a second bridge ring: use the SDK’s existing ring directly.

The Vulkan and EGL imports reference the same AHB allocation. The SDK sees only `texture_2d`, exactly as under Compatibility.

### Per-frame flow

```text
Godot Mobile renders left/right SubViewports
        ↓
frame-post-draw render-thread callback
        ↓
private EGL context current
PopulateNextFrameDesc → SDK AcquireFrame → two Unity texture IDs
        ↓
map IDs to two VkGlEyeBuffers
        ↓
Vulkan acquire AHB images from VK_QUEUE_FAMILY_FOREIGN_EXT
        ↓
fullscreen Vulkan draw:
    Godot viewport VkImageView → AHB RGBA8 VkImage
        ↓
release AHB images to VK_QUEUE_FAMILY_FOREIGN_EXT
        ↓
wait/transfer completion to EGL
        ↓
SubmitCurrentFrame with private EGL context still current
        ↓
SDK compositor samples the existing GL_TEXTURE_2D names
```

The post-draw hook must execute on Godot’s render thread, after the eye viewports have been rendered and Godot has submitted or finalized their commands. The bridge submits its own command buffer to the same Vulkan queue. Queue submission order then orders Godot’s writes before bridge sampling without a semaphore from Godot.

If `frame_post_draw` is emitted on another thread, its handler must schedule a `RenderingServer.call_on_render_thread` callback. Record the render-thread TID on initialization and fail closed if it changes or the callback is not on that TID.

### Destruction order

On provider shutdown or display reinitialization:

1. Stop acquiring/submitting new frames.
2. Wait for the bridge’s outstanding Vulkan fences.
3. With the private EGL context current, delete GL textures and destroy EGLImages.
4. Free any Godot wrapper RIDs, then destroy bridge `VkImageView`, `VkImage`, and `VkDeviceMemory`.
5. Release the AHB reference.
6. Unbind and destroy the EGL surface/context last.

`xr_destroy_texture` may run off-thread. It should remove the Unity-ID mapping and enqueue the complete bridge object for render-thread destruction, replacing the current GL-name-only deletion queue.

## 3. Private EGL context

Create it lazily on the Vulkan render thread before the first provider `gfx.start`.

Use:

- `eglGetDisplay(EGL_DEFAULT_DISPLAY)`.
- The process’s initialized display; call `eglInitialize` defensively and validate the result.
- `EGL_OPENGL_ES_API`.
- A config with:
  - `EGL_RENDERABLE_TYPE = EGL_OPENGL_ES3_BIT_KHR`
  - `EGL_SURFACE_TYPE = EGL_PBUFFER_BIT`
  - RGBA 8/8/8/8
  - depth/stencil 0
- Prefer an ES 3.2 context through `EGL_KHR_create_context`; fall back to `EGL_CONTEXT_CLIENT_VERSION = 3`.
- Prefer `EGL_KHR_surfaceless_context`; otherwise create a 1×1 pbuffer.
- No context sharing is required because Godot owns no GLES context under Vulkan.

Make it current around the complete SDK graphics operation:

1. `GfxThreadStart` and all nested `CreateTexture` calls.
2. `PopulateNextFrameDesc`.
3. EGL-side synchronization.
4. `SubmitCurrentFrame`.
5. Deferred GL/EGL destruction.

Unbind it after each callback with `eglMakeCurrent(..., EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT)`. Vulkan itself does not care, but unbinding prevents unrelated Vulkan-era code from mistaking “an EGL context happens to be current” for permission to interpret Godot handles as GL names. The existing renderer-first gate in `camera_feed` remains mandatory.

All SDK display graphics callbacks must use this one context and thread. Never make it current on the main thread or SDK callback threads.

## 4. Vulkan import and rendering

Obtain the Vulkan instance, physical device, logical device, queue, and queue-family index through `RenderingDevice.get_driver_resource`. Load Vulkan entry points dynamically; route new native loading through `src/native.rs`.

Require and verify callable support for:

- `VK_ANDROID_external_memory_android_hardware_buffer`
- `VK_KHR_external_memory`
- `VK_EXT_queue_family_foreign`
- For asynchronous v2: `VK_KHR_external_semaphore_fd`

Availability is insufficient: the required device procedures must resolve from Godot’s already-created device.

### AHB and image creation

Allocate each AHB as already proven:

```text
format = AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM
usage  = GPU_COLOR_OUTPUT | GPU_SAMPLED_IMAGE
layers = 1
```

Create a matching Vulkan image:

- `VK_IMAGE_TYPE_2D`
- `VK_FORMAT_R8G8B8A8_UNORM`
- extent from the SDK descriptor
- one mip, one layer, one sample
- optimal tiling
- usage:
  - `VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT`
  - `VK_IMAGE_USAGE_TRANSFER_DST_BIT`
  - `VK_IMAGE_USAGE_SAMPLED_BIT`
- `VK_SHARING_MODE_EXCLUSIVE`
- `VkExternalMemoryImageCreateInfo.handleTypes =
  VK_EXTERNAL_MEMORY_HANDLE_TYPE_ANDROID_HARDWARE_BUFFER_BIT_ANDROID`

Call `vkGetAndroidHardwareBufferPropertiesANDROID`, chaining `VkAndroidHardwareBufferFormatPropertiesANDROID`. Require the reported Vulkan format to be RGBA8 UNORM; reject an implementation-defined external-only format.

Intersect:

- `vkGetImageMemoryRequirements().memoryTypeBits`
- AHB properties `memoryTypeBits`

Allocate using a chain containing:

- `VkImportAndroidHardwareBufferInfoANDROID`
- `VkMemoryDedicatedAllocateInfo(image = vk_image)`

Use the AHB-reported allocation size, bind it to the image, and keep the AHB reference until the whole bridge entry is destroyed.

### Primary fill pass

Probe the viewport source on the render thread:

1. `RenderingServer.viewport_get_texture(viewport_rid)`
2. `RenderingServer.texture_get_rd_texture(texture_rid, srgb = false)`
3. `RenderingDevice.texture_get_format`
4. `get_driver_resource(VULKAN_IMAGE_VIEW, rd_rid, 0)`
5. `get_driver_resource(VULKAN_IMAGE_NATIVE_TEXTURE_FORMAT, rd_rid, 0)`

Also log dimensions, samples, usage flags, `VkImage`, view, and native format once.

The default bridge uses a minimal Vulkan graphics pipeline:

- Source: Godot’s exposed viewport `VkImageView`, sampled in
  `VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL`.
- Destination: bridge-owned AHB `VkImageView`.
- One triangle, no vertex buffer.
- No blending.
- Output RGBA8.
- One descriptor set per eye source or update-after-use-safe descriptor ring.
- Classic render pass/framebuffer for the api-4-4 floor; do not require dynamic rendering.
- Fragment shader variants:
  - byte/display-ready passthrough
  - linear-to-sRGB encode
  - diagnostic solid color/frame counter

Validate once with Vulkan validation that the exposed viewport view is in `SHADER_READ_ONLY_OPTIMAL` at the post-draw hook. If Godot 4.7 leaves it in another layout, do not guess silently: record the observed validation state and either use the proven layout while restoring it after the pass, or fall back to a Godot RD draw pass.

For each acquired AHB:

1. Acquire ownership from `VK_QUEUE_FAMILY_FOREIGN_EXT`.
2. Transition `GENERAL → COLOR_ATTACHMENT_OPTIMAL`; first use may use `UNDEFINED`.
3. Render the fullscreen pass.
4. Transition `COLOR_ATTACHMENT_OPTIMAL → GENERAL`.
5. Release ownership to `VK_QUEUE_FAMILY_FOREIGN_EXT`.

The acquire/release barriers use color-attachment write stages/access on the Vulkan side and no Vulkan access mask on the foreign side.

### Alternatives

**(a) `texture_create_from_extension` plus `texture_copy`:** implement first as a diagnostic fast path. Wrap the AHB image as `TEXTURE_TYPE_2D`, `R8G8B8A8_UNORM`, one sample/layer/mip, with `CAN_COPY_TO_BIT` and color-attachment usage. Obtain the source RD RID from the viewport texture, then call `texture_copy`.

Enable it only if source and destination formats are copy-compatible, dimensions match, and the source has `CAN_COPY_FROM_BIT`. A successful return is not proof of correct bytes; Vulkan image copy performs no color conversion. It also does not solve foreign ownership or completion fencing, so raw release barriers remain necessary and Godot’s internal layout tracker must be kept consistent. This complexity is why it is an optimization, not the baseline.

`RenderingServer.texture_rd_create` is unnecessary for `texture_copy`; it only creates an RS texture facade over an RD texture. Use it for debug visualization or if a Godot resource must refer to the AHB.

**(b) Direct SubViewport rendering:** not reachable through supported Godot 4.7 APIs. `texture_rd_create` does not replace a viewport render target. Achieving this requires an engine change or native XR render-target integration and is outside Stage 2.

**(c) Fullscreen Vulkan pass:** selected. It tolerates HDR, RGB10_A2, BGRA, or other sampled source formats and makes the RGBA8 output semantics explicit. On the Adreno tiler it adds one sampled fullscreen pass per eye, but avoids CPU traffic and should remain within the 60 FPS target. Add the exact `vkCmdCopyImage` path later only if GPU timing proves the pass material.

## 5. `CreateTexture` and `QueryTextureDesc`

Branch at `xr_create_texture`:

```text
Compatibility renderer
    → existing GL allocation/adoption unchanged

Vulkan + bridge enabled + color == 0 + layers == 1
    → allocate VkGlEyeBuffer
    → return Unity texture ID

Vulkan bridge disabled/failed, color != 0, or layers > 1
    → log once and fail closed
```

The fake graphics device must continue selecting the SDK’s GLES client-texture branch. Do not expose or emulate `IUnityGraphicsVulkan`.

For Vulkan entries, `XrTexture.gl_id` is the EGLImage-backed `GL_TEXTURE_2D` name. Extend the entry with an opaque bridge-resource index instead of placing non-`Copy` native owners directly into the current small struct.

`QueryTextureDesc` must echo:

- `color`: private-context GL texture name
- original width and height
- original `color_format`
- original flags, including the SDK sRGB flag
- original layer count, which must be 1 in the Stage 2 shipping path
- zero depth fields

The actual storage remains UNORM even if the SDK descriptor carries its sRGB bit, matching the proven compositor behavior.

## 6. Synchronization

### v1: serialized correctness bring-up

Use the explicit foreign ownership barriers above, submit the bridge command buffer on Godot’s graphics queue, then call `vkQueueWaitIdle` before `SubmitCurrentFrame`.

v1 relies on:

- Same-queue submission order to wait for Godot’s viewport rendering.
- Queue-family acquire/release for Vulkan↔GL ownership.
- `vkQueueWaitIdle` for completion and visibility before the SDK samples.
- `AcquireFrame` not returning a compositor buffer until its previous GL read is finished—the same swapchain contract already relied upon by the proven GL path.

This is correctness-first and may cost 60 FPS. Measure CPU callback time, GPU frame time, and FPS. Alternating per-frame red/green fills and an encoded frame counter detect stale frames, tearing, and reuse-before-read. Any mixed or repeated frame beyond expected compositor cadence is a synchronization failure.

### v2: asynchronous Vulkan-to-EGL fence

If v1 misses 60 FPS, replace only `vkQueueWaitIdle`:

1. Create an exportable binary Vulkan semaphore with
   `VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_SYNC_FD_BIT`.
2. Signal it from the bridge submission after both AHB release barriers.
3. Export it using `vkGetSemaphoreFdKHR`.
4. With the private context current, import the fd using
   `EGL_ANDROID_native_fence_sync`.
5. Insert `eglWaitSyncKHR`, destroy the EGLSync after inserting the wait, and call `SubmitCurrentFrame`.
6. Transfer fd ownership exactly as EGL specifies; close it only on failed import.
7. Use a small semaphore pool or one semaphore per SDK buffer; do not reuse a binary semaphore until its exported payload has been consumed.

Require `EGL_ANDROID_native_fence_sync`; otherwise retain v1. The wait must be inserted in the same private context in which the SDK receives and operates on the GL names.

Sign-off should use v2 unless v1 independently holds 60 FPS with adequate margin.

## 7. Color management

Use:

- Godot source: probe, never assume.
- AHB: `R8G8B8A8_UNORM`.
- Vulkan image/view: `VK_FORMAT_R8G8B8A8_UNORM`.
- EGL import: native-buffer EGLImage.
- GL target: `GL_TEXTURE_2D`, confirmed as UNORM-backed.
- SDK descriptor: echo its flags unchanged.
- Compositor: unchanged passthrough sampling.

The compositor constraint is satisfied because the bytes presented through the GL texture are display-ready bytes in UNORM storage. No sample-time sRGB decode is introduced by the bridge texture.

Run three Vulkan A/B modes against a known gray ramp, color patches, and the same 3D scene used for the Compatibility baseline:

1. `UNORM passthrough`: source sampled as numeric bytes, written unchanged.
2. `decode + encode`: sample through an sRGB interpretation, then explicitly encode to sRGB before writing UNORM.
3. `decode only`: deliberate dark control.

Capture the physical glasses display, remove alpha before statistics, and compare per-eye mean, standard deviation, patch RGB, and RMSE. The correct mode should match the Compatibility UNORM baseline; the deliberate control should reproduce the dark-direction signature. Keep mode 1 unless the measurement disproves it.

## 8. Dispatch changes

In `node.rs`:

- Create the stereo rig when either Compatibility rendering or the Vulkan bridge is enabled.
- Keep tracking, phone rendering, session, and signals independent of bridge success.
- Under GL, continue publishing GL native handles.
- Under Vulkan, publish viewport texture RIDs, not `u32` native handles.
- Move Vulkan submission to a post-draw render-thread callback so the sampled eye images are complete.
- Keep camera transforms and projections updated on the main thread as today.

In `unity_plugin.rs`:

- Keep `run_render_thread_tick()` GL-only and retain its existing `renderer_is_gl()` self-defence gate.
- Add a distinct `run_vulkan_render_thread_tick()` which requires:
  - Vulkan renderer
  - bridge enabled and initialized
  - expected render-thread TID
  - private EGL context successfully current
- Refactor only the backend-neutral populate/projection/submit logic into shared helpers.
- Backend fill remains explicit:
  - GL backend calls existing GL blits.
  - Vulkan backend maps texture IDs to bridge entries and submits Vulkan work.
- Never weaken the old gate merely because the private EGL context is current.

## 9. Kill switch and device bring-up

Use `debug.xreal.vulkan_glasses`, default `0` for the first landing. Read it before `GfxThreadStart`; when off, behavior remains exactly Stage 1.

Bring-up order:

1. **Context only:** create/make-current/unbind private EGL context. Log EGL version/extensions/TID. Ten-minute phone and tracking soak.
2. **Dual import only:** allocate one AHB, import to Vulkan and EGL, then destroy it. Use logcat and Vulkan validation.
3. **SDK allocation:** enable Vulkan `CreateTexture`; verify seven RGBA8 AHBs, unique GL names, correct `QueryTextureDesc`, no submission.
4. **Solid colors:** ignore Godot input and Vulkan-clear acquired left/right buffers to distinct colors. Confirm correct eyes through physical-display screencap.
5. **Animated colors/frame ID:** verify rotation through all seven buffers and detect stale/reused content.
6. **One real eye:** fullscreen-pass the left viewport to both eyes.
7. **Stereo:** enable both viewports and verify parallax, projection, head rotation, and translation.
8. **Color A/B:** run the three modes above and retain screenshots plus numeric comparison.
9. **Performance:** v1 timing first; switch to v2 if it misses 60 FPS or has poor margin.
10. **Crash bar:** pass frame 1500/25 seconds, then 10 minutes.
11. **Sign-off soak:** three 60-minute sessions including unplug/replug, app background/foreground, clean Exit-button teardown, and thermal steady state.

Collect filtered logcat, FPS output, Vulkan validation, bridge CPU/GPU timings, `dumpsys meminfo`, and physical-display screencaps at each stage.

## 10. Ranked risks and cheapest probes

| Rank | Risk | Cheapest kill-or-confirm probe |
|---:|---|---|
| 1 | SDK sampling does not inherit the private-context EGL fence dependency | Solid alternating frames with v2; compare against v1 `vkQueueWaitIdle`. Any v2-only stale/tearing frame kills v2. |
| 2 | Godot queue use is not externally serialized at the chosen callback | Log callback TID and enable Vulkan validation with one clear submission. Any queue-thread overlap requires a different hook or an engine-side callback. |
| 3 | Viewport image is not shader-readable in the assumed layout after post-draw | One validation-enabled fullscreen sample; inspect the exact layout diagnostic and restore the observed layout. |
| 4 | Godot’s Vulkan device did not enable the AHB or semaphore extensions | Resolve device functions before `GfxThreadStart`; missing functions fail closed to Stage 1. |
| 5 | `AcquireFrame` does not fully retire the previous GL read for foreign ownership reacquire | Per-buffer frame IDs with aggressive high-contrast animation; compare v1 and additional GL completion diagnostics. |
| 6 | Vulkan Mobile output encoding differs from Compatibility | Required three-mode color A/B with alpha removed and numeric RMSE. |
| 7 | Fullscreen conversion pass costs too much on the tiler | GPU timestamps around both eye passes. If material, enable exact-copy specialization only for a proven compatible format. |
| 8 | Seven AHB imports cause excessive memory or teardown leakage | `dumpsys meminfo` before/after repeated session recreation; require flat memory after ten cycles. |
| 9 | Display startup returns 0×0 or changes resolution | Reject zero dimensions and defer `GfxThreadStart`; log descriptor changes and recreate only after a clean provider stop. |
| 10 | Future callers bypass backend safety | Preserve separate GL and Vulkan entry points with renderer, capability, context, and TID assertions. |