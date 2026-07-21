# Multiview / head-lock investigation — handoff for the next session

Status (updated 2026-07-21): **head-lock is SOLVED** (2026-07-13; see
[`codex-headlock-analysis.md`](codex-headlock-analysis.md)). **Multiview WORKS opt-in**
(`setprop debug.xreal.stereo_mode 2`): the black/gray right eye was solved 2026-07-17 — the causes
were Adreno GLES layer-copy quirks, handled in `src/gl.rs`. Since 2026-07-21 the eye textures
(Multipass 2D and the Multiview array alike) are allocated `GL_RGB10_A2` — Godot's
`gl_compatibility` SubViewport format — and each eye is filled by a single direct
`glCopyImageSubData`, so Multiview's copy cost equals Multipass's. It still gives our
two-SubViewport rig no single-pass rendering benefit, so **Multipass remains the default**.
**Everything below is kept verbatim as a historical record and contains now-superseded conclusions
(esp. "right eye = unfixable gray") — trust this status and the "Final status 2026-07-16" section
at the bottom over the older text.**

## The goal (user-confirmed)

A **peek window**: the glasses render fills the FOV and is **head-locked** (stays in front as you turn),
and the **in-game camera rotation matches the head** so you look around **world-locked** 3D. Recenter
resets "forward". No frame/panel — the whole see-through view is the window (interpretation "A").

## Root cause of the current (Multipass) behaviour — precisely diagnosed on device

The XREAL **compositor world-anchors our submitted layer to the session-start origin**. Established by a
clean sequence of on-glasses experiments:

- **Overshoot.** With the eye cameras tracking the head (1×), the content rotates ≈ **2× the head** →
  the compositor is *also* applying a full head rotation. So: `content = camera(1×) + compositor(1×)`.
- **Camera 0× (identity eye-camera rotation).** Overshoot disappears and the content is **world-stable**,
  but the **whole render stays anchored at the startup-forward direction and drifts** out of view when you
  turn (no look-around). So the compositor world-stabilises the layer relative to the **origin**, and that
  origin is the session-start pose.
- For a correct peek window we need **camera 1× (new content) + compositor 0× (no double)**. The compositor's
  "0×" requires its reprojection **render-pose reference to be live**, not frozen at session start.

## Dead ends — ruled out on device, do NOT retry

| Attempt | Result |
|---|---|
| Drive eye cameras from the **display InputManager pose** (`GetHeadPoseAtTime` @0x48cc8, 16-float / 4×4 matrix) instead of session-manager | no visual change |
| Remove eye-camera head rotation (camera 0×) | overshoot gone, but world-anchored/drifts (see above) |
| `SetGlassesSpaceMode` (0..3), follow/lock firmware toggle | no change (dead ends from earlier sessions too) |
| **Real recenter** `NativePerception::Recenter` via input provider (see below) | recenter callback fires, but does **not** move the glasses render to forward |
| **Per-frame HMD update** `UpdateDeviceState(0)` → `UpdateHMDState` → `OnBeforeRender` (updates the `DM+0x100` frame timestamp from `GetHMDTimeNanos`) | fires every frame, no crash, **no visual change** — so the frozen reference is NOT this timestamp |

Conclusion: the world-anchoring lives in the **NR compositor (libnr_loader)**, not in any
DisplayManager/InputManager function reachable from `libXREALXRPlugin.so`. **Matching the reference app's
Multiview submission is the remaining grounded hope.**

## Why Multiview

The reference app (renders a correct head-locked peek window with the **same** native
libs) uses `XREALSettings.asset: StereoRendering: 2` (Multiview / Single-Pass-Instanced) +
`InitialTrackingType: 0` (6DoF). We used **Multipass (0)** as a workaround. The stereo mode is now
**selectable at startup** (commit `2132fad`): GDScript `XrealSystem.set_stereo_rendering_mode(0|2)`, the
ProjectSetting `xreal/stereo_rendering_mode` (demo reads it → the API), or
`adb shell setprop debug.xreal.stereo_mode 2`. Default 0.

## Multiview: what works, and the exact blocker

Implemented (commit `e5fb62d`): `stereo_rendering_mode = 2` makes the SDK's `CreateBuffer` request **one**
texture with `textureArrayLength == 2` (`single_buffer:1`). We now:
- `gl.rs::alloc_texture_array` → a `GL_TEXTURE_2D_ARRAY` (2 layers) via `glTexImage3D`.
- `gl.rs::blit_texture_to_layer` → blit each eye's SubViewport into a layer via `glFramebufferTextureLayer`.
- `unity_plugin.rs`: `CreateTexture` allocates the array when `textureArrayLength>=2` (`XrTexture.layers`);
  `run_frame_tick` blits left→layer 0, right→layer 1.

Device-verified: **no crash**; `CreateTexture … arraylen=2 -> gl_tex`; `blit_to_layer … read_ok=true
draw_ok=true` for **both** layers (the earlier `GL_INVALID_FRAMEBUFFER_OPERATION` is gone for our texture);
`frame_tick … passes=1 tex0=1 filled=1 submit=Some(0)`.

**BUT nothing displays (black).** In the `single_buffer` path the SDK **never registers our texture with
the NR swapchain**: `QueryTextureDesc` / `OverlayBase::SetSwapChainBuffers` are **not called** (our
callbacks log nothing), so the compositor samples its own empty buffer. In `OverlayBase::CreateBuffer`
`GetSwapChainBuffers` returns empty (hence `CreateTexture(color=0)` = we allocate our own), but the
follow-up registration that the Multipass path does (`QueryTextureDesc` → `NativeRendering::SetSwapChainBuffers`)
does not run for the single array texture.

## Next-session plan (Multiview)

Crack the **single-buffer swapchain registration** — get the NR swapchain to sample our
`GL_TEXTURE_2D_ARRAY`:

1. RE **`OverlayBase::CreateBuffer` @0xa8078** fully (single_buffer branch): after
   `NativeRendering::GetSwapChainBuffers` @0xa1b04 returns empty, what path does it take for the array
   texture, and where is the swapchain buffer supposed to come from? (Two `CreateTexture` @0x69530 calls at
   0xa8120 / 0xa8248 — only one path taken.)
2. RE **`OverlayBase::SetSwapChainBuffers` @0xa7d0c** and find **why it isn't called** in single_buffer
   (`PopulateNextFrameDesc` @0x68c7c calls it "for overlays with buffers-not-set" — check the buffers-set
   flag / overlay condition for the single array overlay).
3. Examine **libnr_loader** (`libnr_loader.so`, 4.7 MB, unexamined): `NativeRendering::CreateSwapchainEx`,
   `SetSwapChainBuffers` @0xa1938, `AcquireBuffers` @0xa23e0, `AcquireFrame` @0xa1ea0, `SubmitFrame`
   @0xa3ef0 — how the single array buffer is created and how a GL name is bound. We already have
   `NrSwapchain*`/`NrBufferViewport*` bindings in `native.rs`; consider registering our array texture with
   the NR swapchain **directly** if the SDK won't.
4. Alternative to probe: force the Multipass-style registration to run for the array texture (make
   `SetSwapChainBuffers` fire → `QueryTextureDesc` returns our gl_tex → NR `SetSwapChainBuffers`), or try
   `SetSingleBufferMode(false)` @0x48d44 to see if double-buffer multiview uses the normal registration.
5. Once it displays: re-test head-lock. Multiview submits proper per-eye view matrices, so the compositor's
   reprojection reference may finally be live → verify overshoot gone AND FOV head-locked (peek window).

## UPDATE 2026-07-13 — Multiview now DISPLAYS (registration solved), head-lock still open

The single-buffer registration blocker is **fixed** (commit `4f04008`, see
`codex-multiview-analysis.md`): our `patch_create_display_layer` was forcing the DummyDisplayOverlay in
Multiview; changing it from `nop` to `b 0x6dd18` forces the real DisplayOverlay, so QueryTextureDesc now
runs (0→7) and our `GL_TEXTURE_2D_ARRAY`s register with the NR swapchain. **On-glasses: Multiview now
renders** (no longer black); Multipass unaffected (no regression). Toggle with
`adb shell setprop debug.xreal.stereo_mode 2|0`.

**Remaining: head-lock.** With Multiview rendering, the glasses image is **still world-locked** (drifts /
anchored to the session-start direction) instead of a head-locked peek window — the same compositor
world-anchoring diagnosed earlier (see "Root cause" above: overshoot = camera 1× + compositor 1×; the
compositor's reprojection reference is frozen at session start). The reference app uses the SAME Multiview +
libs and IS head-locked, so the remaining difference is the per-frame pose the real Unity XR provider
feeds the submission that we don't. Next: RE how the NR compositor / `NativeRendering::SubmitFrame`
@0xa3ef0 obtains the layer's head pose (args are `DM+0x100/0x108/0x110` + frame `DM+0x120`), whether the
per-eye `deviceAnchorToEyePose` in `PopulateNextFrameDesc`'s renderParams must carry the head world
orientation, and what per-frame call makes the layer track the head.

## Deep-dive session 2 — the swapchain-registration subsystem, mapped

Goal of this pass: find why, in Multiview (`single_buffer:1`), our array texture is **not** registered
with the NR swapchain (`QueryTextureDesc` never called → compositor samples an empty buffer → black).
Device counts confirm the split: **Multipass = 8 `CreateTexture` + 8 `QueryTextureDesc`** (registered),
**Multiview = 1 `CreateTexture` (arraylen=2) + 0 `QueryTextureDesc`** (not registered). Note: the reference app
also runs `single_buffer:1` yet displays, so single-buffer itself is not the blocker.

Registration flow (all in `libXREALXRPlugin.so`):
- `DisplayManager::CreateDisplayLayer` @0x6dc18 → constructs `DisplayOverlay`/`DummyDisplayOverlay`
  (@0xa6988) into `vector<shared_ptr<DisplayOverlay>>`, then calls **`OverlayBase::Init` @0xa7cc4**.
- **`OverlayBase::Init`** calls three *virtual* methods in order: `vtable[+0x18]`, `vtable[+0x30]`,
  `vtable[+0x40]`. For the real overlay, `+0x18` ≈ `CreateBuffer`, `+0x30` ≈ `SetSwapChainBuffers`. So
  **registration happens at Init time (CreateDisplayLayer), not per-frame** — confirmed on device by
  Multipass logging `CreateTexture` and `QueryTextureDesc` at the *same* timestamp.
- **`OverlayBase::CreateBuffer` @0xa8078**: `[DM+0x10]==0x15` (Vulkan) → `GetSwapChainBuffers` @0xa1b04 →
  `CreateTexture(color=buffer)`. `!=0x15` (**our GLES path**) → count = `vtable[overlay+0x38]` →
  loop `CreateTexture(color=0)` @0x69530, storing each id into the overlay's **buffer vector at
  `overlay+0x20..0x28`**.
- **`OverlayBase::SetSwapChainBuffers` @0xa7d0c**: early-returns if the byte `overlay+0x8` is set. GLES
  path loops the buffer vector `[overlay+0x20..0x28]` → `DisplayManager::QueryTextureDesc` @0x695e8 each
  (this is our registration callback) → `NativeRendering::AcquireBuffers` @0xa23e0; sets `overlay+0x8=1`
  at the end. Vulkan path just sets the flag.
- The `overlay+0x8` "done" flag **starts 0** (the `Overlay(int,int,bool,bool)` ctor @0xa71cc puts its two
  bools at `overlay+0x80/+0x81`, and `+0xc0=1`, `+0x51=0`, `+0xc8=0` — nothing at `+0x8`).
- `PopulateNextFrameDesc` @0x68c7c *also* calls `SetSwapChainBuffers` per overlay (lists at `DM+0x150`
  with sub-overlays `+0x18/+0x28`, and `DM+0x128`) when `overlay+0x8 == 0`.

**Refined open question:** in Multiview, `CreateTexture` fires **once** (the array), but `QueryTextureDesc`
**never** does — even though `overlay+0x8` starts 0. So when `SetSwapChainBuffers` runs (in `Init`, and/or
`PopulateNextFrameDesc`), the overlay's **buffer vector `[overlay+0x20]` is empty** (or the array texture is
owned by a *different* overlay object than the one whose `SetSwapChainBuffers` runs). I.e. the single array
buffer's placement into the overlay's buffer vector differs from the multipass per-eye case.

**Next concrete steps (session 3):**
1. Identify the **overlay type + vtable** actually used for the Multiview single array buffer (DisplayOverlay
   vs ProjectionOverlay vs DummyDisplayOverlay) — dump `[overlay]` (vtable ptr) and compare to the `d5xxx`
   vtables; check `Init`'s `vtable[+0x18]/[+0x30]` resolve to CreateBuffer/SetSwapChainBuffers for it.
2. RE that overlay's `CreateBuffer` store into `overlay+0x20` in the `single_buffer` case (count from
   `vtable[+0x38]` — is it 0 or 1? does the array texture land in the vector?).
3. Runtime confirm: add a **log-trampoline patch** on `SetSwapChainBuffers` @0xa7d0c (reuse the
   `signal_guard` mprotect/nop machinery) to print when it's called and the buffer-vector size — decides
   "vector empty" vs "overlay not iterated".
4. If the SDK won't register it, register our `GL_TEXTURE_2D_ARRAY` with the NR swapchain **directly**
   via the `native.rs` `NrSwapchain*`/`swapchain_set_buffers` bindings (need the NR swapchain handle the
   SDK holds inside `NativeRendering` at `DM+0x58`).
5. `CreateDisplayLayer` is already **patched** (`patch_create_display_layer`, cbz→nop @0x6dc98 forces the
   real `DisplayOverlay`) — re-check that patch is still correct under `single_buffer` (it may force the
   wrong branch / a Dummy vs real overlay mismatch in Multiview).

Relevant new addresses: `CreateDisplayLayer` @0x6dc18, `DisplayOverlay` ctor @0xa6988, `OverlayBase::Init`
@0xa7cc4, `OverlayBase::CreateBuffer` @0xa8078, `OverlayBase::SetSwapChainBuffers` @0xa7d0c, `QueryTextureDesc`
@0x695e8, `GetSwapChainBuffers` @0xa1b04, `AcquireBuffers` @0xa23e0, `Overlay(int,int,bool,bool)` @0xa71cc.

## RE map discovered this session (libXREALXRPlugin.so, SDK 3.1.0, arm64-v8a)

Compute `lib_base = <runtime addr of CreateFrame> - 0x53bd8` (or `desc_ptr - 0xdb400`, both logged).

- **Head pose**: exported `GetHeadPoseAtTime` @0x48cc8 → `InputManager::GetHeadPoseAtTime` @0x7f4a0 →
  `NativePerception::GetHeadPose` @0x96578. Output = **64-byte / 16-float = 4×4 row-major matrix** (rotation
  3×3 upper-left, position last row 12/13/14, homogeneous col 3/7/11=0, [15]=1). Session-manager's
  `XREALGetHeadPoseAtTime` (other lib) = compact 7-float `NrPose` (w-first).
- **Input provider** (device-dumped struct from `RegisterInputProvider`; the struct is **transient** — copy
  the callback code addresses, don't store the pointer):
  - `+0x10` = FillDeviceDefinition
  - `+0x20` = `$_9` → `InputManager::UpdateDeviceState` @0x7a968 (deviceId 0 → `UpdateHMDState`)
  - `+0x28` = `$_10` no-op stub (`mov w0,#1; ret`)
  - `+0x30` = `$_11(void*,void*)` → `NativePerception::Recenter` @0x963cc  ← **real recenter**
  - `+0x38` = `$_12` rumble haptic, `+0x40` = `$_13` → `HandleHapticBuffer`
- `InputManager::UpdateHMDState` @0x7aa3c → `DisplayManager::OnBeforeRender` @0x66fa8 (writes the frame
  timestamp `DM+0x100` from `GetHMDTimeNanos`; also `DM+0x110`) + `NativePerception::GetHeadPose`.
- **Submit**: `DisplayManager::SubmitCurrentFrame` @0x685fc → `SetBufferViewport` @0x681f4 →
  `OverlayBase::SetBufferViewport` @0xa7f74 → `NativeRendering::SetBufferViewport` @0xa2770; then
  `NativeRendering::SubmitFrame` @0xa3ef0 with args `(NativeRendering=DM+0x58, frame=DM+0x120,
  DM+0x100, DM+0x108, DM+0x110)`. SubmitCurrentFrame reads **no** head pose directly.
- `ProjectionOverlay::UpdateViewport` @0xa8c18 → `GetEyePos` only (eye offset, **no head orientation**);
  `CreateViewport` @0xa86c8; `OverlayBase::UpdateViewport` @0xa8374.
- **Swapchain**: `OverlayBase::CreateBuffer` @0xa8078 → `GetSwapChainBuffers` @0xa1b04 + `CreateTexture`
  @0x69530 (×2 branches). `OverlayBase::SetSwapChainBuffers` @0xa7d0c → `QueryTextureDesc` @0x695e8 +
  `NativeRendering::AcquireBuffers` @0xa23e0. `NativeRendering::AcquireFrame` @0xa1ea0 (no pose).
- Exported utility: `RecenterGlasses` @0x4da48 (no-op on our pose), `GetDevicePoseFromHead` @0x54f48,
  `SetSingleBufferMode` @0x48d44, `CreateDisplayLayer` @0x48d24, `SetSkipPresentToMainScreen` @0x48d08.
- Reproduce: `llvm-objdump -d --start-address=… --stop-address=… jniLibs/arm64-v8a/libXREALXRPlugin.so`,
  `llvm-nm --defined-only … | llvm-cxxfilt` (NDK 27.2.12479018 llvm bins).

## What shipped this session (committed on `phase2-glasses-display`)

- `8647846` real recenter wired (`NativePerception::Recenter` via input provider +0x30) + input provider RE.
- `f0b59e8` per-frame HMD update (`UpdateDeviceState`→`OnBeforeRender`) — no visual effect, kept (harmless).
- `e5fb62d` Multiview array-texture rendering (our side works; compositor doesn't sample — see blocker).
- `2132fad` stereo mode selectable at startup (API / ProjectSetting / system property).
- Earlier: `ea616a2` `d916639` `db9b778` (head-pose-pipeline RE, the reference app teardown).

## Tooling (on-device iteration)

- Build: Godot **4.7-stable** console exe only; `cargo ndk -t arm64-v8a -o ./jniLibs build --release`
  (`ANDROID_NDK_HOME=…\ndk\27.2.12479018`); export hangs after writing the APK — poll for a **fresh** APK
  (stable size + EOCD `50 4B 05 06` + mtime < ~200 s) then kill Godot.
- adb: **scrcpy's v37** (`…\scoop\apps\scrcpy\4.0\adb.exe`) only; Wi-Fi `192.168.0.4:5555`. App
  `com.example.godotxreal/com.godot.game.GodotAppLauncher`.
- The glasses layer is `FLAG_SECURE` (no screencap); diagnose via logcat `[xreal] …` + the reference app.
- Multiview A/B: `adb shell setprop debug.xreal.stereo_mode 2` (or `0`) then relaunch.
- The `0x3f800000` GLThread crash rule (still applies): never query **two** head-pose pipelines
  (session-manager + XR-plugin) in the **same frame on the same thread** — see `input-feature-glthread-crash`.
- Reference app Unity source (local, not in this repo): its XR assembly,
  `Assets/XR/Settings/XREALSettings.asset`, `HeadLockFollower.cs` = app-level HUD, not the lever.

## Re-investigation 2026-07-15 — codex + cross-check, NR blocker re-confirmed (verdict UNCHANGED)

Re-ran the Multiview right-eye question with a fresh disassembly pass on `libnr_api.so` /
`libnr_loader.so` (NDK 27.2 llvm), plus an independent cross-check. The prior verdict holds, with two
refinements: the exact reason `NRBufferSpecSetMultiviewLayers(2)` can't reach a client name, and a
**correction** to the previously suggested patch.

### Re-confirmed: no lever reaches a client GL name as an array (patch-free = NO)

- `NRSwapchainSetBuffers` backend: loader `+0x1f9fa8` → api `+0xc1b2dc` → `+0xd1aad4` → `+0xc7cb10` →
  `backend+0x38`. The swapchain object base ctor `+0xebf1b4` zero-fills its state at
  `+0xebf1f8..+0xebf208`, which includes **`object+0x30 = 0`**; no derived ctor ties it to kind 1.
- The virtual that receives a client name (vtable reloc `0x22c69c8 → +0xebea88`) takes
  `w1=index, x2=per-buffer ctx, x3=GL name, w4=bool`; it only stashes index/bool at
  `+0xebeac4/+0xebeac8` and **never reads the BufferSpec layers / array length**.
- Client-name validation is `GL_TEXTURE_2D` **hard-forced**: after the `object+0x30` gate at
  `+0xebeb6c`, the binds/queries at `+0xebeb74 / +0xebeb84 / +0xebeb88 / +0xebeb9c / +0xebebb0` are all
  `GL_TEXTURE_2D` (0xde1). No kind-field write, no kind==1 branch in this block.
  *(Independent cross-check confirmed the `+0xebeb6c` → `+0xebeb84` 2D gate directly.)*
- The array capability is **owned-textures only**: NR-owned path `+0xec13ec` stores kind from arg `w5`
  into `object+0x28` at `+0xec15d4` and branches `kind==1` at `+0xec15f0/+0xec1608` into the
  `glTexImage3D` array allocation. The kind consumed by the sampler bind `+0xec4d2c` comes from its own
  caller arg `w3` (`+0xec4d54`), **not** derived from the client validator `+0xebea88`.
  `NRBufferViewportSetMultiviewLayer` only selects the sampling layer index; it does not promote a
  client image into the array slot `object+0x28`.

⇒ There is no BufferSpec field, swapchain flag, viewport field, or existing NR public call that makes a
single client-submitted GL name be sampled as array layer 1. `NRBufferSpecSetMultiviewLayers(2)`
affects only the owned-allocation path.

### Correction to the earlier patch idea

`codex-righteye-analysis.md` §6 option 3 suggested redirecting the bind at `+0xebeb84` from
`GL_TEXTURE_2D` to an array target. That is **insufficient**: the follow-up texture-level queries stay
2D and the downstream kind/slot stay 0, so a single-constant redirect doesn't make the compositor
sample the array. The smallest viable lever is a **branch hook at the `+0xebea88` function entry** whose
replacement (a) validates the client name as `GL_TEXTURE_2D_ARRAY`, (b) queries depth/layers, (c)
registers the per-image representation as kind 1 / array slot `object+0x28`, then (d) returns to the
original flow. An inline-only fix would need at minimum the four target constants at `+0xebeb74`,
`+0xebeb88`, `+0xebeb9c`, `+0xebebb0` changed **plus** the downstream kind/slot — so "just `+0xebeb84`"
does not work.

### Still not worth doing

Even a correct `libnr_api` backend patch only buys a texture hand-off change: our rig renders two Godot
SubViewports (both eye cameras) every frame in **both** modes (`node.rs::build_stereo_rig` /
`update_stereo`, verified 2026-07-15), so single-pass-instanced yields **zero** GPU/CPU saving here.
Patching a stripped, obfuscated backend's texture-import/vtable path for no benefit is not justified.

**Verdict: unchanged — Multiview shelved, Multipass is the correct stereo path.** This pass adds an
independent re-confirmation of the NR blocker and a corrected minimal-patch location, should anyone ever
revisit it on a future SDK.

### On-device check 2026-07-15 — the reference app really runs Multiview (no multi-pass fallback)

Unity's docs note that Single-Pass Instanced falls back to multi-pass on devices without the
Multiview GL extension. Tested whether the reference app is therefore secretly running multi-pass on
this hardware — it is not:

- **GPU supports Multiview.** The XREAL host GPU is **Adreno 710 / OpenGL ES 3.2**, and its GLES
  extension string (`dumpsys SurfaceFlinger`) includes `GL_OVR_multiview`, `GL_OVR_multiview2`, and
  `GL_OVR_multiview_multisampled_render_to_texture`. So Unity's "no Multiview → fall back" rule does
  not apply here.
- **Runtime logcat of the reference app** confirms it runs stereo mode 2 (Multiview), on GLES3, with
  no fallback: `[XR][SessionManager] InitUserDefinedSettings: stereoRendering=2`,
  `[XR][DisplayManager] Run in stereo mode.`, `[NRSDK] Get Display Stereo Mode: 2`, and the Unity
  graphics device event `deviceType=11` (OpenGLES30; the earlier `deviceType=4` is the Null
  placeholder). No fallback / multi-pass line appears (the only `UnSupported` is
  `NativePerception+Recenter`, an unrelated feature — and notably the reference app hits that too).

⇒ The reference app genuinely uses Multiview on the same hardware and libs, and it works because its
array swapchain is engine/NR-owned (kind=1 path), not a client-submitted GL name imported as
`GL_TEXTURE_2D` (our wall). This refutes the fallback hypothesis and confirms the engine-owned-array
explanation. Verdict unchanged: our port cannot reach that path without patching `libnr_api` or
rebuilding Unity's engine-owned XR array swapchain, and it buys nothing on our two-SubViewport rig.

### On-device screencap 2026-07-16 — reference app's right eye is confirmed working

Captured the glasses physical display (`4626964009369245188`, 3840×1080, FLAG_SECURE but screencap
went through) while the reference app ran in stereo mode 2. RGB-only stats (must `-alpha remove` first —
`fx:mean` otherwise averages in the alpha channel and turns black pixels into 0.25): left eye
stddev=0.0594, right eye stddev=0.0601 (near-identical content density), left-vs-right RMSE=0.065
(parallax). Our broken right eye was a uniform gray (stddev≈0); the reference app's is not — its right
eye renders real content. So Multiview genuinely works on this exact hardware + these exact libs; it is
not a hardware or library limitation.

**Open tension this surfaces.** Both the reference app and our port run GLES3 (Unity graphics
`deviceType=11`). `OverlayBase::CreateBuffer @0xa8078` branches on `[DM+0x10]==0x15` (Vulkan → NR-owned
`GetSwapChainBuffers`) vs else (GLES → `CreateTexture(color=0)`, engine-allocated client name). On GLES
**both** should take the `color=0` client-name path — so the "engine-owned vs client-submitted"
explanation (which was implicitly the Vulkan branch) does not hold on GLES. The real difference must be
elsewhere: how Unity creates/parameterizes its GL array texture (immutable `glTexStorage3D`, internal
format, levels), or a condition in libnr_api's client-import path the earlier RE didn't cover. Any
"make GetSwapChainBuffers non-empty" experiment must be re-grounded against this before coding.

## Final status 2026-07-16 — Multiview shelved, port is Multipass-only

This closes the Multiview line of work. The code is now **Multipass-only**: `stereo_rendering_mode()`
in `src/session.rs` always returns 0 and the `debug.xreal.force_multiview` escape hatch was removed.
The RE'd Multiview machinery (array-texture allocation in `gl.rs`, the two-viewport `signal_guard`
patch at `0x6dc60`, the Multiview descriptor branch in `unity_plugin.rs`) is left in place but dormant
(unreachable), as a record. Consolidated knowledge:

### 1. The right eye renders CORRECTLY — the "gray right eye" was a measurement artifact

The single most important correction: for months the right eye was believed to be a fixed "unfixable
gray" caused by the NR compositor. **That was wrong.** On-device screencaps (both the reference Unity
app and our own build in forced Multiview) show the right eye rendering real content:

- Reference app: left/right luma stddev 0.0594 / 0.0601 (matched), L-vs-R RMSE 0.065 (parallax).
- Our app (forced Multiview): right-eye stddev 0.169 = left; **our Multiview right eye vs our Multipass
  right eye RMSE = 0.038** (i.e. the same correct content), and MV L-vs-R separation == MP L-vs-R.

The earlier "gray, mean 0.2503" came from measuring `magick … -format %[fx:mean]` **without
`-alpha remove`** — on a mostly-black AR frame the alpha channel averages black pixels to ~0.25, which
looked like a uniform gray. Always `-background black -alpha remove -alpha off` before luma stats.

So the landed SDK-side fixes (descriptor read of `renderParams[1]` at desc+0x80; the two-viewport patch
`overlay+0x14=2`; QueryTextureDesc echoing `layers=2`) were **sufficient** — Multiview draws both eyes.
This matches the later `libnr_api` RE: the sampler's 2D-vs-array "kind" is **not** queried from the
texture; it's derived from the swapchain BufferSpec's multiview-layers, which our `overlay+0x14=2` patch
sets via `NRBufferSpecSetMultiviewLayers(2)`. `glTexStorage3D` vs `glTexImage3D` is irrelevant. The
earlier "client GL_TEXTURE_2D_ARRAY is unimportable / unfixable" verdict was **incomplete**.

### 2. The reference Unity app really runs Multiview on this exact hardware

Adreno 710 / GLES 3.2 exposes `GL_OVR_multiview(2)`, so Unity's SPI-fallback rule doesn't apply.
Runtime logcat of the reference app: `stereoRendering=2`, `[DisplayManager] Run in stereo mode`,
`[NRSDK] Get Display Stereo Mode: 2`, GfxDevice `deviceType=11` (GLES3), no fallback. Both it and our
port take the GLES `CreateTexture(color=0)` engine-allocated-array path (confirmed by disassembly of
`OverlayBase::CreateBuffer`) — so there is no "engine-owned vs client" difference on GLES.

### 3. Why Multiview is still not worth enabling

- **No benefit.** Our rig renders two Godot SubViewports (one per eye) every frame in *both* modes,
  then blits into either two 2D textures (Multipass) or the two layers of one array (Multiview). The
  single-pass-instanced win only exists if the *engine* draws both eyes in one instanced pass; Godot's
  SubViewport rig does not. So Multiview = zero GPU/CPU saving here, just a different texture hand-off.
- **It shares a crash.** Forced Multiview crashes a second in — but so does Multipass (see §4). The
  crash is not Multiview-specific, so Multiview offers no stability either.

### 4. Cross-thread `UpdateMetrics` SIGBUS — affects BOTH modes (open follow-up)

`DisplayManager::SubmitCurrentFrame → UpdateMetrics @0x68974` dispatches a **null metrics callback**
(`blr [[DM+0x68]+0x18]` at `+0x58`) → **SIGBUS (BUS_ADRALN, fault 0x13)** on the GLThread. A `fcmp
d0,#1.0; b.le` throttle at `+0x48` gates it to ~1 Hz, so it strikes ~frame 4 (≈1 s in), not on frame 0.
`patch_update_metrics` rewrites the entry to `ret`, **logs success, yet doesn't prevent the crash**:
the one-shot runs on the **main thread** (its `ic ivau`/`isb` sync that thread), but `UpdateMetrics`
executes on the **GLThread** whose instruction fetch still sees the stale prologue (a data read there
sees the `ret`; instruction fetch uses the I-cache). It's a **cross-thread self-modifying-code
coherency race** — it happened to win in earlier sessions, and lost deterministically here (committed
Multipass build crashes ~frame 4 on a patient single launch). The `3a25135` "remove stereo selector"
commit is **not** the cause (git-confirmed; it only touched selectors).

Attempted fix (re-assert the patch on the GLThread each frame, `ensure_update_metrics_patched`) could
not be validated: with `lto=true` the release build **dead-code-eliminated** the new function (its log
strings never reached the .so even though it was called from the live `run_frame_tick`); with
`lto=false` the .so grew but a *different* SIGSEGV appeared earlier in `call_input_update_hmd →
InputManager::UpdateHMDState` (possibly a launch race) before the fix ran. Reverted.

**Recommended follow-up (separate from Multiview):** fix the `UpdateMetrics` crash by re-applying the
patch **on the render/GL thread** — but inline the logic directly in `run_render_thread_tick` /
`run_frame_tick` (which reliably compiles) rather than in a new function LTO can prune, or call the
already-live `patch_update_metrics` from there. Then launch cleanly to rule out the `UpdateHMDState`
race. This is a Multipass-stability issue, not a Multiview one.

### RESOLVED 2026-07-16 — the cross-thread UpdateMetrics SIGBUS is fixed (app stable, both modes)

§4's crash is fixed (commit `6116aa8`). Two parts:

1. **Root cause of the build weirdness (found by an independent RE pass):** `signal_guard::LIB_BASE`
   (a `0`-init static) is only written by `install()`, which was `#[cfg(not(target_os = "android"))]`
   — so on Android it was **never set**. The compiler could then prove `LIB_BASE == 0` on Android and
   dead-code-eliminate every `LIB_BASE`-gated helper (which is also why the SIGSEGV fallback and earlier
   fix attempts silently no-op'd). Fix: `publish_lib_base()` stores `LIB_BASE` on Android **without** the
   SIGSEGV `sigaction` (that's a no-op on Android — ART's `libsigchain` intercepts first — and
   registering it destabilised the process).

2. **The actual fix:** `reassert_update_metrics_on_render_thread()`, called from `run_frame_tick` (which
   runs on the render/GL thread), re-applies the `UpdateMetrics → ret` patch. The one-shot
   `patch_update_metrics` ran on the **main** thread, so the GL thread's instruction fetch kept the stale
   prologue and `UpdateMetrics` still SIGBUS'd on its null "Render Metrics" reporter callback (`blr
   [[DM+0x68]+0x18]`, garbage `0x13`, throttled ~1 Hz → strikes ~1 s in). Re-applying on the GL thread
   lands **that thread's** `isb`, so `UpdateMetrics` returns at its entry and never reaches any reporter
   callback. (`UpdateMetrics` has more than one such callback — the SDK's `FrameMetrics: FPS/drop/early`
   telemetry sinks Unity would provide — so ret-at-entry is cleaner than patching each slot.)

**Device-verified:** was crashing at ~frame 4 (~1 s) deterministically in both Multipass and Multiview;
now runs to frame **#1500+ (25 s+)** stable with live head tracking (DISP euler updating). **Multipass —
the shipping mode — is unblocked.** Multiview stays shelved for the reasons above (no benefit), but its
half of this crash is gone too.

### Re-attempt 2026-07-16 — Multiview now runs *stably* but the right eye is still black (libnr_api, not our stubs)

With the crash fixed, forced Multiview (`stereo_rendering_mode=2`) was re-tested end-to-end to see if
the earlier "right eye works, gray was an artifact" reading held up under a *stable, live* run (the old
crash meant every prior screencap was near a freeze). It does **not**:

- **Runs stably now:** `passes=1`, `arraylen=2`, both blits `read_ok=true draw_ok=true` (layer 0 src=28,
  layer 1 src=30), frame_tick to **#900+**, process ALIVE — no UpdateMetrics SIGBUS. That's the new
  progress the crash fix delivered for Multiview too.
- **Right eye still black:** across 3 screencaps the right half is near-uniform black (grayscale
  stddev ≈ **0.019**, mean ≈ 0.0004) vs the left half's content (stddev ≈ **0.31**). So the compositor
  is **not sampling layer 1** — confirming §"NR blocker", and **retiring** the earlier
  "gray was a measurement artifact" note as wrong (that screencap was a frozen/transient frame).

**Stubbed-callback investigation (the other half of the ask):** whether any Unity-interface callback we
stub with placeholder values gates the layer-1 import. Answer: **no** — see
`codex-stub-callbacks-analysis.md` (RE + independently disassembly-verified here):

- `RegisterTextureProvider` — its argument is Unity's own opaque provider handle (`mov x0, x20` at
  `libXREALXRPlugin.so+0x673a0`), not a callable SDK object; ignoring it loses only Unity-side property
  bookkeeping Godot doesn't use.
- `GetPlatformData` (display iface slot `+0x30`) — **never called** by this SDK binary (no `blr` through
  `DisplayManager+0x08`'s `+0x30` anywhere); our `return 0` is never observed.
- `RegisterDisplayProvider` — only display-state + mirror-view callbacks; unrelated to swapchain/layer setup.
- `gfx_register_device_event_callback` — the SDK never registers a graphics-device event callback.

The layer count and per-viewport layer are built wholly inside `libXREALXRPlugin.so` and sent to NR
correctly (`NRBufferSpecSetMultiviewLayers(2)`, `NRBufferViewportSetMultiviewLayer`); the model is only
lost in `libnr_api.so`'s client-GL-name import (`+0xebeb84` binds `GL_TEXTURE_2D`, never
`GL_TEXTURE_2D_ARRAY`). **Verdict unchanged: Multiview's right eye is unfixable from the Unity-provider
side without patching libnr_api internals, and it carries no perf benefit here → Multipass-only stands.**
`src/session.rs::stereo_rendering_mode()` returns `0`; re-exercising Multiview for future libnr_api RE is
a one-line change (return `2`).

## 2026-07-17 — the reference Unity export resolves the "open tension": Unity uses IMMUTABLE array storage

The "open tension" flagged in the 2026-07-16 sections above ("both apps run GLES3 and both take the
`CreateTexture(color=0)` client-name path, so what is actually different about Unity's GL array texture?")
is now answered. A Unity IL2CPP **Export Project** of the reference app was analysed at
`C:\Users\shien\Documents\Kadinche\Build\layerd_debug`. Crucially this export ships a **`symbols/`
`libunity.so` with the full symbol table + DWARF** (69,363 named functions) alongside the stripped runtime
`libunity.so` — material we never had before (previously only the stripped runtime lib existed). Cross-
referencing the two (disassemble the runtime `.text`, name `bl` targets from the symbols file) let us read
Unity's engine-internal XR texture path directly.

Reproduce: symbols lib `unityLibrary/symbols/arm64-v8a/libunity.so` (`.text` is `NOBITS` — names/DWARF
only); runtime lib `unityLibrary/src/main/jniLibs/arm64-v8a/libunity.so` (real `.text`, stripped). Build an
addr→name map with `llvm-nm --defined-only --demangle` on the symbols lib; disassemble the runtime lib with
`llvm-objdump -d --start-address/--stop-address`; annotate branch targets from the map. Scratchpad scripts:
`disasm.py`, `mapoff.py`.

### 1. Unity's multiview eye texture is a genuine 2D-array — same object type we allocate

`XRTextureManager::SetupRenderTextureFromXRRequest @0xbedcc4` converts the XR plugin's
`UnityXRRenderTextureDesc` into a `RenderTexture`. When `desc.textureArrayLength >= 2` (field at desc+0x58):
`SetDimension(5=Tex2DArray)`, `SetVolumeDepth(textureArrayLength)`, `SetVRUsage(2=TwoEyes)`,
`SetAsEyeTexture(true)`. So at the engine-object level Unity builds **exactly what our port allocates** (a
2-layer `GL_TEXTURE_2D_ARRAY`). There is no secret texture type, and — confirmed separately — **no hidden
C# / SDK lever** (matches the earlier IL2CPP C# RE). This retires the worry that Unity was doing something
categorically different above the GL layer.

### 2. The decisive difference: Unity allocates the array with IMMUTABLE storage (`glTexStorage3D`), we use mutable `glTexImage3D`

`ApiGLES::CreateTexture @0xdd12f8` reaches GL through a function-pointer table on the `ApiGLES` object
(`ldr x8,[x19,#off]; blr x8`). Mapping the offsets via `ApiGLES::Load @0xdb3be4` (each
`adrp+add`=proc-name string → `gles::GetProcAddress_core` → `str x0,[x19,#off]`):

- `[x19+0x510]` = `glTexImage3D`      (mutable 3D/array)
- `[x19+0x530]` = `glTexStorage3DEXT` (immutable 3D/array)
- `[x19+0x548]` = `glCompressedTexImage3D`, `[x19+0x1a8]` = `glTexImage2D`, `[x19+0x538]` = `glTexStorage2DMultisample`, …

The dispatch at `dd150c–dd1530`: for target class `w28 ∈ {3,5,6}` (mask `1<<w28 & 0x68`; 5 = 2D-array) it
sets the `bool&` out-param to 1 (`strb #1,[x25]` = "immutable storage created") and calls
**`glTexStorage3DEXT` (+0x530)** — gated only on the immutable-storage GraphicsCap (`[caps+0x579]`; the
Adreno 710 supports it). `glTexImage3D` (+0x510) is the **fallback** taken only when immutable storage is
unavailable. So Unity's eye array is an **immutable** texture.

Our port (`src/gl.rs::alloc_texture_array`) unconditionally calls `glTexImage3D` → a **mutable**,
single-level array. This is the concrete, previously-unknown engine-side delta the open tension asked for.

### 3. Why immutable storage plausibly explains the black right eye (and the L/R asymmetry)

The actual compositor — `libnr_api.so` (~15 MB system lib; note `+0xebeb84` ≈ 14.7 MB, far larger than the
bundled `libnr_loader.so` 4.7 MB / `libGenie.so` 6.7 MB) — is **not in this export**, so the client-import
code itself can't be re-read here. But: the export's `libXREALXRPlugin.so` and `libnr_loader.so` contain
**no `glTextureView` / `glTexStorage3D` proc-name strings at all**, while `libunity.so` contains
`glTextureView`, `glTextureViewOES/EXT`, `glTexStorage3D(EXT)`. Combined with the archive's libnr_api
finding ("binds `GL_TEXTURE_2D`; array capability is owned-textures only"), the coherent theory is:

> the compositor samples each eye as a plain `GL_TEXTURE_2D`, obtaining per-layer 2D **views** of the array
> (`glTextureView`, which **requires immutable storage**). On the reference app the array is immutable →
> the layer-1 view succeeds → right eye renders. On our port the array is **mutable** → `glTextureView`
> on layer 1 fails (`GL_INVALID_OPERATION`) → right eye samples nothing → **black**, while layer 0 / the
> base still reads → left eye works. This matches the observed L=content / R=black asymmetry exactly (a
> whole-texture-incomplete failure would blacken *both* eyes).

This is a **strong, newly-grounded hypothesis, not a device-confirmed fix** (the view call would live in
libnr_api, absent here). But it upgrades the old "unknown Unity GL parameter" to a specific, testable one.

### 4. The one cheap experiment if Multiview is ever revisited (still not worth shipping)

Change `src/gl.rs::alloc_texture_array` to allocate **immutable** storage:
`glTexStorage3D(GL_TEXTURE_2D_ARRAY, 1, GL_RGBA8, w, h, layers)` (import `glTexStorage3D` from
`libGLESv3.so`; drop the `glTexImage3D` call; set `TEXTURE_BASE_LEVEL=0`/`MAX_LEVEL=0` for completeness),
then A/B on device with `force_multiview 1` and the physical-display screencap (RGB-only stats, `-alpha
remove`). If the right eye lights up, the immutable-view theory is confirmed.

**Verdict unchanged.** Even if this fixes the right eye, Multiview yields **zero** GPU/CPU benefit on our
two-SubViewport rig (both eye cameras are drawn every frame in both modes; single-pass-instanced only pays
off when the *engine* draws both eyes in one instanced pass, which Godot's SubViewport rig does not). So
Multipass remains the shipping path; this section only closes the intellectual loop the export opened.

### 2026-07-17 on-device test — immutable storage does NOT fix the right eye (hypothesis REFUTED)

The §4 experiment was run end-to-end on device (Air 2 Ultra, `debug.xreal.stereo_mode 2`, immutable
`alloc_texture_array`). Result: **the right eye is still fully black.**

- `alloc_texture_array 1968x1134x2 immutable=true` — the immutable allocation **succeeds** (`glTexStorage3D`
  path taken, not the mutable fallback).
- Both eye projections valid (`R: l=-0.3959 r=0.3975 …`, no longer zero), `blit_to_layer … layer=1 src=32
  read_ok=true draw_ok=true` (layer 1 fills fine), `frame_tick #3300+` stable, no crash.
- Physical-display screencap (`4626964537055662852`, `-alpha remove` grayscale): **left stddev=0.314,
  right stddev=0.0 / mean=0.0** — right half is uniformly black. (Multipass on the same build: left
  0.161 / right 0.169 — both eyes render.)

So immutable storage is a real Unity/port difference but **not** the cause of the black right eye. The
compositor (`libnr_api.so`) does not present layer 1 regardless of immutable vs mutable storage — the
`glTextureView` theory in §3 is refuted for this SDK. The wall is genuinely inside libnr_api's
client-GL-name import (`+0xebeb84` binds `GL_TEXTURE_2D`), unreachable from the engine side and not fixable
via storage parameters. **Multiview stays shelved; Multipass is the shipping path.** (One startup caveat
observed: if the app launches while the glasses are *not yet active*, the first session grabs a 0×0 eye
resolution — `alloc_texture_array 0x0x2 immutable=false`, both eyes black — until it is relaunched with the
display live. This is unrelated to Multiview; it's a display-readiness race in the bootstrap retry.)

### 2026-07-17 color-space test — `GL_RGBA8` (UNORM) is the correct eye-texture format (confirmed on device)

Follow-up to the export analysis: the Unity export showed Unity's eye textures are sRGB-typed (Unity runs
in **Linear** color space, so its render targets are `SRGB8_ALPHA8` and the hardware does linear↔sRGB).
Our port allocates `GL_RGBA8` (UNORM) and ignores the descriptor's sRGB flag (`flags=0x12`,
`color_format=0`). Question: is our RGBA8 color-correct, or a latent gamma bug?

A/B'd on device (Multipass, a temporary `debug.xreal.srgb_eye` probe): allocate the eye texture as
`GL_SRGB8_ALPHA8` with the **same bytes** (the probe disabled `GL_FRAMEBUFFER_SRGB` during the blit so no
re-encode — verified `was_enabled=false cap_unsupported=false`, i.e. a verbatim byte copy), changing only
the texture's sRGB *type*. Result: the sRGB-typed eye came out **~26% darker** (L 0.0577→0.0429,
R 0.0643→0.0476; ratio ≈0.74) across both eyes.

Conclusion: the XREAL compositor **passthrough-samples** the eye texture (respecting its sRGB type at
sample time via hardware sRGB→linear decode, then writing to the display without re-encoding). So:

- **`GL_RGBA8` (UNORM) is correct for this port.** Godot's `gl_compatibility` renderer outputs
  display-ready, sRGB-encoded bytes; storing them UNORM (no sample-time decode) shows them verbatim on the
  glasses = correct brightness. This is why our shipped Multipass color looks right (camera passthrough,
  etc.).
- **`GL_SRGB8_ALPHA8` would be wrong for us** (compositor decodes → ~26% too dark).
- Unity gets away with sRGB-typed targets because it renders in *linear* space (its bytes are meant to be
  decoded); our pipeline already outputs encoded bytes, so decoding them double-handles the gamma.

The `srgb_eye` probe was reverted after the test (answer is definitive: RGBA8). This closes the
color-space question the Multiview export analysis raised.

## 2026-07-17 — SOLVED: Multiview renders correctly. The root cause was never the compositor.

**Multiview's black right eye is fixed and it now renders both eyes correctly on device.** Every earlier
section that blames `libnr_api` / "the compositor can't sample layer 1" is **WRONG** — kept above only as
a record of the wrong trail. The actual bug was on *our* side, in how we filled the array layers.

### The decisive experiment: a per-layer colour probe

A temporary `debug.xreal.layer_probe` painted each physical array layer a distinct solid colour via
`glClear` (layer 0 = red, 1 = green, 2 = blue, 3 = white). On-device screencap: **left eye = red (layer 0),
right eye = green (layer 1)**. So the compositor *does* present layer 1 to the right eye — the "libnr_api
imports as GL_TEXTURE_2D, layer 1 unsampleable" verdict was false. (This also refuted a "right eye = layer
2" hypothesis: it's layer 1.)

A second probe blitted the known-good LEFT content into *both* layers with `glBlitFramebuffer`: left eye
showed it, **right eye stayed black**. So `glClear` into layer 1 works but `glBlitFramebuffer` into layer 1
does not — isolating the write op as the culprit.

### Two Adreno GLES driver quirks, both in `blit_texture_to_layer`

1. **`glBlitFramebuffer` into a `glFramebufferTextureLayer` attachment at layer > 0 is a silent no-op**
   on this Adreno driver — `glCheckFramebufferStatus` returns COMPLETE, the blit reports no error, yet
   nothing is written. Layer 0 works (left eye rendered), layer 1 silently didn't (black right eye).
   `glClear` into the same attachment *does* work (hence the colour probe lit layer 1).
2. **`glCopyImageSubData` can write layer > 0, but it is a raw byte copy with no format conversion.** The
   Godot `gl_compatibility` eye SubViewport is not plain `RGBA8`, so copying it directly into the `RGBA8`
   array scrambled the colours (Multiview looked colour-corrupted in *both* eyes vs Multipass — the tell
   that it was a copy-path issue, not a per-eye one).

### The fix (`src/gl.rs::blit_texture_to_layer`)

Blit the eye SubViewport into a persistent **`RGBA8` scratch texture** first (`glBlitFramebuffer`, which
converts the format exactly as the Multipass eye blit does → matching colours), then **`glCopyImageSubData`
the scratch into the array layer** (`RGBA8`→`RGBA8`, exact, and layer > 0 works). Device-verified: right
eye renders, both eyes colour-match Multipass (user-confirmed), L-vs-R parallax present.

### Status

Multiview is now a **working** option (`debug.xreal.stereo_mode 2`); default stays **Multipass** only
because Multiview still buys zero GPU on the two-SubViewport rig (§3 above). The diagnostic `layer_probe`
was removed after the fix; the `glCopyImageSubData`/scratch fix and the `stereo_mode` flag are kept. The
immutable-storage `glTexStorage3D` allocation is retained (harmless, matches Unity) though it was not what
fixed the right eye.

### Why it works but does NOT reduce load — and how Unity actually fills the array (RE)

Our Multiview is now correct but **not** faster than Multipass — in fact it's slightly heavier (the extra
scratch blit + copy). The point of single-pass-instanced is to draw the geometry **once**; our rig never
does that. Confirmed by RE of the reference `libunity.so` (the export):

- **Unity renders both eyes directly into the array in one pass** via `GL_OVR_multiview`. `libunity.so`
  references `glFramebufferTextureMultiviewOVR` / `glFramebufferTextureMultisampleMultiviewOVR` (attach the
  array as a multiview render target), the shader keyword `STEREO_MULTIVIEW_ON`, `#extension
  GL_OVR_multiview2 : require`, and `stereoTargetEyeIndex = int(maliHack[int(gl_ViewID_OVR)].x)` (the
  vertex shader picks the eye/layer from `gl_ViewID_OVR`). The eye array texture **is** the camera's render
  target (`SetAsEyeTexture` + `VRUsage=TwoEyes` + `Tex2DArray`). **No copy.**
- The XREAL XR plugin only exposes `NRBufferSpecSetMultiviewLayers` / `NRBufferViewportSetMultiviewLayer`
  (layer-count metadata); the multiview *rendering* is entirely in the engine (`libunity.so`).

Our port instead draws **two** Godot SubViewports (two full geometry passes — the opposite of single-pass)
and then copies each into a layer. So both the second pass and the copy are pure overhead vs Unity.

**The only way to get the real single-pass win** is for the engine (Godot) to draw both eyes in one
multiview instanced pass straight into the array. Godot *does* support this in its Forward+/Mobile
renderers' native OpenXR path (it uses the same `glFramebufferTextureMultiviewOVR`), but our port uses the
Compatibility renderer + a hand-rolled two-SubViewport rig + a hand-rolled XREAL-SDK emulation, none of
which are multiview. Reaching parity would mean rendering the world once through Godot's native
multiview/XR path into the array and handing that to the XREAL compositor — a substantial redesign, tracked
as a possible future direction, not done here. Until then: **Multiview is correct but Multipass is the
default because it is at least as cheap and simpler.**
