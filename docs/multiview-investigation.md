# Multiview / head-lock investigation — handoff for the next session

Status: **head-lock still unsolved.** The reachable levers are exhausted (device-tested); the last
grounded path is **Multiview**, whose rendering is implemented but **displays nothing** — blocked on the
**NR-compositor (libnr_loader) single-buffer swapchain registration**. This doc hands that off.

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
DisplayManager/InputManager function reachable from `libXREALXRPlugin.so`. **Matching LayeredClient's
Multiview submission is the remaining grounded hope.**

## Why Multiview

LayeredClient (the reference app that renders a correct head-locked peek window with the **same** native
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
- Earlier: `ea616a2` `d916639` `db9b778` (head-pose-pipeline RE, LayeredClient teardown).

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
- LayeredClient Unity source: `C:\Users\shien\Documents\Kadinche\LayeredClient` (its `Layered.XREAL`
  assembly, `Assets/XR/Settings/XREALSettings.asset`, `HeadLockFollower.cs` = app-level HUD, not the lever).
