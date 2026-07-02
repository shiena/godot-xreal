# Phase 2 frame submission — RE-confirmed plan

Status: **plan**, derived from a clean disassembly pass of `libXREALXRPlugin.so`
(SDK v3.1.0, arm64-v8a) cross-checked against Unity's public XR SDK header
`IUnityXRDisplay.h`. This supersedes the trial-and-error probes recorded in
`docs/reverse-engineering.md` for the display path. Nothing here is device-verified yet;
the "verify on device" section lists what to confirm.

## TL;DR

`libXREALXRPlugin.so`'s display path is a **stock Unity XR SDK display provider**. Every
struct the SDK reads/writes (`UnityXRNextFrameDesc`, `UnityXRFrameSetupHints`,
`UnityXRRenderTextureDesc`, `UnityXRRenderingCapabilities`) is the public Unity ABI, and the
interface the SDK calls back into (`IUnityXRDisplayInterface`) is the public method table. The
demangled symbols carry the type names verbatim (e.g.
`DisplayManager::PopulateNextFrameDesc(void*, UnityXRFrameSetupHints const*, UnityXRNextFrameDesc*)`),
so the offsets are **known, not guessed**.

The reason Godot content never reaches the glasses is that our fake `IUnityXrDisplay`
(`src/unity_plugin.rs`) implements only the first **3** interface slots
(`RegisterLifecycleProvider`, `RegisterProvider`, `RegisterProviderForGraphicsThread`). The SDK
allocates its render textures by calling slot **+0x18 `CreateTexture`**, reads them back through
**+0x20 `QueryTextureDesc`**, and frees them via **+0x28 `DestroyTexture`** — slots our struct
does not have. So `DisplayManager::CreateTexture` calls a garbage pointer past the end of the
static, no engine textures are ever created, `SetSwapChainBuffers` registers nothing, and the
compositor shows black.

**The fix is to speak the real protocol**: implement the full `IUnityXRDisplayInterface`, have
`CreateTexture` allocate a GL texture Godot owns, have `QueryTextureDesc` hand its GL name back,
and each frame render the Godot scene into the texture named by
`UnityXRNextFrameDesc.renderPasses[k].textureId`. This deletes the entire
`NRRendering*`/`CreateFrame`/`0xdb410` line of attack — those were poking the compositor directly
instead of going through the provider protocol the SDK already drives.

## Confirmed ABI

### `IUnityXRDisplayInterface` method table (GUID `0x940E64D2E52243EC,0xA348F3026B1B1193`)

This is the interface our `get_interface` returns for the display GUID. Slot → method confirmed
by both the public header and the disassembly of the `DisplayManager` wrappers that call each slot:

| Slot | Method | Confirmed by |
|---:|---|---|
| +0x00 | `RegisterLifecycleProvider(pluginName, id, UnityLifecycleProvider*)` | `LoadDisplay` |
| +0x08 | `RegisterProvider(handle, UnityXRDisplayProvider*)` | `Initialize` calls `[[DM+0x8]+0x08]` |
| +0x10 | `RegisterProviderForGraphicsThread(handle, UnityXRDisplayGraphicsThreadProvider*)` | `Initialize` calls `[[DM+0x8]+0x10]` |
| +0x18 | `CreateTexture(handle, const UnityXRRenderTextureDesc*, UnityXRRenderTextureId* out)` | `DisplayManager::CreateTexture` @0x69530 calls `[[DM+0x8]+0x18]` |
| +0x20 | `QueryTextureDesc(handle, UnityXRRenderTextureId, UnityXRRenderTextureDesc* out)` | `DisplayManager::QueryTextureDesc` @0x695e8 calls `[[DM+0x8]+0x20]` |
| +0x28 | `DestroyTexture(handle, UnityXRRenderTextureId)` | `DisplayManager::DestroyTexture` @0x69638 calls `[[DM+0x8]+0x28]` |
| +0x30 | `GetPlatformData(handle, void** out)` | header |
| +0x38 | `CreateOcclusionMesh(...)` | header |
| +0x40 | `DestroyOcclusionMesh(...)` | header |
| +0x48 | `SetOcclusionMesh(...)` | header |

`DM+0x8` is the stored `IUnityXRDisplayInterface*` (our fake); `DM+0x38` is the
`UnitySubsystemHandle` passed as the first arg to every callback (set in `Initialize` from the
handle the SDK hands `LoadDisplay`). We can treat the handle as an opaque token and ignore it.

### `UnityXRRenderTextureDesc` (the CreateTexture/QueryTextureDesc payload)

Reconstructed from `DisplayManager::CreateTexture` (builds it on the stack) and
`DisplayManager::QueryTextureDesc` (reads it back). Size 0x30.

| Offset | Field | Notes |
|---:|---|---|
| +0x00 | `colorFormat` (u32) | passed 0 by the SDK on this device |
| +0x08 | `color` (`UnityXRTextureData`, 8 bytes) | **the native GL texture name** — CreateTexture receives it as the `void*` 4th arg (NULL on GLES), QueryTextureDesc returns it here |
| +0x10 | `depthFormat` (u32) | 0 |
| +0x18 | `depth` (8 bytes) | 0 |
| +0x20 | `width` (u32) | 1968 on One Pro |
| +0x24 | `height` (u32) | 1134 on One Pro |
| +0x28 | `textureArrayLength` (u32) | |
| +0x2c | `flags` (u32) | 2, or 18 when `[DM+0x40][0]==1` (sRGB/color-space bit) |

`QueryTextureDesc` reads exactly `[out+0x08]` (color handle), `[out+0x20]`, `[out+0x24]`
(w/h) and `[out+0x2c]` (flags), so those four fields are the ones our implementation must fill.

### `UnityXRNextFrameDesc` (what `PopulateNextFrameDesc` writes)

Per-render-pass stride is **0xfc bytes**; `renderPassesCount` at +0x580, `mirrorBlitMode` at
+0x584. Confirmed from the stores in `PopulateNextFrameDesc` @0x68c7c:

| Offset | Field |
|---:|---|
| +0x000 | `renderPasses[0].textureId` (u32) = `DisplayManager+0x1b4` |
| +0x004 | `renderPasses[0].cullingPassIndex` |
| +0x008 … | `renderPasses[0].renderParams[0]` (pose quat+pos, then projection/FOV) |
| +0x0f8 | `renderPasses[0].renderParamsCount` (1 mono, 2 for the stereo second param) |
| +0x0fc | `renderPasses[1].textureId` (u32) = `DisplayManager+0x1b8` |
| +0x1f4 | `renderPasses[1].renderParamsCount` |
| ~+0x1f8 … +0x580 | `cullingPasses[...]` (deviceAnchorToCullingPose, projection, separation) |
| +0x580 | `renderPassesCount` (1 = single-pass/mono, 2 = multi-pass stereo) |
| +0x584 | `mirrorBlitMode` |

The `textureId` values come from `NativeRendering::AcquireFrame()` (@0xa1ea0), whose result is
stored in `DisplayManager+0x120` and then the per-eye buffer id is pulled through the overlay
vtable and written to `DM+0x1b4`/`DM+0x1b8`. **These ids are exactly the ids our `CreateTexture`
returned** — the SDK is telling the engine "render eye k into the texture you called id N".

> Note: the old `desc = lib_base+0xdb400` probe (the "0xa6 gate byte", "frame handle at
> desc+0x18") was passing the SDK's function-local static as if it were a `UnityXRNextFrameDesc`.
> Those bytes are just `renderPasses[0]` fields being written. The `0xdb410` gate belongs to the
> unrelated internal `CreateFrame`/`SubmitCurrentFrame` compositor path and is a dead end for us.

### `UnityXRFrameSetupHints` (input to PopulateNextFrameDesc)

Read-only for us. `PopulateNextFrameDesc` reads `appSetup.renderViewport`/`zNear`/`zFar` at
+0x04..+0x24, `singlePassRendering` at +0x00, `use16BitColorBuffers`/`sRGB` at +0x1c/+0x1d,
`textureResolutionScale` at +0x28, and `changedFlags` at +0x50 (bit 6 → `SetFocusPlane`). We can
pass a zero-filled hints struct; the SDK falls back to its own eye data via `InputManager`.

### `UnityXRRenderingCapabilities` (output of GfxThreadStart)

3 bools: `noSinglePassRenderingSupport`, `invalidateRenderStateAfterEachCallback`,
`skipPresentToMainScreen`. `GfxThreadStart` writes only bytes 0 and 2. A zeroed 64-byte buffer
(as today) is safe.

## Confirmed call graph (GLES3 path)

`[DM+0x10]` is the graphics API tag; `!= 0x15` selects the GLES path (0x15 is the Vulkan branch).
On this project (Compatibility/GLES3) we are always on the GLES path.

```
InitUserDefinedSettings
  └ DisplayManager::LoadDisplay(IUnityInterfaces*, settings)
       └ RegisterLifecycleProvider(display)           slot +0x00   (our fake)
  … CreateSession …
provider.initialize  → DisplayManager::Initialize(handle)
       ├ store handle at DM+0x38
       ├ RegisterProviderForGraphicsThread(...)        slot +0x10   → GFX_THREAD_PROVIDER
       ├ RegisterProvider(display-state, ...)          slot +0x08
       └ construct NativeDisplay / NativeRendering / NativeMetrics

── on Godot render thread (EGL live) ──────────────────────────────
provider gfx.start → DisplayManager::GfxThreadStart(caps)   @0x67d14
       ├ NativeDisplay::StartOrResume / NativeRendering::StartOrResume / NativeMetrics
       ├ GetFrameBufferMode() → 2 (stereo)
       └ if overlay-vector empty: DisplayManager::CreateDisplayLayer()   @0x6dc18
            └ (patched cbz→nop) DisplayOverlay → OverlayBase::CreateBuffer()   @0xa8078
                 ├ GetRecommandBufferCount() → 7
                 └ ×7: DisplayManager::CreateTexture(1968,1134,fmt, color=NULL)   @0x69530
                        └ IUnityXRDisplay::CreateTexture[+0x18]   ← OUR CODE allocates GL tex, returns id
                    stores {ptr=null, id} in overlay buffer vector

── each frame, SDK gfx thread calls ──────────────────────────────
gfx.populate_next_frame_desc → DisplayManager::PopulateNextFrameDesc(_, hints, desc)  @0x68c7c
       ├ for each overlay with buffers-not-set: OverlayBase::SetSwapChainBuffers()   @0xa7d0c
       │     └ ×7: DisplayManager::QueryTextureDesc(id)   @0x695e8
       │            └ IUnityXRDisplay::QueryTextureDesc[+0x20]  ← OUR CODE returns GL name at desc+0x08
       │        collect GL names → NativeRendering::SetSwapChainBuffers(nr_swapchain, [names])  @0xa1938
       ├ AcquireFrame() → DM+0x120, per-eye buffer id → DM+0x1b4 / DM+0x1b8
       └ write renderPasses[k].textureId = that id, renderPassesCount = 2

gfx.submit_current_frame → DisplayManager::SubmitCurrentFrame()  @0x685fc   (SDK-driven)
       composites the GL textures to the glasses
```

The engine's only jobs in this graph are: **(1) allocate/track GL textures in
CreateTexture/QueryTextureDesc/DestroyTexture, and (2) between populate and submit, render the
Godot scene into the texture named by each render pass.**

## Root cause recap

1. **Truncated interface.** `IUnityXrDisplay` in `src/unity_plugin.rs` has 3 fn pointers; the SDK
   calls up to slot +0x28. `CreateTexture`/`QueryTextureDesc`/`DestroyTexture` dispatch into
   memory past the static → no engine textures exist → `SetSwapChainBuffers` registers nothing.
2. **Wrong mental model.** `src/native.rs` tries to own the compositor directly
   (`NRRenderingCreate`/`NRSwapchainCreate`/`NRFrameCreate` via `libnr_loader.so`, plus the
   `0xdb400`/`0xdb410` gate pokes). That path is blocked (the frame-wrapper global is never
   initialized) and is unnecessary: the SDK already drives the whole compositor once the provider
   protocol is satisfied.

## Implementation plan (`src/`)

### 1. `src/unity_plugin.rs` — complete the display interface

- Extend `IUnityXrDisplay` from 3 members to the full table through +0x48 (add `create_texture`,
  `query_texture_desc`, `destroy_texture`, `get_platform_data`, and 3 occlusion-mesh stubs). Order
  must match the table above exactly.
- Add the two Unity structs as `#[repr(C)]`:
  - `UnityXrRenderTextureDesc` (0x30, fields per the table; only `color`, `width`, `height`,
    `flags` matter).
  - `UnityXrNextFrameDesc` we do **not** need as a full struct — read `renderPasses[k].textureId`
    at byte offsets `0x00` and `0xfc`, and `renderPassesCount` at `0x580`, from the `desc` pointer
    the SDK passes to our populate driver.
- Implement the callbacks:
  - `create_texture(handle, desc*, out_id*)`: read `desc.width`/`height`/`flags`; allocate a GL
    texture through the shared `GlApi` (see step 3); assign a small monotonic `u32` id; store
    `id → (GLuint, w, h)` in a `Mutex<HashMap<u32, TexEntry>>`; write id to `*out_id`; return 0.
  - `query_texture_desc(handle, id, out*)`: look up the entry; zero `*out`; write GL name to
    `out+0x08`, width to `+0x20`, height to `+0x24`, flags to `+0x2c`; return 0.
  - `destroy_texture(handle, id)`: drop the entry (defer `glDeleteTextures` to the render thread);
    return 0.
  - `get_platform_data` / occlusion stubs: return 0, write nothing.
- The texture map must be reachable from the frame tick (step 2), so keep it in a module static
  (`static XR_TEXTURES: Mutex<...>`) alongside the existing provider statics.

### 2. `src/unity_plugin.rs` — per-frame render into the acquired textures

Replace the current `run_frame_tick` (which drives `PopulateNextFrameDesc` into a throwaway
buffer and also calls the dead `submit_nr_frame`) with:

- Allocate one properly sized `desc` buffer once (≥0x588 bytes, zeroed) reused each frame; it must
  stay a **plain engine buffer, never `lib_base+0xdb400`**.
- Call `populate_next_frame_desc(ctx, user_data, zero_hints, desc)`.
- Read `renderPassesCount` (`desc+0x580`), then for each pass read `textureId`
  (`desc+0x00`, `desc+0xfc`). Map id → GLuint via `XR_TEXTURES`.
- Blit/draw the Godot per-eye image into each GL texture (step 4).
- Do **not** call `SubmitCurrentFrame` from Rust — the SDK's own gfx thread calls it. Our populate
  call is what makes `SetSwapChainBuffers` run and what advances `AcquireFrame`.
- This must run on Godot's render thread (EGL context live) — it already does, via
  `run_render_thread_tick` → `RenderingServer::call_on_render_thread` in `node.rs`.

### 3. Move GL helpers out of `native.rs` into a shared module

`native.rs` already has `GlApi` (`create_rgba_textures`, `create_egl_image`, `delete_textures`,
`take_gl_error`). Lift the GL texture allocation into something `unity_plugin.rs` can call (a
`crate::gl` module, or expose the existing `GlApi`). CreateTexture needs `glGenTextures` +
`glTexStorage2D`/`glTexImage2D` at the requested size with an RGBA8/sRGB internal format chosen
from the `flags` sRGB bit.

### 4. Godot-side per-eye rendering

This is the one genuinely new rendering-integration piece. Options, cheapest first:

- **(a) Blit the main viewport** into both eye textures each frame (mono content on both eyes).
  Proves the pipeline end-to-end with no camera math: `glBindFramebuffer` a temporary FBO with the
  eye texture as color attachment, then blit Godot's framebuffer / a `SubViewport` color texture
  into it (`glBlitFramebuffer`). Good first milestone — "Godot image on the glasses at all".
- **(b) Two `SubViewport`s** (one per eye) with cameras offset by IPD and the projection from the
  render pass's `renderParams` (FOV lives in the desc at `renderPasses[k].renderParams[0]`). Copy
  each SubViewport's color texture into the matching eye texture. This is the real stereo path and
  the bridge toward Phase 4's `XRInterfaceExtension`.

Start with (a) to validate CreateTexture→QueryTextureDesc→SetSwapChainBuffers→compositor, then
move to (b).

### 5. Delete / quarantine the dead compositor path

Once (a) shows pixels, remove from the frame hot path: `NrRenderingApi::submit_nr_frame`,
`start_nr_rendering`, the `NRSwapchain*`/`NRBufferViewport*`/`NRFrame*` probes, and the
`display_manager_submit_frame_probe` / `0xdb400` machinery in `native.rs`. Keep the symbol
resolution scaffolding only if useful for diagnostics. The `signal_guard.rs` patches stay:
- `patch_handle_action_callback` — still needed (null `NativeGlasses` crash).
- `patch_create_display_layer` (`cbz→nop`) — still needed; it is what forces the real
  `DisplayOverlay` → `CreateBuffer` → our `CreateTexture`.

## Verify on device (in order)

1. **Interface reached.** Log inside `create_texture`; expect 7 calls with `1968x1134` right after
   `GfxThreadStart`, matching the Unity reference log `DisplayManager CreateTexture: 1968 1134 …`.
2. **Buffers registered.** Log inside `query_texture_desc` (7 calls) and confirm the SDK logs
   `NativeRendering SetSwapChainBuffers` / `Start rendering success` (it currently never does).
3. **Frame ids flow.** In the frame tick, log `renderPassesCount` and the two `textureId`s; expect
   count 2 and ids matching those returned by `create_texture`.
4. **Pixels.** With option (a), a solid/clear color or the Godot UI should appear on the glasses.
   External capture: `dumpsys SurfaceFlinger` layer content (screencap of the external HDMI
   display returns 0 bytes; use the SurfaceView layer dump instead).

## Risks / open questions

- **Shared GL context.** The SDK's gfx/GLThread must be in the same EGL share group as the context
  our `CreateTexture` allocates in, or the compositor can't sample our texture names. Unity relies
  on exactly this; needs device confirmation. If they are not shared, fall back to allocating the
  textures as `AHardwareBuffer`-backed + `EGLImage` (the machinery already exists in `native.rs`)
  and return the GL name bound to the AHB.
- **Texture format.** `flags` 2 vs 18 encodes an sRGB/color-space choice. Pick the internal format
  from that bit; wrong choice shows a gamma-shifted or rejected buffer.
- **`renderParams` FOV/pose for stereo (option b).** The per-eye projection is in the desc but its
  exact field offsets within `renderParams` are not fully mapped yet — only needed for (b), not (a).
- **SDK version pinning.** All offsets are v3.1.0 arm64-v8a. Pin the SDK; re-dump on upgrade.

## Reproduce the disassembly

```bash
XR=jniLibs/arm64-v8a/libXREALXRPlugin.so
llvm-nm --defined-only "$XR" | llvm-cxxfilt | grep -E 'DisplayManager::|OverlayBase::'
llvm-objdump -d --start-address=0x69530 --stop-address=0x69684 "$XR"   # CreateTexture/QueryTextureDesc/DestroyTexture
llvm-objdump -d --start-address=0x68c7c --stop-address=0x69358 "$XR"   # PopulateNextFrameDesc
llvm-objdump -d --start-address=0xa8078 --stop-address=0xa8358 "$XR"   # OverlayBase::CreateBuffer
llvm-objdump -d --start-address=0xa7d0c --stop-address=0xa7ec4 "$XR"   # OverlayBase::SetSwapChainBuffers
```

Unity XR ABI reference: `IUnityXRDisplay.h`
(e.g. <https://github.com/StephenHodgson/Native-Unity-API/blob/main/IUnityXRDisplay.h>).
