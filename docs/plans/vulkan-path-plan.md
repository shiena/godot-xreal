# Vulkan rendering path — staged plan for a re-attempt

Status: **stage 0 = GO, stage 1a = first data point OK** (device-verified 2026-07-30 on Beam Pro).

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
- **Stage 1a first data point**: the demo boots and renders correctly under Vulkan Mobile on the
  Beam Pro (Adreno 710, `renderingDevice: vulkan`, no crash at idle; soak + workload still to do).

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
