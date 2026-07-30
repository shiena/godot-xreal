# Vulkan rendering path — staged plan for a re-attempt

Status: **ALL FOUR STAGES WORKING ON DEVICE (2026-07-31)** - stereo glasses rendering at 58-60
FPS (parity with GL: 59.4 vs 59.8), camera on the vk_rd path, FPV stream rendering live in the
browser, mp4 recording to the gallery. Every stage's design was cross-checked against a codex
second opinion (the review sections below + docs/archive/codex-vulkan-stage*-design.md).
Remaining before flipping the Vulkan preset's defaults ON (`debug.xreal.vulkan_glasses` is still
opt-in): the 60 min x 3 thermal sign-off soak, the libmedia_codec periodic-IDR follow-up (stage-4
notes), and encoder-only mode. Stages: phone screen -> glasses rendering -> camera rendering ->
FPV stream, one commit each.

- **Stage 2 device results (Beam Pro, 2026-07-30)**: 14 opaque-fd eye slots imported and
  registered (7 per eye); solid-color probe correct per eye (left red / right blue, screencap of
  display id 4626964009369245188); real stereo content with live head tracking; crash bar passed
  (fill #5400+); **58-60 FPS with the pipelined-fence sync (now the default; the QueueWaitIdle
  fallback `vk_sync 0` measured 52-53)**; 10 min soak: 20/20 alive checks, 60 FPS at thermal
  steady state, clean Exit-button teardown; **color A/B vs the GL build: cyan object mean
  (151,236,254) IDENTICAL, pink floor within 2/255** - the raw-copy path carries display-ready
  bytes exactly as designed, no sRGB double-transform. **Vulkan-vs-GL FPS parity, same method
  (frame_tick 300-frame intervals, back-to-back runs): GL 59.8 vs Vulkan 59.4** - both
  vsync-locked, the bridge's overhead is inside measurement noise.

- **Stage 0 PASSED**: `src/ahb_probe.rs` (one-shot, render thread, `debug.xreal.ahb_probe 0` to
  skip). RGBA8 1968x1134 with `GPU_COLOR_OUTPUT|GPU_SAMPLED_IMAGE`: isSupported=1, allocate ok
  (row stride 1984 px), EGLImage import + `GL_TEXTURE_2D` bind ok, FBO clear+readback exact
  ([51,102,204,255]), blit-out+readback exact. The Vulkan→GL eye-buffer bridge is viable.
- **Renderer dispatch in place**: `gl.rs::renderer_is_gl()` resolves the runtime renderer once and
  gates the GL glasses path (`node.rs`): Compatibility → unchanged; Vulkan → skip stereo rig +
  swapchain drive, keep tracking/signals/phone display. Verified both ways on device.
- **Vulkan test build**: export preset "Android Vulkan" (package `com.example.godotxreal.vulkan`,
  coexists with the GL app) with `command_line/extra_args="--rendering-method mobile"`. NOTE the
  dead ends, device-verified: `rendering/renderer/rendering_method.mobile` is a REGISTERED setting
  (default "mobile") that the Android export resolves per preset AT EXPORT TIME and bakes into the
  manifest — so (a) removing the line flips EVERY Android export to Vulkan, and (b) custom-feature
  overrides (`.some_feature` suffix) never reach the Android renderer choice. The baked `_cl_`
  command line is the per-preset mechanism that works (the 4.7 boot log then reads
  "renderer: mobile (Default)"; the two APKs' assets differ only in `_cl_`, verified by unzip).
- **Stage 1 (phone screen) DONE**: the demo boots and renders correctly under Vulkan Mobile on the
  Beam Pro (Adreno 710, `renderingDevice: vulkan`). 10 min soak: no crash, no validation error,
  head tracking live throughout, clean exit through the app's Exit button. **Perf baseline for
  stages 2-4: 60 FPS** (406 samples at 60, 102 at 59, 9 at 61, 4 at 58; `--print-fps` is in the
  Vulkan preset's command line). `video_encoder::start()` now refuses to run under Vulkan, so the
  Stream and Record toggles fail cleanly through their existing error paths instead of feeding the
  encoder a VkImage handle (device-verified: warning logged, demo shows "recorder start failed",
  toggle flips back, app survives).
- **GL touch-point audit (mine + a second opinion), stage 1 outcome**: the only genuinely unsafe
  pattern under Vulkan is *interpreting Godot's native handle as a GL texture name*. Photo and
  blend capture use Godot's own viewport readback (`get_texture().get_image()`), not GL FFI, so
  they are renderer-agnostic. `camera_feed`'s PBO fast path already gates on
  `gl::has_current_context()`, false under Vulkan, and falls back to the `Image` path.
  `stream_push_frame` cannot reach GL because the encoder can never be active. So the single
  `start()` gate is sufficient; an extra gate there would be dead code.

### Stage 1 second-opinion review (two designs compared, 2026-07-30)

A second architect audited the same code independently. Where it beat my design, adopted:

- **ADOPTED - `camera_feed`'s direct path must also gate on `renderer_is_gl()`**, not on
  `has_current_context()` alone. My reasoning ("false under Vulkan, so the fallback already
  happens") holds only until stage 2 creates a private EGL context on the render thread: a context
  that happens to be current would let the direct path treat a `VkImage` as a GL name. Check the
  renderer FIRST, so Vulkan never touches the GL/EGL loader.
- **ADOPTED - a self-defence gate at the top of `run_render_thread_tick()`.** `node.rs` is the only
  caller today; the gate keeps a future second caller out of `CreateTexture` / blit / submit. Its
  corollary is right too: do NOT push `renderer_is_gl()` down into the `gl.rs` primitives, because
  stage 2 will drive those same helpers from a private EGL context while Godot runs Vulkan. Gate
  the entry point, not the toolbox.
- **ADOPTED - expose a capability (`XrealSystem.is_render_texture_encoder_supported()`) and refuse
  in the GDScript components before pairing / permissions.** Backed by what the device actually
  did: tapping Record under Vulkan popped the RECORD_AUDIO permission dialog and only then failed.
  Rust keeps the hard safety gate; GDScript avoids the pointless side effects and reports a
  specific reason instead of a generic "start failed". Stage 4 then flips one function.

Rejected, with reasons:

- **`submit_frame`/`stream_push_frame` returning a new `-2` "unsupported renderer" code.** The
  encoder cannot be active while `start()` refuses, so `submit_frame` already returns `-1` without
  touching GL: this adds an API contract for a state that cannot occur. Stage 4 revisits `start()`
  and `submit_frame()` together anyway.
- **A `renderer_is_gl()` gate inside `ahb_probe::run_once()`.** The probe is deliberately a
  GL-mechanism probe, and stage 2 may want to run it (or its successor) under Vulkan from the
  private EGL context. Gating it internally would block that. The caller in `node.rs` gates it.
- **A verification-only `xreal/demo_phone_3d_preview` setting plus a fixed 3D workload.** Worth it
  if stage 1 were the end of the line, but stage 2 puts the 3D world on screen through the eye
  SubViewports by construction, which is the same workload without a demo-side flag to carry.
- **60 min x 3 sessions before moving on.** Kept as the bar for the stage-2 sign-off, not stage 1:
  10 min clean at 60 FPS is enough to know the phone path is not the risk, and the thermal-soak
  budget is better spent once eye rendering is in.

### Stage 2 second-opinion review (two designs compared, 2026-07-30)

Both designs were written independently (ours first, then codex's, neither seeing the other; the
full codex document is `docs/archive/codex-vulkan-stage2-design.md`). They agree on the skeleton:
one RGBA8 AHB per `xr_create_texture` slot (7 x 1968x1134, ~62 MiB, reusing the SDK's own ring),
EGLImage-backed GL names handed to the SDK unchanged, a private surfaceless ES3 EGL context on the
render thread, Multipass only, direct SubViewport-into-AHB rendering out of reach in 4.7, UNORM
end-to-end for color, a kill-switch prop, and the same bring-up ladder shape.

**ADOPTED from codex - the architecture.** Our first cut drove the fill through the main
`RenderingDevice.texture_copy`, and that is architecturally broken: the main RD *defers* the copy
into Godot's frame command buffer (the main RD forbids `submit()`/`sync()`), so within one frame
there is no way to order copy completion before `SubmitCurrentFrame`, and no way to interleave the
foreign queue-family acquire/release barriers in submission order around a copy we do not submit.
codex's structure fixes exactly this: a **post-draw render-thread hook** submits a
**bridge-owned command buffer** to Godot's graphics queue (same-queue submission order puts
Godot's eye rendering before the bridge work), with explicit `VK_QUEUE_FAMILY_FOREIGN_EXT`
acquire/release around the fill, **v1 sync = `vkQueueWaitIdle`** before `SubmitCurrentFrame`,
**v2 = exportable `SYNC_FD` semaphore -> `EGL_ANDROID_native_fence_sync` -> `eglWaitSyncKHR`**.
Also adopted: bind/unbind the private EGL context around each SDK graphics operation instead of
leaving it current (keeps "a context is current" from ever reading as "GL renderer" to bystander
code); a separate `run_vulkan_render_thread_tick()` sharing backend-neutral helpers, so the GL
tick keeps its `renderer_is_gl()` self-defence gate untouched; publish viewport RIDs (not u32 GL
handles) from node.rs under Vulkan; full-bundle deferred destruction through the
`xr_destroy_texture` queue; `debug.xreal.vulkan_glasses` default **off** for the first landing;
the three-mode color A/B (UNORM passthrough / decode+encode / decode-only control); and the
eleven-step bring-up ladder with the risk table.

**AMENDED (ours kept) - the fill operation inside the bridge's command buffer.** codex's primary
is a fullscreen sampled Vulkan pass (pipeline + embedded SPIR-V + descriptors); ours is
`vkCmdCopyImage` from the viewport's VkImage (via `get_driver_resource(TEXTURE, rd_rid)`) into
the AHB VkImage. Copy wins as **fill v1**: a fraction of the code, and raw-byte semantics carry
Godot's display-ready sRGB-encoded bytes unaltered (a *blit* would sRGB-decode a typed source and
land ~26% dark; a copy cannot). Cost: the source must be transitioned
SHADER_READ_ONLY -> TRANSFER_SRC_OPTIMAL and restored exactly (Godot must never notice), and copy
legality requires the probed viewport format to be RGBA8 UNORM/SRGB-typed. The fullscreen pass
stays in the design as **fill v2**, implemented only if the format probe (16F/10-bit source) or
validation kills v1 - it samples in SHADER_READ_ONLY and never touches Godot's layouts, and it is
the only variant that can re-encode arbitrary sources.

**REJECTED from ours, with reasons**: main-RD `texture_copy` as the production path (the ordering
analysis above; codex reached the same verdict from the layout-tracker side); leaving the private
EGL context permanently current; an empty `vkQueueSubmit` + exportable fence as v2 (codex's
semaphore on the bridge submission itself is the same fence without the extra submission).

Minor codex nits found while cross-checking: `VULKAN_IMAGE_NATIVE_TEXTURE_FORMAT` is not in the
4.7 `DriverResource` enum (use `TEXTURE_DATA_FORMAT`), and `texture_get_rd_texture`'s `srgb`
parameter is the ex-builder form in gdext 0.5.3.

## Next steps (resume here)

1. **Stage 1 leftovers: DONE (2026-07-30)**: the `camera_feed` renderer-first gate, the
   `run_render_thread_tick()` self-defence gate (one-shot warning), and
   `XrealSystem.is_render_texture_encoder_supported()` with the refusal in `xreal_stream.gd` /
   `xreal_video_recorder.gd` and the Stream / Record grey-out in
   `demo/main.gd::_apply_capabilities()`. Code-verified (clippy, tests, gdlint); the on-device
   spot-check rides along with the stage-2 soak.
2. **Stage 2 - glasses rendering. Design settled (see the review above); building**: bridge-owned
   post-draw command buffer on Godot's queue, one share bundle per `xr_create_texture` slot,
   fill v1 = `vkCmdCopyImage` with exact source-layout restore (fill v2 = fullscreen pass if the
   probe or validation kills v1), v1 sync = `vkQueueWaitIdle` (v2 = SYNC_FD semaphore -> EGL
   fence), private EGL context bound/unbound around each SDK graphics op,
   `debug.xreal.vulkan_glasses` default off. Keep the fake-`IUnityXRDisplay` GL submission
   unchanged. Bring-up ladder and instruments:
   `docs/archive/codex-vulkan-stage2-design.md` section 9. **Re-run the RGBA8-vs-sRGB color A/B
   on device** (the old measurement was against gl_compatibility output) and the crash bar of
   frame #1500+ / 25 s+, then the 60 min x 3 soak.

   **Share mechanics pivot, device-forced (2026-07-30): OPAQUE_FD, not AHB.** The stage-0 AHB
   share assumed the Vulkan side could import an AHardwareBuffer, but that import needs
   `VK_ANDROID_external_memory_android_hardware_buffer`, and **Godot 4.7 never enables it on its
   device** (checked in 4.7-stable `_register_requested_device_extension`). Device-verified
   failure modes on the Beam Pro: `vkGetDeviceProcAddr` returns null for
   `vkGetAndroidHardwareBufferPropertiesANDROID`, and the `vkGetInstanceProcAddr`-resolved stub
   "succeeds" with `memoryTypeBits = 0`. The working, fully in-spec route inverts the export
   direction: allocate an exportable `VkImage` on Godot's device, `vkGetMemoryFdKHR` (its
   extension `VK_KHR_external_memory_fd` IS enabled by Godot, and `VK_KHR_external_memory` is
   core 1.1) -> OPAQUE_FD -> `GL_EXT_memory_object_fd` import (`glImportMemoryFdEXT` +
   `glTexStorageMem2DEXT`; the Adreno 710 advertises the extension, checked via
   `dumpsys SurfaceFlinger`). The barriers use `VK_QUEUE_FAMILY_EXTERNAL` (core 1.1) instead of
   `FOREIGN_EXT` (extension Godot lacks) for the same reason. Stage 0's conclusion still stands -
   one allocation visible to both APIs - only the import mechanics changed. Note the sync-v2
   implication: `VK_KHR_external_semaphore_fd` is NOT enabled by Godot, so if `vkQueueWaitIdle`
   ever misses 60 FPS the v2 escalation needs a patched export template after all (record the
   measurement first).
3. **Stage 3 - camera rendering.** **3a DONE (2026-07-30)**: the `Image` fallback works under
   Vulkan on device - `path=image` at the camera's ~29 fps, live color image in both eyes
   alongside the AR scene, Project FPS holds 57-60, camera off/exit clean. The stage-1
   capability grey-out (Record/Stream disabled under Vulkan) also verified visually. 3b: recover
   the ~525 us class per the design review below.

### Stage 3 second-opinion review (two designs compared, 2026-07-30)

Both designs (ours in the session scratchpad, codex's in
`docs/archive/codex-vulkan-stage3-design.md`) independently picked the same primary:
**`RenderingDevice.texture_update` on the render thread + persistent `Texture2DRD` wrappers**,
R8/RG8 textures with SAMPLING|CAN_UPDATE, getters widened to `Texture2D`, main thread keeps the
SDK grab, GL PBO path untouched, and both explicitly rejected extending the stage-2 raw bridge
first (staging-ring + own vkCmdCopyBufferToImage stays the escalation if `texture_update`'s
extra CPU copy measures as the dominant cost, via `texture_create_from_extension`, never raw
writes into Godot-created textures).

**ADOPTED from codex - the contracts.** (1) `frame_changed` fires only after the render thread
has actually issued both `texture_update`s, published back on the *next* main poll (a one-poll
pipeline); our draft emitted after merely scheduling the upload, which would let a handler
observe new `y_cpu` with stale textures - a real violation of the feed's emit-last invariant.
(2) A two-slot latest-wins mailbox with a `dropped_pending` counter, never overwriting the slot
the render thread borrowed. (3) Use `call_on_render_thread`, NOT the stage-2 frame-drawn
callback, which is deliberately post-render and would add a guaranteed extra frame. (4) Explicit
teardown order (clear `texture_rd_rid` -> `free_rid` on the render thread -> drop wrappers; no
Drop-based cleanup). (5) `feed_camera_server=true` keeps the whole Image path. (6) The probe
ladder (format-support query, Y-only stall probe, cross-plane generation-mismatch synthetic).
Kill switch `debug.xreal.vulkan_camera`, default OFF for the first landing, sampled at capture
start. Path label `vk_rd`.

**Stage-3b device results (Beam Pro, 2026-07-31): PASS, default flipped ON.** `path=vk_rd` live
at the camera's ~30 fps, RD textures + Texture2DRD wrappers created, colors identical to the
Image path (camera-panel mean RGB matched exactly in an on-device A/B), Project FPS 57-59,
5 min camera soak clean (no demotion, no upload errors). Measured per-grab means (n=120):
vk_rd total 2004 us (acquire 513, snapshot 234, interleave 124, texture_update Y 794 /
CbCr 333, queue_wait 29) vs Image path total 2219 us. The honest reading: under Vulkan the
Image path never had the GL driver's per-texel tiling cost, so the win is NOT the old
"multi-ms -> 525 us" - it is ~10% total and a ~60% cut of the main-thread share (871 us vs
2200 us per grab at 30 Hz). `debug.xreal.vulkan_camera 0` reverts to the Image path.
4. **Stage 4 - FPV stream: WORKING ON DEVICE (2026-07-31).** Device results, Beam Pro + One Pro:
   the encoder starts on the Vulkan tick, the ping-pong bundles alternate (`vk encoder copy/fed`
   logs, status=0 at ~60 feeds/s), and the **browser (fpv_server.py + mpegts.js) rendered the
   live AR stream** (640x360, t advancing, mse=open); **Record -> mp4 -> gallery PASSED** (12 s
   H.264 1280x720 + AAC, real AR content verified by frame extraction, async stop finalized
   before publish). Two findings along the way:
   - **Fixed**: the components sampled the encoder backend at `_ready`, before the bridge
     initializes, and silently stayed on the GL push path (`da10e2f`).
   - **Periodic IDR: root cause found (codex RE, `docs/archive/codex-idr-analysis.md`) and a
     workaround wired.** libmedia_codec 3.1.0 sets `intra-refresh-period=10`, which replaces
     periodic IDR with cyclic intra refresh; `i-frame-interval=1` is correct but overridden. No
     JSON field or HWEncoder* export changes it, and the lib never calls
     `AMediaCodec_setParameters`. Fix taken: reach the underlying `AMediaCodec*` through the
     encoder object layout codex confirmed (`*(handle+0x88)` -> `*(+0x08)`) and inject Android's
     `request-sync` once a second via `libmediandk.so`. **Default ON** through the
     `xreal/idr_workaround` ProjectSetting (registered in `plugin.gd`, so it shows in Project
     Settings and end users can turn it off without adb), overridable at runtime by
     `debug.xreal.idr_hack` (0/1). It depends on that opaque layout, pinned to Build ID
     75a6536f531fa7de046db96609c7e119ad5287f4, so an SDK bump needs the offsets re-checked.
     **Device-verified 2026-07-31: WORKS** - `request-sync -> 0`, MediaCodec logged
     `coding.request-sync-frame.value = 1`, and a browser that reloaded (late-joined) 12 s INTO
     the stream rendered the AR view within ~4 s (black forever without the hack). Rejected
     alternatives: a binary patch of the vendored .so (`nop` the intra-refresh `setInt32` at
     0x20DA70 - clean but rewrites a gitignored vendor lib and fights the vendor flow), and the
     receiver-only ceiling (connect the viewer before start).
   - **Encoder-only mode (Vulkan, glasses kill switch OFF): WIRED.** The bridge machinery is
     split from the glasses kill switch: `ensure_init()` brings the Vulkan side up on demand,
     `bridge_ready()` is the encoder's gate, `glasses_enabled()` stays the eye-rendering gate.
     node.rs registers the tick when glasses OR the encoder wants it; the tick runs
     `submit_encoder_only()` (encoder-bundle copy alone, no eyes, no SDK compositor) when glasses
     are off. `get_render_texture_encoder_backend()` returns 2 for any Vulkan renderer with a
     RenderingDevice. (Needs a live SDK session, i.e. glasses connected, since node.rs's process
     early-returns without one.) **Device-verified 2026-07-31: WORKS** - with
     `debug.xreal.vulkan_glasses 0` (phone-only, head tracking live, no eye rendering), Stream
     ran through `submit_encoder_only` (ping-pong copy/fed status=0) and the browser rendered the
     live AR view.

### Stage 4 second-opinion review (two designs compared, 2026-07-31)

Both designs (ours in the session scratchpad, codex's in
`docs/archive/codex-vulkan-stage4-design.md`) picked the same body: one more stage-2-style
opaque-fd bundle at stream size, filled by `vkCmdCopyImage` in the existing bridge command
buffer, `HWEncoderUpdateSurface` called inside the Vulkan tick with the private EGL context
bound, a viewport-RID publish API replacing the GL-name push under Vulkan, the GL path
byte-identical, and the recorder riding the same path.

**ADOPTED from codex - it caught a real bug in ours.** Our draft called `UpdateSurface` right
after the same tick's submit, but the pipelined fence proves completion only at the NEXT tick:
the encoder could sample a bundle mid-copy. codex's fix is a **ping-pong pair of encoder
bundles** - each tick encodes the bundle whose copy the entry fence just proved complete, and
copies the new frame into the other - one frame of fixed latency, no `vkQueueWaitIdle` return.
Also adopted: the **whole encoder lifecycle moves onto the tick thread** with the private
context bound (Create/SetConfigration/Start/Stop/Destroy - the share-group capture point inside
libmedia_codec is unknown, so never call any of it without the context), driven by an async
state machine (Idle/StartPending/Running/StopPending/Failed) that GDScript observes instead of
assuming synchronous starts; `glFinish()` after `UpdateSurface` before the bundle's next Vulkan
reuse; the encoded timestamp stored at copy-record time (it describes the copied frame, not the
encode tick); `get_render_texture_encoder_backend()` (0/1/2) with Vulkan support keyed to the
bridge machinery, NOT the glasses kill switch, and the frame-drawn tick registered whenever
either has work; `stream_push_frame` returns `-2` under Vulkan; the recorder's `finished(path)`
deferred until Stop/Destroy actually completed on the tick (no racing the muxer); and the
alternating-color tearing probe with a `vk_encoder_sync=0` wait-idle comparison mode.

**REJECTED (deferred) from codex**: Frida/eglCreateContext share-argument tracing as a
prerequisite - the production design takes tick-thread startup regardless, so the trace only
explains, never changes, the decision; and `HWEncoderStartWithRenderInstance` stays out until
RE'd (codex agrees).

Working branch: `feat/vulkan-path`. Soak helper: the scratchpad `soak_vulkan.ps1` (launch, watch,
screenshot, exit through the Exit button, extract FPS + error logs).

Original memo (2026-07-30, from a godot-gsplat planning session) follows.
The one prior attempt is a single line in `port-plan.md` round c: "Vulkan crashed in the Forward
Mobile swapchain" — recorded **before** the fake `IUnityXRDisplay` recipe, the session-bootstrap
fixes and the `UpdateMetrics` patch existed, so that result says little about a re-attempt on
today's rig.

## Why (what Vulkan buys)

- **Path unification with Android XR / Project Aura.** Aura runs standard OpenXR on Godot's
  Vulkan **Mobile** renderer. Everything renderer-side built for a Vulkan One Pro path (shaders,
  RenderingDevice compute, `Texture2DRD` consumers) transfers to Aura unchanged; the
  godot-xreal-specific surface shrinks to eye-texture hand-off + SDK ABI (tracking/camera).
- **RenderingDevice / compute unlocked on One Pro.** The gl_compatibility renderer has no
  RenderingDevice; GPU-compute consumers (e.g. godot-gsplat's compute depth sort lineage)
  currently cannot run at all on this port.
- **`Texture2DRD`** enables a clean re-optimization of the camera upload path (stage 3).

Renderer choice: **Mobile (forward mobile), NOT Forward+.** Adreno 710 is a tiler; Forward+
assumes desktop bandwidth. Everything needed (compute, multiview, Texture2DRD) exists in Mobile.

## Grounding facts (from this repo's RE record — do not re-derive)

- The SDK's own Vulkan branch exists (`OverlayBase::CreateBuffer` `[DM+0x10]==0x15` → NR-owned
  buffers; `NRRenderingCreateVulkanInstance/Device`, `libVulkanSupport.so`) **but requires
  emulating Unity's `IUnityGraphicsVulkan`** (see `reference/reverse-engineering.md`), and even the
  reference Unity app runs GLES3 (`deviceType=11`) on this hardware — the branch is unproven on
  device. **Non-goal: do not attempt it.** Keep the proven GL client-texture-name submission.
- Camera: the public C ABI hands out **CPU-mapped plane pointers only** — no AHardwareBuffer, no
  dmabuf (`VIDIOC_EXPBUF` never called), no surface-mode decoder
  (`archive/camera-zero-copy-investigation.md`, `archive/codex-camera-acquire-analysis.md`).
  Zero-copy camera import is impossible in ANY renderer; the floor is one CPU→GPU upload.
- gralloc probe on X4000/Beam Pro (usage `CPU_WRITE_OFTEN|GPU_SAMPLED_IMAGE`): `R8=0`,
  **`RGBA8=1`**, `YCbCr420=1`. The EGL import machinery
  (`eglGetNativeClientBufferANDROID` → `eglCreateImageKHR` → `glEGLImageTargetTexture2DOES`)
  was written in **commit `c5b9a67`** (reverted in `b041ff9`) but **never executed past the R8
  allocation failure** — revive it for the stage-0 probe.
- This driver bites: `glBlitFramebuffer` to array layer > 0 is a silent no-op, `glCopyImageSubData`
  is a raw byte copy (`archive/multiview-investigation.md`). Probe on device before believing any
  "standard mechanism works" claim, including the ones in this memo.

## The hinge: Vulkan→GL sharing via a self-allocated AHardwareBuffer

The compositor consumes GL texture names. Under a Vulkan renderer Godot owns no EGL context, so
the bridge is: allocate an **RGBA8 AHardwareBuffer** per eye → bind as the Vulkan render target
(`VK_ANDROID_external_memory_android_hardware_buffer`) → import the same AHB as an EGLImage-backed
`GL_TEXTURE_2D` → hand that GL name to the SDK exactly as today. The SDK never sees the AHB.
Note this is the **opposite direction** from the camera finding above: the camera wall was "the
SDK provides no AHB to import"; here we allocate our own.

## Stages (each independently landable, with a kill-switch)

### Stage 0 — AHB bridge probe (NO Vulkan needed; runnable on the current GL build today)

1. `AHardwareBuffer_isSupported` / `allocate`: RGBA8, eye-buffer size (1968×1134), usage
   **`GPU_COLOR_OUTPUT | GPU_SAMPLED_IMAGE`** — this usage combo is UNPROBED (the existing probe
   used CPU_WRITE|GPU_SAMPLED). It is the standard Android composition path, but see the driver
   caveat above.
2. EGLImage import + `glEGLImageTargetTexture2DOES` on `GL_TEXTURE_2D` (revive `c5b9a67` code),
   render into it via FBO, sample it, and ideally hand it to the SDK as an eye texture once.
3. **No-Go** ⇒ there is no practical Vulkan→GL share on Android (CPU readback ≈ 9 MB/eye/frame is
   disqualifying) ⇒ shelve the whole plan. **Go** ⇒ proceed.

### Stage 1 — Vulkan on the phone screen

- **1a.** Plain Godot app, Mobile renderer, phone display, NO XREAL session: does Godot Vulkan run
  stably on this Adreno 710 / Android 14 at all? Include a representative workload (e.g. the
  godot-gsplat splat MultiMesh scene) for a perf baseline.
- **1b.** Add the session bootstrap + head tracking (still phone display). This is where the old
  "Forward Mobile swapchain crash" gets re-diagnosed cheaply — 1a vs 1b splits "Godot/driver
  problem" from "session/JNI/display-mode interaction".

### Stage 2 — glasses submission

- New piece of work: a **private EGL context on the render thread** owned by godot-xreal (Godot no
  longer provides one) for creating the AHB-backed GL names and running the submission-side GL.
  AHB/EGLImage are context-portable, so the compositor still samples them.
- Vulkan side: AHB-backed `VkImage`; wrap for Godot via `RenderingDevice.texture_create_from_extension`
  (render directly), or fall back to one RD `texture_copy` per eye from the viewport texture
  (simpler first; optimize later).
- Keep the existing fake-`IUnityXRDisplay` GL submission unchanged.
- **Re-verify color on device.** The "eye texture must be `GL_RGBA8` UNORM, sRGB-typed is ~26%
  dark" A/B (`archive/multiview-investigation.md`) was measured against gl_compatibility output.
  The Vulkan Mobile renderer's linear→sRGB output differs; re-run the A/B before trusting colors.

### Stage 3 — camera re-optimization (pure perf; nothing blocks earlier stages)

The camera **works from day one** under Vulkan via the existing `Image` fallback
(`gl::has_current_context()` gate in `camera_feed.rs`) at the old multi-ms cost. Then recover the
~525 µs class with `RenderingDevice.texture_update` (staging-buffer copy replaces the PBO trick —
the Adreno per-texel tiling cost was the enemy, see `camera-feed-plan.md`) + `Texture2DRD` for the
Y/CbCr textures. The `cpu_luma_step` CPU path is unaffected throughout.

## Ship shape

One Pro keeps gl_compatibility as the shipping default; Vulkan is a second export preset
(`rendering_method` is a project-level setting) until stage 2 passes color verification and soak
(the historical crash bar: frame #1500+ / 25 s+ stable, per `multiview-investigation.md`).
