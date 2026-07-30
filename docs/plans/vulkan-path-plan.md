# Vulkan rendering path — staged plan for a re-attempt

Status: **stage 0 = GO, stage 1 (phone screen) = done, adopted follow-ups implemented; next =
stage 2 design** (stage 1 device-verified 2026-07-30 on Beam Pro). Stages: phone screen -> glasses
rendering -> camera rendering -> FPV stream, one commit each, each design cross-checked against a
second opinion.

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

## Next steps (resume here)

1. **Stage 1 leftovers: DONE (2026-07-30)**: the `camera_feed` renderer-first gate, the
   `run_render_thread_tick()` self-defence gate (one-shot warning), and
   `XrealSystem.is_render_texture_encoder_supported()` with the refusal in `xreal_stream.gd` /
   `xreal_video_recorder.gd` and the Stream / Record grey-out in
   `demo/main.gd::_apply_capabilities()`. Code-verified (clippy, tests, gdlint); the on-device
   spot-check rides along with the stage-2 soak.
2. **Stage 2 - glasses rendering.** Consult a second opinion first, then build: a private EGL
   context on the render thread (Godot no longer provides one), RGBA8 AHardwareBuffers per eye
   (stage 0 proved the share), the AHB-backed `VkImage` reached through
   `RenderingDevice.texture_create_from_extension` + `RenderingServer.texture_rd_create` (both
   confirmed present in the 0.5.3 bindings, alongside `RenderingDevice.get_driver_resource` for
   pulling the eye SubViewport's `VkImage`), or a simpler first cut of one `RenderingDevice`
   `texture_copy` per eye. Keep the fake-`IUnityXRDisplay` GL submission unchanged. **Re-run the
   RGBA8-vs-sRGB color A/B on device** (the old measurement was against gl_compatibility output)
   and the crash bar of frame #1500+ / 25 s+.
3. **Stage 3 - camera rendering.** Confirm on device that the `Image` fallback works under Vulkan
   (expected, it is renderer-agnostic), then recover the ~525 us class with
   `RenderingDevice.texture_update` + `Texture2DRD`.
4. **Stage 4 - FPV stream.** Reuse the stage-2 private EGL context + AHB share to give the encoder
   a real GL name, then lift the `video_encoder::start()` gate. Verify with the PC receivers in
   `scripts/stream_server/`.

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
